mod app;
mod infra;
pub(crate) mod runtime;
#[cfg(feature = "test-support")]
pub(crate) mod test_support;

mod ui;

use std::process::ExitCode;

use crate::app::execution::Tool;

#[must_use]
pub fn run_terraform(arguments: &[std::ffi::OsString]) -> ExitCode {
    runtime::invocation::run(Tool::Terraform, arguments)
}

#[must_use]
pub fn run_default() -> Option<ExitCode> {
    runtime::invocation::run_default()
}

#[must_use]
pub fn run_tofu(arguments: &[std::ffi::OsString]) -> ExitCode {
    runtime::invocation::run(Tool::OpenTofu, arguments)
}
