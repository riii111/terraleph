use std::{
    collections::BTreeSet,
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
};

use super::{
    command::{
        INIT_ARGUMENTS_ENVIRONMENT, ProcessRunner, TerraformCommand, TerraformExecutionError,
        refused_error, run_successful,
    },
    configuration,
};
use crate::{
    app::execution::{ExecutionEvent, InitializationReason, LockFileChange, Tool},
    infra::CancellationToken,
};

const LOCK_FILE: &str = ".terraform.lock.hcl";
const BACKEND_INITIALIZATION_REQUIRED: &str = "Backend initialization required";

// `data_dir` is TF_DATA_DIR as Terraform reads it: relative to the directory it runs in.
pub(crate) fn reason(
    tool: Tool,
    root: &Path,
    data_dir: Option<&OsStr>,
) -> io::Result<Option<InitializationReason>> {
    if !configuration::has_configuration(root, tool)? {
        return Ok(None);
    }
    let has_backend = configuration::read_configuration(root, tool, data_dir)?.has_backend;
    let data_dir = data_directory(root, data_dir);
    let lock = match fs::read_to_string(root.join(LOCK_FILE)) {
        Ok(lock) => Some(Ok(lock)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => Some(Err(error)),
    };
    // Without a backend, a configuration may need no init at all, so only a locked provider or a
    // module call shows that one is needed; Terraform reports anything else on its own.
    if has_backend {
        if !data_dir.is_dir() {
            return Ok(Some(InitializationReason::NotInitialized));
        }
        if !backend_initialized(&data_dir) {
            return Ok(Some(InitializationReason::BackendNotInitialized));
        }
    }
    if let Some(lock) = lock {
        let reason = lock.map_or(Some(InitializationReason::LockFileUnreadable), |lock| {
            missing_provider(&lock, &data_dir)
        });
        if reason.is_some() {
            return Ok(reason);
        }
    }
    let calls = configuration::module_calls(root, tool)?;
    Ok(missing_module(&calls, &data_dir))
}

pub(crate) fn run(
    tool: Tool,
    root: &Path,
    reason: &InitializationReason,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    event_sink: &mut dyn FnMut(ExecutionEvent),
) -> Result<Option<LockFileChange>, TerraformExecutionError> {
    if let Some(option) = env::var_os(INIT_ARGUMENTS_ENVIRONMENT)
        .and_then(|value| refused_option(&value.to_string_lossy()))
    {
        return Err(refused_error(
            tool,
            TerraformCommand::Init,
            format!(
                "{INIT_ARGUMENTS_ENVIRONMENT} requests {option}, which only a manual init may run"
            ),
        ));
    }
    let mut arguments = arguments(reason);
    arguments.push(OsString::from("-no-color"));
    let lock = read_lock(root);
    run_successful(
        tool,
        root,
        TerraformCommand::Init,
        &arguments,
        cancellation,
        runner,
        Some(event_sink),
    )?;
    Ok(lock_file_change(lock, read_lock(root)))
}

// A plan on the user's terminal reports its diagnostics as boxed, possibly colored text.
pub(crate) fn plan_output_reason(line: &str) -> Option<InitializationReason> {
    let text = strip_ansi(line);
    let text = text.trim_start_matches(['│', '╷', '╵', ' ']);
    plan_reason(text.strip_prefix("Error: ")?)
}

pub(crate) fn plan_reason(summary: &str) -> Option<InitializationReason> {
    [
        BACKEND_INITIALIZATION_REQUIRED,
        "Required plugins are not installed",
        "Inconsistent dependency lock file",
        "Module not installed",
        "Module source has changed",
    ]
    .into_iter()
    .find(|prefix| summary.starts_with(prefix))
    .map(|summary| InitializationReason::PlanRequested { summary })
}

// Init never runs with -migrate-state, -reconfigure, or -upgrade, so a change that needs one of
// them fails here and is left to the user. Once a backend is initialized, it is left untouched,
// because running init again without the user's original -backend-config can change it.
fn arguments(reason: &InitializationReason) -> Vec<OsString> {
    let mut arguments = vec![OsString::from("init"), OsString::from("-input=false")];
    let initializes_backend = match reason {
        InitializationReason::NotInitialized | InitializationReason::BackendNotInitialized => true,
        InitializationReason::PlanRequested { summary } => {
            *summary == BACKEND_INITIALIZATION_REQUIRED
        }
        InitializationReason::LockFileUnreadable
        | InitializationReason::ProviderNotInstalled { .. }
        | InitializationReason::ModuleNotInstalled { .. } => false,
    };
    if !initializes_backend {
        arguments.push(OsString::from("-backend=false"));
    }
    arguments
}

fn refused_option(value: &str) -> Option<&'static str> {
    value.split_whitespace().find_map(|argument| {
        let option = argument.trim_start_matches('-');
        let name = option.split_once('=').map_or(option, |(name, _)| name);
        ["migrate-state", "reconfigure", "upgrade", "force-copy"]
            .into_iter()
            .find(|refused| *refused == name)
    })
}

fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            if characters.next() == Some('[') {
                for code in characters.by_ref() {
                    if code.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn data_directory(root: &Path, data_dir: Option<&OsStr>) -> PathBuf {
    data_dir
        .filter(|value| !value.is_empty())
        .map_or_else(|| root.join(".terraform"), |value| root.join(value))
}

fn backend_initialized(data_dir: &Path) -> bool {
    fs::read(data_dir.join("terraform.tfstate"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| {
            value
                .get("backend")
                .is_some_and(serde_json::Value::is_object)
        })
}

// The lock file records installed external providers; built-in providers need no cache.
fn missing_provider(lock: &str, data_dir: &Path) -> Option<InitializationReason> {
    let Ok(body) = hcl::from_str::<hcl::Body>(lock) else {
        return Some(InitializationReason::LockFileUnreadable);
    };
    body.blocks()
        .filter(|block| block.identifier() == "provider")
        .find_map(|block| {
            let Some(provider) = block.labels().first() else {
                return Some(InitializationReason::LockFileUnreadable);
            };
            let Some(version) = block
                .body
                .attributes()
                .find(|attr| attr.key() == "version")
                .and_then(|attr| {
                    if let hcl::Expression::String(value) = &attr.expr {
                        Some(value)
                    } else {
                        None
                    }
                })
            else {
                return Some(InitializationReason::LockFileUnreadable);
            };
            (!data_dir
                .join("providers")
                .join(provider.as_str())
                .join(version)
                .is_dir())
            .then(|| InitializationReason::ProviderNotInstalled {
                provider: provider.as_str().to_owned(),
            })
        })
}

fn missing_module(calls: &BTreeSet<String>, data_dir: &Path) -> Option<InitializationReason> {
    if calls.is_empty() {
        return None;
    }
    let installed = fs::read(data_dir.join("modules/modules.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value.get("Modules")?.as_array().map(|modules| {
                modules
                    .iter()
                    .filter_map(|module| module.get("Key")?.as_str())
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            })
        })
        .unwrap_or_default();
    calls
        .iter()
        .find(|call| !installed.contains(*call))
        .map(|name| InitializationReason::ModuleNotInstalled { name: name.clone() })
}

fn read_lock(root: &Path) -> Option<Vec<u8>> {
    fs::read(root.join(LOCK_FILE)).ok()
}

fn lock_file_change(before: Option<Vec<u8>>, after: Option<Vec<u8>>) -> Option<LockFileChange> {
    match (before, after) {
        (None, Some(_)) => Some(LockFileChange::Created),
        (Some(before), Some(after)) if before != after => Some(LockFileChange::Updated),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(tempfile::TempDir);

    impl Fixture {
        fn new(configuration: &str) -> Self {
            let directory = tempfile::tempdir().expect("fixture directory should be created");
            fs::write(directory.path().join("main.tf"), configuration)
                .expect("configuration should be written");
            Self(directory)
        }

        fn root(&self) -> &Path {
            self.0.path()
        }

        fn write(&self, path: &str, contents: &str) {
            let path = self.root().join(path);
            fs::create_dir_all(path.parent().expect("fixture files have a parent"))
                .expect("fixture parent should be created");
            fs::write(path, contents).expect("fixture file should be written");
        }

        fn reason(&self, data_dir: Option<&str>) -> Option<InitializationReason> {
            reason(Tool::Terraform, self.root(), data_dir.map(OsStr::new))
                .expect("synthetic configuration should be readable")
        }
    }

    const BACKEND: &str = "terraform {\n  backend \"local\" {}\n}\n";
    const BACKEND_METADATA: &str = r#"{"backend":{"type":"local"}}"#;
    const LOCK: &str =
        "provider \"registry.terraform.io/example/synthetic\" {\n  version = \"1.0.0\"\n}\n";

    #[test]
    fn backend_requires_metadata_and_locked_provider_installations() {
        let fixture = Fixture::new(BACKEND);
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::NotInitialized)
        );

        fixture.write(".terraform/modules/.keep", "");
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::BackendNotInitialized)
        );

        fixture.write(".terraform/terraform.tfstate", BACKEND_METADATA);
        assert_eq!(fixture.reason(None), None);

        fixture.write(".terraform.lock.hcl", LOCK);
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::ProviderNotInstalled {
                provider: "registry.terraform.io/example/synthetic".to_owned(),
            })
        );

        fixture.write(
            ".terraform/providers/registry.terraform.io/example/synthetic/1.0.0/.keep",
            "",
        );
        assert_eq!(fixture.reason(None), None);
    }

    #[test]
    fn data_directory_follows_tf_data_dir_relative_to_the_execution_directory() {
        let fixture = Fixture::new(BACKEND);
        fixture.write("custom/terraform.tfstate", BACKEND_METADATA);

        assert_eq!(fixture.reason(Some("custom")), None);
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::NotInitialized)
        );
        assert_eq!(
            fixture.reason(Some("")),
            Some(InitializationReason::NotInitialized)
        );
    }

    #[test]
    fn configuration_without_backend_needs_only_locked_providers_and_modules() {
        let fixture = Fixture::new("resource \"terraform_data\" \"example\" {}\n");
        assert_eq!(fixture.reason(None), None);

        fixture.write(".terraform.lock.hcl", LOCK);
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::ProviderNotInstalled {
                provider: "registry.terraform.io/example/synthetic".to_owned(),
            })
        );
    }

    #[test]
    fn directory_without_configuration_is_not_initialized() {
        let directory = tempfile::tempdir().expect("fixture directory should be created");

        let reason = reason(Tool::Terraform, directory.path(), None)
            .expect("an empty directory should be readable");

        assert_eq!(reason, None);
    }

    #[test]
    fn unreadable_lock_file_requires_initialization() {
        let fixture = Fixture::new(BACKEND);
        fixture.write(".terraform/terraform.tfstate", BACKEND_METADATA);

        for (name, lock) in [
            ("broken", "provider {"),
            ("unlabeled", "provider {\n  version = \"1.0.0\"\n}\n"),
            (
                "versionless",
                "provider \"registry.terraform.io/example/synthetic\" {}\n",
            ),
        ] {
            fixture.write(".terraform.lock.hcl", lock);
            assert_eq!(
                fixture.reason(None),
                Some(InitializationReason::LockFileUnreadable),
                "case: {name}"
            );
        }
    }

    #[test]
    fn module_calls_require_matching_installed_modules() {
        let fixture = Fixture::new(&format!(
            "{BACKEND}module \"network\" {{\n  source = \"./network\"\n}}\n"
        ));
        fixture.write(".terraform/terraform.tfstate", BACKEND_METADATA);
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::ModuleNotInstalled {
                name: "network".to_owned(),
            })
        );

        fixture.write(
            ".terraform/modules/modules.json",
            r#"{"Modules":[{"Key":"","Source":"","Dir":"."},{"Key":"network","Source":"./network","Dir":"network"}]}"#,
        );
        assert_eq!(fixture.reason(None), None);

        fixture.write(
            "storage.tf.json",
            r#"{"module":{"storage":{"source":"./storage"}}}"#,
        );
        assert_eq!(
            fixture.reason(None),
            Some(InitializationReason::ModuleNotInstalled {
                name: "storage".to_owned(),
            })
        );
    }

    #[test]
    fn only_backend_initialization_runs_init_with_the_backend() {
        struct ArgumentCase {
            name: &'static str,
            reason: InitializationReason,
            initializes_backend: bool,
        }

        for case in [
            ArgumentCase {
                name: "not_initialized",
                reason: InitializationReason::NotInitialized,
                initializes_backend: true,
            },
            ArgumentCase {
                name: "backend",
                reason: InitializationReason::BackendNotInitialized,
                initializes_backend: true,
            },
            ArgumentCase {
                name: "plan_backend",
                reason: plan_reason("Backend initialization required, please run init")
                    .expect("backend summary should require init"),
                initializes_backend: true,
            },
            ArgumentCase {
                name: "provider",
                reason: InitializationReason::ProviderNotInstalled {
                    provider: "registry.terraform.io/example/synthetic".to_owned(),
                },
                initializes_backend: false,
            },
            ArgumentCase {
                name: "plan_module",
                reason: plan_reason("Module not installed")
                    .expect("module summary should require init"),
                initializes_backend: false,
            },
        ] {
            let arguments = arguments(&case.reason);

            assert_eq!(
                &arguments[..2],
                ["init", "-input=false"],
                "case: {}",
                case.name
            );
            assert_eq!(
                !arguments.contains(&OsString::from("-backend=false")),
                case.initializes_backend,
                "case: {}",
                case.name
            );
            assert!(
                !arguments.iter().any(|argument| {
                    let argument = argument.to_string_lossy();
                    argument.contains("upgrade")
                        || argument.contains("migrate")
                        || argument.contains("reconfigure")
                }),
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn plan_reason_keeps_only_known_summaries() {
        assert_eq!(
            plan_reason("Required plugins are not installed: example"),
            Some(InitializationReason::PlanRequested {
                summary: "Required plugins are not installed",
            })
        );
        assert_eq!(plan_reason("Invalid reference"), None);
    }

    #[test]
    fn plan_output_reason_reads_boxed_and_colored_errors() {
        assert_eq!(
            plan_output_reason(
                "\u{1b}[31m│\u{1b}[0m \u{1b}[0m\u{1b}[1m\u{1b}[31mError: \u{1b}[0m\u{1b}[0m\u{1b}[1mModule source has changed\u{1b}[0m"
            ),
            Some(InitializationReason::PlanRequested {
                summary: "Module source has changed",
            })
        );
        assert_eq!(
            plan_output_reason("│ Error: Inconsistent dependency lock file"),
            Some(InitializationReason::PlanRequested {
                summary: "Inconsistent dependency lock file",
            })
        );
        assert_eq!(plan_output_reason("│ Module not installed"), None);
        assert_eq!(plan_output_reason("│ Error: Invalid reference"), None);
    }

    #[test]
    fn init_environment_arguments_refuse_migration_reconfiguration_and_upgrades() {
        for (name, value, expected) in [
            ("backend_config", "-backend-config=path=state.tfstate", None),
            (
                "migrate",
                "-backend-config=a -migrate-state",
                Some("migrate-state"),
            ),
            ("reconfigure", "--reconfigure", Some("reconfigure")),
            ("upgrade", "-upgrade=true", Some("upgrade")),
            ("force_copy", "-force-copy", Some("force-copy")),
        ] {
            assert_eq!(refused_option(value), expected, "case: {name}");
        }
    }

    #[test]
    fn lock_file_change_reports_creation_and_updates_only() {
        assert_eq!(
            lock_file_change(None, Some(b"a".to_vec())),
            Some(LockFileChange::Created)
        );
        assert_eq!(
            lock_file_change(Some(b"a".to_vec()), Some(b"b".to_vec())),
            Some(LockFileChange::Updated)
        );
        assert_eq!(
            lock_file_change(Some(b"a".to_vec()), Some(b"a".to_vec())),
            None
        );
        assert_eq!(lock_file_change(None, None), None);
    }
}
