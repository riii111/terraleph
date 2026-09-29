use std::{ffi::OsString, path::Path};

use crate::app::execution::Tool;
use crate::infra::CancellationToken;

use super::command::{
    ProcessRunner, TerraformCommand, TerraformExecutionError, invalid_output, run_successful,
};

pub(crate) fn read_workspace_with_arguments(
    tool: Tool,
    root: &Path,
    global_arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
) -> Result<String, TerraformExecutionError> {
    let mut arguments = global_arguments.to_vec();
    arguments.extend([OsString::from("workspace"), OsString::from("show")]);
    let output = run_successful(
        tool,
        root,
        TerraformCommand::WorkspaceShow,
        &arguments,
        cancellation,
        runner,
        None,
    )?;
    let workspace = String::from_utf8(output.stdout)
        .map_err(|error| invalid_output(tool, TerraformCommand::WorkspaceShow, error))?;
    let workspace = workspace.trim();
    if workspace.is_empty() {
        return Err(invalid_output(
            tool,
            TerraformCommand::WorkspaceShow,
            "workspace name is empty",
        ));
    }
    Ok(workspace.to_owned())
}
