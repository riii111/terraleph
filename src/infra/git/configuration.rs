use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::infra::CancellationToken;
use crate::infra::terraform::hcl::HclSourceFile;

use super::{
    command::{GitInterrupted, checked_git, nul_fields},
    diff::{ComparisonBasis, GitDiff},
};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ConfigurationSnapshot {
    files: Vec<ConfigurationFile>,
    issues: Vec<String>,
}

impl std::fmt::Debug for ConfigurationSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfigurationSnapshot")
            .field(
                "files",
                &self.files.iter().map(|file| &file.path).collect::<Vec<_>>(),
            )
            .field("issues", &self.issues)
            .finish()
    }
}

impl ConfigurationSnapshot {
    #[must_use]
    pub(crate) fn changed_paths(&self, other: &Self) -> Vec<PathBuf> {
        let mut paths = self
            .files
            .iter()
            .map(|file| file.path.clone())
            .chain(other.files.iter().map(|file| file.path.clone()))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .filter(|path| self.file_contents(path) != other.file_contents(path))
            .collect()
    }

    #[must_use]
    pub(crate) fn issues(&self) -> &[String] {
        &self.issues
    }

    #[must_use]
    pub(crate) fn differing_source_paths(
        &self,
        before: &[HclSourceFile],
        after: &[HclSourceFile],
        additional_paths: &[PathBuf],
    ) -> Vec<PathBuf> {
        let mut paths = before
            .iter()
            .map(|source| source.path().to_owned())
            .chain(after.iter().map(|source| source.path().to_owned()))
            .chain(additional_paths.iter().cloned())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .filter(|path| {
                if additional_paths.contains(path)
                    && !before
                        .iter()
                        .chain(after)
                        .any(|source| source.path() == path)
                {
                    return true;
                }
                let expected = after
                    .iter()
                    .find(|source| source.path() == path)
                    .map(|source| source.source().as_bytes());
                self.file_contents(path) != expected
            })
            .collect()
    }

