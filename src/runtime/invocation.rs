use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use crate::app::{
    environments::{Environment, EnvironmentAvailability},
    execution::{Tool, VariableSources},
};
use crate::infra::terraform::{
    self,
    configuration::{self, ExecutionLocation},
    discovery,
};

pub use targets::EnvironmentTargets;

mod targets;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subcommand {
    Plan,
    Apply,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "these fields preserve Terraform's independent CLI boolean options"
)]
pub(crate) struct Invocation {
    tool: Tool,
    subcommand: Subcommand,
    launch_root: PathBuf,
    directory: PathBuf,
    global_arguments: Vec<OsString>,
    effective_arguments: Vec<OsString>,
    json: bool,
    auto_approve: bool,
    input: bool,
    saved_plan: Option<OsString>,
    detailed_exitcode: bool,
    initial_overview: bool,
    named_environments: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Delegate,
    Single,
    Multiple,
}

pub(crate) fn run(tool: Tool, targets: &EnvironmentTargets, arguments: &[OsString]) -> ExitCode {
    run_with_mode(tool, targets, arguments, false)
}

pub(crate) fn run_default(targets: &EnvironmentTargets) -> Option<ExitCode> {
    if !interactive(
        [
            io::stdin().is_terminal(),
            io::stdout().is_terminal(),
            io::stderr().is_terminal(),
        ],
        env::var_os("CI").as_deref(),
        env::var_os("TF_IN_AUTOMATION").as_deref(),
    ) {
        return None;
    }
    Some(run_with_mode(
        Tool::Terraform,
        targets,
        &[OsString::from("plan")],
        true,
    ))
}

fn run_with_mode(
    tool: Tool,
    targets: &EnvironmentTargets,
    arguments: &[OsString],
    default_entry: bool,
) -> ExitCode {
    match execute(tool, targets, arguments, default_entry) {
        Ok(exit) => exit,
        Err(error) => {
            super::report_error(&error.to_string());
            ExitCode::from(1)
        }
    }
}

fn execute(
    tool: Tool,
    targets: &EnvironmentTargets,
    arguments: &[OsString],
    default_entry: bool,
) -> io::Result<ExitCode> {
    let executable = terraform::resolve_executable(tool)?;
    let Some(root) = env::current_dir().ok() else {
        if default_entry {
            return Err(io::Error::other(
                "the default Terraform plan requires a local working directory",
            ));
        }
        if !targets.is_empty() {
            return Err(unreviewable_targets());
        }
        return terraform::delegate(&executable, arguments);
    };
    let Some(mut invocation) = review_invocation(tool, arguments, &root) else {
        if default_entry {
            return Err(io::Error::other(
                "the default Terraform plan does not support the current Terraform options; use `terraleph plan` to choose an explicit command",
            ));
        }
        // Delegating would run Terraform in the launch directory instead of the named targets.
        if !targets.is_empty() {
            return Err(unreviewable_targets());
        }
        return terraform::delegate(&executable, arguments);
    };
    invocation.initial_overview = default_entry;
    match select_entry(
        &mut invocation,
        env::var_os("TF_DATA_DIR").as_deref(),
        targets,
    )? {
        Entry::Delegate if default_entry => {
            return Err(io::Error::other(
                "the default Terraform plan supports local execution only; use `terraleph terraform plan` for unsupported backends",
            ));
        }
        Entry::Delegate => return terraform::delegate(&executable, arguments),
        Entry::Single => {}
        Entry::Multiple => {
            let environments = if targets.names_directories() {
                discovery::inspect_targets(&targets.directories(invocation.directory()), tool)?
            } else {
                discover_with_notice(invocation.directory(), tool, targets.max_depth())?
            };
            validate_discovery(&environments, targets.max_depth())?;
            return super::environments::run(&invocation, environments);
        }
    }
    let variable_sources = invocation.variable_sources()?;
    Ok(super::run_invocation(
        &executable,
        &invocation,
        variable_sources,
    ))
}

