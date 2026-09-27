#![cfg(windows)]

use std::{
    env, fs,
    io::{self, Read, Write},
    process::{Command, Stdio},
};

#[test]
fn delegation_preserves_windows_exit_bits_and_streams() {
    let root = env::temp_dir().join(format!(
        "terraleph-windows-delegation-{}",
        std::process::id()
    ));
    fs::create_dir(&root).unwrap();
    fs::copy(env::current_exe().unwrap(), root.join("terraform.exe")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_terraleph"))
        .args(["terraform", "--exact", "fake_terraform", "--nocapture"])
        .env("PATH", &root)
        .env("TERRALEPH_FAKE_WINDOWS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"synthetic input")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    fs::remove_dir_all(root).unwrap();

    assert_eq!(output.status.code(), Some(-1_073_741_510));
    assert!(String::from_utf8_lossy(&output.stdout).contains("synthetic input"));
    assert_eq!(output.stderr, b"synthetic stderr");
}

#[test]
#[expect(
    clippy::exit,
    reason = "fake CLI must produce a full-width Windows process exit status"
)]
fn fake_terraform() {
    if env::var_os("TERRALEPH_FAKE_WINDOWS").is_none() {
        return;
    }
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).unwrap();
    io::stdout().write_all(&input).unwrap();
    io::stdout().flush().unwrap();
    io::stderr().write_all(b"synthetic stderr").unwrap();
    io::stderr().flush().unwrap();
    std::process::exit(-1_073_741_510);
}
