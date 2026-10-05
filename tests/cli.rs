use std::process::Command;

use rstest::rstest;

#[rstest]
#[case::help("--help")]
#[case::version("--version")]
fn help_and_version_work_without_a_terminal_or_terraform(#[case] arg: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_terraleph"))
        .env("PATH", "")
        .arg(arg)
        .output()
        .expect("CLI should start");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        !output.stdout.contains(&0x1b),
        "CLI must not initialize a TUI"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("terraleph"));
}

#[test]
fn no_arguments_without_a_terminal_prints_help_without_terraform() {
    let output = Command::new(env!("CARGO_BIN_EXE_terraleph"))
        .env("PATH", "")
        .env_remove("CI")
        .env_remove("TF_IN_AUTOMATION")
        .output()
        .expect("CLI should start");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: terraleph"));
}

#[cfg(unix)]
mod pty_tests {
    use rstest::rstest;
    use std::{
        env,
        ffi::OsStr,
        fs,
        os::unix::{ffi::OsStrExt, fs::PermissionsExt},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    mod environment_plans {
        use super::*;
        use std::fmt::Write;

        fn fixture(names: &[&str]) -> Fixture {
            let fixture = Fixture::new();
            fs::remove_file(fixture.root.join("main.tf")).unwrap();
            for name in names {
                let directory = fixture.root.join(name);
                fs::create_dir(&directory).unwrap();
                fs::write(
                    directory.join("main.tf"),
                    "terraform {\n backend \"local\" {}\n}",
                )
                .unwrap();
            }
            fixture
        }

        fn calls(fixture: &Fixture, command: &str) -> Vec<String> {
            fs::read_to_string(&fixture.invocations)
                .unwrap()
                .lines()
                .filter_map(|line| {
                    let (directory, arguments) = line.split_once('|').unwrap();
                    (arguments.split_whitespace().next() == Some(command)).then(|| {
                        Path::new(directory)
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .into_owned()
                    })
                })
                .collect()
        }

        fn assert_clean(fixture: &Fixture, result: &PtyResult) {
            assert!(result.restored);
            let paths = fs::read_to_string(fixture.plan_path_record.with_extension("all")).unwrap();
            assert!(
                paths.lines().all(|path| !Path::new(path).exists()),
                "{paths}"
            );
            assert!(calls(fixture, "apply").is_empty());
            assert_eq!(
                fs::read_dir(fixture.directory.join("owned-plans"))
                    .unwrap()
                    .count(),
                0
            );
        }

        #[test]
        fn sequential_plans_preserve_options_and_clean_all_results() {
            for (scenario, expected, plan_count, init_count) in [
                ("env_success", 0, 2, 2),
                ("env_detailed", 2, 2, 2),
                ("env_init_failure", 1, 1, 2),
                ("env_excluded", 1, 1, 1),
                ("env_reinit", 0, 3, 2),
                ("env_reinit_failure", 1, 3, 2),
            ] {
                let fixture = fixture(&["a-ready", "b-other"]);
                let other = fixture.root.join("b-other");
                match scenario {
                    "env_init_failure" => {
                        fs::write(other.join("fail-init"), "").unwrap();
                    }
                    "env_excluded" => {
                        fs::write(other.join("main.tf"), "terraform {\n cloud {}\n}").unwrap();
                    }
                    "env_reinit" | "env_reinit_failure" => {
                        fs::create_dir(other.join(".terraform")).unwrap();
                        fs::write(
                            other.join(".terraform/terraform.tfstate"),
                            r#"{"backend":{"type":"local"}}"#,
                        )
                        .unwrap();
                        fs::write(other.join("require-init"), "").unwrap();
                        if scenario == "env_reinit_failure" {
                            fs::write(other.join("always-reinit"), "").unwrap();
                        }
                    }
                    _ => {}
                }
                let args = if scenario == "env_detailed" {
                    vec![
                        "-detailed-exitcode",
                        "-parallelism=3",
                        "-input=true",
                        "-var-file=common.tfvars",
                    ]
                } else {
                    Vec::new()
                };
                let result = fixture.run_with_arguments(
                    scenario,
                    120,
                    40,
                    "tofu",
                    &[&["plan"], args.as_slice()].concat(),
                );

                assert_eq!(result.exit_code, expected, "{scenario}");
                let plans = calls(&fixture, "plan");
                assert_eq!(plans[0], "a-ready");
                assert_eq!(plans.len(), plan_count, "{scenario}");
                assert_eq!(calls(&fixture, "init").len(), init_count, "{scenario}");
                let invocations = fs::read_to_string(&fixture.invocations).unwrap();
                for line in invocations.lines().filter(|line| line.contains("|plan ")) {
                    assert!(line.ends_with("-json -input=false -detailed-exitcode"));
                    if scenario == "env_detailed" {
                        assert!(line.contains("-parallelism=3"));
                        assert!(line.contains(&format!(
                            "-var-file={}/common.tfvars",
                            fixture.root.canonicalize().unwrap().display()
                        )));
                    }
                }
                assert!(fixture.invoked_tools().iter().all(|tool| tool == "tofu"));
                assert!(
                    fixture
                        .forwarded_cli_arguments()
                        .iter()
                        .all(|line| line.ends_with('='))
                );
                assert_clean(&fixture, &result);
            }
        }

        fn plan_output(fixture: &Fixture, environment: &str) -> String {
            fs::read_to_string(&fixture.invocations)
                .unwrap()
                .lines()
                .filter_map(|line| line.split_once('|'))
                .find(|(directory, arguments)| {
                    Path::new(directory).file_name().unwrap() == environment
                        && arguments.starts_with("plan ")
                })
                .and_then(|(_, arguments)| arguments.split("-out=").nth(1))
                .map(|path| path.split_whitespace().next().unwrap().to_owned())
                .unwrap()
        }

        #[test]
        fn plan_detail_applies_only_the_open_environment_saved_plan() {
            let fixture = fixture(&["a-dev", "b-stg"]);

            let result = fixture.run("env_apply", 120, 40);

            assert_eq!(result.exit_code, 0);
            result.assert_restored();
            result.observed("env_plan_detail");
            result.observed("env_apply_confirmation");
            result.observed("env_apply_success");
            assert_eq!(calls(&fixture, "plan"), ["a-dev", "b-stg"]);
            assert_eq!(calls(&fixture, "apply"), ["b-stg"]);
            let reviewed = plan_output(&fixture, "b-stg");
            let other = plan_output(&fixture, "a-dev");
            let apply = fs::read_to_string(&fixture.invocations)
                .unwrap()
                .lines()
                .find_map(|line| {
                    line.split_once('|')
                        .map(|(_, arguments)| arguments)
                        .filter(|arguments| arguments.starts_with("apply "))
                })
                .unwrap()
                .to_owned();
            assert!(apply.starts_with("apply -json -input=false "), "{apply}");
            assert!(apply.ends_with(&reviewed), "{apply}");
            assert!(!apply.contains(&other), "{apply}");
            let paths = fs::read_to_string(fixture.plan_path_record.with_extension("all")).unwrap();
            assert!(
                paths.lines().all(|path| !Path::new(path).exists()),
                "{paths}"
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn non_utf8_environment_applies_its_reviewed_saved_plan() {
            let fixture = fixture(&["a-dev"]);
            let name = OsStr::from_bytes(b"b-stg-\xff");
            let directory = fixture.root.join(name);
            fs::create_dir(&directory).unwrap();
            fs::write(
                directory.join("main.tf"),
                "terraform {\n backend \"local\" {}\n}",
            )
            .unwrap();

            let result = fixture.run("env_apply", 120, 40);

            assert_eq!(result.exit_code, 0);
            result.assert_restored();
            result.observed("env_apply_success");
            let invocations = fs::read(&fixture.invocations).unwrap();
            let calls: Vec<_> = invocations
                .split(|byte| *byte == b'\n')
                .filter_map(|line| {
                    let separator = line.iter().position(|byte| *byte == b'|')?;
                    let directory = Path::new(OsStr::from_bytes(&line[..separator]));
                    let arguments = String::from_utf8(line[separator + 1..].to_vec()).unwrap();
                    Some((directory.file_name().unwrap(), arguments))
                })
                .collect();
            let applies: Vec<_> = calls
                .iter()
                .filter(|(_, arguments)| arguments.starts_with("apply "))
                .collect();
            assert_eq!(applies.len(), 1, "{calls:?}");
            assert_eq!(applies[0].0, name);
            let reviewed = calls
                .iter()
                .find(|(directory, arguments)| *directory == name && arguments.starts_with("plan "))
                .and_then(|(_, arguments)| arguments.split("-out=").nth(1))
                .map(|path| path.split_whitespace().next().unwrap())
                .unwrap();
            assert!(applies[0].1.ends_with(reviewed), "{calls:?}");
        }

        #[test]
        fn no_argument_plan_discovers_multiple_environments_and_opens_the_matrix() {
            let fixture = fixture(&["a-dev", "b-stg", "c-prod"]);
            fs::write(&fixture.show_json, OVERVIEW_PLAN_JSON).unwrap();
            fs::write(&fixture.show_text, OVERVIEW_PLAN_TEXT).unwrap();

            let result = fixture.run_with_arguments("env_default_matrix", 120, 40, "", &[]);

            assert_eq!(result.exit_code, 0);
            result.observed("default_matrix");
            assert_eq!(calls(&fixture, "plan").len(), 3);
            assert!(
                fixture
                    .invoked_tools()
                    .iter()
                    .all(|tool| tool == "terraform")
            );
            assert_clean(&fixture, &result);
        }

        #[test]
        fn pty_multi_environment_relations_open_the_selected_raw_plan_and_restore_terminal() {
            let fixture = fixture(&["a-dev", "b-stg", "c-prod"]);
            fixture.use_overview_relations_plan();

            let result = fixture.run("env_relations", 165, 50);

            assert_eq!(result.exit_code, 0);
            result.assert_restored();
            result.observed("relations_pane");
            result.observed("relations_environment_switched");
            result.observed("relations_raw_plan");
            result.observed("relations_overview_restored");
            assert_eq!(calls(&fixture, "plan").len(), 3);
            assert_clean(&fixture, &result);
        }

        #[rstest]
        #[case::small(80, 24)]
        #[case::large(160, 60)]
        #[case::narrow(40, 16)]
        fn matrix_filters_members_and_restores_the_selected_cell(
            #[case] columns: u16,
            #[case] rows: u16,
        ) {
            let fixture = fixture(&["a-dev", "b-stg", "c-prod"]);
            let changes: Vec<_> = (90..110).map(|index| serde_json::json!({
                "address": format!("terraform_data.server[{index}]"),
                "change": {"actions": ["update"], "before": {"input": "old"}, "after": {"input": "new"}}
            })).collect();
            fs::write(&fixture.show_json, serde_json::json!({"format_version": "1.0", "applyable": true, "resource_changes": changes}).to_string()).unwrap();
            let text = (90..110).fold(String::new(), |mut text, index| { let _ = write!(text, "  # terraform_data.server[{index}] will be updated in-place\n  ~ resource \"terraform_data\" \"server\" {{\n    ~ input = \"old\" -> \"new\"\n  }}\n\n"); text });
            fs::write(&fixture.show_text, format!("Terraform will perform the following actions:\n\n{text}Plan: 0 to add, 20 to change, 0 to destroy.\n")).unwrap();

            let result = fixture.run("env_matrix", columns, rows);

            assert_eq!(result.exit_code, 0);
            result.observed("restored_matrix_selection");
            assert_clean(&fixture, &result);
        }

        #[test]
        fn matrix_reaches_twelfth_environment_by_columns_and_raw_tabs() {
            let names: Vec<_> = (0..12).map(|index| format!("env-{index:02}")).collect();
            let fixture = fixture(&names.iter().map(String::as_str).collect::<Vec<_>>());

            let result = fixture.run("env_many", 80, 24);

            assert_eq!(result.exit_code, 0);
            result.observed("twelfth_environment");
            result.observed("eleventh_environment");
            assert_clean(&fixture, &result);
        }

        #[test]
        fn show_failure_remains_visible_alongside_successful_plan_warnings() {
            let fixture = fixture(&["a-ready", "b-error"]);
            for marker in ["warning-plan", "invalid-show"] {
                fs::write(fixture.root.join("b-error").join(marker), "").unwrap();
            }

            let result = fixture.run("env_show_failure", 120, 40);

            assert_eq!(result.exit_code, 1);
            result.observed("warning_and_failure");
            assert_clean(&fixture, &result);
        }

        #[test]
        fn interrupted_child_stops_the_queue_and_returns_130() {
            let fixture = fixture(&["a-interrupted", "z-pending"]);
            fs::write(fixture.root.join("a-interrupted/interrupt-plan"), "").unwrap();

            let result = fixture.run("env_child_interrupt", 80, 24);

            assert_eq!(result.exit_code, 130);
            assert_eq!(calls(&fixture, "plan"), ["a-interrupted"]);
            assert_clean(&fixture, &result);
        }

        #[test]
        fn termination_signal_stops_the_running_plan_and_removes_every_owned_plan() {
            let fixture = fixture(&["a-ready", "z-slow"]);
            fs::write(fixture.root.join("z-slow/slow-plan"), "").unwrap();

            let result = fixture.run("env_signal_term", 80, 24);

            assert_eq!(result.exit_code, 143);
            result.observed("signal_sent");
            assert_clean(&fixture, &result);
            assert_child_reaped(&fixture.pid_record);
            assert!(
                fs::read_to_string(&fixture.signal_log)
                    .unwrap()
                    .contains("plan_present=True")
            );
        }

        #[test]
        fn failed_environment_retries_without_replanning_ready_environment() {
            let fixture = fixture(&["a-ready", "b-error"]);
            fs::write(fixture.root.join("b-error/fail-plan"), "").unwrap();

            let result =
                fixture.run_with_arguments("env_retry", 120, 40, "plan", &["-detailed-exitcode"]);

            assert_eq!(result.exit_code, 2);
            assert_eq!(calls(&fixture, "plan"), ["a-ready", "b-error", "b-error"]);
            assert_eq!(calls(&fixture, "init"), ["a-ready", "b-error"]);
            assert_clean(&fixture, &result);
        }

        #[test]
        fn ready_plan_is_reviewable_while_later_environment_runs() {
            for (scenario, expected) in [("env_partial", 0), ("env_cancel", 130)] {
                let fixture = fixture(&["a-ready", "z-slow"]);
                fs::write(fixture.root.join("z-slow/slow-plan"), "").unwrap();

                let result = fixture.run(scenario, 80, 24);

                assert_eq!(result.exit_code, expected, "{scenario}");
                assert!(result.observed.contains("ready_review_while_running"));
                assert_eq!(calls(&fixture, "plan"), ["a-ready", "z-slow"]);
                assert_clean(&fixture, &result);
                if scenario == "env_cancel" {
                    let pid: i32 = fs::read_to_string(&fixture.pid_record)
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    // SAFETY: signal zero only checks whether the recorded child still exists.
                    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
                    assert!(
                        fs::read_to_string(&fixture.signal_log)
                            .unwrap()
                            .contains("plan_present=True")
                    );
                }
            }
        }
    }

    const PLAN_JSON: &str = r#"{
  "format_version": "1.0",
  "applyable": true,
  "resource_changes": [{
    "address": "terraform_data.api",
    "change": {
      "actions": ["update"],
      "before": {"secret": "old-secret"},
      "before_sensitive": {"secret": true},
      "after": {"secret": "must-not-be-logged"},
      "after_sensitive": {"secret": true}
    }
  }],
  "output_changes": {"endpoint": {"change": {"after": "must-not-be-logged", "after_sensitive": true}}}
}"#;

    const PLAN_TEXT: &str = r#"Terraform will perform the following actions:

  # terraform_data.api will be updated in-place
  ~ resource "terraform_data" "api" {
      ~ input = "old" -> "new"
        secret = (sensitive value)
    }

Plan: 0 to add, 1 to change, 0 to destroy.
"#;

    const OVERVIEW_PLAN_JSON: &str = r#"{
  "format_version": "1.0",
  "applyable": true,
  "resource_changes": [
    {"address":"terraform_data.api","change":{"actions":["update"],"before":{"input":"old"},"after":{"input":"new"}}},
    {"address":"terraform_data.server[\"one\"]","change":{"actions":["update"],"before":{"input":"old"},"after":{"input":"new"}}},
    {"address":"terraform_data.server[\"two\"]","change":{"actions":["update"],"before":{"input":"old"},"after":{"input":"new"}}}
  ],
  "output_changes": {"endpoint": {"change": {"actions":["update"],"after":"new"}}}
}"#;

    const OVERVIEW_RELATIONS_PLAN_JSON: &str = r#"{
  "format_version": "1.0",
  "applyable": true,
  "resource_changes": [
    {"address":"terraform_data.api","change":{"actions":["update"],"before":{"input":"old"},"after":{"input":"new"}}},
    {"address":"terraform_data.server[\"one\"]","change":{"actions":["update"],"before":{"input":"old"},"after":{"input":"new"}}},
    {"address":"terraform_data.server[\"two\"]","change":{"actions":["update"],"before":{"input":"old"},"after":{"input":"new"}}}
  ],
  "configuration": {
    "root_module": {
      "resources": [
        {"mode":"managed","type":"terraform_data","name":"api","expressions":{"input":{"references":["terraform_data.server.id","terraform_data.server"]}}},
        {"mode":"managed","type":"terraform_data","name":"server","expressions":{}}
      ]
    }
  }
}"#;

