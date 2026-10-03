use std::{
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::Output,
};

use crate::infra::CancellationToken;

use super::command::{GitCommandError, parse_error, run_git, run_git_with_env};

pub(super) enum DiscoveryError {
    OutsideRepository(String),
    Failed(GitCommandError),
}

pub(super) enum HeadError {
    Unavailable(String),
    Failed(GitCommandError),
}

pub(super) enum CompareRefError {
    Unavailable(String),
    Ambiguous(String),
    Failed(GitCommandError),
}

pub(super) fn discover_repository(
    root: &Path,
    cancellation: &CancellationToken,
) -> Result<PathBuf, DiscoveryError> {
    let output = run_git(
        root,
        "discover repository",
        ["rev-parse", "--show-toplevel"],
        cancellation,
    )
    .map_err(DiscoveryError::Failed)?;
    if !output.status.success() {
        let error = GitCommandError::from_output("discover repository", &output);
        return Err(DiscoveryError::OutsideRepository(error.message));
    }

    let repository_root = String::from_utf8(output.stdout)
        .map_err(|error| {
            DiscoveryError::Failed(parse_error("discover repository", &error.to_string()))
        })?
        .trim()
        .to_owned();
    fs::canonicalize(repository_root).map_err(|error| {
        DiscoveryError::Failed(parse_error("resolve repository root", &error.to_string()))
    })
}

pub(super) fn resolve_head(
    repository_root: &Path,
    cancellation: &CancellationToken,
) -> Result<String, HeadError> {
    let output = run_git(
        repository_root,
        "resolve HEAD",
        ["rev-parse", "--verify", "HEAD^{commit}"],
        cancellation,
    )
    .map_err(HeadError::Failed)?;
    if output.status.success() {
        single_commit(&output, "resolve HEAD").map_err(HeadError::Failed)
    } else {
        let error = GitCommandError::from_output("resolve HEAD", &output);
        Err(HeadError::Unavailable(error.message))
    }
}

pub(super) fn resolve_compare_ref(
    repository_root: &Path,
    compare_ref: &str,
    cancellation: &CancellationToken,
) -> Result<String, CompareRefError> {
    resolve_compare_ref_with_env_and_cancellation(repository_root, compare_ref, &[], cancellation)
}

fn resolve_compare_ref_with_env_and_cancellation(
    repository_root: &Path,
    compare_ref: &str,
    environment: &[(&str, &str)],
    cancellation: &CancellationToken,
) -> Result<String, CompareRefError> {
    if compare_ref.is_empty() {
        return Err(CompareRefError::Unavailable(
            "the comparison ref is empty".to_owned(),
        ));
    }

    if compare_ref.starts_with("refs/") {
        return resolve_commit_revision(repository_root, compare_ref, environment, cancellation)
            .map_err(|error| CompareRefError::Unavailable(error.message));
    }

    if !is_valid_comparison_ref_name(repository_root, compare_ref, environment, cancellation)? {
        return resolve_commit_revision(repository_root, compare_ref, environment, cancellation)
            .map_err(|error| CompareRefError::Unavailable(error.message));
    }

    if let Some(commit) =
        resolve_git_directory_ref(repository_root, compare_ref, environment, cancellation)?
    {
        return Ok(commit);
    }

    let candidates =
        comparison_ref_candidates(repository_root, compare_ref, environment, cancellation)?;
    if !candidates.is_empty() {
        if candidates.len() > 1 {
            return Err(CompareRefError::Ambiguous(format!(
                "the comparison ref is ambiguous; candidates: {}",
                candidates.join(", ")
            )));
        }
        return resolve_commit_revision(repository_root, &candidates[0], environment, cancellation)
            .map_err(|error| CompareRefError::Unavailable(error.message));
    }

    resolve_commit_revision(repository_root, compare_ref, environment, cancellation)
        .map_err(|error| CompareRefError::Unavailable(error.message))
}

fn comparison_ref_candidates(
    repository_root: &Path,
    compare_ref: &str,
    environment: &[(&str, &str)],
    cancellation: &CancellationToken,
) -> Result<Vec<String>, CompareRefError> {
    let patterns = [
        format!("refs/{compare_ref}"),
        format!("refs/tags/{compare_ref}"),
        format!("refs/heads/{compare_ref}"),
        format!("refs/remotes/{compare_ref}"),
        format!("refs/remotes/{compare_ref}/HEAD"),
    ];
    let mut args = vec![
        OsString::from("for-each-ref"),
        OsString::from("--format=%(refname)"),
        OsString::from("--"),
    ];
    args.extend(patterns.into_iter().map(OsString::from));
    let output = run_git_with_env(
        repository_root,
        "list comparison ref candidates",
        args,
        environment,
        cancellation,
    )
    .map_err(CompareRefError::Failed)?;
    if !output.status.success() {
        return Err(CompareRefError::Failed(GitCommandError::from_output(
            "list comparison ref candidates",
            &output,
        )));
    }

    String::from_utf8(output.stdout)
        .map_err(|error| {
            CompareRefError::Failed(parse_error(
                "list comparison ref candidates",
                &error.to_string(),
            ))
        })
        .map(|output| {
            output
                .lines()
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
                .map(str::to_owned)
                .collect()
        })
}

