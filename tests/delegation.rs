#![cfg(unix)]

use rstest::rstest;
use std::{
    env,
    ffi::OsString,
    fs,
    io::Write,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
        process::ExitStatusExt,
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    directory: PathBuf,
    bin: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let directory = env::temp_dir().join(format!(
            "terraleph-delegation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let bin = directory.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(
            bin.join("terraform"),
            r#"#!/bin/sh
printf '%s\000' "$@" > "$RECORD/argv"
printf '%s' "$TF_CLI_ARGS" > "$RECORD/env"
printf '%s' "$TF_CLI_ARGS_apply" > "$RECORD/env-apply"
printf '%s' "$TF_VAR_synthetic" > "$RECORD/var"
pwd > "$RECORD/cwd"
if [ "$READ_STDIN" = 1 ]; then /bin/cat; fi
printf 'fake stdout\n'
printf 'fake stderr\n' >&2
if [ "$SIGNAL_EXIT" = 1 ]; then kill -TERM $$; fi
exit 37
"#,
        )
        .unwrap();
        fs::set_permissions(bin.join("terraform"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::copy(bin.join("terraform"), bin.join("tofu")).unwrap();
        fs::set_permissions(bin.join("tofu"), fs::Permissions::from_mode(0o755)).unwrap();
        Self { directory, bin }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_terraleph"));
        self.configure(&mut command);
        command
    }
    fn configure(&self, command: &mut Command) {
        command
            .current_dir(&self.directory)
            .env("PATH", &self.bin)
            .env("RECORD", &self.directory)
            .env_remove("CI")
            .env_remove("TF_IN_AUTOMATION")
            .env_remove("TF_CLI_ARGS")
            .env_remove("TF_CLI_ARGS_plan")
            .env_remove("TF_CLI_ARGS_apply")
            .env_remove("TF_DATA_DIR");
    }
    fn pty_command(&self) -> Command {
        let python = Command::new("which").arg("python3").output().unwrap();
        assert!(python.status.success());
        let mut command = Command::new(String::from_utf8(python.stdout).unwrap().trim());
        command
            .arg("-c")
            .arg(include_str!("support/cli/delegation_pty.py"))
            .arg(env!("CARGO_BIN_EXE_terraleph"));
        self.configure(&mut command);
        command
    }
    fn recorded_arguments(&self) -> Vec<u8> {
        fs::read(self.directory.join("argv")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

#[test]
fn delegation_preserves_arguments_environment_cwd_streams_and_exit_status() {
    let fixture = Fixture::new();
    let mut child = fixture
        .command()
        .args([
            "terraform",
            "-chdir=directory with spaces",
            "apply",
            "-var",
            "name=one two",
            "-var",
            "name=three",
            "-unknown",
        ])
        .env("TF_CLI_ARGS", "-json -var 'x=env space'")
        .env("TF_CLI_ARGS_apply", "-auto-approve")
        .env("TF_VAR_synthetic", "synthetic value")
        .env("READ_STDIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"stdin bytes\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(37));
    assert_eq!(output.stdout, b"stdin bytes\nfake stdout\n");
    assert_eq!(output.stderr, b"fake stderr\n");
    assert_eq!(
        fixture.recorded_arguments(),
        b"-chdir=directory with spaces\0apply\0-var\0name=one two\0-var\0name=three\0-unknown\0"
    );
    assert_eq!(
        fs::read(fixture.directory.join("env")).unwrap(),
        b"-json -var 'x=env space'"
    );
    assert_eq!(
        fs::read(fixture.directory.join("env-apply")).unwrap(),
        b"-auto-approve"
    );
    assert_eq!(
        fs::read(fixture.directory.join("var")).unwrap(),
        b"synthetic value"
    );
    assert_eq!(
        PathBuf::from(
            fs::read_to_string(fixture.directory.join("cwd"))
                .unwrap()
                .trim()
        )
        .canonicalize()
        .unwrap(),
        fixture.directory.canonicalize().unwrap()
    );
}

#[rstest]
#[case::help(&["terraform", "-help"], b"-help\0")]
#[case::version(&["terraform", "-version"], b"-version\0")]
#[case::other_command(&["terraform", "workspace", "list"], b"workspace\0list\0")]
#[case::plan_shorthand(&["plan", "-future"], b"plan\0-future\0")]
#[case::apply_shorthand(&["apply", "saved plan"], b"apply\0saved plan\0")]
#[case::tofu_help(&["tofu", "-help"], b"-help\0")]
fn tool_entries_delegate_without_rewriting(#[case] args: &[&str], #[case] expected: &[u8]) {
    let fixture = Fixture::new();
    let output = fixture.command().args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(37));
    assert_eq!(fixture.recorded_arguments(), expected);
}

#[test]
fn delegation_preserves_non_utf8_arguments_and_signal_termination() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args([
            OsString::from("plan"),
            OsString::from_vec(b"-var=name=\xff".to_vec()),
        ])
        .env("SIGNAL_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(fixture.recorded_arguments(), b"plan\0-var=name=\xff\0");
    assert_eq!(output.status.signal(), Some(libc::SIGTERM));
}

#[rstest]
#[case::symbolic(false)]
#[case::hard(true)]
fn recursive_executable_is_rejected_before_launch(#[case] hard: bool) {
    let fixture = Fixture::new();
    let executable = fixture.bin.join("terraform");
    fs::remove_file(&executable).unwrap();
    if hard {
        fs::hard_link(env!("CARGO_BIN_EXE_terraleph"), &executable).unwrap();
    } else {
        symlink(env!("CARGO_BIN_EXE_terraleph"), &executable).unwrap();
    }
    let output = fixture
        .command()
        .args(["terraform", "plan"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Terraleph itself"));
    assert!(!fixture.directory.join("argv").exists());
}

#[rstest]
#[case::ci("CI", "true", "", &[])]
#[case::automation("TF_IN_AUTOMATION", "false", "", &[])]
#[case::json("TF_CLI_ARGS_plan", "-json", "", &[])]
#[case::environment_unknown_option("TF_CLI_ARGS_plan", "-future", "", &[])]
#[case::explicit_unknown_option("UNUSED", "", "", &["plan", "-future"])]
#[case::auto_approve("TF_CLI_ARGS_apply", "-auto-approve", "", &["apply"])]
#[case::hcp("UNUSED", "", "terraform {\n cloud {}\n}", &[])]
#[case::broken("UNUSED", "", "terraform {", &[])]
fn tty_delegation_does_not_initialize_terminal_or_generate_plan(
    #[case] key: &str,
    #[case] value: &str,
    #[case] source: &str,
    #[case] args: &[&str],
) {
    let fixture = Fixture::new();
    fs::write(fixture.directory.join("main.tf"), source).unwrap();
    let output = fixture
        .pty_command()
        .args(if args.is_empty() { &["plan"] } else { args })
        .env(key, value)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(37),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!output.stdout.contains(&0x1b));
    assert!(output.stdout.starts_with(b"fake stdout"));
    let expected = if args.is_empty() {
        vec!["plan"]
    } else {
        args.to_vec()
    };
    assert_eq!(
        fixture.recorded_arguments(),
        expected
            .iter()
            .flat_map(|arg| arg.bytes().chain([0]))
            .collect::<Vec<_>>()
    );
    assert!(!fixture.directory.join(".terraform").exists());
}

#[test]
fn path_symlink_preserves_the_terraform_name_for_dispatcher_shims() {
    let fixture = Fixture::new();
    let dispatcher = fixture.bin.join("dispatcher");
    fs::write(
        &dispatcher,
        "#!/bin/sh\nprintf '%s' \"${0##*/}\"\nexit 37\n",
    )
    .unwrap();
    fs::set_permissions(&dispatcher, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(fixture.bin.join("terraform")).unwrap();
    symlink(&dispatcher, fixture.bin.join("terraform")).unwrap();

    let output = fixture
        .command()
        .args(["terraform", "-version"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(37));
    assert_eq!(output.stdout, b"terraform");
}

mod environment_discovery {
    use super::*;

    #[rstest]
    #[case::no_candidates(None, "No environment candidates")]
    #[case::hcp(Some("terraform {\n cloud {}\n}"), "Excluded: HCP execution")]
    #[case::broken(Some("terraform {"), "Error:")]
    fn unusable_environments_fail_before_running_the_cli(
        #[case] source: Option<&str>,
        #[case] expected: &str,
    ) {
        let fixture = Fixture::new();
        if let Some(source) = source {
            fs::create_dir(fixture.directory.join("dev")).unwrap();
            fs::write(fixture.directory.join("dev/main.tf"), source).unwrap();
        }

        let output = fixture.pty_command().arg("plan").output().unwrap();

        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
        assert!(!output.stdout.contains(&0x1b));
        assert!(!fixture.directory.join("argv").exists());
        if source.is_some() {
            assert!(String::from_utf8_lossy(&output.stdout).contains("No executable environments"));
        }
    }

    #[rstest]
    #[case::out(&["plan", "-out=review.plan"], None, "-out")]
    #[case::generate(&["plan", "-generate-config-out", "imports.tf"], None, "-generate-config-out")]
    #[case::data_dir(&["plan"], Some("shared"), "TF_DATA_DIR")]
    fn forbidden_multiple_environment_options_run_no_child_commands(
        #[case] arguments: &[&str],
        #[case] data_dir: Option<&str>,
        #[case] expected: &str,
    ) {
        let fixture = Fixture::new();
        fs::create_dir(fixture.directory.join("dev")).unwrap();
        fs::write(
            fixture.directory.join("dev/main.tf"),
            "terraform {\n backend \"local\" {}\n}",
        )
        .unwrap();
        let mut command = fixture.pty_command();
        command.args(arguments);
        if let Some(data_dir) = data_dir {
            command.env("TF_DATA_DIR", data_dir);
        }

        let output = command.output().unwrap();

        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
        assert!(!fixture.directory.join("argv").exists());
        assert!(!fixture.directory.join("review.plan").exists());
        assert!(!fixture.directory.join("imports.tf").exists());
    }
}
