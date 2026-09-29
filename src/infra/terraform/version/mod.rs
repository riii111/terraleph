use std::{ffi::OsString, path::Path};

use serde_json::Value;

use crate::app::execution::Tool;
use crate::infra::CancellationToken;

use super::command::{
    ProcessRunner, TerraformCommand, TerraformExecutionError, invalid_output, run_successful,
};

pub(crate) fn read_version_with_arguments(
    tool: Tool,
    root: &Path,
    global_arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
) -> Result<String, TerraformExecutionError> {
    let mut arguments = global_arguments.to_vec();
    arguments.extend([OsString::from("version"), OsString::from("-json")]);
    let output = run_successful(
        tool,
        root,
        TerraformCommand::Version,
        &arguments,
        cancellation,
        runner,
        None,
    )?;
    let document = serde_json::from_slice::<Value>(&output.stdout)
        .map_err(|error| invalid_output(tool, TerraformCommand::Version, error))?;
    document
        .get("terraform_version")
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid_output(
                tool,
                TerraformCommand::Version,
                "terraform_version is missing",
            )
        })
}
