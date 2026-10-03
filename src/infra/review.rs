use std::{
    ffi::OsStr,
    fmt::{Display, Formatter},
    path::{Path, PathBuf},
};

use crate::app::{
    attribution::{AnalysisIssue, SourceFileAnalysis, attribute_changes, mark_analysis_incomplete},
    execution::{ExecutionEvent, Tool},
    review::git::{PlanReview, ReviewComparison, ReviewComparisonBasis, ReviewComparisonStatus},
};
use crate::infra::CancellationToken;

use super::{
    git::{self, ComparisonBasis, ConfigurationComparison, ConfigurationSnapshot, GitDiff},
    terraform::{self, hcl},
};

#[derive(Debug)]
pub(crate) enum ReviewError {
    Interrupted,
    Terraform(terraform::test_support::PlanTestError),
}

impl Display for ReviewError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Interrupted => formatter.write_str("review was interrupted"),
            Self::Terraform(error) => Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for ReviewError {}

impl From<terraform::test_support::PlanTestError> for ReviewError {
    fn from(error: terraform::test_support::PlanTestError) -> Self {
        Self::Terraform(error)
    }
}

impl From<terraform::test_support::CommandTerraformExecutionError> for ReviewError {
    fn from(error: terraform::test_support::CommandTerraformExecutionError) -> Self {
        Self::Terraform(terraform::test_support::PlanTestError::Terraform(error))
    }
}

impl From<git::GitInterrupted> for ReviewError {
    fn from(_: git::GitInterrupted) -> Self {
        Self::Interrupted
    }
}

#[derive(Debug)]
#[expect(
    dead_code,
    reason = "dormant review events retain intermediate results for attribution"
)]
enum ReviewEvent {
    Terraform(ExecutionEvent),
    RepositoryRoot(Option<PathBuf>),
    Git(Option<String>),
    Workspace(String),
}