    const OVERVIEW_PLAN_TEXT: &str = r#"Terraform will perform the following actions:

  # terraform_data.api will be updated in-place
  ~ resource "terraform_data" "api" {
      ~ input = "old" -> "new"
    }

  # terraform_data.server["one"] will be updated in-place
  ~ resource "terraform_data" "server" {
      ~ input = "old" -> "new"
    }

  # terraform_data.server["two"] will be updated in-place
  ~ resource "terraform_data" "server" {
      ~ input = "old" -> "new"
    }

Changes to Outputs:
  ~ endpoint = "old" -> "new"

Plan: 0 to add, 3 to change, 0 to destroy.
"#;

    const FAKE_TERRAFORM: &str = include_str!("support/cli/fake_terraform.sh");
    const PTY_DRIVER: &str = include_str!("support/cli/pty_driver.py");

    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
        bin: PathBuf,
        invocations: PathBuf,
        plan_path_record: PathBuf,
        pid_record: PathBuf,
        signal_log: PathBuf,
        show_json: PathBuf,
        show_text: PathBuf,
        env_log: PathBuf,
        tool_log: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_root_name(OsStr::new("plain directory with spaces"))
        }

        fn with_root_name(root_name: &OsStr) -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let directory =
                env::temp_dir().join(format!("terraleph-cli-pty-{}-{id}", std::process::id()));
            let root = directory.join(root_name);
            let bin = directory.join("fake-bin");
            fs::create_dir_all(&root).expect("fixture root should be created");
            fs::write(root.join("main.tf"), "").expect("single environment configuration");
            fs::create_dir(&bin).expect("fake bin should be created");

            let invocations = directory.join("invocations");
            let plan_path_record = directory.join("plan-path");
            let pid_record = directory.join("terraform-pid");
            let signal_log = directory.join("signals");
            let show_json = directory.join("show.json");
            let show_text = directory.join("show.txt");
            let env_log = directory.join("environment");
            let tool_log = directory.join("tools");
            fs::write(&show_json, PLAN_JSON).expect("fake show JSON should be written");
            fs::write(&show_text, PLAN_TEXT).expect("fake show text should be written");
            fs::write(&invocations, "").expect("invocation log should be created");
            fs::write(&env_log, "").expect("environment log should be created");
            fs::write(&tool_log, "").expect("tool log should be created");
            fs::write(&signal_log, "").expect("signal log should be created");
            let terraform = bin.join("terraform");
            fs::write(&terraform, FAKE_TERRAFORM).expect("fake Terraform should be written");
            fs::set_permissions(&terraform, fs::Permissions::from_mode(0o755))
                .expect("fake Terraform should be executable");
            let tofu = bin.join("tofu");
            fs::write(&tofu, FAKE_TERRAFORM).expect("fake OpenTofu should be written");
            fs::set_permissions(&tofu, fs::Permissions::from_mode(0o755))
                .expect("fake OpenTofu should be executable");

            Self {
                directory,
                root,
                bin,
                invocations,
                plan_path_record,
                pid_record,
                signal_log,
                show_json,
                show_text,
                env_log,
                tool_log,
            }
        }

        fn run(&self, scenario: &str, columns: u16, rows: u16) -> PtyResult {
            self.run_with_command(scenario, columns, rows, "plan")
        }

        fn use_overview_plan(&self) {
            fs::write(&self.show_json, OVERVIEW_PLAN_JSON)
                .expect("overview show JSON should be written");
            fs::write(&self.show_text, OVERVIEW_PLAN_TEXT)
                .expect("overview show text should be written");
        }

        fn use_overview_relations_plan(&self) {
            fs::write(&self.show_json, OVERVIEW_RELATIONS_PLAN_JSON)
                .expect("overview relations JSON should be written");
            fs::write(&self.show_text, OVERVIEW_PLAN_TEXT)
                .expect("overview plan text should be written");
        }

        fn run_with_command(
            &self,
            scenario: &str,
            columns: u16,
            rows: u16,
            command: &str,
        ) -> PtyResult {
            self.run_with_arguments(scenario, columns, rows, command, &[])
        }

        fn run_with_arguments(
            &self,
            scenario: &str,
            columns: u16,
            rows: u16,
            command: &str,
            arguments: &[&str],
        ) -> PtyResult {
            let original_path = env::var_os("PATH").expect("PATH should be available");
            let mut path_entries = vec![self.bin.clone()];
            path_entries.extend(env::split_paths(&original_path));
            let path = env::join_paths(path_entries).expect("test PATH should be valid");
            let mut process = Command::new("python3");
            process
                .arg("-c")
                .arg(PTY_DRIVER)
                .arg(env!("CARGO_BIN_EXE_terraleph"))
                .arg(&self.root)
                .arg(columns.to_string())
                .arg(rows.to_string())
                .arg(scenario);
            if !command.is_empty() {
                process.arg(command);
            }
            process
                .args(arguments)
                .env("PATH", path)
                .env("TERRALEPH_FAKE_MODE", scenario)
                .env("TERRALEPH_FAKE_INVOCATIONS", &self.invocations)
                .env("TERRALEPH_FAKE_PLAN_PATH", &self.plan_path_record)
                .env("TERRALEPH_FAKE_PID_PATH", &self.pid_record)
                .env("TERRALEPH_FAKE_SIGNAL_LOG", &self.signal_log)
                .env("TERRALEPH_FAKE_SHOW_JSON", &self.show_json)
                .env("TERRALEPH_FAKE_SHOW_TEXT", &self.show_text)
                .env("TERRALEPH_FAKE_ENV_LOG", &self.env_log)
                .env("TERRALEPH_FAKE_TOOL_LOG", &self.tool_log)
                .env_remove("TF_IN_AUTOMATION")
                .env_remove("CI")
                .env_remove("TF_CLI_ARGS")
                .env_remove("TF_CLI_ARGS_plan")
                .env("TF_CLI_CONFIG_FILE", "/dev/null")
                .env("CHECKPOINT_DISABLE", "1");
            if scenario == "default_ci" {
                process.env("CI", "1");
            }
            if scenario.starts_with("env_") || scenario.starts_with("signal_hangup") {
                let plans = self.directory.join("owned-plans");
                fs::create_dir(&plans).unwrap();
                process.env("TMPDIR", plans).env_remove("TF_DATA_DIR");
            }
            if scenario == "env_detailed" {
                process
                    .env("TF_WORKSPACE", "chosen-production")
                    .env("TF_CLI_ARGS", "-parallelism=9");
            }
            if scenario == "panic" {
                process.env("TERRALEPH_TEST_PANIC_AFTER_DRAW", "1");
            }
            if scenario == "cli_args" {
                process.env("TF_CLI_ARGS", "-no-color");
                process.env("TF_CLI_ARGS_plan", "-refresh=false");
            }
            let output = process.output().expect("PTY driver should start");
            assert!(
                output.status.success(),
                "PTY driver failed: {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            PtyResult::parse(&String::from_utf8_lossy(&output.stdout))
        }

        fn invocation_arguments(&self) -> Vec<String> {
            fs::read(&self.invocations)
                .expect("invocations should be readable")
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| {
                    let separator = line
                        .iter()
                        .position(|byte| *byte == b'|')
                        .expect("invocation should contain directory and arguments");
                    assert_eq!(Path::new(OsStr::from_bytes(&line[..separator])), self.root);
                    String::from_utf8_lossy(&line[separator + 1..]).into_owned()
                })
                .collect()
        }

        fn assert_saved_plan_removed(&self) {
            if !self.plan_path_record.exists() {
                return;
            }
            let path = fs::read_to_string(&self.plan_path_record)
                .expect("fake Terraform should record its plan path");
            assert!(
                !Path::new(path.trim()).exists(),
                "saved plan remains: {}",
                path.trim()
            );
        }

        fn assert_no_owned_plans(&self) {
            let remaining: Vec<_> = fs::read_dir(self.directory.join("owned-plans"))
                .expect("owned plan directory should be readable")
                .map(|entry| {
                    entry
                        .expect("owned plan entry should be readable")
                        .file_name()
                })
                .filter(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with("terraleph-") && name.ends_with(".tfplan")
                })
                .collect();
            assert!(remaining.is_empty(), "owned plans remain: {remaining:?}");
        }

        fn forwarded_cli_arguments(&self) -> Vec<String> {
            fs::read_to_string(&self.env_log)
                .expect("fake Terraform environment log should be readable")
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn signal_count(&self) -> usize {
            fs::read_to_string(&self.signal_log)
                .expect("signal log should be readable")
                .lines()
                .count()
        }

        fn invoked_tools(&self) -> Vec<String> {
            fs::read_to_string(&self.tool_log)
                .expect("fake tool log should be readable")
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).expect("PTY fixture should be removed");
        }
    }

    struct PtyResult {
        exit_code: i32,
        restored: bool,
        cursor_restored: bool,
        observed: String,
    }

    impl PtyResult {
        fn parse(output: &str) -> Self {
            let field = |name: &str| {
                output
                    .lines()
                    .find_map(|line| line.strip_prefix(name))
                    .unwrap_or_else(|| panic!("PTY driver omitted {name}: {output}"))
            };
            Self {
                exit_code: field("exit=").parse().expect("exit should be numeric"),
                restored: field("restored=") == "true",
                cursor_restored: field("cursor_restored=") == "true",
                observed: field("observed=").to_owned(),
            }
        }

        fn assert_restored(&self) {
            assert!(self.restored, "alternate screen was not restored");
            assert!(self.cursor_restored, "cursor visibility was not restored");
        }

        fn assert_no_tui(&self) {
            assert!(!self.restored, "plan failure unexpectedly entered the TUI");
            assert!(
                !self.cursor_restored,
                "plan failure unexpectedly changed the cursor"
            );
        }

        fn observed(&self, event: &str) {
            assert!(
                self.observed.split(',').any(|observed| observed == event),
                "PTY event {event} was not observed: {}",
                self.observed
            );
        }
    }

    #[test]
    fn pty_opentofu_runs_plan_and_both_show_modes_through_the_selected_executable() {
        let fixture = Fixture::new();
        let result = fixture.run_with_arguments("full_text", 100, 24, "tofu", &["plan"]);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("plan_text");
        assert_eq!(fixture.invoked_tools(), vec!["tofu".to_owned(); 5]);
        let arguments = fixture.invocation_arguments();
        assert!(arguments[0].starts_with("plan -detailed-exitcode -out="));
        assert_eq!(arguments[1], "workspace show");
        assert!(arguments[2].starts_with("show -no-color "));
        assert!(arguments[3].starts_with("show -json "));
        assert_eq!(arguments[4], "providers schema -json");
        assert_eq!(arguments.len(), 5);
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_no_argument_plan_opens_overview_with_the_first_row_and_relation_selected() {
        let fixture = Fixture::new();
        fixture.use_overview_relations_plan();
        let result = fixture.run_with_arguments("default_overview", 100, 24, "", &[]);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("default_overview");
        result.observed("default_overview_first_row_selected");
        assert!(fixture.invocation_arguments()[0].starts_with("plan -detailed-exitcode -out="));
        assert_eq!(fixture.invoked_tools(), vec!["terraform".to_owned(); 5]);
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_no_argument_plan_in_ci_prints_help_without_starting_terraform() {
        let fixture = Fixture::new();
        let result = fixture.run_with_arguments("default_ci", 100, 24, "", &[]);

        assert_eq!(result.exit_code, 0);
        result.assert_no_tui();
        result.observed("default_help");
        assert!(fixture.invocation_arguments().is_empty());
    }

    #[test]
    fn pty_no_argument_plan_rejects_hcp_without_delegating_to_terraform() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("main.tf"), "terraform {\n cloud {}\n}\n").unwrap();

        let result = fixture.run_with_arguments("unsupported_default", 100, 24, "", &[]);

        assert_eq!(result.exit_code, 1);
        result.assert_no_tui();
        result.observed("unsupported_default");
        assert!(fixture.invocation_arguments().is_empty());
    }

    #[test]
    fn pty_success_keeps_plan_status_above_plan_text() {
        let fixture = Fixture::new();
        let result = fixture.run("diagnostic_success", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("plan_status_and_text");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_apply_no_changes_exits_without_confirmation_or_apply() {
        let fixture = Fixture::new();
        let result = fixture.run_with_command("no_changes", 100, 24, "apply");

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("no_changes");
        assert!(
            fixture
                .invocation_arguments()
                .iter()
                .all(|arguments| !arguments.starts_with("apply "))
        );
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_detailed_exit_code_is_returned_after_review() {
        let fixture = Fixture::new();
        let result =
            fixture.run_with_arguments("detailed", 100, 24, "plan", &["-detailed-exitcode"]);

        assert_eq!(result.exit_code, 2);
        result.assert_restored();
        result.observed("plan_text");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_apply_maps_plan_only_options_away_from_apply() {
        let fixture = Fixture::new();
        let result = fixture.run_with_arguments(
            "apply_mapping",
            100,
            24,
            "apply",
            &["-var", "name=value", "-parallelism", "4"],
        );

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("apply_success");
        let arguments = fixture.invocation_arguments();
        assert!(arguments[0].contains("-var name=value"));
        assert!(arguments[0].contains("-parallelism 4"));
        assert_eq!(arguments[5], "workspace show");
        assert!(arguments[6].contains("-parallelism 4"));
        assert!(!arguments[6].contains("-var"));
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_user_owned_output_path_is_not_removed() {
        let fixture = Fixture::new();
        let result =
            fixture.run_with_arguments("user_output", 100, 24, "plan", &["-out=review.tfplan"]);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        let output = fixture.root.join("review.tfplan");
        assert!(output.exists());
        assert!(fixture.invocation_arguments()[0].contains(&output.display().to_string()));
        fs::remove_file(output).expect("user-owned output should be cleaned by the test");
    }

    #[test]
    fn pty_managed_children_do_not_receive_cli_argument_environment_again() {
        let fixture = Fixture::new();
        let result = fixture.run("cli_args", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("plan_text");
        assert!(
            fixture.invocation_arguments()[0]
                .starts_with("plan -no-color -refresh=false -detailed-exitcode -out=")
        );
        assert!(fixture.forwarded_cli_arguments().iter().all(|line| {
            matches!(
                line.as_str(),
                "TF_CLI_ARGS=" | "TF_CLI_ARGS_plan=" | "TF_CLI_ARGS_apply="
            )
        }));
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_filter_navigation_restores_the_full_review_and_terminal() {
        let fixture = Fixture::new();
        let result = fixture.run("filter_navigation", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("plan_position");
        result.observed("filter_input");
        result.observed("filter_matches");
        result.observed("filter_confirmed");
        result.observed("filter_help");
        result.observed("filter_context");
        result.observed("filter_copy");
        result.observed("filter_apply_confirmation");
        result.observed("filter_apply_cancelled");
        result.observed("filter_cleared");
        assert!(
            fixture
                .invocation_arguments()
                .iter()
                .all(|arguments| !arguments.starts_with("apply "))
        );
        fixture.assert_saved_plan_removed();
    }

    fn assert_single_apply_of_reviewed_plan(fixture: &Fixture) {
        let arguments = fixture.invocation_arguments();
        let plans = arguments
            .iter()
            .filter(|arguments| arguments.starts_with("plan "))
            .collect::<Vec<_>>();
        let applies = arguments
            .iter()
            .filter(|arguments| arguments.starts_with("apply "))
            .collect::<Vec<_>>();
        assert_eq!(plans.len(), 1, "{arguments:?}");
        assert_eq!(applies.len(), 1, "{arguments:?}");
        assert!(applies[0].starts_with("apply -json -input=false "));
        assert_eq!(
            plans[0].split("-out=").nth(1),
            applies[0].split("-json -input=false ").nth(1)
        );
    }

    #[rstest]
    #[case::plan("plan_apply", "plan")]
    #[case::default_entry("default_apply", "")]
    fn pty_plan_entry_applies_the_reviewed_saved_plan_without_replanning(
        #[case] scenario: &str,
        #[case] command: &str,
    ) {
        let fixture = Fixture::new();
        let result = fixture.run_with_command(scenario, 100, 24, command);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        if command.is_empty() {
            result.observed("default_overview");
        }
        result.observed("plan_apply_offered");
        result.observed("apply_confirmation");
        result.observed("apply_success");
        assert_single_apply_of_reviewed_plan(&fixture);
        fixture.assert_saved_plan_removed();
    }

    #[cfg(target_os = "linux")]
    #[rstest]
    #[case::plan("plan_apply", "plan")]
    #[case::default_entry("default_apply", "")]
    #[case::apply_entry("apply_success", "apply")]
    fn pty_non_utf8_directory_applies_the_reviewed_saved_plan(
        #[case] scenario: &str,
        #[case] command: &str,
    ) {
        let fixture = Fixture::with_root_name(OsStr::from_bytes(b"infra-\xff"));
        let result = fixture.run_with_command(scenario, 100, 24, command);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("apply_success");
        assert_single_apply_of_reviewed_plan(&fixture);
        fixture.assert_saved_plan_removed();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pty_non_utf8_directory_applies_the_reviewed_inline_output_plan() {
        let fixture = Fixture::with_root_name(OsStr::from_bytes(b"infra-\xff"));
        let result =
            fixture.run_with_arguments("plan_apply", 100, 24, "plan", &["-out=review.tfplan"]);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("apply_success");
        let output = fixture.root.join("review.tfplan");
        assert!(output.exists());
        let arguments = fixture.invocation_arguments();
        let plans = arguments
            .iter()
            .filter(|arguments| arguments.starts_with("plan "))
            .count();
        let applies = arguments
            .iter()
            .filter(|arguments| arguments.starts_with("apply "))
            .collect::<Vec<_>>();
        assert_eq!(plans, 1, "{arguments:?}");
        assert_eq!(
            applies,
            [&format!("apply -json -input=false {}", output.display())]
        );
    }

    #[test]
    fn pty_overview_groups_filters_and_returns_to_the_raw_plan() {
        let fixture = Fixture::new();
        fixture.use_overview_plan();
        let result = fixture.run("overview_navigation", 120, 30);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("overview_opened");
        result.observed("overview_relations_focused");
        result.observed("overview_relations_maximized");
        result.observed("overview_split_restored");
        result.observed("overview_relations_raw");
        result.observed("overview_relations_restored");
        result.observed("overview_changes_focused");
        result.observed("overview_expanded");
        result.observed("overview_raw_block");
        result.observed("overview_restored");
        result.observed("overview_filtered");
        result.observed("overview_full_plan");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_quit_requires_enter_and_keeps_other_result_actions_available() {
        let fixture = Fixture::new();
        let result = fixture.run("quit_confirmation", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("quit_confirmation");
        result.observed("quit_repeat");
        result.observed("quit_cancelled");
        result.observed("quit_ctrl_c");
        result.observed("quit_ctrl_c_cancelled");
        result.observed("quit_copy");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_ctrl_c_in_empty_filter_requires_enter_to_quit_without_apply() {
        let fixture = Fixture::new();
        let result = fixture.run("empty_filter_quit", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("empty_filter_confirmed");
        result.observed("empty_filter_ctrl_c");
        result.observed("empty_filter_ctrl_c_cancelled");
        result.observed("empty_filter_ctrl_c_again");
        assert!(
            fixture
                .invocation_arguments()
                .iter()
                .all(|arguments| !arguments.starts_with("apply "))
        );
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_apply_success_switches_focus_uses_the_saved_plan_once_and_cleans_it_after_quit() {
        let fixture = Fixture::new();
        let result = fixture.run_with_command("apply_log_view", 100, 24, "apply");

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("plan_help");
        result.observed("apply_filter_input");
        result.observed("apply_filter_no_matches");
        result.observed("apply_filter_confirmed");
        result.observed("apply_confirmation");
        result.observed("apply_help");
        result.observed("apply_context");
        result.observed("apply_started");
        result.observed("apply_logs_focused");
        result.observed("apply_targets_focused");
        result.observed("apply_logs_refocused");
        result.observed("apply_success");
        let arguments = fixture.invocation_arguments();
        assert_eq!(arguments.len(), 7);
        assert!(arguments[1].starts_with("workspace show"));
        assert_eq!(arguments[4], "providers schema -json");
        assert_eq!(arguments[5], "workspace show");
        assert!(arguments[6].starts_with("apply -json -input=false "));
        assert_eq!(
            arguments[0].split("-out=").nth(1),
            arguments[6].split("-json -input=false ").nth(1)
        );
        assert_eq!(
            arguments
                .iter()
                .filter(|arguments| arguments.starts_with("apply "))
                .count(),
            1
        );
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_apply_failure_keeps_the_result_and_returns_failure() {
        let fixture = Fixture::new();
        let result = fixture.run_with_command("apply_failure", 100, 24, "apply");

        assert_eq!(result.exit_code, 1);
        result.assert_restored();
        result.observed("apply_confirmation");
        result.observed("apply_started");
        result.observed("apply_failure");
        assert_eq!(
            fixture
                .invocation_arguments()
                .iter()
                .filter(|arguments| arguments.starts_with("apply "))
                .count(),
            1
        );
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_apply_interrupt_waits_for_terraform_and_returns_130() {
        let fixture = Fixture::new();
        let result = fixture.run_with_command("apply_interrupt", 100, 24, "apply");

        assert_eq!(result.exit_code, 130);
        result.assert_restored();
        result.observed("apply_confirmation");
        result.observed("apply_started");
        result.observed("apply_interrupted");
        assert_eq!(
            fixture
                .invocation_arguments()
                .iter()
                .filter(|arguments| arguments.starts_with("apply "))
                .count(),
            1
        );
        fixture.assert_saved_plan_removed();
        assert_child_reaped(&fixture.pid_record);
    }

    #[rstest]
    #[case::no("apply_no")]
    #[case::escape("apply_escape")]
    fn pty_declining_apply_returns_to_the_same_review_without_running_apply(
        #[case] scenario: &str,
    ) {
        let fixture = Fixture::new();
        let result = fixture.run_with_command(scenario, 100, 24, "apply");

        assert_eq!(result.exit_code, 1);
        result.assert_restored();
        result.observed("apply_confirmation");
        result.observed("plan_restored");
        assert!(
            fixture
                .invocation_arguments()
                .iter()
                .all(|arguments| !arguments.starts_with("apply "))
        );
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_plan_failure_skips_workspace_and_show() {
        let fixture = Fixture::new();
        let result = fixture.run("failure", 100, 24);

        assert_eq!(result.exit_code, 1);
        result.assert_no_tui();
        result.observed("failed");
        assert_eq!(fixture.invocation_arguments().len(), 1);
        assert!(fixture.invocation_arguments()[0].starts_with("plan "));
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_uninitialized_backend_runs_init_before_the_plan() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("main.tf"),
            "terraform {\n  backend \"local\" {}\n}\n",
        )
        .unwrap();

        let result = fixture.run("full_text", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        let arguments = fixture.invocation_arguments();
        assert_eq!(arguments[0], "init -input=false -no-color");
        assert!(arguments[1].starts_with("plan -detailed-exitcode -out="));
        assert_eq!(arguments.len(), 6);
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_plan_requesting_init_initializes_once_and_plans_again() {
        let fixture = Fixture::new();

        let result = fixture.run("single_reinit", 100, 24);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        let arguments = fixture.invocation_arguments();
        assert!(arguments[0].starts_with("plan -detailed-exitcode -out="));
        assert_eq!(arguments[1], "init -input=false -backend=false -no-color");
        assert_eq!(arguments[2], arguments[0]);
        assert_eq!(arguments.len(), 7);
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_init_failure_stops_before_the_plan_without_opening_review() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("main.tf"),
            "terraform {\n  backend \"local\" {}\n}\n",
        )
        .unwrap();

        let result = fixture.run("init_failure", 100, 24);

        assert_eq!(result.exit_code, 1);
        result.assert_no_tui();
        assert_eq!(
            fixture.invocation_arguments(),
            ["init -input=false -no-color"]
        );
    }

    #[test]
    fn pty_ctrl_c_reaps_terraform_cleans_the_plan_and_returns_130() {
        let fixture = Fixture::new();
        let result = fixture.run("interrupt", 100, 24);

        assert_eq!(result.exit_code, 130);
        result.assert_no_tui();
        result.observed("terraform_started");
        result.observed("interrupt_requested");
        fixture.assert_saved_plan_removed();
        assert_eq!(fixture.signal_count(), 1);
        assert_child_reaped(&fixture.pid_record);
    }

    #[test]
    fn pty_termination_signal_restores_terminal_and_removes_the_saved_plan() {
        let fixture = Fixture::new();

        let result = fixture.run("signal_review_term", 100, 24);

        assert_eq!(result.exit_code, 143);
        result.assert_restored();
        result.observed("signal_sent");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_termination_signal_during_plan_stops_before_review() {
        struct SignalCase {
            scenario: &'static str,
            expected: i32,
        }

        for case in [
            SignalCase {
                scenario: "plan_signal_group_hup",
                expected: 129,
            },
            SignalCase {
                scenario: "plan_signal_group_term",
                expected: 143,
            },
            SignalCase {
                scenario: "plan_signal_parent_int",
                expected: 130,
            },
        ] {
            let fixture = Fixture::new();

            let result = fixture.run(case.scenario, 100, 24);

            assert_eq!(result.exit_code, case.expected, "case: {}", case.scenario);
            result.assert_no_tui();
            fixture.assert_saved_plan_removed();
            assert_child_reaped(&fixture.pid_record);
        }
    }

    #[test]
    fn pty_terminal_closed_before_review_removes_the_saved_plan() {
        let fixture = Fixture::new();

        let result = fixture.run("hangup_before_review", 100, 24);

        assert_eq!(result.exit_code, 129);
        result.observed("terminal_closed");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_closed_terminal_removes_the_saved_plan() {
        for scenario in ["signal_hangup", "signal_hangup_ignored"] {
            let fixture = Fixture::new();

            let result = fixture.run(scenario, 100, 24);

            assert_eq!(result.exit_code, 129, "case: {scenario}");
            result.observed("terminal_closed");
            fixture.assert_saved_plan_removed();
            fixture.assert_no_owned_plans();
        }
    }

    #[test]
    fn pty_termination_signal_keeps_the_user_owned_output() {
        let fixture = Fixture::new();

        let result = fixture.run_with_arguments(
            "signal_review_term",
            100,
            24,
            "plan",
            &["-out=review.tfplan"],
        );

        assert_eq!(result.exit_code, 143);
        result.assert_restored();
        assert!(fixture.root.join("review.tfplan").exists());
    }

    #[test]
    fn pty_termination_signal_during_apply_waits_for_terraform() {
        let fixture = Fixture::new();

        let result = fixture.run_with_command("signal_apply_term", 100, 24, "apply");

        assert_eq!(result.exit_code, 143);
        result.assert_restored();
        result.observed("apply_started");
        fixture.assert_saved_plan_removed();
        assert_child_reaped(&fixture.pid_record);
    }

    #[test]
    fn pty_narrow_terminal_recovers_after_resize() {
        let fixture = Fixture::new();
        let result = fixture.run("narrow", 20, 20);

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("narrow");
        result.observed("resized");
        fixture.assert_saved_plan_removed();
    }

    #[test]
    fn pty_apply_confirmation_waits_for_resize_before_starting() {
        let fixture = Fixture::new();
        let result = fixture.run_with_command("apply_resize", 100, 24, "apply");

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("apply_input_yes");
        result.observed("apply_confirmation_narrow");
        result.observed("apply_narrow_enter_redrawn");
        result.observed("apply_confirmation_resized");
        result.observed("apply_started");
        result.observed("apply_second_enter");
        result.observed("apply_result");
        assert_eq!(
            fixture
                .invocation_arguments()
                .iter()
                .filter(|arguments| arguments.starts_with("apply "))
                .count(),
            1
        );
        fixture.assert_saved_plan_removed();
    }

    // TERRALEPH_TEST_PANIC_AFTER_DRAW is honored only in builds with the test-support feature.
    #[cfg(feature = "test-support")]
    #[test]
    fn pty_panic_restores_terminal_and_cleans_any_created_plan() {
        let fixture = Fixture::new();
        let result = fixture.run("panic", 100, 24);

        assert_ne!(result.exit_code, 0);
        result.assert_restored();
        fixture.assert_saved_plan_removed();
    }

    #[test]
    #[ignore = "requires Terraform CLI and the cloudless basic scenario"]
    fn pty_basic_scenario_shows_the_full_plan_and_restores_terminal() {
        let scenario = BasicScenario::setup();
        let result = scenario.run();

        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("plan_text");
        result.observed("apply_confirmation");
        result.observed("apply_result");
    }

    #[test]
    #[ignore = "requires Terraform CLI and the interactive demo"]
    fn single_demo_opens_the_tui_after_noninteractive_setup() {
        let output = Command::new("python3")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args([
                "tests/support/cli/pty_driver.py",
                "python3",
                env!("CARGO_MANIFEST_DIR"),
                "120",
                "30",
                "demo",
                "fixtures/demo.py",
                "single",
            ])
            .env("RUSTC_WRAPPER", "")
            .env_remove("TF_IN_AUTOMATION")
            .env_remove("CI")
            .output()
            .expect("basic scenario demo PTY should start");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = PtyResult::parse(&String::from_utf8_lossy(&output.stdout));
        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("demo_tui");
        result.observed("demo_overview");
    }

    #[rstest]
    #[case::single("single")]
    #[case::multi("multi")]
    #[ignore = "requires Terraform CLI and the interactive demo"]
    fn demo_plan_detail_applies_the_open_environment(#[case] mode: &str) {
        let output = Command::new("python3")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args([
                "tests/support/cli/pty_driver.py",
                "python3",
                env!("CARGO_MANIFEST_DIR"),
                "120",
                "40",
                "demo_apply",
                "fixtures/demo.py",
                mode,
            ])
            .env("RUSTC_WRAPPER", "")
            .env_remove("TF_IN_AUTOMATION")
            .env_remove("CI")
            .output()
            .expect("demo apply PTY should start");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = PtyResult::parse(&String::from_utf8_lossy(&output.stdout));
        assert_eq!(result.exit_code, 0);
        result.assert_restored();
        result.observed("demo_plan_detail");
        result.observed("demo_apply_confirmation");
        result.observed("demo_apply_success");
    }

    struct BasicScenario {
        directory: PathBuf,
    }

    impl BasicScenario {
        fn setup() -> Self {
            let output = Command::new("python3")
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .args(["fixtures/basic/plan.py", "test", "setup"])
                .output()
                .expect("basic scenario setup should start");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self {
                directory: PathBuf::from(
                    String::from_utf8(output.stdout)
                        .expect("scenario path should be UTF-8")
                        .trim(),
                ),
            }
        }

        fn run(&self) -> PtyResult {
            let output = Command::new("python3")
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .arg("tests/support/cli/pty_driver.py")
                .arg(env!("CARGO_BIN_EXE_terraleph"))
                .arg(&self.directory)
                .args(["100", "24", "basic_workflow", "apply"])
                .env_remove("TF_IN_AUTOMATION")
                .env_remove("CI")
                .env_remove("TF_CLI_ARGS")
                .env_remove("TF_CLI_ARGS_plan")
                .env("TF_DATA_DIR", self.directory.join(".terraform"))
                .env("TF_CLI_CONFIG_FILE", "/dev/null")
                .env("CHECKPOINT_DISABLE", "1")
                .output()
                .expect("basic scenario PTY should start");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            PtyResult::parse(&String::from_utf8_lossy(&output.stdout))
        }
    }

    impl Drop for BasicScenario {
        fn drop(&mut self) {
            clean_basic_scenario(&self.directory);
        }
    }

    fn clean_basic_scenario(directory: &Path) {
        let output = Command::new("python3")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args(["fixtures/basic/plan.py", "test", "clean"])
            .arg(directory)
            .output()
            .expect("basic scenario cleanup should start");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_child_reaped(path: &Path) {
        let pid = fs::read_to_string(path)
            .expect("Terraform pid should be recorded")
            .trim()
            .parse::<u32>()
            .expect("Terraform pid should be numeric");
        assert!(
            !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .expect("kill should start")
                .success(),
            "Terraform process {pid} remains alive"
        );
    }
}
