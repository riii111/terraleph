use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "terraleph-entry-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("parent/dev")).unwrap();
        fs::write(
            root.join("parent/dev/main.tf"),
            "terraform {\n backend \"local\" {}\n}",
        )
        .unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn parse(&self, tool: Tool, args: &[&str], environment: &[(&str, &str)]) -> Invocation {
        parse_for_tool(
            tool,
            &args.iter().map(OsString::from).collect::<Vec<_>>(),
            &self.0,
            |name| {
                environment
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            },
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn chdir_configuration_selects_single_before_any_child_discovery() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("parent/main.tf"), "").unwrap();
    let mut invocation = fixture.parse(
        Tool::Terraform,
        &["-chdir=parent", "plan", "-out=review.plan"],
        &[],
    );

    assert_eq!(
        select_entry(
            &mut invocation,
            Some(OsStr::new("custom-data")),
            &EnvironmentTargets::default()
        )
        .unwrap(),
        Entry::Single
    );
    assert_eq!(
        invocation.effective_arguments,
        [OsString::from("-out=review.plan")]
    );
}

#[rstest]
#[case::broken("terraform {")]
#[case::hcp("terraform {\n cloud {}\n}")]
fn indeterminate_or_hcp_parent_delegates_without_discovering_children(#[case] source: &str) {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("parent/main.tf"), source).unwrap();
    let mut invocation = fixture.parse(Tool::Terraform, &["-chdir=parent", "plan", "-out=x"], &[]);

    assert_eq!(
        select_entry(&mut invocation, None, &EnvironmentTargets::default()).unwrap(),
        Entry::Delegate
    );
}

#[test]
fn apply_in_an_empty_parent_never_selects_multiple_environments() {
    let fixture = Fixture::new();
    let mut invocation = fixture.parse(Tool::Terraform, &["-chdir=parent", "apply"], &[]);

    assert_eq!(
        select_entry(&mut invocation, None, &EnvironmentTargets::default()).unwrap(),
        Entry::Single
    );
}

#[test]
fn opentofu_parent_files_prevent_terraform_child_discovery() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("parent/main.tofu"), "").unwrap();
    let mut invocation = fixture.parse(Tool::OpenTofu, &["-chdir=parent", "plan"], &[]);

    assert_eq!(
        select_entry(&mut invocation, None, &EnvironmentTargets::default()).unwrap(),
        Entry::Single
    );
}

#[rstest]
#[case::out_separate(&["--out", "plan"], &[], "-out")]
#[case::generate_inline(&["-generate-config-out=imports.tf"], &[], "-generate-config-out")]
#[case::environment(&[], &[("TF_CLI_ARGS_plan", "-out=plan")], "-out")]
fn multiple_environment_options_are_rejected_before_discovery(
    #[case] args: &[&str],
    #[case] environment: &[(&str, &str)],
    #[case] expected: &str,
) {
    let fixture = Fixture::new();
    let mut arguments = vec!["-chdir=parent", "plan"];
    arguments.extend_from_slice(args);
    let mut invocation = fixture.parse(Tool::Terraform, &arguments, environment);

    let error = select_entry(&mut invocation, None, &EnvironmentTargets::default()).unwrap_err();

    assert!(error.to_string().contains(expected));
}

#[test]
fn multiple_var_files_resolve_at_chdir_root_and_preserve_option_values_and_order() {
    let fixture = Fixture::new();
    let absolute = fixture.0.join("absolute.tfvars");
    let mut invocation = fixture.parse(
        Tool::OpenTofu,
        &[
            "-chdir=parent",
            "plan",
            "-var-file",
            "vars with spaces.tfvars",
            "--var-file",
            absolute.to_str().unwrap(),
            "-var",
            "-out=not-an-option",
            "-target=terraform_data.a",
        ],
        &[("TF_CLI_ARGS_plan", "-var-file=shared.tfvars -parallelism=2")],
    );
    fs::write(
        fixture.0.join("parent/dev/local.auto.tfvars"),
        "name = \"synthetic\"",
    )
    .unwrap();

    assert_eq!(
        select_entry(&mut invocation, None, &EnvironmentTargets::default()).unwrap(),
        Entry::Multiple
    );
    assert_eq!(invocation.tool(), Tool::OpenTofu);
    assert_eq!(invocation.directory(), fixture.0.join("parent"));
    assert_eq!(
        invocation.plan_arguments(),
        vec![
            OsString::from(format!(
                "-var-file={}",
                fixture.0.join("parent/shared.tfvars").display()
            )),
            OsString::from("-parallelism=2"),
            OsString::from("-var-file"),
            fixture
                .0
                .join("parent/vars with spaces.tfvars")
                .into_os_string(),
            OsString::from("--var-file"),
            absolute.clone().into_os_string(),
            OsString::from("-var"),
            OsString::from("-out=not-an-option"),
            OsString::from("-target=terraform_data.a"),
            OsString::from("-detailed-exitcode"),
        ]
    );
    let sources =
        variable_sources(&fixture.0.join("parent/dev"), &invocation.plan_arguments()).unwrap();
    assert_eq!(
        sources.automatic_files(),
        &[fixture.0.join("parent/dev/local.auto.tfvars")]
    );
    assert_eq!(
        sources.explicit_files(),
        &[
            fixture.0.join("parent/shared.tfvars"),
            fixture.0.join("parent/vars with spaces.tfvars"),
            absolute
        ]
    );
}

fn targets(arguments: &[&str]) -> EnvironmentTargets {
    EnvironmentTargets::parse_leading(&arguments.iter().map(OsString::from).collect::<Vec<_>>())
        .unwrap()
        .0
}

#[test]
fn named_targets_override_the_single_environment_of_a_configured_directory() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("parent/main.tf"), "").unwrap();
    let mut invocation = fixture.parse(Tool::Terraform, &["-chdir=parent", "plan"], &[]);

    assert_eq!(
        select_entry(&mut invocation, None, &targets(&["--env-dir", "dev"])).unwrap(),
        Entry::Multiple
    );
    assert_eq!(invocation.directory(), fixture.0.join("parent"));
    assert!(!invocation.requires_backend());
}

#[test]
fn named_targets_are_rejected_for_apply() {
    let fixture = Fixture::new();
    let mut invocation = fixture.parse(Tool::Terraform, &["-chdir=parent", "apply"], &[]);

    let error = select_entry(&mut invocation, None, &targets(&["--env-dir", "dev"])).unwrap_err();

    assert!(error.to_string().contains("--env-dir"), "{error}");
}
