use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
};

use crate::app::{
    copy::{self, CopyEffect},
    execution::{
        Diagnostic, ExecutionContext, ExecutionEvent, ExecutionEventKind, InitializationReason,
        LockFileChange, PreparationEvent, PreparationStage, Tool, directory_display_name,
    },
    review::PlanReview,
    session::{self, Action, Effect, ReviewSessionState, SessionState},
};

pub(crate) mod comparison;
pub(crate) mod overview;

const DEVELOPMENT_ENVIRONMENT_NAMES: &[&str] =
    &["dev", "develop", "development", "local", "sandbox"];
const TEST_ENVIRONMENT_NAMES: &[&str] = &["test", "qa", "int", "integration"];
const STAGING_ENVIRONMENT_NAMES: &[&str] = &["stg", "stage", "staging", "preprod", "uat"];
const PRODUCTION_ENVIRONMENT_NAMES: &[&str] = &["prod", "production", "prd"];

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Environment {
    pub(crate) tool: Tool,
    pub(crate) availability: EnvironmentAvailability,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentAvailability {
    Available { directory: PathBuf },
    Error { directory: PathBuf, message: String },
}

#[derive(Debug)]
pub(crate) enum EnvironmentState {
    Unselected,
    Pending,
    Running,
    Ready {
        session: Box<SessionState>,
        changed: bool,
    },
    Error,
    ExcludedHcp,
}

#[derive(Debug)]
pub(crate) struct EnvironmentPlan {
    pub(crate) tool: Tool,
    directory: PathBuf,
    relative_path: String,
    // Known only from an acquired plan, because reading it before then would run the tool for a
    // candidate the user has not chosen.
    workspace: Option<String>,
    // Set once the user chooses the environment, so a candidate that was never chosen does not
    // decide the exit code even when discovery already failed it.
    target: bool,
    state: EnvironmentState,
    preparation: Preparation,
    diagnostics: Vec<Diagnostic>,
    failure: Option<String>,
}

// The latest step of the current acquisition. It stays after completion, so a failure names the
// step that failed and a ready plan still reports a lock file change made by its init.
#[derive(Debug, Default)]
pub(crate) struct Preparation {
    stage: Option<PreparationStage>,
    initialization: Option<InitializationReason>,
    lock_file: Option<LockFileChange>,
    latest_output: Option<String>,
}

pub(crate) enum PlanResult {
    Ready {
        review: Box<PlanReview>,
        changed: bool,
    },
    Error(String),
    ExcludedHcp(String),
}

pub(crate) struct EnvironmentSession {
    plans: Vec<EnvironmentPlan>,
    exploration_root: Option<PathBuf>,
    walk_limit: Option<usize>,
    detailed_exitcode: bool,
    interrupted: bool,
    revision: u64,
}

impl Environment {
    pub(crate) const fn is_available(&self) -> bool {
        matches!(self.availability, EnvironmentAvailability::Available { .. })
    }

    pub(crate) fn directory(&self) -> &Path {
        match &self.availability {
            EnvironmentAvailability::Available { directory }
            | EnvironmentAvailability::Error { directory, .. } => directory,
        }
    }
}

impl EnvironmentSession {
    pub(crate) fn new(environments: Vec<Environment>, detailed_exitcode: bool) -> Self {
        let mut session = Self {
            revision: 0,
            plans: environments.into_iter().map(EnvironmentPlan::new).collect(),
            exploration_root: None,
            walk_limit: None,
            detailed_exitcode,
            interrupted: false,
        };
        session.sort_plans();
        session
    }

    pub(crate) fn with_exploration_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        for plan in &mut self.plans {
            plan.relative_path = relative_display_path(&plan.directory, &root);
        }
        self.exploration_root = Some(root);
        self.sort_plans();
        self
    }

    pub(crate) const fn with_walk_limit(mut self, walk_limit: Option<usize>) -> Self {
        self.walk_limit = walk_limit;
        self
    }

    // The order is fixed before any plan runs, so it uses only the path and never a workspace
    // that a later plan reveals.
    fn sort_plans(&mut self) {
        self.plans.sort_by(|a, b| {
            let left_name = &a.relative_path;
            let right_name = &b.relative_path;
            environment_stage(final_component(left_name))
                .cmp(&environment_stage(final_component(right_name)))
                .then_with(|| natural_cmp(left_name, right_name))
                .then_with(|| left_name.cmp(right_name))
                .then_with(|| a.directory.cmp(&b.directory))
        });
    }

    pub(crate) fn plans(&self) -> &[EnvironmentPlan] {
        &self.plans
    }

    pub(crate) fn exploration_root(&self) -> Option<&Path> {
        self.exploration_root.as_deref()
    }

    pub(crate) const fn walk_limit(&self) -> Option<usize> {
        self.walk_limit
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    // Choosing a candidate only queues it; `start_next` still runs one acquisition at a time.
    pub(crate) fn request_plan(&mut self, index: usize) -> bool {
        let Some(plan) = self.plans.get_mut(index) else {
            return false;
        };
        if self.interrupted || !matches!(plan.state, EnvironmentState::Unselected) {
            return false;
        }
        plan.target = true;
        plan.state = EnvironmentState::Pending;
        self.revision += 1;
        true
    }

    pub(crate) fn request_all_plans(&mut self) -> bool {
        let mut requested = false;
        for index in 0..self.plans.len() {
            requested |= self.request_plan(index);
        }
        requested
    }

    pub(crate) fn start_next(&mut self) -> Option<usize> {
        if self.interrupted
            || self
                .plans
                .iter()
                .any(|plan| matches!(plan.state, EnvironmentState::Running))
        {
            return None;
        }
        let (index, plan) = self
            .plans
            .iter_mut()
            .enumerate()
            .find(|(_, plan)| matches!(plan.state, EnvironmentState::Pending))?;
        plan.state = EnvironmentState::Running;
        Some(index)
    }

    pub(crate) fn retry(&mut self, index: usize) -> bool {
        let Some(plan) = self.plans.get_mut(index) else {
            return false;
        };
        if self.interrupted || !matches!(plan.state, EnvironmentState::Error) {
            return false;
        }
        plan.target = true;
        plan.state = EnvironmentState::Pending;
        plan.preparation = Preparation::default();
        plan.diagnostics.clear();
        plan.failure = None;
        self.revision += 1;
        true
    }

    pub(crate) fn record_preparation(&mut self, index: usize, event: PreparationEvent) -> bool {
        let Some(plan) = self.plans.get_mut(index) else {
            return false;
        };
        if self.interrupted || !matches!(plan.state, EnvironmentState::Running) {
            return false;
        }
        plan.preparation.record(event)
    }

    pub(crate) fn complete(
        &mut self,
        index: usize,
        result: PlanResult,
        diagnostics: Vec<Diagnostic>,
    ) -> bool {
        let Some(plan) = self.plans.get_mut(index) else {
            return false;
        };
        if self.interrupted || !matches!(plan.state, EnvironmentState::Running) {
            return false;
        }
        plan.diagnostics = diagnostics;
        plan.state = match result {
            PlanResult::Ready { review, changed } => {
                plan.workspace = Some(review.workspace().to_owned());
                EnvironmentState::Ready {
                    session: Box::new(SessionState::Review(Box::new(ReviewSessionState::new(
                        review.with_diagnostics(std::mem::take(&mut plan.diagnostics)),
                    )))),
                    changed,
                }
            }
            PlanResult::Error(message) => {
                plan.failure = Some(message);
                EnvironmentState::Error
            }
            PlanResult::ExcludedHcp(message) => {
                plan.failure = Some(message);
                plan.target = false;
                EnvironmentState::ExcludedHcp
            }
        };
        self.revision += 1;
        true
    }

    pub(crate) fn update_review(
        &mut self,
        index: usize,
        action: Action,
        now: std::time::Instant,
    ) -> Option<Effect> {
        if !matches!(
            action,
            Action::ReviewSearchChanged(_)
                | Action::Copy(_)
                | Action::CopyCompleted { .. }
                | Action::OpenApplyConfirmation
        ) {
            return None;
        }
        if matches!(action, Action::OpenApplyConfirmation) && !self.can_start_apply() {
            return None;
        }
        let EnvironmentState::Ready { session, .. } = &mut self.plans.get_mut(index)?.state else {
            return None;
        };
        session::update(session, action, now)
    }

    // One environment applies at a time, and only after every plan has been acquired, so an
    // apply never runs beside another Terraform process started by this session.
    pub(crate) fn can_start_apply(&self) -> bool {
        !self.interrupted
            && !self.acquiring()
            && !self.plans.iter().any(|plan| {
                plan.session()
                    .is_some_and(|session| session.review().is_none())
            })
    }

    pub(crate) fn session_mut(&mut self, index: usize) -> Option<&mut SessionState> {
        match &mut self.plans.get_mut(index)?.state {
            EnvironmentState::Ready { session, .. } => Some(session),
            _ => None,
        }
    }

    pub(crate) fn acquiring(&self) -> bool {
        self.plans.iter().any(|plan| {
            matches!(
                plan.state,
                EnvironmentState::Pending | EnvironmentState::Running
            )
        })
    }

    pub(crate) fn clear_expired_copy_feedback(&mut self, now: std::time::Instant) -> bool {
        let mut cleared = false;
        for plan in &mut self.plans {
            if let EnvironmentState::Ready { session, .. } = &mut plan.state
                && let Some(feedback) = session.copy_feedback_mut()
            {
                cleared |= feedback.clear_expired(now);
            }
        }
        cleared
    }

    pub(crate) const fn interrupt(&mut self) {
        self.interrupted = true;
    }

    pub(crate) fn exit_code(&self) -> u8 {
        if self.interrupted {
            return 130;
        }
        let targets = self
            .plans
            .iter()
            .filter(|plan| plan.target)
            .collect::<Vec<_>>();
        if targets.is_empty()
            || targets
                .iter()
                .any(|plan| !matches!(plan.state, EnvironmentState::Ready { .. }))
        {
            return 1;
        }
        if self.detailed_exitcode
            && targets
                .iter()
                .any(|plan| matches!(plan.state, EnvironmentState::Ready { changed: true, .. }))
        {
            2
        } else {
            0
        }
    }
}

impl EnvironmentPlan {
    fn new(environment: Environment) -> Self {
        let (directory, state, failure) = match environment.availability {
            EnvironmentAvailability::Available { directory } => {
                (directory, EnvironmentState::Unselected, None)
            }
            EnvironmentAvailability::Error { directory, message } => {
                (directory, EnvironmentState::Error, Some(message))
            }
        };
        Self {
            tool: environment.tool,
            relative_path: directory_display_name(&directory),
            directory,
            workspace: None,
            target: false,
            state,
            preparation: Preparation::default(),
            diagnostics: Vec::new(),
            failure,
        }
    }

    pub(crate) const fn state(&self) -> &EnvironmentState {
        &self.state
    }

    pub(crate) const fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    // The path from the exploration root tells candidates apart before any workspace is known;
    // a workspace joins it only once a plan has determined it.
    pub(crate) fn display_name(&self) -> String {
        match self.workspace() {
            Some(workspace) if workspace != "default" => {
                format!("{}:{workspace}", self.relative_path)
            }
            _ => self.relative_path.clone(),
        }
    }

    pub(crate) const fn is_target(&self) -> bool {
        self.target
    }

    pub(crate) fn is_production(&self) -> bool {
        self.review()
            .and_then(|review| review.review().context().is_production())
            .unwrap_or_else(|| {
                // An undetermined workspace stands in as `default`, which is never a production
                // token, so only the path decides.
                ExecutionContext::loading(&self.directory)
                    .with_workspace(self.workspace().unwrap_or("default"))
                    .is_production()
                    == Some(true)
            })
    }

    pub(crate) fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    pub(crate) fn review(&self) -> Option<&ReviewSessionState> {
        self.session().and_then(SessionState::review)
    }

    /// The reviewed plan, which the apply confirmation keeps while it hides the raw plan.
    pub(crate) fn plan_review(&self) -> Option<&PlanReview> {
        self.review()
            .or_else(|| self.session().and_then(SessionState::apply_confirmation))
            .map(ReviewSessionState::review)
    }

    pub(crate) fn session(&self) -> Option<&SessionState> {
        match &self.state {
            EnvironmentState::Ready { session, .. } => Some(session),
            _ => None,
        }
    }

    pub(crate) fn diagnostic(&self) -> CopyEffect {
        let sensitive = self.review().map_or(&[][..], |review| {
            review.review().metadata().sensitive_values()
        });
        let diagnostics = self.review().map_or(self.diagnostics.as_slice(), |review| {
            review.review().diagnostics()
        });
        let effect = copy::diagnostic_effect(diagnostics, self.failure.as_deref(), sensitive);
        if !diagnostics.is_empty()
            && let Some(failure) = self.failure.as_deref()
        {
            return CopyEffect::new(
                effect.target(),
                format!(
                    "{}\n\n{}",
                    copy::sanitize_text(failure, sensitive),
                    effect.text()
                ),
            );
        }
        effect
    }
}

impl Preparation {
    pub(crate) const fn stage(&self) -> Option<PreparationStage> {
        self.stage
    }

    pub(crate) const fn initialization(&self) -> Option<&InitializationReason> {
        self.initialization.as_ref()
    }

    pub(crate) const fn lock_file(&self) -> Option<LockFileChange> {
        self.lock_file
    }

    pub(crate) fn latest_output(&self) -> Option<&str> {
        self.latest_output.as_deref()
    }

    fn record(&mut self, event: PreparationEvent) -> bool {
        match event {
            PreparationEvent::Initializing(reason) => {
                self.enter(PreparationStage::Initializing);
                self.initialization = Some(reason);
            }
            PreparationEvent::Planning => self.enter(PreparationStage::Planning),
            PreparationEvent::Reading => self.enter(PreparationStage::Reading),
            PreparationEvent::LockFile(change) => self.lock_file = Some(change),
            PreparationEvent::Output(event) => {
                let Some(line) = output_line(&event) else {
                    return false;
                };
                self.latest_output = Some(line);
            }
        }
        true
    }

    fn enter(&mut self, stage: PreparationStage) {
        self.stage = Some(stage);
        self.latest_output = None;
    }
}

// No plan value is known before the plan is read, so only the shared redaction applies here.
fn output_line(event: &ExecutionEvent) -> Option<String> {
    let text = match &event.kind {
        ExecutionEventKind::Resource(resource) => resource.message.as_deref()?,
        ExecutionEventKind::Diagnostic(diagnostic) => &diagnostic.summary,
        ExecutionEventKind::Informational { message, .. } => message.as_deref()?,
        ExecutionEventKind::Summary(summary) => summary.message.as_deref()?,
        ExecutionEventKind::Workspace(_) | ExecutionEventKind::Terminated(_) => return None,
    };
    let line = copy::sanitize_text(text.lines().next()?.trim(), &[]);
    (!line.is_empty()).then_some(line)
}

fn relative_display_path(directory: &Path, root: &Path) -> String {
    let shared = directory
        .components()
        .zip(root.components())
        .take_while(|(left, right)| left == right)
        .count();
    if shared == 0 {
        return directory.to_string_lossy().into_owned();
    }
    let components = root
        .components()
        .skip(shared)
        .map(|_| "..".to_owned())
        .chain(
            directory
                .components()
                .skip(shared)
                .map(|component| directory_display_name(Path::new(component.as_os_str()))),
        )
        .collect::<Vec<_>>();
    if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    }
}

