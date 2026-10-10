use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

pub(super) enum Listing {
    Unavailable,
    Empty,
    Directories(Vec<PathBuf>),
}

// A plain `*.tf` stops matching across `/` when GIT_GLOB_PATHSPECS is set.
const CONFIGURATION_PATHSPECS: [&str; 4] = [
    ":(glob)**/*.tf",
    ":(glob)**/*.tf.json",
    ":(glob)**/*.tofu",
    ":(glob)**/*.tofu.json",
];

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

// Git still lists an unignored `.terraform`, whose downloaded modules are not environments.
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

#[cfg(not(unix))]
fn relative_path(entry: &[u8]) -> Option<PathBuf> {
    (!entry.is_empty())
        .then(|| std::str::from_utf8(entry).ok().map(PathBuf::from))
        .flatten()
}
