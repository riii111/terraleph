use super::{EnvironmentSession, PlanResult};

pub(crate) fn exclude_hcp(state: &mut EnvironmentSession, index: usize) {
    assert!(state.request_plan(index));
    assert_eq!(state.start_next(), Some(index));
    assert!(state.complete(
        index,
        PlanResult::ExcludedHcp("Plan excluded because HCP performs the execution.".to_owned()),
        Vec::new()
    ));
}