pub(crate) fn is_production_token(token: &str) -> bool {
    PRODUCTION_ENVIRONMENT_NAMES
        .iter()
        .any(|name| token.eq_ignore_ascii_case(name))
}

fn environment_stage(name: &str) -> u8 {
    let mut stage = None;
    for token in name.split(['-', '_', '/']) {
        let current = if has_stage_token(token, DEVELOPMENT_ENVIRONMENT_NAMES) {
            Some(0)
        } else if has_stage_token(token, TEST_ENVIRONMENT_NAMES) {
            Some(1)
        } else if has_stage_token(token, STAGING_ENVIRONMENT_NAMES) {
            Some(2)
        } else if has_stage_token(token, PRODUCTION_ENVIRONMENT_NAMES) {
            Some(4)
        } else {
            None
        };
        stage = stage.max(current);
    }
    stage.unwrap_or(3)
}

// A parent directory names a group, not the stage of the environment inside it.
fn final_component(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn has_stage_token(token: &str, names: &[&str]) -> bool {
    let name = token.trim_end_matches(|character: char| character.is_ascii_digit());
    !name.is_empty() && names.iter().any(|known| name.eq_ignore_ascii_case(known))
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let (mut left_index, mut right_index) = (0, 0);

    while left_index < left.len() && right_index < right.len() {
        if left[left_index].is_ascii_digit() && right[right_index].is_ascii_digit() {
            let left_end = digit_run_end(left, left_index);
            let right_end = digit_run_end(right, right_index);
            let left_digits = significant_digits(&left[left_index..left_end]);
            let right_digits = significant_digits(&right[right_index..right_end]);
            let numeric_order = left_digits
                .len()
                .cmp(&right_digits.len())
                .then_with(|| left_digits.cmp(right_digits));
            if numeric_order != Ordering::Equal {
                return numeric_order;
            }
            let width_order = (left_end - left_index).cmp(&(right_end - right_index));
            if width_order != Ordering::Equal {
                return width_order;
            }
            left_index = left_end;
            right_index = right_end;
            continue;
        }

        let order = left[left_index]
            .to_ascii_lowercase()
            .cmp(&right[right_index].to_ascii_lowercase());
        if order != Ordering::Equal {
            return order;
        }
        left_index += 1;
        right_index += 1;
    }

    left.len()
        .saturating_sub(left_index)
        .cmp(&right.len().saturating_sub(right_index))
}

fn digit_run_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(bytes.len(), |offset| start + offset)
}

