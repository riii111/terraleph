mod address;
mod apply;
mod command;
pub(crate) mod configuration;
pub(crate) mod discovery;
mod events;
mod line_buffer;
mod providers;
mod state;
// HCL parsing is dormant with Git attribution and remains covered by unit tests.
#[cfg(test)]
pub(crate) mod hcl;
pub(crate) mod init;
mod plan;
mod show;
mod version;
mod workspace;

pub(crate) use apply::run_apply_with_arguments;
pub(crate) use command::{
    ProcessStatus, SystemProcessRunner, TerraformExecutionError, delegate, resolve_executable,
};
pub(crate) use plan::{
    PlanRun, SavedPlan, read_saved_plan_review, remove_orphaned_plans, run_environment_plan,
    run_passthrough_plan, saved_plan_for_plan,
};
pub(crate) use providers::schema::read_provider_schema_with_arguments;
pub(crate) use workspace::read_workspace_with_arguments;

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) use super::command::{
        ProcessOutput, ProcessRunner, ProcessStatus, RunningProcess,
        TerraformExecutionError as CommandTerraformExecutionError,
    };
    pub(crate) use super::plan::test_support::{PlanTestError, run_plan};
}