fn is_valid_comparison_ref_name(
    repository_root: &Path,
    compare_ref: &str,
    environment: &[(&str, &str)],
    cancellation: &CancellationToken,
) -> Result<bool, CompareRefError> {
    let output = run_git_with_env(
        repository_root,
        "validate comparison ref",
        [
            OsStr::new("check-ref-format"),
            OsStr::new("--allow-onelevel"),
            OsStr::new(compare_ref),
        ],
        environment,
        cancellation,
    )
    .map_err(CompareRefError::Failed)?;
    Ok(output.status.success())
}

fn resolve_git_directory_ref(
    repository_root: &Path,
    compare_ref: &str,
    environment: &[(&str, &str)],
    cancellation: &CancellationToken,
) -> Result<Option<String>, CompareRefError> {
    let output = run_git_with_env(
        repository_root,
        "resolve Git directory ref path",
        [
            OsStr::new("rev-parse"),
            OsStr::new("--git-path"),
            OsStr::new(compare_ref),
        ],
        environment,
        cancellation,
    )
    .map_err(CompareRefError::Failed)?;
    if !output.status.success() {
        return Err(CompareRefError::Failed(GitCommandError::from_output(
            "resolve Git directory ref path",
            &output,
        )));
    }

    let path = String::from_utf8(output.stdout)
        .map_err(|error| {
            CompareRefError::Failed(parse_error(
                "resolve Git directory ref path",
                &error.to_string(),
            ))
        })?
        .trim()
        .to_owned();
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        repository_root.join(path)
    };
    if !path.is_file() {
        return Ok(None);
    }

    if cancellation.is_cancelled() {
        return Err(CompareRefError::Failed(GitCommandError::interrupted(
            "read Git directory ref",
        )));
    }
    let content = fs::read(&path).map_err(|error| {
        CompareRefError::Failed(parse_error("read Git directory ref", &error.to_string()))
    })?;
    if !looks_like_git_directory_ref(compare_ref, &content) {
        return Ok(None);
    }

    resolve_commit_revision(repository_root, compare_ref, environment, cancellation)
        .map(Some)
        .map_err(|error| CompareRefError::Unavailable(error.message))
}

fn looks_like_git_directory_ref(compare_ref: &str, content: &[u8]) -> bool {
    if compare_ref == "HEAD"
        && content
            .strip_prefix(b"ref: refs/")
            .is_some_and(|ref_name| !ref_name.is_empty())
    {
        return true;
    }

    let Ok(content) = std::str::from_utf8(content) else {
        return false;
    };
    let lines = content.lines().filter(|line| !line.is_empty());
    let mut has_line = false;
    for line in lines {
        let Some(object_id) = line.split_whitespace().next() else {
            return false;
        };
        if !is_object_id(object_id) {
            return false;
        }
        has_line = true;
    }
    has_line
}

fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_commit_revision(
    repository_root: &Path,
    revision: &str,
    environment: &[(&str, &str)],
    cancellation: &CancellationToken,
) -> Result<String, GitCommandError> {
    let revision = format!("{revision}^{{commit}}");
    let output = run_git_with_env(
        repository_root,
        "resolve comparison ref",
        [
            OsStr::new("rev-parse"),
            OsStr::new("--verify"),
            OsStr::new("--quiet"),
            OsStr::new("--end-of-options"),
            OsStr::new(&revision),
        ],
        environment,
        cancellation,
    )?;
    if output.status.success() {
        single_commit(&output, "resolve comparison ref")
    } else {
        Err(GitCommandError::from_output(
            "resolve comparison ref",
            &output,
        ))
    }
}

fn single_commit(output: &Output, operation: &str) -> Result<String, GitCommandError> {
    let commits = String::from_utf8(output.stdout.clone())
        .map_err(|error| parse_error(operation, &error.to_string()))?
        .lines()
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match commits.as_slice() {
        [commit] => Ok(commit.clone()),
        _ => Err(parse_error(
            operation,
            "Git did not resolve exactly one commit",
        )),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn resolve_compare_ref_with_env(
        repository_root: &Path,
        compare_ref: &str,
        environment: &[(&str, &str)],
    ) -> Option<String> {
        let cancellation = CancellationToken::default();
        resolve_compare_ref_with_env_and_cancellation(
            repository_root,
            compare_ref,
            environment,
            &cancellation,
        )
        .ok()
    }
}