fn significant_digits(digits: &[u8]) -> &[u8] {
    let first_significant = digits
        .iter()
        .position(|digit| *digit != b'0')
        .unwrap_or(digits.len());
    &digits[first_significant..]
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::copy::{CopyResult, CopyTarget};
    use crate::app::plan::Plan;
    use crate::app::review::{PlanMetadata, test_support::plan_document};

    fn available(directory: impl Into<PathBuf>) -> Environment {
        Environment {
            tool: Tool::Terraform,
            availability: EnvironmentAvailability::Available {
                directory: directory.into(),
            },
        }
    }

    fn ready(changed: bool) -> PlanResult {
        ready_in("chosen", changed)
    }

    fn ready_in(workspace: &str, changed: bool) -> PlanResult {
        PlanResult::Ready {
            review: Box::new(PlanReview::new(
                PathBuf::from("/test"),
                workspace.to_owned(),
                plan_document("No changes.\n".to_owned()),
                Plan::empty(),
                PlanMetadata::new(changed),
                Vec::new(),
            )),
            changed,
        }
    }

    fn discovery_error(directory: &str) -> Environment {
        Environment {
            tool: Tool::Terraform,
            availability: EnvironmentAvailability::Error {
                directory: PathBuf::from(directory),
                message: "Synthetic configuration error".to_owned(),
            },
        }
    }

    fn complete_requested(state: &mut EnvironmentSession, result: PlanResult) -> usize {
        let index = state
            .start_next()
            .expect("a requested environment should start");
        assert!(state.complete(index, result, Vec::new()));
        index
    }

    fn record_plan_copy(state: &mut EnvironmentSession, index: usize, now: std::time::Instant) {
        let Some(Effect::WriteClipboard(effect)) =
            state.update_review(index, Action::Copy(CopyTarget::Plan), now)
        else {
            panic!("plan copy should produce a clipboard effect");
        };
        assert!(
            state
                .update_review(
                    index,
                    Action::CopyCompleted {
                        target: effect.target(),
                        result: CopyResult::Written,
                    },
                    now,
                )
                .is_none()
        );
    }

    #[test]
    fn starts_in_path_order_and_keeps_only_one_running() {
        let mut state = EnvironmentSession::new(vec![available("z"), available("a")], false);
        state.request_all_plans();
        assert_eq!(state.plans().len(), 2);
        assert!(
            state
                .plans()
                .iter()
                .all(|plan| matches!(plan.state(), EnvironmentState::Pending))
        );

        let first_index = state.start_next().unwrap();
        assert_eq!(state.plans()[first_index].directory(), Path::new("a"));
        assert!(state.start_next().is_none());
        assert!(state.complete(first_index, ready(true), Vec::new()));
        let next_index = state.start_next().unwrap();

        assert_eq!(state.plans()[next_index].directory(), Path::new("z"));
        assert!(state.plans()[0].review().is_some());
        assert!(matches!(
            state.plans()[1].state(),
            EnvironmentState::Running
        ));
    }

    #[test]
    fn environment_order_uses_stage_natural_names_and_a_stable_path_tie_breaker() {
        let names = [
            "prod10",
            "dev10",
            "DEV",
            "prod",
            "tokyo",
            "stg",
            "dev2",
            "test",
            "production",
            "preprod",
            "local",
            "dev",
            "prd",
            "qa",
            "integration",
            "uat",
            "prod-mirror-dev",
            "PROD",
            "development",
            "sandbox",
            "live",
            "devops",
            "nonprod",
            "int",
            "stage",
            "develop",
            "prod2",
        ];
        let environments = names
            .iter()
            .map(|name| available(format!("/synthetic/{name}")))
            .chain([available("/synthetic/z/dev"), available("/synthetic/a/dev")])
            .collect();

        let state = EnvironmentSession::new(environments, false);

        let ordered = state
            .plans()
            .iter()
            .map(EnvironmentPlan::display_name)
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            [
                "DEV",
                "dev",
                "dev",
                "dev",
                "dev2",
                "dev10",
                "develop",
                "development",
                "local",
                "sandbox",
                "int",
                "integration",
                "qa",
                "test",
                "preprod",
                "stage",
                "stg",
                "uat",
                "devops",
                "live",
                "nonprod",
                "tokyo",
                "prd",
                "PROD",
                "prod",
                "prod-mirror-dev",
                "prod2",
                "prod10",
                "production",
            ]
        );
        assert_eq!(state.plans()[0].directory(), Path::new("/synthetic/DEV"));
        assert_eq!(state.plans()[1].directory(), Path::new("/synthetic/a/dev"));
        assert_eq!(state.plans()[2].directory(), Path::new("/synthetic/dev"));
        assert_eq!(state.plans()[3].directory(), Path::new("/synthetic/z/dev"));
    }

    #[test]
    fn production_environment_is_detected_from_the_path_before_plan_completion() {
        let state = EnvironmentSession::new(
            vec![
                available("/repo/prod"),
                available("/repo/apps"),
                available("/repo/nonprod"),
            ],
            false,
        );

        for plan in state.plans() {
            let expected = plan.directory() == Path::new("/repo/prod");
            assert_eq!(
                plan.is_production(),
                expected,
                "{}",
                plan.directory().display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_production_environment_is_detected_before_plan_completion() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let directory = PathBuf::from(OsString::from_vec(b"/repo/prod-\xff".to_vec()));

        let state = EnvironmentSession::new(vec![available(directory)], false);

        assert!(state.plans()[0].is_production());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_default_workspace_environments_get_distinct_escaped_names() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let environments = [
            b"/repo/infra-\xff".as_slice(),
            b"/repo/infra-\xfe".as_slice(),
        ]
        .map(|directory| available(PathBuf::from(OsString::from_vec(directory.to_vec()))));

        let state = EnvironmentSession::new(environments.into(), false);

        assert_eq!(
            state
                .plans()
                .iter()
                .map(EnvironmentPlan::display_name)
                .collect::<Vec<_>>(),
            [r"infra-\xfe", r"infra-\xff"]
        );
    }

    #[test]
    fn order_uses_the_final_path_component_stage_without_parent_tokens() {
        let environments = ["/repo/prod/dev", "/repo/dev/prod", "/repo/stg"].map(available);

        let state =
            EnvironmentSession::new(environments.into(), false).with_exploration_root("/repo");

        assert_eq!(
            state
                .plans()
                .iter()
                .map(EnvironmentPlan::display_name)
                .collect::<Vec<_>>(),
            ["prod/dev", "stg", "dev/prod"]
        );
    }

    #[test]
    fn environment_order_stays_fixed_as_plan_acquisition_completes() {
        let environments =
            ["prod", "stg", "dev"].map(|name| available(format!("/synthetic/{name}")));
        let mut state = EnvironmentSession::new(environments.into(), false);
        state.request_all_plans();
        let original_order = state
            .plans()
            .iter()
            .map(|plan| plan.directory().to_owned())
            .collect::<Vec<_>>();

        for expected in ["dev", "stg", "prod"] {
            let index = state.start_next().unwrap();
            assert_eq!(state.plans()[index].display_name(), expected);
            assert!(state.complete(index, ready(false), Vec::new()));
            assert_eq!(
                state
                    .plans()
                    .iter()
                    .map(|plan| plan.directory().to_owned())
                    .collect::<Vec<_>>(),
                original_order
            );
        }
    }

    mod target_selection {
        use super::*;

        #[test]
        fn candidates_start_unselected_and_only_requested_ones_start() {
            let mut state = EnvironmentSession::new(vec![available("a"), available("b")], false);
            assert!(
                state
                    .plans()
                    .iter()
                    .all(|plan| matches!(plan.state(), EnvironmentState::Unselected))
            );
            assert!(state.start_next().is_none());
            assert!(!state.acquiring());

            assert!(state.request_plan(1));
            assert!(!state.request_plan(1));
            assert!(!state.request_plan(99));

            assert_eq!(state.start_next(), Some(1));
            assert!(state.start_next().is_none());
            assert!(matches!(
                state.plans()[0].state(),
                EnvironmentState::Unselected
            ));
            assert!(!state.plans()[0].is_target());
        }

        #[test]
        fn unselected_candidates_do_not_block_apply_or_decide_the_exit_code() {
            for (detailed, changed, expected) in
                [(false, true, 0), (true, true, 2), (true, false, 0)]
            {
                let mut state = EnvironmentSession::new(
                    vec![
                        available("a"),
                        available("b"),
                        available("c"),
                        discovery_error("d"),
                    ],
                    detailed,
                );
                test_support::exclude_hcp(&mut state, 2);
                assert_eq!(state.exit_code(), 1, "nothing chosen yet");
                assert!(state.request_plan(0));

                complete_requested(&mut state, ready(changed));

                assert!(state.can_start_apply());
                assert_eq!(
                    state.exit_code(),
                    expected,
                    "detailed={detailed}, changed={changed}"
                );
            }
        }

        #[test]
        fn retrying_a_discovery_error_makes_its_result_decide_the_exit_code() {
            let mut state =
                EnvironmentSession::new(vec![available("a"), discovery_error("d")], false);
            assert!(state.request_plan(0));
            complete_requested(&mut state, ready(false));
            assert_eq!(state.exit_code(), 0);

            assert!(state.retry(1));
            assert!(state.plans()[1].is_target());
            assert_eq!(state.exit_code(), 1);
            complete_requested(&mut state, PlanResult::Error("still broken".to_owned()));

            assert_eq!(state.exit_code(), 1);
        }

        #[test]
        fn partial_success_keeps_ready_results_while_only_the_failure_retries() {
            let mut state = EnvironmentSession::new(vec![available("a"), available("b")], false);
            assert!(state.request_all_plans());
            assert!(!state.request_all_plans());
            complete_requested(&mut state, ready(true));
            let failed = complete_requested(&mut state, PlanResult::Error("failed".to_owned()));
            assert_eq!(failed, 1);

            assert!(state.retry(failed));

            assert!(state.plans()[0].review().is_some());
            assert!(!state.can_start_apply());
            assert_eq!(state.start_next(), Some(failed));
            assert!(state.complete(failed, ready(false), Vec::new()));
            assert!(state.plans()[0].review().is_some());
            assert!(state.can_start_apply());
        }

        #[test]
        fn interruption_rejects_new_requests() {
            let mut state = EnvironmentSession::new(vec![available("a")], false);

            state.interrupt();

            assert!(!state.request_plan(0));
            assert!(!state.request_all_plans());
            assert!(state.start_next().is_none());
        }
    }

    mod identity {
        use super::*;

        #[test]
        fn workspace_stays_undetermined_until_a_plan_reports_it() {
            let mut state = EnvironmentSession::new(vec![available("/repo/apps/web")], false)
                .with_exploration_root("/repo");
            assert_eq!(state.plans()[0].workspace(), None);
            assert_eq!(state.plans()[0].display_name(), "apps/web");

            assert!(state.request_plan(0));
            complete_requested(&mut state, ready_in("blue", false));

            assert_eq!(state.plans()[0].workspace(), Some("blue"));
            assert_eq!(state.plans()[0].display_name(), "apps/web:blue");
        }

        #[test]
        fn shared_workspace_names_stay_distinct_by_path_and_default_adds_nothing() {
            let mut state = EnvironmentSession::new(
                vec![
                    available("/repo/app"),
                    available("/repo/network"),
                    available("/repo/shared"),
                ],
                false,
            )
            .with_exploration_root("/repo");
            assert!(state.request_all_plans());

            for workspace in ["prod", "prod", "default"] {
                complete_requested(&mut state, ready_in(workspace, false));
            }

            assert_eq!(
                state
                    .plans()
                    .iter()
                    .map(EnvironmentPlan::display_name)
                    .collect::<Vec<_>>(),
                ["app:prod", "network:prod", "shared"]
            );
        }

        #[test]
        fn targets_outside_or_at_the_root_keep_distinct_paths() {
            let state = EnvironmentSession::new(
                [
                    "/work/a/prod",
                    "/work/b/prod",
                    "/work/repo",
                    "/work/repo/repo",
                ]
                .map(available)
                .into(),
                false,
            )
            .with_exploration_root("/work/repo");

            assert_eq!(
                state
                    .plans()
                    .iter()
                    .map(EnvironmentPlan::display_name)
                    .collect::<Vec<_>>(),
                [".", "repo", "../a/prod", "../b/prod"]
            );
        }
    }

    mod preparation {
        use super::*;
        use crate::app::execution::{DiagnosticSeverity, DiagnosticSource, EventStream};
        use std::time::Instant;

        fn output(summary: &str) -> PreparationEvent {
            PreparationEvent::Output(ExecutionEvent {
                received_at: Instant::now(),
                kind: ExecutionEventKind::Diagnostic(Diagnostic {
                    severity: DiagnosticSeverity::Unknown,
                    summary: summary.to_owned(),
                    detail: None,
                    address: None,
                    position: None,
                    source: DiagnosticSource::NonJson {
                        stream: EventStream::Stdout,
                    },
                }),
            })
        }

        #[test]
        fn running_acquisition_records_steps_and_keeps_the_lock_change_after_completion() {
            let mut state = EnvironmentSession::new(vec![available("a")], false);
            state.request_all_plans();
            let index = state.start_next().unwrap();

            assert!(state.record_preparation(
                index,
                PreparationEvent::Initializing(InitializationReason::NotInitialized),
            ));
            assert!(state.record_preparation(
                index,
                output("- Downloading https://deploy:token@modules.example.test/network.zip"),
            ));
            let preparation = state.plans()[index].preparation();
            assert_eq!(preparation.stage(), Some(PreparationStage::Initializing));
            assert_eq!(
                preparation.initialization(),
                Some(&InitializationReason::NotInitialized)
            );
            assert_eq!(
                preparation.latest_output(),
                Some("- Downloading https://(sensitive value)@modules.example.test/network.zip")
            );

            assert!(
                state
                    .record_preparation(index, PreparationEvent::LockFile(LockFileChange::Created))
            );
            assert!(state.record_preparation(index, PreparationEvent::Planning));
            let preparation = state.plans()[index].preparation();
            assert_eq!(preparation.stage(), Some(PreparationStage::Planning));
            assert_eq!(preparation.latest_output(), None);

            assert!(state.complete(index, ready(false), Vec::new()));
            assert_eq!(
                state.plans()[index].preparation().lock_file(),
                Some(LockFileChange::Created)
            );
        }

        #[test]
        fn progress_outside_a_running_acquisition_is_ignored_and_retry_clears_it() {
            let mut state = EnvironmentSession::new(vec![available("a"), available("b")], false);
            state.request_all_plans();
            assert!(!state.record_preparation(1, PreparationEvent::Planning));
            let index = state.start_next().unwrap();
            assert!(state.record_preparation(index, PreparationEvent::Planning));
            assert!(state.complete(index, PlanResult::Error("failed".to_owned()), Vec::new()));

            assert!(!state.record_preparation(index, PreparationEvent::Reading));
            assert_eq!(
                state.plans()[index].preparation().stage(),
                Some(PreparationStage::Planning)
            );

            assert!(state.retry(index));
            assert_eq!(state.plans()[index].preparation().stage(), None);
            let next = state.start_next().unwrap();
            state.interrupt();
            assert!(!state.record_preparation(next, PreparationEvent::Reading));
        }
    }

    #[test]
    fn retries_only_errors_and_rejects_duplicate_completions() {
        let mut state = EnvironmentSession::new(
            vec![
                available("a"),
                available("b"),
                Environment {
                    tool: Tool::Terraform,
                    availability: EnvironmentAvailability::Available {
                        directory: PathBuf::from("c"),
                    },
                },
            ],
            true,
        );
        test_support::exclude_hcp(&mut state, 2);
        state.request_all_plans();
        let index = state.start_next().unwrap();
        assert!(!state.retry(0));
        assert!(state.complete(index, PlanResult::Error("failed".to_owned()), Vec::new()));
        assert!(state.retry(0));
        assert!(!state.retry(0));
        assert!(!state.retry(1));
        assert!(!state.retry(2));
        assert!(!state.retry(99));
        let retry_index = state.start_next().unwrap();

        assert!(state.complete(retry_index, ready(false), Vec::new()));
        assert!(!state.complete(
            retry_index,
            PlanResult::Error("duplicate".to_owned()),
            Vec::new()
        ));
        assert!(!state.retry(0));
        assert!(!state.complete(99, ready(true), Vec::new()));
        assert_eq!(state.plans()[0].workspace(), Some("chosen"));
        assert!(state.plans()[0].failure.is_none());
    }

    fn is_confirming(state: &EnvironmentSession, index: usize) -> bool {
        state.plans()[index]
            .session()
            .and_then(SessionState::apply_confirmation)
            .is_some()
    }

    #[test]
    fn apply_confirmation_opens_only_for_the_selected_ready_environment() {
        let mut state = EnvironmentSession::new(vec![available("a"), available("b")], false);
        state.request_all_plans();
        for _ in 0..2 {
            let index = state.start_next().unwrap();
            assert!(state.complete(index, ready(true), Vec::new()));
        }

        assert!(
            state
                .update_review(1, Action::OpenApplyConfirmation, std::time::Instant::now())
                .is_none()
        );

        assert!(is_confirming(&state, 1));
        assert!(state.plans()[0].review().is_some());
        assert!(!state.can_start_apply());
        state.update_review(0, Action::OpenApplyConfirmation, std::time::Instant::now());
        assert!(state.plans()[0].review().is_some());

        let session = state.session_mut(1).unwrap();
        session::update(session, Action::CancelApply, std::time::Instant::now());
        assert!(state.plans()[1].review().is_some());
        assert!(state.can_start_apply());
    }

    #[test]
    fn apply_waits_until_every_environment_plan_is_acquired() {
        let mut state = EnvironmentSession::new(vec![available("a"), available("b")], false);
        state.request_all_plans();
        let first = state.start_next().unwrap();
        assert!(state.complete(first, ready(true), Vec::new()));

        state.update_review(
            first,
            Action::OpenApplyConfirmation,
            std::time::Instant::now(),
        );
        assert!(state.plans()[first].review().is_some());

        let second = state.start_next().unwrap();
        assert!(state.complete(second, ready(false), Vec::new()));
        state.update_review(
            first,
            Action::OpenApplyConfirmation,
            std::time::Instant::now(),
        );
        assert!(is_confirming(&state, first));
    }

    #[test]
    fn environment_without_changes_does_not_open_apply_confirmation() {
        let mut state = EnvironmentSession::new(vec![available("a")], false);
        state.request_all_plans();
        let run = state.start_next().unwrap();
        state.complete(run, ready(false), Vec::new());

        state.update_review(0, Action::OpenApplyConfirmation, std::time::Instant::now());
        assert!(state.plans()[0].review().is_some());
    }

    #[test]
    fn clears_expired_copy_feedback_across_ready_environments() {
        let mut state = EnvironmentSession::new(vec![available("b"), available("a")], false);
        state.request_all_plans();
        for _ in 0..2 {
            let index = state.start_next().unwrap();
            assert!(state.complete(index, ready(false), Vec::new()));
        }

        let copied_at = std::time::Instant::now();
        record_plan_copy(&mut state, 0, copied_at);
        record_plan_copy(&mut state, 1, copied_at + std::time::Duration::from_secs(1));

        assert!(state.clear_expired_copy_feedback(copied_at + std::time::Duration::from_secs(3)));
        assert!(!state.clear_expired_copy_feedback(copied_at + std::time::Duration::from_secs(3)));
        assert!(state.clear_expired_copy_feedback(copied_at + std::time::Duration::from_secs(4)));
        assert!(!state.clear_expired_copy_feedback(copied_at + std::time::Duration::from_secs(5)));
    }

    #[test]
    fn exit_code_uses_current_results_and_interruption_precedes_incomplete_results() {
        for (detailed, changed, expected) in [
            (false, false, 0),
            (false, true, 0),
            (true, false, 0),
            (true, true, 2),
        ] {
            let mut state = EnvironmentSession::new(vec![available("a")], detailed);
            state.request_all_plans();
            assert_eq!(state.exit_code(), 1);
            let first = state.start_next().unwrap();
            state.complete(first, PlanResult::Error("failed".to_owned()), Vec::new());
            assert_eq!(state.exit_code(), 1);
            state.retry(0);
            let second = state.start_next().unwrap();
            state.complete(second, ready(changed), Vec::new());
            assert_eq!(
                state.exit_code(),
                expected,
                "detailed={detailed}, changed={changed}"
            );
            state.interrupt();
            assert_eq!(state.exit_code(), 130);
            assert!(!state.complete(second, ready(false), Vec::new()));
            assert!(state.start_next().is_none());
        }
        let mut state = EnvironmentSession::new(
            vec![Environment {
                tool: Tool::Terraform,
                availability: EnvironmentAvailability::Available {
                    directory: PathBuf::from("hcp"),
                },
            }],
            true,
        );
        test_support::exclude_hcp(&mut state, 0);
        assert_eq!(state.exit_code(), 1);
    }
}