fn unreviewable_targets() -> io::Error {
    io::Error::other(
        "--env-dir and --max-depth need an interactive plan review; they cannot be used without a terminal or with options that are passed straight to Terraform",
    )
}

fn select_entry(
    invocation: &mut Invocation,
    data_dir: Option<&OsStr>,
    targets: &EnvironmentTargets,
) -> io::Result<Entry> {
    if targets.names_directories() {
        if invocation.is_apply() {
            return Err(io::Error::other(
                "--env-dir selects environments to plan; use `plan`, then apply a reviewed plan with `a`",
            ));
        }
        invocation.prepare_multiple(data_dir)?;
        invocation.named_environments = true;
        return Ok(Entry::Multiple);
    }
    let entry = select_search_entry(invocation, data_dir)?;
    if targets.has_max_depth() && entry != Entry::Multiple {
        return Err(io::Error::other(format!(
            "--max-depth limits the search below a directory without configuration, but {} is not searched; omit --max-depth to use it as one environment",
            invocation.directory().display()
        )));
    }
    Ok(entry)
}

fn select_search_entry(invocation: &mut Invocation, data_dir: Option<&OsStr>) -> io::Result<Entry> {
    let Ok(has_configuration) =
        configuration::has_configuration(invocation.directory(), invocation.tool())
    else {
        return Ok(Entry::Delegate);
    };
    if invocation.is_apply() || has_configuration {
        return Ok(
            if matches!(
                configuration::execution_location_for_tool(
                    invocation.directory(),
                    invocation.tool(),
                    data_dir
                ),
                Ok(ExecutionLocation::Local)
            ) {
                Entry::Single
            } else {
                Entry::Delegate
            },
        );
    }
    invocation.prepare_multiple(data_dir)?;
    Ok(Entry::Multiple)
}

// Reading a large tree takes time before the screen opens, so the search is announced on stderr
// and the line is cleared once the candidates are known.
fn discover_with_notice(root: &Path, tool: Tool, max_depth: usize) -> io::Result<Vec<Environment>> {
    let notice = format!(
        "Searching for environments in {} (up to {max_depth} levels)...",
        root.display()
    );
    let mut stderr = io::stderr();
    let _ = write!(stderr, "{notice}");
    let _ = stderr.flush();
    let environments = discovery::discover(root, tool, max_depth);
    // Overwriting with spaces needs no escape sequence, which a terminal without the TUI would
    // otherwise show as raw text.
    let _ = write!(stderr, "\r{}\r", " ".repeat(notice.chars().count()));
    let _ = stderr.flush();
    environments
}

