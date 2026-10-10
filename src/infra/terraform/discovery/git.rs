use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

// Explicit glob magic keeps `**/` matching any depth whatever pathspec defaults Git runs with.
const CONFIGURATION_PATHSPECS: [&str; 4] = [
    ":(glob)**/*.tf",
    ":(glob)**/*.tf.json",
    ":(glob)**/*.tofu",
    ":(glob)**/*.tofu.json",
];

// Returns the directories below `root` holding configuration that Git tracks, or would track if
// added, or `None` when Git cannot list them, such as outside a work tree or without Git.
// Submodule contents are not listed. Tracked files deleted from the work tree leave no directory.
pub(super) fn configuration_directories(root: &Path) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ])
        .args(CONFIGURATION_PATHSPECS)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let directories = output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(relative_path)
        .filter_map(|path| path.parent().map(Path::to_owned))
        .filter(|directory| !directory.as_os_str().is_empty() && !has_hidden_component(directory))
        .collect::<BTreeSet<_>>();
    Some(
        directories
            .into_iter()
            .map(|directory| root.join(directory))
            .filter(|directory| directory.is_dir())
            .collect(),
    )
}

// Hidden directories, such as `.terraform` with downloaded modules, are skipped as in the walk
// even when Git does not ignore them.
fn has_hidden_component(directory: &Path) -> bool {
    directory
        .components()
        .any(|component| matches!(component, Component::Normal(name) if super::is_hidden(name)))
}

#[cfg(unix)]
fn relative_path(entry: &[u8]) -> Option<PathBuf> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    (!entry.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(entry)))
}

// Git for Windows prints paths as UTF-8.
#[cfg(not(unix))]
fn relative_path(entry: &[u8]) -> Option<PathBuf> {
    (!entry.is_empty())
        .then(|| std::str::from_utf8(entry).ok().map(PathBuf::from))
        .flatten()
}