    fn file_contents(&self, path: &Path) -> Option<&[u8]> {
        self.files
            .iter()
            .find(|file| file.path == path)
            .map(|file| file.contents.as_slice())
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ConfigurationFile {
    path: PathBuf,
    contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigurationComparison {
    changed_paths: Vec<PathBuf>,
    issues: Vec<String>,
}

impl ConfigurationComparison {
    #[must_use]
    pub(crate) fn changed_paths(&self) -> &[PathBuf] {
        &self.changed_paths
    }

    #[must_use]
    pub(crate) fn issues(&self) -> &[String] {
        &self.issues
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigurationComparisons {
    working_tree: ConfigurationComparison,
    head_vs_merge_base: Option<ConfigurationComparison>,
}

impl ConfigurationComparisons {
    #[must_use]
    pub(crate) const fn working_tree(&self) -> &ConfigurationComparison {
        &self.working_tree
    }

    #[must_use]
    pub(crate) const fn head_vs_merge_base(&self) -> Option<&ConfigurationComparison> {
        self.head_vs_merge_base.as_ref()
    }
}

pub(crate) fn capture_working_tree_configuration_with_cancellation(
    root: &Path,
    cancellation: &CancellationToken,
) -> Result<ConfigurationSnapshot, GitInterrupted> {
    let mut snapshot = ConfigurationSnapshot {
        files: Vec::new(),
        issues: Vec::new(),
    };
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            snapshot.issues.push(format!(
                "failed to read Terraform configuration directory {}: {error}",
                root.display()
            ));
            return Ok(snapshot);
        }
    };

    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_configuration_file(path))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if cancellation.is_cancelled() {
            return Err(GitInterrupted);
        }
        match fs::read(&path) {
            Ok(contents) => snapshot.files.push(ConfigurationFile { path, contents }),
            Err(error) => snapshot.issues.push(format!(
                "failed to read Terraform configuration {}: {error}",
                path.display()
            )),
        }
    }
    Ok(snapshot)
}

pub(crate) fn capture_revision_configuration_with_cancellation(
    repository_root: &Path,
    root: &Path,
    revision: &str,
    cancellation: &CancellationToken,
) -> Result<ConfigurationSnapshot, GitInterrupted> {
    let mut snapshot = ConfigurationSnapshot {
        files: Vec::new(),
        issues: Vec::new(),
    };
    let root_relative = match root.strip_prefix(repository_root) {
        Ok(relative) => relative,
        Err(error) => {
            snapshot.issues.push(format!(
                "failed to resolve Terraform root in Git repository: {error}"
            ));
            return Ok(snapshot);
        }
    };
    let root_spec = if root_relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root_relative.to_owned()
    };
    let root_spec = root_spec.as_os_str();
    let output = match checked_git(
        repository_root,
        "list Git Terraform configuration",
        [
            OsStr::new("ls-tree"),
            OsStr::new("-r"),
            OsStr::new("-z"),
            OsStr::new("--name-only"),
            OsStr::new(revision),
            OsStr::new("--"),
            root_spec,
        ],
        cancellation,
    ) {
        Ok(output) => output,
        Err(error) => {
            snapshot
                .issues
                .push(format!("{}: {}", error.operation, error.message));
            if error.is_interrupted() {
                return Err(GitInterrupted);
            }
            return Ok(snapshot);
        }
    };
    let paths = match nul_fields(&output.stdout, "parse Git Terraform configuration") {
        Ok(paths) => paths,
        Err(error) => {
            snapshot
                .issues
                .push(format!("{}: {}", error.operation, error.message));
            return Ok(snapshot);
        }
    };
    for relative in paths {
        if cancellation.is_cancelled() {
            return Err(GitInterrupted);
        }
        let relative = PathBuf::from(relative);
        if !is_configuration_file(&relative)
            || !is_direct_repository_path(repository_root, root, &relative)
        {
            continue;
        }
        let revision_path = format!("{revision}:{}", relative.to_string_lossy());
        match checked_git(
            repository_root,
            "read Git Terraform configuration",
            [OsStr::new("show"), OsStr::new(revision_path.as_str())],
            cancellation,
        ) {
            Ok(output) => snapshot.files.push(ConfigurationFile {
                path: repository_root.join(&relative),
                contents: output.stdout,
            }),
            Err(error) if error.is_interrupted() => return Err(GitInterrupted),
            Err(error) => snapshot.issues.push(format!(
                "{} {}: {}",
                error.operation,
                relative.display(),
                error.message
            )),
        }
    }
    snapshot
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(snapshot)
}

pub(crate) fn compare_configurations_with_cancellation(
    diff: &GitDiff,
    working_tree: &ConfigurationSnapshot,
    cancellation: &CancellationToken,
) -> Result<ConfigurationComparisons, GitInterrupted> {
    let mut capture_revision = |revision: &str| {
        capture_revision_configuration_with_cancellation(
            diff.repository_root()
                .expect("repository root is checked below"),
            diff.root(),
            revision,
            cancellation,
        )
    };
    compare_configurations_with_capture_cancellable(diff, working_tree, &mut capture_revision)
}

fn compare_configurations_with_capture_cancellable(
    diff: &GitDiff,
    working_tree: &ConfigurationSnapshot,
    capture_revision: &mut dyn FnMut(&str) -> Result<ConfigurationSnapshot, GitInterrupted>,
) -> Result<ConfigurationComparisons, GitInterrupted> {
    let head = match (diff.repository_root(), diff.head_commit()) {
        (Some(_), Some(head_commit)) => Some(capture_revision(head_commit)?),
        _ => None,
    };
    let working_tree = match (diff.repository_root(), diff.head_commit(), head.as_ref()) {
        (None, _, _) => unavailable_comparison("Git repository root is unavailable"),
        (_, None, _) => unavailable_comparison("Git comparison commit is unavailable"),
        (Some(_), Some(_), Some(head)) => comparison_between(head, working_tree),
        (Some(_), Some(_), None) => unreachable!("a resolved HEAD must have a snapshot"),
    };
    let head_vs_merge_base = if diff.basis() == ComparisonBasis::HeadVsMergeBase {
        Some(
            match (
                diff.repository_root(),
                diff.merge_base(),
                diff.head_commit(),
                head.as_ref(),
            ) {
                (None, _, _, _) => unavailable_comparison("Git repository root is unavailable"),
                (_, Some(merge_base), Some(head_commit), Some(head)) => {
                    if merge_base == head_commit {
                        comparison_between(head, head)
                    } else {
                        let merge_base = capture_revision(merge_base)?;
                        comparison_between(&merge_base, head)
                    }
                }
                _ => unavailable_comparison("Git comparison commits are unavailable"),
            },
        )
    } else {
        None
    };

    Ok(ConfigurationComparisons {
        working_tree,
        head_vs_merge_base,
    })
}