fn run_review(
    root: &Path,
    compare_ref: Option<&str>,
    cancellation: &CancellationToken,
    event_sink: &mut dyn FnMut(ReviewEvent),
) -> Result<PlanReview, ReviewError> {
    run_review_with_dependencies(
        root,
        compare_ref,
        cancellation,
        &terraform::SystemProcessRunner,
        None,
        event_sink,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "review stages keep their cancellation boundaries in execution order"
)]
fn run_review_with_dependencies(
    root: &Path,
    compare_ref: Option<&str>,
    cancellation: &CancellationToken,
    runner: &dyn terraform::test_support::ProcessRunner,
    after_git_diff: Option<&mut dyn FnMut()>,
    event_sink: &mut dyn FnMut(ReviewEvent),
) -> Result<PlanReview, ReviewError> {
    let git_diff = collect_git_diff(root, compare_ref, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    if let Some(after_git_diff) = after_git_diff {
        after_git_diff();
    }
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let execution_root = git_diff.root().to_owned();
    let repository_root = git_diff.repository_root().map(Path::to_owned);
    event_sink(ReviewEvent::RepositoryRoot(repository_root.clone()));
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let configuration_before =
        git::capture_working_tree_configuration_with_cancellation(&execution_root, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let git_branch = git::current_branch(&execution_root, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    event_sink(ReviewEvent::Git(git_branch.clone()));
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }

    let workspace = terraform::read_workspace_with_arguments(
        Tool::Terraform,
        &execution_root,
        &[],
        cancellation,
        runner,
    )?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    event_sink(ReviewEvent::Workspace(workspace.clone()));
    let mut terraform_event_sink = |event| event_sink(ReviewEvent::Terraform(event));
    let plan = terraform::test_support::run_plan(
        &execution_root,
        cancellation,
        runner,
        &mut terraform_event_sink,
    )?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let configuration_after =
        git::capture_working_tree_configuration_with_cancellation(&execution_root, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }

    let source_files = parse_git_sources(&git_diff, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let configuration_comparisons: git::ConfigurationComparisons =
        git::compare_configurations_with_cancellation(
            &git_diff,
            &configuration_before,
            cancellation,
        )?;
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let mut analysis_issues = git_analysis_issues(&git_diff);
    let current_native_paths = configuration_comparisons
        .working_tree()
        .changed_paths()
        .iter()
        .filter(|path| is_supported_native_configuration_path(&execution_root, path))
        .cloned()
        .collect::<Vec<_>>();
    for path in configuration_before.differing_source_paths(
        git_diff.before(),
        git_diff.after(),
        &current_native_paths,
    ) {
        push_unique(
            &mut analysis_issues,
            AnalysisIssue::configuration_changed(&path),
        );
    }
    add_configuration_issues(
        &mut analysis_issues,
        &configuration_before,
        &configuration_after,
        configuration_comparisons.working_tree(),
        configuration_comparisons.head_vs_merge_base(),
    );
    let unsupported_comparison = configuration_comparisons
        .head_vs_merge_base()
        .unwrap_or_else(|| configuration_comparisons.working_tree());
    add_unsupported_comparison_changes(&mut analysis_issues, &git_diff, unsupported_comparison);

    let mut attributions = attribute_changes(
        plan.resource_changes
            .iter()
            .filter(|change| change.kind.is_standard_change()),
        &source_files,
        git_diff.changed_lines(),
    );
    mark_analysis_incomplete(&mut attributions, &analysis_issues);
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }

    Ok(PlanReview::new(
        execution_root,
        workspace,
        plan,
        source_files,
        attributions,
        review_comparison(&git_diff),
        analysis_issues,
    )
    .with_repository_root(repository_root)
    .with_git(git_branch.unwrap_or_else(|| "unavailable".to_owned())))
}

fn collect_git_diff(
    root: &Path,
    compare_ref: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<GitDiff, git::GitInterrupted> {
    compare_ref.map_or_else(
        || git::collect_diff_with_cancellation(root, cancellation),
        |compare_ref| {
            git::collect_diff_against_ref_with_cancellation(root, compare_ref, cancellation)
        },
    )
}

fn parse_git_sources(
    diff: &GitDiff,
    cancellation: &CancellationToken,
) -> Result<Vec<SourceFileAnalysis>, ReviewError> {
    let inputs = diff
        .before()
        .iter()
        .chain(diff.after())
        .cloned()
        .collect::<Vec<_>>();
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    let parsed = hcl::parse_files(inputs);
    if cancellation.is_cancelled() {
        return Err(ReviewError::Interrupted);
    }
    Ok(parsed.into_files())
}

fn git_analysis_issues(diff: &GitDiff) -> Vec<AnalysisIssue> {
    diff.status()
        .message()
        .map_or_else(Vec::new, |message| vec![AnalysisIssue::git(message)])
}

fn add_configuration_issues(
    issues: &mut Vec<AnalysisIssue>,
    before: &ConfigurationSnapshot,
    after: &ConfigurationSnapshot,
    working_tree_comparison: &ConfigurationComparison,
    head_vs_merge_base: Option<&ConfigurationComparison>,
) {
    for path in before.changed_paths(after) {
        push_unique(issues, AnalysisIssue::configuration_changed(&path));
    }
    for message in before.issues().iter().chain(after.issues()) {
        push_unique(
            issues,
            AnalysisIssue::configuration_unavailable(None, message.clone()),
        );
    }
    for message in working_tree_comparison.issues() {
        push_unique(
            issues,
            AnalysisIssue::configuration_unavailable(None, message.clone()),
        );
    }
    if let Some(comparison) = head_vs_merge_base {
        for message in comparison.issues() {
            push_unique(
                issues,
                AnalysisIssue::configuration_unavailable(None, message.clone()),
            );
        }
        for path in working_tree_comparison.changed_paths() {
            push_unique(issues, AnalysisIssue::configuration_differs_from_head(path));
        }
    }
}

fn add_unsupported_comparison_changes(
    issues: &mut Vec<AnalysisIssue>,
    diff: &GitDiff,
    comparison: &ConfigurationComparison,
) {
    for path in comparison.changed_paths() {
        if !is_supported_native_configuration_path(diff.root(), path) {
            push_unique(issues, AnalysisIssue::unsupported_configuration(path));
        }
    }
}

fn is_supported_native_configuration_path(root: &Path, path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == OsStr::new("tf"))
        && path.parent() == Some(root)
}

fn push_unique(issues: &mut Vec<AnalysisIssue>, issue: AnalysisIssue) {
    if !issues.contains(&issue) {
        issues.push(issue);
    }
}

fn review_comparison(diff: &GitDiff) -> ReviewComparison {
    let basis = match diff.basis() {
        ComparisonBasis::WorkingTreeVsHead => ReviewComparisonBasis::WorkingTreeVsHead,
        ComparisonBasis::HeadVsMergeBase => ReviewComparisonBasis::HeadVsMergeBase,
    };
    let status = diff
        .status()
        .message()
        .map_or(ReviewComparisonStatus::Complete, |message| {
            ReviewComparisonStatus::Incomplete(message.to_owned())
        });
    ReviewComparison::new(basis, diff.compare_ref().map(str::to_owned), status)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::VecDeque,
        fs, io,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    use crate::{
        app::attribution::{AnalysisIssueKind, AttributionStatus},
        infra::terraform::test_support::{
            ProcessOutput, ProcessRunner, ProcessStatus, RunningProcess,
        },
    };
    use serde_json::json;

    use super::*;

    static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

    struct TestRepository {
        path: PathBuf,
    }

    impl TestRepository {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "terraleph-review-{}-{}",
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
            fs::write(self.path.join(relative), source).expect("test source should be written");
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

    fn git(repository: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct FakeRunner {
        outputs: RefCell<VecDeque<ProcessOutput>>,
        mutate_on_plan: Option<(PathBuf, String)>,
    }

    impl FakeRunner {
        fn new(plan: ProcessOutput, mutate_on_plan: Option<(PathBuf, String)>) -> Self {
            Self {
                outputs: RefCell::new(VecDeque::from([
                    ProcessOutput::new(b"default\n".to_vec(), Vec::new()),
                    plan.clone(),
                    plan,
                ])),
                mutate_on_plan,
            }
        }
    }

    struct FakeProcess {
        output: Option<ProcessOutput>,
    }

    impl RunningProcess for FakeProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>> {
            Ok(Some(ProcessStatus::Exited(0)))
        }

        fn request_interrupt(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessStatus> {
            Ok(ProcessStatus::Exited(0))
        }

        fn collect_output(mut self: Box<Self>) -> io::Result<ProcessOutput> {
            self.output
                .take()
                .ok_or_else(|| io::Error::other("fake process output was already collected"))
        }
    }

    impl ProcessRunner for FakeRunner {
        fn start(
            &self,
            _tool: Tool,
            _root: &Path,
            arguments: &[std::ffi::OsString],
        ) -> io::Result<Box<dyn RunningProcess>> {
            if arguments.first().is_some_and(|argument| argument == "plan")
                && let Some((path, source)) = &self.mutate_on_plan
            {
                fs::write(path, source).expect("fake Terraform should mutate configuration");
            }
            let output = self
                .outputs
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| io::Error::other("fake process response was exhausted"))?;
            Ok(Box::new(FakeProcess {
                output: Some(output),
            }))
        }
    }

    fn plan_output(address: Option<&str>) -> ProcessOutput {
        let resource_changes = address.map_or_else(Vec::new, |address| {
            vec![json!({
                "address": address,
                "mode": "managed",
                "change": {"actions": ["update"]},
            })]
        });
        ProcessOutput::new(
            json!({
                "format_version": "1.0",
                "resource_changes": resource_changes,
            })
            .to_string()
            .into_bytes(),
            Vec::new(),
        )
    }

    fn run_fake_review(
        root: &Path,
        compare_ref: Option<&str>,
        plan: ProcessOutput,
        mutate_on_plan: Option<(PathBuf, String)>,
    ) -> PlanReview {
        let runner = FakeRunner::new(plan, mutate_on_plan);
        run_review_with_dependencies(
            root,
            compare_ref,
            &CancellationToken::default(),
            &runner,
            None,
            &mut |_| {},
        )
        .expect("fake Terraform review should succeed")
    }

    fn run_fake_review_after_git_diff(
        root: &Path,
        plan: ProcessOutput,
        mutate_after_git_diff: impl FnOnce(),
    ) -> PlanReview {
        let runner = FakeRunner::new(plan, None);
        let mut mutate_after_git_diff = Some(mutate_after_git_diff);
        let mut after_git_diff = || {
            mutate_after_git_diff
                .take()
                .expect("Git diff hook should run once")();
        };
        run_review_with_dependencies(
            root,
            None,
            &CancellationToken::default(),
            &runner,
            Some(&mut after_git_diff),
            &mut |_| {},
        )
        .expect("fake Terraform review should succeed")
    }

    #[test]
    fn reports_interruption_before_starting_terraform_after_git_diff() {
        let repository = TestRepository::new();
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"before\"\n}\n",
        );
        repository.commit("initial");
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"after\"\n}\n",
        );
        let cancellation = CancellationToken::default();
        let mut cancel_after_git_diff = || cancellation.cancel();
        let runner = FakeRunner::new(plan_output(None), None);

        let result = run_review_with_dependencies(
            &repository.path,
            None,
            &cancellation,
            &runner,
            Some(&mut cancel_after_git_diff),
            &mut |_| {},
        );

        assert!(matches!(result, Err(ReviewError::Interrupted)));
    }

    #[test]
    fn forwards_repository_root_before_branch_and_workspace() {
        let repository = TestRepository::new();
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"before\"\n}\n",
        );
        repository.commit("initial");
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"after\"\n}\n",
        );
        let expected_repository_root =
            fs::canonicalize(&repository.path).expect("repository root should be canonicalized");

        let mut events = Vec::new();
        let review = run_review_with_dependencies(
            &repository.path,
            None,
            &CancellationToken::default(),
            &FakeRunner::new(plan_output(None), None),
            None,
            &mut |event| events.push(event),
        )
        .expect("fake Terraform review should succeed");

        assert_eq!(
            review.repository_root(),
            Some(expected_repository_root.as_path())
        );
        assert!(matches!(
            events.first(),
            Some(ReviewEvent::RepositoryRoot(Some(root)))
                if root == &expected_repository_root
        ));
        assert!(matches!(events.get(1), Some(ReviewEvent::Git(_))));
        assert!(matches!(events.get(2), Some(ReviewEvent::Workspace(_))));
    }

    #[test]
    fn forwards_unavailable_repository_root_after_git_discovery_failure() {
        let root = std::env::temp_dir().join(format!(
            "terraleph-review-event-outside-{}",
            NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("outside root should be created");
        let mut events = Vec::new();

        let review = run_review_with_dependencies(
            &root,
            None,
            &CancellationToken::default(),
            &FakeRunner::new(plan_output(None), None),
            None,
            &mut |event| events.push(event),
        )
        .expect("review should preserve plan data outside Git");

        assert_eq!(review.repository_root(), None);
        assert!(matches!(
            events.first(),
            Some(ReviewEvent::RepositoryRoot(None))
        ));
        fs::remove_dir(&root).expect("outside root should be removed");
    }

    #[test]
    #[ignore = "requires Terraform CLI"]
    fn reviews_the_basic_scenario_with_four_direct_matches_and_one_no_match() {
        let setup = Command::new("python3")
            .args([
                concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/basic/plan.py"),
                "test",
                "setup",
            ])
            .output()
            .expect("scenario setup should start");
        assert!(
            setup.status.success(),
            "scenario setup failed: {}",
            String::from_utf8_lossy(&setup.stderr)
        );
        let directory = PathBuf::from(
            String::from_utf8(setup.stdout)
                .expect("scenario path should be UTF-8")
                .trim(),
        );

        let mut ignore_event = |_| {};
        let result = run_review(
            &directory,
            None,
            &CancellationToken::default(),
            &mut ignore_event,
        );
        let cleanup = Command::new("python3")
            .args([
                concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/basic/plan.py"),
                "test",
                "clean",
            ])
            .arg(&directory)
            .output()
            .expect("scenario cleanup should start");
        assert!(
            cleanup.status.success(),
            "scenario cleanup failed: {}",
            String::from_utf8_lossy(&cleanup.stderr)
        );

        let review = result.expect("review should be returned");
        assert_eq!(review.root(), directory);
        assert_eq!(review.workspace(), "default");
        assert_eq!(
            review.comparison().basis(),
            ReviewComparisonBasis::WorkingTreeVsHead
        );
        assert!(review.comparison().status().is_complete());
        assert_eq!(review.attributions().len(), 5);
        assert_eq!(
            review
                .attributions()
                .iter()
                .filter(|attribution| attribution.status() == AttributionStatus::Direct)
                .count(),
            4
        );
        assert_eq!(review.needs_review_count(), 1);
    }

    #[test]
    fn keeps_plan_data_when_git_is_outside_a_repository() {
        let root = std::env::temp_dir().join(format!(
            "terraleph-review-outside-{}",
            NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("outside root should be created");

        let review = run_fake_review(&root, None, plan_output(None), None);

        assert_eq!(review.plan().summary().total(), 0);
        assert!(!review.comparison().status().is_complete());
        assert!(
            review
                .analysis_issues()
                .iter()
                .any(|issue| { issue.kind() == AnalysisIssueKind::Git })
        );
        fs::remove_dir(&root).expect("outside root should be removed");
    }

    #[test]
    fn marks_module_resource_addresses_as_incomplete_without_root_matching() {
        let repository = TestRepository::new();
        repository.write("main.tf", "module \"child\" {\n  source = \"./child\"\n}\n");
        repository.commit("initial");
        repository.write(
            "main.tf",
            "module \"child\" {\n  source = \"./changed-child\"\n}\n",
        );

        let review = run_fake_review(
            &repository.path,
            None,
            plan_output(Some("module.child.terraform_data.value")),
            None,
        );

        assert_eq!(review.attributions().len(), 1);
        assert_eq!(review.needs_review_count(), 1);
        assert!(
            review.attributions()[0]
                .analysis()
                .issues()
                .iter()
                .any(|issue| { issue.kind() == AnalysisIssueKind::UnsupportedAddress })
        );
    }

    #[test]
    fn marks_configuration_changed_during_plan_as_incomplete() {
        let repository = TestRepository::new();
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"before\"\n}\n",
        );
        repository.commit("initial");

        let review = run_fake_review(
            &repository.path,
            None,
            plan_output(Some("terraform_data.value")),
            Some((
                repository.path.join("main.tf"),
                "resource \"terraform_data\" \"value\" {\n  input = \"during\"\n}\n".to_owned(),
            )),
        );

        assert!(review.analysis_issues().iter().any(|issue| {
            issue.kind() == AnalysisIssueKind::ConfigurationChanged
                && issue.path().is_some_and(|path| path.ends_with("main.tf"))
        }));
        assert!(review.attributions()[0].needs_review());
    }

    #[test]
    fn marks_native_configurations_changed_or_deleted_after_git_collection_as_incomplete() {
        let repository = TestRepository::new();
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"head\"\n}\n",
        );
        repository.write(
            "second.tf",
            "resource \"terraform_data\" \"second\" {\n  input = \"head\"\n}\n",
        );
        repository.write(
            "third.tf",
            "resource \"terraform_data\" \"third\" {\n  input = \"head\"\n}\n",
        );
        repository.commit("initial");
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"intended\"\n}\n",
        );

        let review = run_fake_review_after_git_diff(
            &repository.path,
            plan_output(Some("terraform_data.value")),
            || {
                repository.write(
                    "second.tf",
                    "resource \"terraform_data\" \"second\" {\n  input = \"unintended\"\n}\n",
                );
                fs::remove_file(repository.path.join("third.tf"))
                    .expect("late configuration deletion should succeed");
            },
        );

        for path in ["second.tf", "third.tf"] {
            assert!(
                review.analysis_issues().iter().any(|issue| {
                    issue.kind() == AnalysisIssueKind::ConfigurationChanged
                        && issue
                            .path()
                            .is_some_and(|issue_path| issue_path.ends_with(path))
                }),
                "late configuration change should be reported for {path}"
            );
        }
        assert!(review.attributions()[0].needs_review());
    }

    #[test]
    fn checks_json_variables_and_lockfile_changes_for_incomplete_analysis() {
        let repository = TestRepository::new();
        repository.write("main.tf", "resource \"terraform_data\" \"value\" {}\n");
        repository.commit("initial");
        for path in [
            "config.tf.json",
            "values.tfvars",
            "values.tfvars.json",
            ".terraform.lock.hcl",
        ] {
            repository.write(path, "changed\n");
        }

        let review = run_fake_review(&repository.path, None, plan_output(None), None);

        for path in [
            "config.tf.json",
            "values.tfvars",
            "values.tfvars.json",
            ".terraform.lock.hcl",
        ] {
            assert!(
                review.analysis_issues().iter().any(|issue| {
                    issue.kind() == AnalysisIssueKind::ConfigurationChanged
                        && issue
                            .path()
                            .is_some_and(|issue_path| issue_path.ends_with(path))
                }),
                "configuration change should be reported for {path}"
            );
        }
    }

    #[test]
    fn marks_dirty_execution_settings_in_head_comparison_as_incomplete() {
        let repository = TestRepository::new();
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"head\"\n}\n",
        );
        repository.commit("initial");
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"dirty\"\n}\n",
        );

        let review = run_fake_review(
            &repository.path,
            Some("main"),
            plan_output(Some("terraform_data.value")),
            None,
        );

        assert_eq!(
            review.comparison().basis(),
            ReviewComparisonBasis::HeadVsMergeBase
        );
        assert!(review.analysis_issues().iter().any(|issue| {
            issue.kind() == AnalysisIssueKind::ConfigurationChanged
                && issue.path().is_some_and(|path| path.ends_with("main.tf"))
        }));
        assert!(review.attributions()[0].needs_review());
    }

    #[test]
    fn compares_clean_head_and_dirty_non_native_settings_against_the_correct_snapshots() {
        let repository = TestRepository::new();
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"base\"\n}\n",
        );
        repository.commit("base");
        git(&repository.path, &["branch", "compare"]);
        repository.write(
            "main.tf",
            "resource \"terraform_data\" \"value\" {\n  input = \"head\"\n}\n",
        );
        repository.commit("head");
        repository.write("values.tfvars", "value = \"dirty\"\n");

        let review = run_fake_review(
            &repository.path,
            Some("compare"),
            plan_output(Some("terraform_data.value")),
            None,
        );

        assert!(!review.analysis_issues().iter().any(|issue| {
            issue.message().contains("differs from HEAD")
                && issue.path().is_some_and(|path| path.ends_with("main.tf"))
        }));
        assert!(review.analysis_issues().iter().any(|issue| {
            issue.kind() == AnalysisIssueKind::ConfigurationChanged
                && issue
                    .path()
                    .is_some_and(|path| path.ends_with("values.tfvars"))
                && issue.message().contains("differs from HEAD")
        }));
    }
}
