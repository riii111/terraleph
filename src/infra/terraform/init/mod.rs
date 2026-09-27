use std::{ffi::OsString, fs, path::Path};

use super::command::{
    ProcessRunner, ProcessStatus, TerraformCommand, TerraformExecutionError, interrupted_error,
    non_zero_error, run_command_with_events,
};
use crate::{
    app::execution::{ExecutionEvent, Tool},
    infra::CancellationToken,
};

pub(crate) fn needed(root: &Path) -> bool {
    let backend_initialized = fs::read(root.join(".terraform/terraform.tfstate"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|value| {
            value
                .get("backend")
                .is_some_and(serde_json::Value::is_object)
        });
    if !backend_initialized {
        return true;
    }
    // The lock file records installed external providers; built-in providers need no cache.
    let Ok(lock) = fs::read_to_string(root.join(".terraform.lock.hcl")) else {
        return false;
    };
    let Ok(body) = hcl::from_str::<hcl::Body>(&lock) else {
        return true;
    };
    body.blocks()
        .filter(|block| block.identifier() == "provider")
        .any(|block| {
            let Some(provider) = block.labels().first() else {
                return true;
            };
            let Some(version) = block
                .body
                .attributes()
                .find(|attr| attr.key() == "version")
                .and_then(|attr| {
                    if let hcl::Expression::String(value) = &attr.expr {
                        Some(value)
                    } else {
                        None
                    }
                })
            else {
                return true;
            };
            !root
                .join(".terraform/providers")
                .join(provider.as_str())
                .join(version)
                .is_dir()
        })
}

pub(crate) fn run(
    tool: Tool,
    root: &Path,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    event_sink: &mut dyn FnMut(ExecutionEvent),
) -> Result<(), TerraformExecutionError> {
    let process = run_command_with_events(
        tool,
        root,
        TerraformCommand::Init,
        &["init", "-input=false", "-no-color"].map(OsString::from),
        cancellation,
        runner,
        Some(event_sink),
    )?;
    if process.interrupted {
        return Err(interrupted_error(tool, TerraformCommand::Init, process));
    }
    if process.status != Some(ProcessStatus::Exited(0)) {
        return Err(non_zero_error(tool, TerraformCommand::Init, process));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "terraleph-init-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(directory.join(".terraform")).unwrap();
            Self(directory)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn initialization_requires_backend_metadata_and_locked_provider_installations() {
        let fixture = Fixture::new();
        assert!(needed(&fixture.0));
        fs::write(
            fixture.0.join(".terraform/terraform.tfstate"),
            r#"{"backend":{"type":"local"}}"#,
        )
        .unwrap();
        assert!(!needed(&fixture.0));
        fs::write(
            fixture.0.join(".terraform.lock.hcl"),
            "provider \"registry.terraform.io/example/synthetic\" {\n version = \"1.0.0\"\n}\n",
        )
        .unwrap();
        assert!(needed(&fixture.0));
        fs::create_dir_all(
            fixture
                .0
                .join(".terraform/providers/registry.terraform.io/example/synthetic/1.0.0"),
        )
        .unwrap();
        assert!(!needed(&fixture.0));
    }
}
