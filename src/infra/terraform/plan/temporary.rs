use std::{
    env,
    fs::OpenOptions,
    io,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const PREFIX: &str = "terraleph-";
const SUFFIX: &str = ".tfplan";

pub(super) fn create_plan_path() -> io::Result<PathBuf> {
    let directory = env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let process_id = std::process::id();
    for attempt in 0..100 {
        let path = directory.join(format!(
            "{PREFIX}{process_id}-{timestamp}-{attempt}{SUFFIX}"
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique Terraform plan path",
    ))
}

// A process killed without running its cleanup (SIGKILL, power loss) leaves its plan behind.
// Removal is limited to plans whose creating PID no longer exists; a reused PID keeps the plan
// until that process exits, which errs on the side of never touching a live session's plan.
#[cfg(unix)]
pub(crate) fn remove_orphaned_plans() {
    remove_orphaned_plans_in(&env::temp_dir(), std::process::id(), process_exists);
}

// Without a portable liveness check, keeping every plan is the only choice that cannot remove a
// running session's plan.
#[cfg(windows)]
pub(crate) const fn remove_orphaned_plans() {}

#[cfg(unix)]
fn remove_orphaned_plans_in(directory: &Path, own_process: u32, exists: impl Fn(u32) -> bool) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    let user = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let Some(owner) = entry.file_name().to_str().and_then(plan_owner) else {
            continue;
        };
        if owner == own_process || exists(owner) {
            continue;
        }
        // DirEntry metadata does not follow symlinks, so a link named like a plan is kept.
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() && metadata.uid() == user {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(unix)]
fn plan_owner(name: &str) -> Option<u32> {
    let fields = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let mut fields = fields.split('-');
    let [Some(process), Some(timestamp), Some(attempt), None] =
        [fields.next(), fields.next(), fields.next(), fields.next()]
    else {
        return None;
    };
    [process, timestamp, attempt]
        .iter()
        .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| process.parse().ok())
        .flatten()
}

#[cfg(unix)]
fn process_exists(process: u32) -> bool {
    let Ok(process) = libc::pid_t::try_from(process) else {
        return true;
    };
    if process <= 0 {
        return true;
    }
    // SAFETY: signal zero only checks whether the PID exists; no signal is delivered.
    let result = unsafe { libc::kill(process, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn plan_owner_accepts_only_the_generated_name_shape() {
        struct NameCase {
            name: &'static str,
            input: &'static str,
            expected: Option<u32>,
        }

        for case in [
            NameCase {
                name: "generated",
                input: "terraleph-4242-1700000000000000000-0.tfplan",
                expected: Some(4242),
            },
            NameCase {
                name: "missing_attempt",
                input: "terraleph-4242-1700000000000000000.tfplan",
                expected: None,
            },
            NameCase {
                name: "extra_field",
                input: "terraleph-4242-1-0-1.tfplan",
                expected: None,
            },
            NameCase {
                name: "non_numeric",
                input: "terraleph-review-1-0.tfplan",
                expected: None,
            },
            NameCase {
                name: "other_extension",
                input: "terraleph-4242-1-0.tfplan.bak",
                expected: None,
            },
            NameCase {
                name: "user_plan",
                input: "review.tfplan",
                expected: None,
            },
        ] {
            assert_eq!(plan_owner(case.input), case.expected, "case: {}", case.name);
        }
    }

    #[test]
    fn orphan_sweep_removes_only_plans_of_exited_processes() {
        let directory = tempfile::tempdir().expect("plan directory should be created");
        let exited = directory.path().join("terraleph-100-1-0.tfplan");
        let running = directory.path().join("terraleph-200-1-0.tfplan");
        let own = directory.path().join("terraleph-300-1-0.tfplan");
        let unrelated = directory.path().join("terraleph-100-notes.tfplan");
        let user_plan = directory.path().join("review.tfplan");
        for path in [&exited, &running, &own, &unrelated, &user_plan] {
            fs::write(path, "").expect("plan fixture should be written");
        }

        remove_orphaned_plans_in(directory.path(), 300, |process| process == 200);

        assert!(!exited.exists());
        assert!(running.exists());
        assert!(own.exists());
        assert!(unrelated.exists());
        assert!(user_plan.exists());
    }

    #[test]
    fn orphan_sweep_keeps_symlinks_named_like_plans() {
        let directory = tempfile::tempdir().expect("plan directory should be created");
        let target = directory.path().join("target");
        fs::write(&target, "").expect("link target should be written");
        let link = directory.path().join("terraleph-100-1-0.tfplan");
        std::os::unix::fs::symlink(&target, &link).expect("symlink should be created");

        remove_orphaned_plans_in(directory.path(), 300, |_| false);

        assert!(link.symlink_metadata().is_ok());
        assert!(target.exists());
    }

    #[test]
    fn process_liveness_distinguishes_running_and_reaped_processes() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("child should start");
        let reaped = child.id();
        child.wait().expect("child should be reaped");

        assert!(process_exists(std::process::id()));
        assert!(!process_exists(reaped));
    }
}