fn comparison_between(
    before: &ConfigurationSnapshot,
    after: &ConfigurationSnapshot,
) -> ConfigurationComparison {
    let mut issues = before.issues.clone();
    issues.extend(after.issues.iter().cloned());
    ConfigurationComparison {
        changed_paths: before.changed_paths(after),
        issues,
    }
}

fn unavailable_comparison(message: &str) -> ConfigurationComparison {
    ConfigurationComparison {
        changed_paths: Vec::new(),
        issues: vec![message.to_owned()],
    }
}

fn is_direct_repository_path(repository_root: &Path, root: &Path, path: &Path) -> bool {
    repository_root.join(path).parent() == Some(root)
}

fn is_configuration_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|name| name.to_str());
    path.extension()
        .is_some_and(|extension| extension == OsStr::new("tf"))
        || name.is_some_and(|name| {
            name.ends_with(".tf.json")
                || name.ends_with(".tfvars")
                || name.ends_with(".tfvars.json")
                || name == ".terraform.lock.hcl"
        })
}

#[cfg(test)]
mod tests {
    use std::{
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

    fn collect_diff(root: &Path) -> GitDiff {
        super::super::diff::collect_diff_with_cancellation(root, &CancellationToken::new())
            .expect("Git diff should not be interrupted")
    }

    fn collect_diff_against_ref(root: &Path, compare_ref: &str) -> GitDiff {
        super::super::diff::collect_diff_against_ref_with_cancellation(
            root,
            compare_ref,
            &CancellationToken::new(),
        )
        .expect("Git diff should not be interrupted")
    }

    fn capture_working_tree_configuration(root: &Path) -> ConfigurationSnapshot {
        capture_working_tree_configuration_with_cancellation(root, &CancellationToken::new())
            .expect("working tree configuration should not be interrupted")
    }

    fn capture_revision_configuration(
        repository_root: &Path,
        root: &Path,
        revision: &str,
    ) -> ConfigurationSnapshot {
        capture_revision_configuration_with_cancellation(
            repository_root,
            root,
            revision,
            &CancellationToken::new(),
        )
        .expect("Git configuration should not be interrupted")
    }

    fn compare_configurations(
        diff: &GitDiff,
        working_tree: &ConfigurationSnapshot,
    ) -> ConfigurationComparisons {
        compare_configurations_with_cancellation(diff, working_tree, &CancellationToken::new())
            .expect("Git configuration should not be interrupted")
    }

    fn compare_configurations_with_capture(
        diff: &GitDiff,
        working_tree: &ConfigurationSnapshot,
        capture_revision: &mut dyn FnMut(&str) -> ConfigurationSnapshot,
    ) -> ConfigurationComparisons {
        let mut capture_revision = |revision: &str| Ok(capture_revision(revision));
        compare_configurations_with_capture_cancellable(diff, working_tree, &mut capture_revision)
            .expect("test Git configuration capture should not be interrupted")
    }

    struct TestRepository {
        path: PathBuf,
    }

    impl TestRepository {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "terraleph-configuration-{}-{}",
                std::process::id(),
                NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("test repository should be created");
            git(&path, &["init", "--quiet", "--initial-branch=main"]);
            git(&path, &["config", "user.email", "test@example.com"]);
            git(&path, &["config", "user.name", "Terraleph Test"]);
            Self { path }
        }

        fn write(&self, relative: &str, source: &str) {
            fs::write(self.path.join(relative), source)
                .expect("test configuration should be written");
        }

        fn commit(&self, message: &str) {
            git(&self.path, &["add", "."]);
            git(&self.path, &["commit", "--quiet", "-m", message]);
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).expect("test repository should be removed");
        }
    }

    fn git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn shares_the_head_snapshot_when_merge_base_is_head() {
        let repository = TestRepository::new();
        repository.write("main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        let diff = collect_diff_against_ref(&repository.path, "HEAD");
        let working_tree = capture_working_tree_configuration(&repository.path);
        let mut revisions = Vec::new();

        let comparisons =
            compare_configurations_with_capture(&diff, &working_tree, &mut |revision| {
                revisions.push(revision.to_owned());
                ConfigurationSnapshot {
                    files: Vec::new(),
                    issues: Vec::new(),
                }
            });

        assert_eq!(
            revisions,
            vec![diff.head_commit().expect("HEAD should resolve")]
        );
        assert!(comparisons.head_vs_merge_base().is_some());
    }

    #[test]
    fn captures_head_and_merge_base_once_for_a_commit_comparison() {
        let repository = TestRepository::new();
        repository.write("main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("base");
        git(&repository.path, &["branch", "compare"]);
        repository.write(
            "main.tf",
            "resource \"example\" \"one\" {\n  value = \"head\"\n}\n",
        );
        repository.commit("head");
        let diff = collect_diff_against_ref(&repository.path, "compare");
        let repository_root = diff
            .repository_root()
            .expect("repository root should resolve")
            .to_owned();
        let root = diff.root().to_owned();
        let working_tree = capture_working_tree_configuration(&root);
        let mut revisions = Vec::new();

        let comparisons =
            compare_configurations_with_capture(&diff, &working_tree, &mut |revision| {
                revisions.push(revision.to_owned());
                capture_revision_configuration(&repository_root, &root, revision)
            });

        assert_eq!(revisions.len(), 2);
        assert_eq!(
            revisions[0],
            diff.head_commit().expect("HEAD should resolve")
        );
        assert_eq!(
            revisions[1],
            diff.merge_base().expect("merge-base should resolve")
        );
        assert!(
            comparisons.working_tree().changed_paths().is_empty(),
            "working tree comparison changed paths: {:?}",
            comparisons.working_tree().changed_paths()
        );
        assert_eq!(
            comparisons
                .head_vs_merge_base()
                .expect("compare-ref basis should retain a comparison")
                .changed_paths(),
            &[root.join("main.tf")]
        );
    }

    #[test]
    fn keeps_unavailable_commit_comparison_reason() {
        let repository = TestRepository::new();
        repository.write("main.tf", "resource \"example\" \"one\" {}\n");
        repository.commit("initial");
        let diff = collect_diff_against_ref(&repository.path, "missing");
        let working_tree = capture_working_tree_configuration(&repository.path);

        let comparisons = compare_configurations(&diff, &working_tree);

        assert!(comparisons.working_tree().issues().is_empty());
        assert_eq!(
            comparisons
                .head_vs_merge_base()
                .expect("compare-ref basis should retain a comparison")
                .issues(),
            &["Git comparison commits are unavailable"]
        );
    }

    #[test]
    fn keeps_unavailable_head_reason_without_a_commit_comparison() {
        let repository = TestRepository::new();
        repository.write("main.tf", "resource \"example\" \"one\" {}\n");
        let diff = collect_diff(&repository.path);
        let working_tree = capture_working_tree_configuration(&repository.path);

        let comparisons = compare_configurations(&diff, &working_tree);

        assert_eq!(
            comparisons.working_tree().issues(),
            &["Git comparison commit is unavailable"]
        );
        assert!(comparisons.head_vs_merge_base().is_none());
    }
}
