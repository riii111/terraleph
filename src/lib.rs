mod app;
mod infra;
pub(crate) mod runtime;
#[cfg(feature = "test-support")]
pub(crate) mod test_support;

mod ui;

use std::process::ExitCode;

use crate::app::execution::Tool;

pub use crate::runtime::invocation::EnvironmentTargets;

#[must_use]
pub fn run_terraform(targets: &EnvironmentTargets, arguments: &[std::ffi::OsString]) -> ExitCode {
    runtime::invocation::run(Tool::Terraform, targets, arguments)
}

#[must_use]
pub fn run_default(targets: &EnvironmentTargets) -> Option<ExitCode> {
    runtime::invocation::run_default(targets)
}

#[must_use]
pub fn run_tofu(targets: &EnvironmentTargets, arguments: &[std::ffi::OsString]) -> ExitCode {
    runtime::invocation::run(Tool::OpenTofu, targets, arguments)
}
