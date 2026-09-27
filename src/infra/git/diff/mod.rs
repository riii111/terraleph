use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::app::attribution::{SourceLineChange, SourceRange, SourceSide};
use crate::infra::CancellationToken;

use crate::infra::terraform::hcl::HclSourceFile;

mod hunks;
mod name_status;
mod source;

use super::{
    command::{GitCommandError, GitInterrupted},
    merge_base::{MergeBaseError, resolve_merge_base},
    rev_parse::{
        CompareRefError, DiscoveryError, HeadError, discover_repository, resolve_compare_ref,
        resolve_head,
    },
};

use self::{
    hunks::{add_missing_added_line_ranges, changed_lines, changed_lines_between},
    name_status::{
        changed_files, changed_files_between, files_without_head, merge_untracked, untracked_files,
    },
    source::{load_after_sources, load_commit_sources, load_sources},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComparisonBasis {
    WorkingTreeVsHead,
    HeadVsMergeBase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitDiffStatus {
    Complete,
    OutsideRepository {
        message: String,
    },
    HeadUnavailable {
        message: String,
    },
    CompareRefUnavailable {
        reference: String,
        message: String,
    },
    AmbiguousCompareRef {
        reference: String,
        message: String,
    },
    NoCommonAncestor {
        reference: String,
        message: String,
    },
    AmbiguousMergeBase {
        reference: String,
        merge_bases: Vec<String>,
        message: String,
    },
    Failed {
        operation: String,
        message: String,
    },
}

impl GitDiffStatus {
    #[must_use]
    pub(crate) fn message(&self) -> Option<&str> {
        match self {
            Self::Complete => None,
            Self::OutsideRepository { message }
            | Self::HeadUnavailable { message }
            | Self::CompareRefUnavailable { message, .. }
            | Self::AmbiguousCompareRef { message, .. }
            | Self::NoCommonAncestor { message, .. }
            | Self::AmbiguousMergeBase { message, .. }
            | Self::Failed { message, .. } => Some(message),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComparisonMetadata {
    basis: ComparisonBasis,
    compare_ref: Option<String>,
    resolved_commit: Option<String>,
    head_commit: Option<String>,
    merge_base: Option<String>,
}

impl ComparisonMetadata {
    const fn working_tree() -> Self {
        Self {
            basis: ComparisonBasis::WorkingTreeVsHead,
            compare_ref: None,
            resolved_commit: None,
            head_commit: None,
            merge_base: None,
        }
    }

    fn for_compare_ref(compare_ref: &str) -> Self {
        Self {
            basis: ComparisonBasis::HeadVsMergeBase,
            compare_ref: Some(compare_ref.to_owned()),
            resolved_commit: None,
            head_commit: None,
            merge_base: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitDiff {
    root: PathBuf,
    repository_root: Option<PathBuf>,
    comparison: ComparisonMetadata,
    status: GitDiffStatus,
    before: Vec<HclSourceFile>,
    after: Vec<HclSourceFile>,
    changed_lines: Vec<SourceLineChange>,
}

impl GitDiff {
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub(crate) fn repository_root(&self) -> Option<&Path> {
        self.repository_root.as_deref()
    }

    #[must_use]
    pub(crate) const fn basis(&self) -> ComparisonBasis {
        self.comparison.basis
    }

    #[must_use]
    pub(crate) fn compare_ref(&self) -> Option<&str> {
        self.comparison.compare_ref.as_deref()
    }

    #[must_use]
    pub(crate) fn head_commit(&self) -> Option<&str> {
        self.comparison.head_commit.as_deref()
    }

    #[must_use]
    pub(crate) fn merge_base(&self) -> Option<&str> {
        self.comparison.merge_base.as_deref()
    }

    #[must_use]
    pub(crate) const fn status(&self) -> &GitDiffStatus {
        &self.status
    }

    #[must_use]
    pub(crate) fn before(&self) -> &[HclSourceFile] {
        &self.before
    }

    #[must_use]
    pub(crate) fn after(&self) -> &[HclSourceFile] {
        &self.after
    }

    #[must_use]
    pub(crate) fn changed_lines(&self) -> &[SourceLineChange] {
        &self.changed_lines
    }
}

struct ResolvedComparison {
    comparison: ComparisonMetadata,
    before_revision: String,
    after_revision: String,
}

struct ComparisonResolutionError {
    comparison: ComparisonMetadata,
    failure: ComparisonResolutionFailure,
}

enum ComparisonResolutionFailure {
    HeadUnavailable(String),
    HeadFailed(GitCommandError),
    CompareRefUnavailable(String),
    AmbiguousCompareRef(String),
    CompareRefFailed(GitCommandError),
    NoCommonAncestor,
    AmbiguousMergeBase(Vec<String>),
    MergeBaseFailed(GitCommandError),
}

pub(crate) fn collect_diff_with_cancellation(
    root: &Path,
    cancellation: &CancellationToken,
) -> Result<GitDiff, GitInterrupted> {
    let root = match fs::canonicalize(root) {
        Ok(root) if root.is_dir() => root,
        Ok(root) => {
            return Ok(failed_diff(
                root,
                None,
                "read Terraform root",
                "the Terraform root is not a directory",
            ));
        }
        Err(error) => {
            let message = error.to_string();
            return Ok(failed_diff(
                root.to_owned(),
                None,
                "read Terraform root",
                &message,
            ));
        }
    };

    let repository_root = match discover_repository(&root, cancellation) {
        Ok(repository_root) => repository_root,
        Err(DiscoveryError::OutsideRepository(message)) => {
            return Ok(unavailable_diff(
                root,
                None,
                GitDiffStatus::OutsideRepository { message },
            ));
        }
        Err(DiscoveryError::Failed(error)) => {
            return failed_diff_from_error(root, None, &error);
        }
    };

    let root_relative = match root.strip_prefix(&repository_root) {
        Ok(relative) => relative,
        Err(error) => {
            let message = error.to_string();
            return Ok(failed_diff(
                root,
                Some(repository_root),
                "resolve Terraform root",
                &message,
            ));
        }
    };
    let root_spec = if root_relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root_relative.to_owned()
    };

    match resolve_head(&repository_root, cancellation) {
        Ok(head_commit) => {
            collect_head_diff(root, repository_root, &root_spec, head_commit, cancellation)
        }
        Err(HeadError::Unavailable(message)) => {
            collect_without_head(root, repository_root, &root_spec, message, cancellation)
        }
        Err(HeadError::Failed(error)) => {
            failed_diff_from_error(root, Some(repository_root), &error)
        }
    }
}

pub(crate) fn collect_diff_against_ref_with_cancellation(
    root: &Path,
    compare_ref: &str,
    cancellation: &CancellationToken,
) -> Result<GitDiff, GitInterrupted> {
    let comparison = ComparisonMetadata::for_compare_ref(compare_ref);
    let root = match fs::canonicalize(root) {
        Ok(root) if root.is_dir() => root,
        Ok(root) => {
            return Ok(failed_diff_with_comparison(
                root,
                None,
                "read Terraform root",
                "the Terraform root is not a directory",
                comparison,
            ));
        }
        Err(error) => {
            let message = error.to_string();
            return Ok(failed_diff_with_comparison(
                root.to_owned(),
                None,
                "read Terraform root",
                &message,
                comparison,
            ));
        }
    };

    let repository_root = match discover_repository(&root, cancellation) {
        Ok(repository_root) => repository_root,
        Err(DiscoveryError::OutsideRepository(message)) => {
            return Ok(unavailable_diff_with_comparison(
                root,
                None,
                GitDiffStatus::OutsideRepository { message },
                comparison,
            ));
        }
        Err(DiscoveryError::Failed(error)) => {
            return failed_diff_with_comparison_error(root, None, &error, comparison);
        }
    };

    let root_relative = match root.strip_prefix(&repository_root) {
        Ok(relative) => relative,
        Err(error) => {
            let message = error.to_string();
            return Ok(failed_diff_with_comparison(
                root,
                Some(repository_root),
                "resolve Terraform root",
                &message,
                comparison,
            ));
        }
    };
    let root_spec = if root_relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root_relative.to_owned()
    };

    let resolution =
        match resolve_comparison(&repository_root, compare_ref, comparison, cancellation) {
            Ok(resolution) => resolution,
            Err(error) => return (*error).into_diff(root, Some(repository_root)),
        };

    collect_commit_diff(
        root,
        repository_root,
        &root_spec,
        &resolution.before_revision,
        &resolution.after_revision,
        resolution.comparison,
        cancellation,
    )
}

fn collect_commit_diff(
    root: PathBuf,
    repository_root: PathBuf,
    root_spec: &Path,
    before_revision: &str,
    after_revision: &str,
    comparison: ComparisonMetadata,
    cancellation: &CancellationToken,
) -> Result<GitDiff, GitInterrupted> {
    let changed_files = match changed_files_between(
        &repository_root,
        &root,
        root_spec,
        before_revision,
        after_revision,
        cancellation,
    ) {
        Ok(files) => files,
        Err(error) => {
            return failed_diff_with_comparison_error(
                root,
                Some(repository_root),
                &error,
                comparison,
            );
        }
    };
    let (before, after) = match load_commit_sources(
        &repository_root,
        &changed_files,
        before_revision,
        after_revision,
        cancellation,
    ) {
        Ok(sources) => sources,
        Err(error) => {
            return failed_diff_with_comparison_error(
                root,
                Some(repository_root),
                &error,
                comparison,
            );
        }
    };
    let mut changed_lines = match changed_lines_between(
        &repository_root,
        &root,
        root_spec,
        before_revision,
        after_revision,
        cancellation,
    ) {
        Ok(changed_lines) => changed_lines,
        Err(error) => {
            return failed_diff_with_comparison_error(
                root,
                Some(repository_root),
                &error,
                comparison,
            );
        }
    };
    add_missing_added_line_ranges(&mut changed_lines, &after, &changed_files);
    changed_lines.sort_by(|left, right| {
        left.path()
            .cmp(right.path())
            .then_with(|| source_side_order(left.side()).cmp(&source_side_order(right.side())))
            .then_with(|| left.range().start_line().cmp(&right.range().start_line()))
    });

    Ok(GitDiff {
        root,
        repository_root: Some(repository_root),
        comparison,
        status: GitDiffStatus::Complete,
        before,
        after,
        changed_lines,
    })
}

fn collect_head_diff(
    root: PathBuf,
    repository_root: PathBuf,
    root_spec: &Path,
    head_commit: String,
    cancellation: &CancellationToken,
) -> Result<GitDiff, GitInterrupted> {
    let changed_files = match changed_files(&repository_root, &root, root_spec, cancellation) {
        Ok(files) => files,
        Err(error) => {
            return failed_diff_from_error(root, Some(repository_root), &error);
        }
    };
    let untracked_files = match untracked_files(&repository_root, &root, root_spec, cancellation) {
        Ok(files) => files,
        Err(error) => {
            return failed_diff_from_error(root, Some(repository_root), &error);
        }
    };
    let changed_files = merge_untracked(changed_files, untracked_files);
    let (before, after) = match load_sources(&repository_root, &changed_files, cancellation) {
        Ok(sources) => sources,
        Err(error) => {
            return failed_diff_from_error(root, Some(repository_root), &error);
        }
    };
    let changed_lines = match changed_lines(&repository_root, &root, root_spec, cancellation) {
        Ok(changed_lines) => changed_lines,
        Err(error) => {
            return failed_diff_from_error(root, Some(repository_root), &error);
        }
    };
    let mut changed_lines = changed_lines;
    add_missing_added_line_ranges(&mut changed_lines, &after, &changed_files);
    changed_lines.sort_by(|left, right| {
        left.path()
            .cmp(right.path())
            .then_with(|| source_side_order(left.side()).cmp(&source_side_order(right.side())))
            .then_with(|| left.range().start_line().cmp(&right.range().start_line()))
    });

    Ok(GitDiff {
        root,
        repository_root: Some(repository_root),
        comparison: ComparisonMetadata {
            basis: ComparisonBasis::WorkingTreeVsHead,
            compare_ref: None,
            resolved_commit: None,
            head_commit: Some(head_commit),
            merge_base: None,
        },
        status: GitDiffStatus::Complete,
        before,
        after,
        changed_lines,
    })
}

fn collect_without_head(
    root: PathBuf,
    repository_root: PathBuf,
    root_spec: &Path,
    message: String,
    cancellation: &CancellationToken,
) -> Result<GitDiff, GitInterrupted> {
    let files = match files_without_head(&repository_root, &root, root_spec, cancellation) {
        Ok(files) => files,
        Err(error) => {
            return failed_diff_from_error(root, Some(repository_root), &error);
        }
    };
    let after = match load_after_sources(&repository_root, &files, cancellation) {
        Ok(after) => after,
        Err(error) => {
            return failed_diff_from_error(root, Some(repository_root), &error);
        }
    };
    let changed_lines = after
        .iter()
        .filter_map(|file| {
            let line_count = file.source().lines().count();
            (line_count > 0).then(|| {
                SourceLineChange::new(
                    file.path().to_owned(),
                    SourceSide::After,
                    SourceRange::new(1, line_count),
                )
            })
        })
        .collect();

    Ok(GitDiff {
        root,
        repository_root: Some(repository_root),
        comparison: ComparisonMetadata::working_tree(),
        status: GitDiffStatus::HeadUnavailable { message },
        before: Vec::new(),
        after,
        changed_lines,
    })
}

fn resolve_comparison(
    repository_root: &Path,
    compare_ref: &str,
    mut comparison: ComparisonMetadata,
    cancellation: &CancellationToken,
) -> Result<ResolvedComparison, Box<ComparisonResolutionError>> {
    let head_commit = resolve_head(repository_root, cancellation).map_err(|error| {
        Box::new(ComparisonResolutionError {
            comparison: comparison.clone(),
            failure: match error {
                HeadError::Unavailable(message) => {
                    ComparisonResolutionFailure::HeadUnavailable(message)
                }
                HeadError::Failed(error) => ComparisonResolutionFailure::HeadFailed(error),
            },
        })
    })?;
    comparison.head_commit = Some(head_commit.clone());

    let resolved_commit =
        resolve_compare_ref(repository_root, compare_ref, cancellation).map_err(|error| {
            Box::new(ComparisonResolutionError {
                comparison: comparison.clone(),
                failure: match error {
                    CompareRefError::Unavailable(message) => {
                        ComparisonResolutionFailure::CompareRefUnavailable(message)
                    }
                    CompareRefError::Ambiguous(message) => {
                        ComparisonResolutionFailure::AmbiguousCompareRef(message)
                    }
                    CompareRefError::Failed(error) => {
                        ComparisonResolutionFailure::CompareRefFailed(error)
                    }
                },
            })
        })?;
    comparison.resolved_commit = Some(resolved_commit.clone());

    let merge_base = resolve_merge_base(
        repository_root,
        &resolved_commit,
        &head_commit,
        cancellation,
    )
    .map_err(|error| {
        Box::new(ComparisonResolutionError {
            comparison: comparison.clone(),
            failure: match error {
                MergeBaseError::NoCommonAncestor => ComparisonResolutionFailure::NoCommonAncestor,
                MergeBaseError::Ambiguous(merge_bases) => {
                    ComparisonResolutionFailure::AmbiguousMergeBase(merge_bases)
                }
                MergeBaseError::Failed(error) => {
                    ComparisonResolutionFailure::MergeBaseFailed(error)
                }
            },
        })
    })?;
    comparison.merge_base = Some(merge_base.clone());

    Ok(ResolvedComparison {
        comparison,
        before_revision: merge_base,
        after_revision: head_commit,
    })
}

impl ComparisonResolutionError {
    fn into_diff(
        self,
        root: PathBuf,
        repository_root: Option<PathBuf>,
    ) -> Result<GitDiff, GitInterrupted> {
        let Some(reference) = self.comparison.compare_ref.clone() else {
            return Ok(failed_diff_with_comparison(
                root,
                repository_root,
                "resolve comparison",
                "comparison ref is missing",
                self.comparison,
            ));
        };
        let status = match self.failure {
            ComparisonResolutionFailure::HeadUnavailable(message) => {
                GitDiffStatus::HeadUnavailable { message }
            }
            ComparisonResolutionFailure::HeadFailed(error)
            | ComparisonResolutionFailure::CompareRefFailed(error)
            | ComparisonResolutionFailure::MergeBaseFailed(error) => {
                if error.is_interrupted() {
                    return Err(GitInterrupted);
                }
                GitDiffStatus::Failed {
                    operation: error.operation,
                    message: error.message,
                }
            }
            ComparisonResolutionFailure::CompareRefUnavailable(message) => {
                GitDiffStatus::CompareRefUnavailable { reference, message }
            }
            ComparisonResolutionFailure::AmbiguousCompareRef(message) => {
                GitDiffStatus::AmbiguousCompareRef { reference, message }
            }
            ComparisonResolutionFailure::NoCommonAncestor => GitDiffStatus::NoCommonAncestor {
                reference,
                message: "the comparison ref and HEAD have no common ancestor".to_owned(),
            },
            ComparisonResolutionFailure::AmbiguousMergeBase(merge_bases) => {
                let message = format!(
                    "the comparison basis has multiple merge-bases: {}",
                    merge_bases.join(", ")
                );
                GitDiffStatus::AmbiguousMergeBase {
                    reference,
                    merge_bases,
                    message,
                }
            }
        };
        Ok(unavailable_diff_with_comparison(
            root,
            repository_root,
            status,
            self.comparison,
        ))
    }
}

const fn source_side_order(side: SourceSide) -> u8 {
    match side {
        SourceSide::Before => 0,
        SourceSide::After => 1,
    }
}

const fn unavailable_diff(
    root: PathBuf,
    repository_root: Option<PathBuf>,
    status: GitDiffStatus,
) -> GitDiff {
    unavailable_diff_with_comparison(
        root,
        repository_root,
        status,
        ComparisonMetadata::working_tree(),
    )
}

const fn unavailable_diff_with_comparison(
    root: PathBuf,
    repository_root: Option<PathBuf>,
    status: GitDiffStatus,
    comparison: ComparisonMetadata,
) -> GitDiff {
    GitDiff {
        root,
        repository_root,
        comparison,
        status,
        before: Vec::new(),
        after: Vec::new(),
        changed_lines: Vec::new(),
    }
}

fn failed_diff(
    root: PathBuf,
    repository_root: Option<PathBuf>,
    operation: &str,
    message: &str,
) -> GitDiff {
    failed_diff_with_comparison(
        root,
        repository_root,
        operation,
        message,
        ComparisonMetadata::working_tree(),
    )
}

fn failed_diff_from_error(
    root: PathBuf,
    repository_root: Option<PathBuf>,
    error: &GitCommandError,
) -> Result<GitDiff, GitInterrupted> {
    if error.is_interrupted() {
        return Err(GitInterrupted);
    }
    Ok(failed_diff(
        root,
        repository_root,
        &error.operation,
        &error.message,
    ))
}

fn failed_diff_with_comparison(
    root: PathBuf,
    repository_root: Option<PathBuf>,
    operation: &str,
    message: &str,
    comparison: ComparisonMetadata,
) -> GitDiff {
    unavailable_diff_with_comparison(
        root,
        repository_root,
        GitDiffStatus::Failed {
            operation: operation.to_owned(),
            message: message.to_owned(),
        },
        comparison,
    )
}

fn failed_diff_with_comparison_error(
    root: PathBuf,
    repository_root: Option<PathBuf>,
    error: &GitCommandError,
    comparison: ComparisonMetadata,
) -> Result<GitDiff, GitInterrupted> {
    if error.is_interrupted() {
        return Err(GitInterrupted);
    }
    Ok(failed_diff_with_comparison(
        root,
        repository_root,
        &error.operation,
        &error.message,
        comparison,
    ))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use std::process::Command;

    use super::super::rev_parse::tests::resolve_compare_ref_with_env;
    use super::*;

    static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

    fn collect_diff(root: &Path) -> GitDiff {
        collect_diff_with_cancellation(root, &CancellationToken::new())
            .expect("Git diff should not be interrupted")
    }

    fn collect_diff_against_ref(root: &Path, compare_ref: &str) -> GitDiff {
        collect_diff_against_ref_with_cancellation(root, compare_ref, &CancellationToken::new())
            .expect("Git diff should not be interrupted")
    }

    struct TestRepository {
        path: PathBuf,
    }

    impl TestRepository {
        fn new() -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is before Unix epoch")
                .as_nanos();
            let suffix = format!(
                "terraleph-git-{}-{}-{}",
                std::process::id(),
                suffix,
                NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(suffix);
            fs::create_dir(&path).expect("create temporary repository");
            git(&path, &["init", "--quiet"]);
            git(&path, &["config", "user.email", "test@example.com"]);
            git(&path, &["config", "user.name", "Terraleph Test"]);
            Self { path }
        }

        fn commit(&self, message: &str) {
            git(&self.path, &["add", "."]);
            git(&self.path, &["commit", "--quiet", "-m", message]);
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("remove temporary repository");
        }
    }

    fn git(repository: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .expect("run git in test repository");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .expect("run git in test repository");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn write(repository: &TestRepository, relative: &str, source: &str) {
        let path = repository.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create source parent");
        }
        fs::write(path, source).expect("write source");
    }

    fn source_names(files: &[HclSourceFile]) -> Vec<&Path> {
        files.iter().map(HclSourceFile::path).collect()
    }

    #[test]
    fn combines_staged_and_unstaged_changes_against_head() {
        let repository = TestRepository::new();
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"before\"\n}\n",
        );
        repository.commit("initial");
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"after\"\n  extra = true\n}\n",
        );
        git(&repository.path, &["add", "main.tf"]);
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"after\"\n  extra = false\n}\n",
        );

        let result = collect_diff(&repository.path);

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(result.basis(), ComparisonBasis::WorkingTreeVsHead);
        assert_eq!(
            result.before()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"before\"\n}\n"
        );
        assert_eq!(
            result.after()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"after\"\n  extra = false\n}\n"
        );
        assert_eq!(result.changed_lines().len(), 2);
        assert_eq!(result.changed_lines()[0].side(), SourceSide::Before);
        assert_eq!(result.changed_lines()[0].range(), SourceRange::new(2, 2));
        assert_eq!(result.changed_lines()[1].side(), SourceSide::After);
        assert_eq!(result.changed_lines()[1].range(), SourceRange::new(2, 3));
    }

    #[test]
    fn compares_commits_without_including_dirty_worktree_changes() {
        let repository = TestRepository::new();
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"base\"\n}\n",
        );
        repository.commit("initial");
        git(&repository.path, &["branch", "compare"]);
        git(&repository.path, &["switch", "compare"]);
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"compare\"\n}\n",
        );
        repository.commit("compare change");
        git(&repository.path, &["switch", "-c", "feature"]);
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"feature\"\n}\n",
        );
        repository.commit("feature change");
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"dirty\"\n}\n",
        );

        let result = collect_diff_against_ref(&repository.path, "compare");

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(result.basis(), ComparisonBasis::HeadVsMergeBase);
        assert_eq!(result.compare_ref(), Some("compare"));
        assert_eq!(
            result.before()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"compare\"\n}\n"
        );
        assert_eq!(
            result.after()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"feature\"\n}\n"
        );
        assert_eq!(
            result.comparison.resolved_commit.as_deref(),
            result.merge_base()
        );
        assert!(result.head_commit().is_some());
        assert_eq!(result.changed_lines().len(), 2);
        assert_eq!(result.changed_lines()[0].range(), SourceRange::new(2, 2));
        assert_eq!(result.changed_lines()[1].range(), SourceRange::new(2, 2));
    }

    #[test]
    fn reports_an_unavailable_comparison_ref_with_the_requested_ref() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");

        let result = collect_diff_against_ref(&repository.path, "missing");

        assert!(matches!(
            result.status(),
            GitDiffStatus::CompareRefUnavailable { reference, message }
                if reference == "missing" && !message.is_empty()
        ));
        assert_eq!(result.compare_ref(), Some("missing"));
        assert!(result.comparison.resolved_commit.is_none());
        assert!(result.head_commit().is_some());
    }

    #[test]
    fn reports_an_ambiguous_ref_named_like_a_git_directory_file_without_git_warnings() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        git(&repository.path, &["branch", "config"]);
        git(&repository.path, &["tag", "config"]);
        git(
            &repository.path,
            &["config", "core.warnAmbiguousRefs", "false"],
        );

        let result = collect_diff_against_ref(&repository.path, "config");

        assert!(matches!(
            result.status(),
            GitDiffStatus::AmbiguousCompareRef { reference, message }
                if reference == "config"
                    && message.contains("refs/heads/config")
                    && message.contains("refs/tags/config")
        ));
        assert_eq!(result.compare_ref(), Some("config"));
        assert!(result.comparison.resolved_commit.is_none());
        assert!(result.head_commit().is_some());
    }

    #[test]
    fn resolves_an_unambiguous_ref_when_git_trace_writes_to_stderr() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        git(&repository.path, &["branch", "compare"]);
        let expected_commit = git_output(&repository.path, &["rev-parse", "HEAD"]);

        let result =
            resolve_compare_ref_with_env(&repository.path, "compare", &[("GIT_TRACE", "1")]);

        assert_eq!(result.as_deref(), Some(expected_commit.as_str()));
    }

    #[test]
    fn resolves_a_fully_qualified_ref_without_short_name_expansion() {
        let repository = TestRepository::new();
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"branch\"\n}\n",
        );
        repository.commit("branch target");
        git(&repository.path, &["branch", "foo"]);
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n",
        );
        repository.commit("tag target");
        git(&repository.path, &["tag", "refs/heads/foo"]);

        let result = collect_diff_against_ref(&repository.path, "refs/heads/foo");

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(
            result.before()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"branch\"\n}\n"
        );
        assert_eq!(
            result.after()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n"
        );
    }

    #[test]
    fn resolves_a_git_directory_pseudo_ref_before_namespace_expansion() {
        let repository = TestRepository::new();
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"pseudo-ref\"\n}\n",
        );
        repository.commit("pseudo-ref target");
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n",
        );
        repository.commit("tag target");
        git(&repository.path, &["tag", "ORIG_HEAD"]);
        let orig_head_path =
            git_output(&repository.path, &["rev-parse", "--git-path", "ORIG_HEAD"]);
        let pseudo_ref_target = git_output(&repository.path, &["rev-parse", "HEAD^"]);
        let orig_head_path = PathBuf::from(orig_head_path);
        let orig_head_path = if orig_head_path.is_absolute() {
            orig_head_path
        } else {
            repository.path.join(orig_head_path)
        };
        assert!(
            Path::new(&orig_head_path)
                .parent()
                .is_some_and(Path::is_dir),
            "{orig_head_path:?}"
        );
        fs::write(orig_head_path, format!("{pseudo_ref_target}\n"))
            .expect("write Git directory pseudo-ref");

        let result = collect_diff_against_ref(&repository.path, "ORIG_HEAD");

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(
            result.before()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"pseudo-ref\"\n}\n"
        );
        assert_eq!(
            result.after()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n"
        );
    }

    #[test]
    fn resolves_a_custom_git_directory_root_ref_before_namespace_expansion() {
        let repository = TestRepository::new();
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"root-ref\"\n}\n",
        );
        repository.commit("root-ref target");
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n",
        );
        repository.commit("tag target");
        git(&repository.path, &["tag", "CUSTOM_HEAD"]);
        let root_ref_path = git_output(
            &repository.path,
            &["rev-parse", "--git-path", "CUSTOM_HEAD"],
        );
        let root_ref_target = git_output(&repository.path, &["rev-parse", "HEAD^"]);
        let root_ref_path = PathBuf::from(root_ref_path);
        let root_ref_path = if root_ref_path.is_absolute() {
            root_ref_path
        } else {
            repository.path.join(root_ref_path)
        };
        fs::write(root_ref_path, format!("{root_ref_target}\n"))
            .expect("write Git directory root ref");

        let result = collect_diff_against_ref(&repository.path, "CUSTOM_HEAD");

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(
            result.before()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"root-ref\"\n}\n"
        );
        assert_eq!(
            result.after()[0].source(),
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n"
        );
    }

    #[test]
    fn reports_a_failed_root_ref_without_falling_back_to_a_namespace_ref() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("root-ref target");
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"tag\"\n}\n",
        );
        repository.commit("tag target");
        git(&repository.path, &["tag", "CUSTOM_HEAD"]);
        let root_ref_path = git_output(
            &repository.path,
            &["rev-parse", "--git-path", "CUSTOM_HEAD"],
        );
        let blob_target = git_output(&repository.path, &["rev-parse", "HEAD^:main.tf"]);
        let root_ref_path = PathBuf::from(root_ref_path);
        let root_ref_path = if root_ref_path.is_absolute() {
            root_ref_path
        } else {
            repository.path.join(root_ref_path)
        };
        fs::write(root_ref_path, format!("{blob_target}\n")).expect("write Git directory root ref");

        let result = collect_diff_against_ref(&repository.path, "CUSTOM_HEAD");

        assert!(matches!(
            result.status(),
            GitDiffStatus::CompareRefUnavailable { reference, message }
                if reference == "CUSTOM_HEAD" && !message.is_empty()
        ));
        assert!(result.comparison.resolved_commit.is_none());
        assert!(result.head_commit().is_some());
    }

    #[test]
    fn reports_when_comparison_commits_have_no_common_ancestor() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        git(&repository.path, &["branch", "root"]);
        git(&repository.path, &["switch", "--orphan", "unrelated"]);
        write(&repository, "other.tf", "resource \"example\" \"two\" {}\n");
        repository.commit("unrelated");

        let result = collect_diff_against_ref(&repository.path, "root");

        assert!(matches!(
            result.status(),
            GitDiffStatus::NoCommonAncestor { reference, message }
                if reference == "root" && !message.is_empty()
        ));
        assert!(result.comparison.resolved_commit.is_some());
        assert!(result.head_commit().is_some());
        assert!(result.merge_base().is_none());
    }

    #[test]
    fn reports_ambiguous_comparison_when_merge_base_is_not_unique() {
        let repository = TestRepository::new();
        write(&repository, "base.tf", "resource \"example\" \"base\" {}\n");
        repository.commit("initial");
        git(&repository.path, &["branch", "base"]);
        git(&repository.path, &["switch", "-c", "branch-a"]);
        write(&repository, "a.tf", "resource \"example\" \"a\" {}\n");
        repository.commit("branch a");
        git(&repository.path, &["branch", "a-tip"]);
        git(&repository.path, &["switch", "-c", "branch-b", "base"]);
        write(&repository, "b.tf", "resource \"example\" \"b\" {}\n");
        repository.commit("branch b");
        git(&repository.path, &["branch", "b-tip"]);
        git(&repository.path, &["switch", "branch-a"]);
        git(
            &repository.path,
            &["merge", "--no-ff", "--no-edit", "b-tip"],
        );
        git(&repository.path, &["switch", "branch-b"]);
        git(
            &repository.path,
            &["merge", "--no-ff", "--no-edit", "a-tip"],
        );

        let result = collect_diff_against_ref(&repository.path, "branch-a");

        assert!(matches!(
            result.status(),
            GitDiffStatus::AmbiguousMergeBase {
                reference,
                merge_bases,
                message,
            } if reference == "branch-a" && merge_bases.len() == 2 && !message.is_empty()
        ));
        assert!(result.comparison.resolved_commit.is_some());
        assert!(result.head_commit().is_some());
        assert!(result.merge_base().is_none());
    }

    #[test]
    fn fixes_diff_path_prefixes_despite_git_configuration() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        git(&repository.path, &["config", "diff.noPrefix", "true"]);
        write(
            &repository,
            "main.tf",
            "resource \"example\" \"one\" {\n  value = true\n}\n",
        );

        let result = collect_diff(&repository.path);
        let expected_path = repository
            .path
            .canonicalize()
            .expect("repository root should be canonical")
            .join("main.tf");

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(result.changed_lines().len(), 2);
        assert_eq!(result.changed_lines()[0].side(), SourceSide::Before);
        assert_eq!(result.changed_lines()[1].side(), SourceSide::After);
        assert_eq!(result.changed_lines()[0].path(), expected_path);
        assert_eq!(result.changed_lines()[1].path(), expected_path);
    }

    #[test]
    fn ignores_changes_that_are_restored_to_head() {
        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");

        let result = collect_diff(&repository.path);

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert!(result.before().is_empty());
        assert!(result.after().is_empty());
        assert!(result.changed_lines().is_empty());
    }

    #[test]
    fn treats_rename_as_deleted_before_and_added_after() {
        let repository = TestRepository::new();
        write(
            &repository,
            "old name 日本.tf",
            "resource \"example\" \"old\" {}\n",
        );
        repository.commit("initial");
        fs::rename(
            repository.path.join("old name 日本.tf"),
            repository.path.join("new name 日本.tf"),
        )
        .expect("rename source");
        git(&repository.path, &["add", "-A"]);

        let result = collect_diff(&repository.path);
        let repository_root = result.root().to_owned();

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(
            source_names(result.before()),
            vec![repository_root.join("old name 日本.tf").as_path()]
        );
        assert_eq!(
            source_names(result.after()),
            vec![repository_root.join("new name 日本.tf").as_path()]
        );
        assert_eq!(result.changed_lines().len(), 2);
        assert!(result.changed_lines().iter().any(|change| {
            change.path() == repository_root.join("old name 日本.tf")
                && change.side() == SourceSide::Before
        }));
        assert!(result.changed_lines().iter().any(|change| {
            change.path() == repository_root.join("new name 日本.tf")
                && change.side() == SourceSide::After
        }));
    }

    #[test]
    fn includes_non_ignored_untracked_tf_and_excludes_ignored_tf() {
        let repository = TestRepository::new();
        write(&repository, ".gitignore", "ignored.tf\n");
        write(
            &repository,
            "tracked.tf",
            "resource \"example\" \"one\" {}\n",
        );
        repository.commit("initial");
        write(
            &repository,
            "new file.tf",
            "resource \"example\" \"new\" {}\n",
        );
        write(
            &repository,
            "ignored.tf",
            "resource \"example\" \"ignored\" {}\n",
        );

        let result = collect_diff(&repository.path);
        let repository_root = result.root().to_owned();

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(
            source_names(result.after()),
            vec![repository_root.join("new file.tf").as_path()]
        );
        assert_eq!(result.changed_lines().len(), 1);
        assert_eq!(result.changed_lines()[0].range(), SourceRange::new(1, 1));
    }

    #[test]
    fn only_collects_changes_directly_under_the_terraform_root() {
        let repository = TestRepository::new();
        write(
            &repository,
            "infra/prod/main.tf",
            "resource \"example\" \"one\" {}\n",
        );
        write(
            &repository,
            "outside.tf",
            "resource \"example\" \"outside\" {}\n",
        );
        repository.commit("initial");
        write(
            &repository,
            "infra/prod/main.tf",
            "resource \"example\" \"one\" {\n  value = true\n}\n",
        );
        write(
            &repository,
            "outside.tf",
            "resource \"example\" \"outside\" {\n  value = true\n}\n",
        );

        let result = collect_diff(&repository.path.join("infra/prod"));
        let root = result.root().to_owned();

        assert_eq!(result.status(), &GitDiffStatus::Complete);
        assert_eq!(result.before().len(), 1);
        assert_eq!(result.after().len(), 1);
        assert!(
            result
                .changed_lines()
                .iter()
                .all(|change| { change.path() == root.join("main.tf") })
        );
    }

    #[test]
    fn distinguishes_outside_repository_and_missing_head() {
        let outside = std::env::temp_dir().join(format!(
            "terraleph-outside-{}",
            NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&outside).expect("create outside directory");
        let outside_result = collect_diff(&outside);
        fs::remove_dir(&outside).expect("remove outside directory");

        assert!(matches!(
            outside_result.status(),
            GitDiffStatus::OutsideRepository { .. }
        ));

        let repository = TestRepository::new();
        write(&repository, "main.tf", "resource \"example\" \"one\" {}\n");
        let no_head_result = collect_diff(&repository.path);

        assert!(matches!(
            no_head_result.status(),
            GitDiffStatus::HeadUnavailable { .. }
        ));
        assert_eq!(no_head_result.after().len(), 1);
        assert_eq!(no_head_result.changed_lines().len(), 1);
    }

    #[test]
    fn missing_head_uses_only_existing_working_tree_files() {
        let repository = TestRepository::new();
        write(
            &repository,
            "removed.tf",
            "resource \"example\" \"removed\" {}\n",
        );
        write(
            &repository,
            "present.tf",
            "resource \"example\" \"present\" {}\n",
        );
        git(&repository.path, &["add", "removed.tf", "present.tf"]);
        fs::remove_file(repository.path.join("removed.tf")).expect("remove staged source");

        let result = collect_diff(&repository.path);
        let root = result.root().to_owned();

        assert!(matches!(
            result.status(),
            GitDiffStatus::HeadUnavailable { .. }
        ));
        assert_eq!(
            source_names(result.after()),
            vec![root.join("present.tf").as_path()]
        );
        assert_eq!(result.changed_lines().len(), 1);
        assert_eq!(result.changed_lines()[0].path(), root.join("present.tf"));
    }

    #[test]
    fn reports_root_read_failure_separately() {
        let repository = TestRepository::new();
        let file = repository.path.join("not-a-root");
        fs::write(&file, "not a directory").expect("write invalid root");

        let result = collect_diff(&file);

        assert!(matches!(result.status(), GitDiffStatus::Failed { .. }));
        assert!(!matches!(result.status(), GitDiffStatus::Complete));
        assert!(result.status().message().is_some());
    }
}