fn validate_discovery(environments: &[Environment], max_depth: usize) -> io::Result<()> {
    if environments.is_empty() {
        return Err(io::Error::other(format!(
            "No environment candidates: no subdirectory within {max_depth} levels has a backend or cloud block.\nName environments with --env-dir, or search deeper with --max-depth."
        )));
    }
    if !environments.iter().any(Environment::is_available) {
        let reasons = environments
            .iter()
            .filter_map(|environment| match &environment.availability {
                EnvironmentAvailability::ExcludedHcp { directory } => {
                    Some(format!("{}: Excluded: HCP execution", directory.display()))
                }
                EnvironmentAvailability::Error { directory, message } => {
                    Some(format!("{}: Error: {message}", directory.display()))
                }
                EnvironmentAvailability::Available { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(io::Error::other(format!(
            "No executable environments: all candidates are excluded or have errors.\n{reasons}"
        )));
    }
    Ok(())
}

fn review_invocation(tool: Tool, arguments: &[OsString], root: &Path) -> Option<Invocation> {
    let terminals = [
        io::stdin().is_terminal(),
        io::stdout().is_terminal(),
        io::stderr().is_terminal(),
    ];
    if !interactive(
        terminals,
        env::var_os("CI").as_deref(),
        env::var_os("TF_IN_AUTOMATION").as_deref(),
    ) {
        return None;
    }

    let invocation = parse_for_tool(tool, arguments, root, |name| env::var_os(name))?;
    if !invocation.review_candidate() {
        return None;
    }
    Some(invocation)
}

fn interactive(terminals: [bool; 3], ci: Option<&OsStr>, automation: Option<&OsStr>) -> bool {
    terminals.into_iter().all(|terminal| terminal)
        && !ci.is_some_and(|value| {
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        && automation.is_none_or(OsStr::is_empty)
}

impl Invocation {
    const fn review_candidate(&self) -> bool {
        !self.json
            && !self.auto_approve
            && self.saved_plan.is_none()
            && (matches!(self.subcommand, Subcommand::Plan) || self.input)
    }
}

fn parse_for_tool(
    tool: Tool,
    arguments: &[OsString],
    root: &Path,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Option<Invocation> {
    let mut offset = 0;
    let mut directory = root.to_path_buf();
    if let Some(first) = arguments.first()?.to_str() {
        if let Some(value) = first.strip_prefix("-chdir=") {
            if value.is_empty() {
                return None;
            }
            directory = root.join(value);
            offset = 1;
        } else if first == "-chdir" {
            let value = arguments.get(1)?.to_str()?;
            if value.is_empty() {
                return None;
            }
            directory = root.join(value);
            offset = 2;
        }
    }
    let command = arguments.get(offset)?.to_str()?;
    let subcommand = match command {
        "plan" => Subcommand::Plan,
        "apply" => Subcommand::Apply,
        _ => return None,
    };
    let mut effective_arguments = Vec::new();
    for name in ["TF_CLI_ARGS".to_owned(), format!("TF_CLI_ARGS_{command}")] {
        if let Some(value) = lookup(&name) {
            effective_arguments.extend(
                split_arguments(value.to_str()?)?
                    .into_iter()
                    .map(OsString::from),
            );
        }
    }
    effective_arguments.extend_from_slice(&arguments[offset + 1..]);
    let mut invocation = Invocation {
        tool,
        subcommand,
        launch_root: root.to_path_buf(),
        directory,
        global_arguments: arguments[..offset].to_vec(),
        effective_arguments,
        json: false,
        auto_approve: false,
        input: !lookup("TF_INPUT")
            .is_some_and(|value| value == "0" || value.eq_ignore_ascii_case("false")),
        saved_plan: None,
        detailed_exitcode: false,
        initial_overview: false,
        named_environments: false,
    };
    classify_options(&mut invocation)?;
    Some(invocation)
}

fn classify_options(invocation: &mut Invocation) -> Option<()> {
    let mut arguments = invocation.effective_arguments.iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.to_str()?;
        if !argument.starts_with('-') {
            if invocation.subcommand != Subcommand::Apply || invocation.saved_plan.is_some() {
                return None;
            }
            invocation.saved_plan = Some(OsString::from(argument));
            // Go flag parsing stops at the first positional argument.
            if arguments.next().is_some() {
                return None;
            }
            break;
        }
        let option = argument
            .strip_prefix("--")
            .unwrap_or_else(|| &argument[1..]);
        let (name, value) = option
            .split_once('=')
            .map_or((option, None), |(name, value)| (name, Some(value)));
        match name {
            "json" => invocation.json = flag_bool(value)?,
            "auto-approve" if invocation.subcommand == Subcommand::Apply => {
                invocation.auto_approve = flag_bool(value)?;
            }
            "input" => invocation.input = flag_bool(value)?,
            "destroy" | "refresh-only" | "refresh" | "lock" | "compact-warnings" | "no-color" => {
                flag_bool(value)?;
            }
            "detailed-exitcode" if invocation.subcommand == Subcommand::Plan => {
                invocation.detailed_exitcode = flag_bool(value)?;
            }
            "var" | "var-file" | "target" | "replace" | "parallelism" | "lock-timeout" => {
                if value.is_none() {
                    arguments.next()?.to_str()?;
                }
            }
            "out" | "generate-config-out" if invocation.subcommand == Subcommand::Plan => {
                if value.is_none() {
                    arguments.next()?.to_str()?;
                }
            }
            _ => return None,
        }
    }
    Some(())
}

impl Invocation {
    pub(crate) const fn tool(&self) -> Tool {
        self.tool
    }

    pub(crate) fn launch_root(&self) -> &Path {
        &self.launch_root
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn global_arguments(&self) -> &[OsString] {
        &self.global_arguments
    }

    pub(crate) fn plan_arguments(&self) -> Vec<OsString> {
        let mut arguments = self
            .effective_arguments
            .iter()
            .filter(|argument| option_name(argument).is_none_or(|option| option != "auto-approve"))
            .cloned()
            .collect::<Vec<_>>();
        if self.subcommand == Subcommand::Plan {
            arguments
                .retain(|argument| option_name(argument).as_deref() != Some("detailed-exitcode"));
        }
        arguments.push(OsString::from("-detailed-exitcode"));
        arguments
    }

    pub(crate) fn apply_arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        let mut index = 0;
        while index < self.effective_arguments.len() {
            let argument = &self.effective_arguments[index];
            let Some(option) = option_name(argument) else {
                arguments.push(argument.clone());
                index += 1;
                continue;
            };
            if matches!(
                option.as_str(),
                "auto-approve"
                    | "input"
                    | "var"
                    | "var-file"
                    | "target"
                    | "replace"
                    | "refresh"
                    | "refresh-only"
                    | "destroy"
                    | "detailed-exitcode"
                    | "out"
                    | "generate-config-out"
            ) {
                index += 1;
                if !argument.to_string_lossy().contains('=')
                    && matches!(
                        option.as_str(),
                        "var" | "var-file" | "target" | "replace" | "out" | "generate-config-out"
                    )
                {
                    index += 1;
                }
                continue;
            }
            arguments.push(argument.clone());
            index += 1;
        }
        arguments
    }

    pub(crate) const fn detailed_exitcode(&self) -> bool {
        self.detailed_exitcode
    }

    pub(crate) const fn initial_overview(&self) -> bool {
        self.initial_overview
    }

    // A named directory needs no backend block, while a discovered one that loses its block is no
    // longer the candidate the user chose.
    pub(crate) const fn requires_backend(&self) -> bool {
        !self.named_environments
    }

    pub(crate) const fn is_apply(&self) -> bool {
        matches!(self.subcommand, Subcommand::Apply)
    }

    fn prepare_multiple(&mut self, data_dir: Option<&OsStr>) -> io::Result<()> {
        if data_dir.is_some_and(|value| !value.is_empty()) {
            return Err(io::Error::other(
                "TF_DATA_DIR is not supported for multiple environments; use each environment's own data directory.",
            ));
        }
        let root = fs::canonicalize(&self.directory)?;
        let mut index = 0;
        while index < self.effective_arguments.len() {
            let argument = &self.effective_arguments[index];
            let name = option_name(argument)
                .expect("options were classified before environment selection");
            let raw = argument.to_string_lossy();
            let inline = raw.split_once('=');
            if matches!(name.as_str(), "out" | "generate-config-out") {
                return Err(io::Error::other(format!(
                    "-{name} is not supported for multiple environments."
                )));
            }
            if name == "var-file" {
                if let Some((flag, value)) = inline {
                    let mut resolved = OsString::from(format!("{flag}="));
                    resolved.push(root.join(value));
                    self.effective_arguments[index] = resolved;
                } else {
                    index += 1;
                    self.effective_arguments[index] =
                        root.join(&self.effective_arguments[index]).into_os_string();
                }
            } else if inline.is_none()
                && matches!(
                    name.as_str(),
                    "var" | "target" | "replace" | "parallelism" | "lock-timeout"
                )
            {
                index += 1;
            }
            index += 1;
        }
        self.directory = root;
        Ok(())
    }

    fn variable_sources(&self) -> io::Result<VariableSources> {
        variable_sources(&self.directory, &self.effective_arguments)
    }
}

pub(crate) fn variable_sources(
    directory: &Path,
    arguments: &[OsString],
) -> io::Result<VariableSources> {
    let mut automatic_files = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| {
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
            name == "terraform.tfvars"
                || name == "terraform.tfvars.json"
                || name.ends_with(".auto.tfvars")
                || name.ends_with(".auto.tfvars.json")
        })
        .collect::<Vec<_>>();
    automatic_files.sort();

    let mut explicit_files = Vec::new();
    let mut has_var_argument = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let Some(option) = option_name(argument) else {
            index += 1;
            continue;
        };
        let raw_argument = argument.to_string_lossy();
        let inline_value = raw_argument
            .split_once('=')
            .and_then(|(name, value)| (name.trim_start_matches('-') == option).then_some(value));
        let name = option.as_str();
        match name {
            "var" => has_var_argument = true,
            "var-file" => {
                let value = inline_value.map(str::to_owned).or_else(|| {
                    arguments
                        .get(index + 1)
                        .map(|value| value.to_string_lossy().into_owned())
                });
                if let Some(value) = value {
                    let path = Path::new(&value);
                    explicit_files.push(if path.is_absolute() {
                        path.to_owned()
                    } else {
                        directory.join(path)
                    });
                }
                if inline_value.is_none() {
                    index += 1;
                }
            }
            _ => {}
        }
        index += 1;
    }

    let mut environment_variables = env::vars_os()
        .filter_map(|(name, _)| {
            let name = name.to_string_lossy();
            name.starts_with("TF_VAR_").then(|| name.into_owned())
        })
        .collect::<Vec<_>>();
    environment_variables.sort();

    Ok(VariableSources::new(
        automatic_files,
        explicit_files,
        has_var_argument,
        environment_variables,
    ))
}

fn option_name(argument: &OsStr) -> Option<String> {
    let argument = argument.to_string_lossy();
    let argument = argument.strip_prefix('-')?;
    let argument = argument.strip_prefix('-').unwrap_or(argument);
    Some(
        argument
            .split_once('=')
            .map_or(argument, |(name, _)| name)
            .to_owned(),
    )
}

fn flag_bool(value: Option<&str>) -> Option<bool> {
    match value {
        None | Some("1" | "t" | "T" | "TRUE" | "true" | "True") => Some(true),
        Some("0" | "f" | "F" | "FALSE" | "false" | "False") => Some(false),
        _ => None,
    }
}

fn split_arguments(value: &str) -> Option<Vec<String>> {
    let mut arguments = Vec::new();
    let mut argument = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some('"'), '"') | (Some('\''), '\'') => quote = None,
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None | Some('"'), '\\') => {
                argument.push(chars.next()?);
                started = true;
            }
            (None, ch) if ch.is_whitespace() => {
                if started {
                    arguments.push(std::mem::take(&mut argument));
                    started = false;
                }
            }
            _ => {
                argument.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        arguments.push(argument);
    }
    Some(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use tempfile::TempDir;

    fn parse(
        arguments: &[OsString],
        root: &Path,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> Option<Invocation> {
        parse_for_tool(Tool::Terraform, arguments, root, lookup)
    }

    fn invocation(args: &[&str], environment: &[(&str, &str)]) -> Option<Invocation> {
        parse(
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            Path::new("/root"),
            |name| {
                environment
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            },
        )
    }

    #[test]
    fn environment_options_precede_explicit_options_without_deduplicating_values() {
        let parsed = invocation(
            &["plan", "-var", "name=last value", "-json=false"],
            &[
                ("TF_CLI_ARGS", "-var 'name=first value' -json"),
                ("TF_CLI_ARGS_plan", "-var-file=second.tfvars"),
                ("TF_CLI_ARGS_apply", "-json"),
            ],
        )
        .expect("known options");

        assert!(parsed.review_candidate());
        assert_eq!(
            parsed.effective_arguments,
            [
                "-var",
                "name=first value",
                "-json",
                "-var-file=second.tfvars",
                "-var",
                "name=last value",
                "-json=false"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn opentofu_uses_the_shared_cli_argument_environment_contract() {
        let parsed = parse_for_tool(
            Tool::OpenTofu,
            &[OsString::from("plan"), OsString::from("-refresh=true")],
            Path::new("/root"),
            |name| match name {
                "TF_CLI_ARGS" => Some(OsString::from("-input=false")),
                "TF_CLI_ARGS_plan" => Some(OsString::from("-refresh=false")),
                _ => None,
            },
        )
        .expect("OpenTofu options should be recognized");

        assert_eq!(parsed.tool(), Tool::OpenTofu);
        assert!(parsed.review_candidate());
        assert_eq!(
            parsed.plan_arguments()[..2],
            ["-input=false", "-refresh=false"].map(OsString::from)
        );
    }

    #[rstest]
    #[case::json(&["plan", "-json"], &[])]
    #[case::input_disabled(&["apply", "-input=false"], &[])]
    #[case::environment_input(&["apply"], &[("TF_INPUT", "0")])]
    #[case::saved_plan(&["apply", "saved.tfplan"], &[])]
    fn noninteractive_options_delegate(
        #[case] args: &[&str],
        #[case] environment: &[(&str, &str)],
    ) {
        assert!(
            !invocation(args, environment)
                .expect("recognized arguments")
                .review_candidate()
        );
    }

    #[test]
    fn boolean_precedence_uses_last_value_in_original_subcommand() {
        let parsed = invocation(
            &["apply", "-auto-approve=false", "-input=true"],
            &[
                ("TF_CLI_ARGS", "-json -auto-approve=false"),
                ("TF_CLI_ARGS_apply", "-json=false -auto-approve"),
                ("TF_CLI_ARGS_plan", "-json"),
                ("TF_INPUT", "0"),
            ],
        )
        .expect("valid flags");
        assert!(parsed.review_candidate());
    }

    #[rstest]
    #[case::help(&["plan", "-help"], &[])]
    #[case::version(&["-version"], &[])]
    #[case::missing_value(&["plan", "-var"], &[])]
    #[case::invalid_boolean(&["plan", "-json=maybe"], &[])]
    #[case::bad_quotes(&["plan"], &[("TF_CLI_ARGS", "-var 'unterminated")])]
    #[case::global_in_environment(&["plan"], &[("TF_CLI_ARGS", "-chdir=other")])]
    fn unclassified_arguments_delegate(
        #[case] args: &[&str],
        #[case] environment: &[(&str, &str)],
    ) {
        assert!(invocation(args, environment).is_none());
    }

    #[test]
    fn chdir_forms_resolve_from_the_original_directory_and_remain_global() {
        struct ChdirCase {
            name: &'static str,
            arguments: &'static [&'static str],
            expected_directory: &'static str,
            expected_global_arguments: &'static [&'static str],
            expected_apply: bool,
        }

        for case in [
            ChdirCase {
                name: "equals_form",
                arguments: &["-chdir=directory with spaces", "plan"],
                expected_directory: "/root/directory with spaces",
                expected_global_arguments: &["-chdir=directory with spaces"],
                expected_apply: false,
            },
            ChdirCase {
                name: "separate_form",
                arguments: &["-chdir", "directory", "apply"],
                expected_directory: "/root/directory",
                expected_global_arguments: &["-chdir", "directory"],
                expected_apply: true,
            },
        ] {
            let parsed = invocation(case.arguments, &[]).expect("chdir");

            assert_eq!(
                parsed.directory,
                Path::new(case.expected_directory),
                "case: {}",
                case.name
            );
            assert_eq!(
                parsed.global_arguments,
                case.expected_global_arguments
                    .iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>(),
                "case: {}",
                case.name
            );
            assert_eq!(
                parsed.is_apply(),
                case.expected_apply,
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn apply_plan_and_apply_arguments_have_separate_option_ownership() {
        let parsed = invocation(
            &[
                "apply",
                "-var",
                "name=value",
                "-target=terraform_data.api",
                "-parallelism",
                "4",
                "-lock-timeout",
                "30s",
                "-lock=false",
                "-compact-warnings",
                "-auto-approve=false",
            ],
            &[("TF_CLI_ARGS_apply", "-var-file=env.tfvars")],
        )
        .expect("apply arguments");

        assert_eq!(
            parsed.plan_arguments(),
            [
                "-var-file=env.tfvars",
                "-var",
                "name=value",
                "-target=terraform_data.api",
                "-parallelism",
                "4",
                "-lock-timeout",
                "30s",
                "-lock=false",
                "-compact-warnings",
                "-detailed-exitcode",
            ]
            .map(OsString::from)
        );
        assert_eq!(
            parsed.apply_arguments(),
            [
                "-parallelism",
                "4",
                "-lock-timeout",
                "30s",
                "-lock=false",
                "-compact-warnings",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn plan_only_options_are_not_passed_to_apply() {
        let parsed = invocation(
            &[
                "plan",
                "-detailed-exitcode",
                "-out=user.tfplan",
                "-generate-config-out",
                "generated.tf",
                "-lock=false",
            ],
            &[],
        )
        .expect("plan arguments");

        assert_eq!(parsed.apply_arguments(), [OsString::from("-lock=false")]);
    }

    #[test]
    fn variable_sources_keep_only_file_names_and_argument_presence() {
        let fixture = TempDir::new().expect("variable source fixture should be created");
        let directory = fixture.path();
        fs::write(
            directory.join("terraform.tfvars"),
            "secret = \"must not be shown\"",
        )
        .expect("automatic variable file should be created");
        fs::write(
            directory.join("ignored.txt"),
            "secret = \"must not be shown\"",
        )
        .expect("ignored variable file should be created");

        let sources = variable_sources(
            directory,
            &[
                OsString::from("-var-file=explicit.tfvars"),
                OsString::from("-var-file"),
                OsString::from("nested.tfvars"),
                OsString::from("-var"),
                OsString::from("name=secret"),
            ],
        )
        .expect("variable sources should be collected");

        assert_eq!(
            sources.automatic_files(),
            &[directory.join("terraform.tfvars")]
        );
        assert_eq!(
            sources.explicit_files(),
            &[
                directory.join("explicit.tfvars"),
                directory.join("nested.tfvars")
            ]
        );
        assert!(sources.has_var_argument());
        let debug = format!("{sources:?}");
        assert!(!debug.contains("must not be shown"));
        assert!(!debug.contains("name=secret"));
    }

    #[rstest]
    #[case::implicit(&["plan"], false)]
    #[case::enabled(&["plan", "-detailed-exitcode"], true)]
    #[case::disabled(&["plan", "-detailed-exitcode=false"], false)]
    fn detailed_exit_code_is_preserved_only_when_requested(
        #[case] args: &[&str],
        #[case] expected: bool,
    ) {
        let parsed = invocation(args, &[]).expect("plan arguments");
        assert_eq!(parsed.detailed_exitcode(), expected);
        assert_eq!(
            parsed.plan_arguments().last(),
            Some(&OsString::from("-detailed-exitcode"))
        );
    }

    #[test]
    fn environment_splitting_preserves_literal_expansions_and_empty_values() {
        assert_eq!(
            split_arguments(r#"-var 'x=$HOME $(touch marker)' -var "x=`id`" ''"#),
            Some(vec![
                "-var".into(),
                "x=$HOME $(touch marker)".into(),
                "-var".into(),
                "x=`id`".into(),
                String::new()
            ])
        );
    }

    #[test]
    fn terminal_and_automation_conditions_control_interactivity() {
        for (name, terminals, ci, automation, expected) in [
            ("interactive", [true; 3], None, None, true),
            ("stdin", [false, true, true], None, None, false),
            ("stdout", [true, false, true], None, None, false),
            ("stderr", [true, true, false], None, None, false),
            ("ci", [true; 3], Some("true"), None, false),
            ("ci_false", [true; 3], Some("false"), None, true),
            ("ci_zero", [true; 3], Some("0"), None, true),
            (
                "automation_false_is_active",
                [true; 3],
                None,
                Some("false"),
                false,
            ),
            ("empty_automation", [true; 3], None, Some(""), true),
        ] {
            assert_eq!(
                interactive(terminals, ci.map(OsStr::new), automation.map(OsStr::new)),
                expected,
                "{name}"
            );
        }
    }

    mod environments;
}
