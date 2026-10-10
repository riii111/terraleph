use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

pub(super) enum Listing {
    // Outside a work tree, or Git could not run.
    Unavailable,
    // A work tree in which Git lists no configuration file, such as one that ignores it all.
    Empty,
    // May be empty once hidden and deleted directories are left out; that is still Git's answer.
    Directories(Vec<PathBuf>),
}

// Explicit glob magic keeps `**/` matching any depth whatever pathspec defaults Git runs with.
const CONFIGURATION_PATHSPECS: [&str; 4] = [
    ":(glob)**/*.tf",
    ":(glob)**/*.tf.json",
    ":(glob)**/*.tofu",
    ":(glob)**/*.tofu.json",
];

// Lists the directories below `root` holding configuration that Git tracks, or would track if
// added. Submodule contents are not listed.
pub(super) fn list_configuration(root: &Path) -> Listing {
    let Ok(output) = Command::new("git")
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
    else {
        return Listing::Unavailable;
    };
    if !output.status.success() {
        return Listing::Unavailable;
    }
    let files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(relative_path)
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Listing::Empty;
    }
    let directories = files
        .iter()
        .filter_map(|path| path.parent())
        .filter(|directory| !directory.as_os_str().is_empty() && !has_hidden_component(directory))
        .collect::<BTreeSet<_>>();
    Listing::Directories(
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
