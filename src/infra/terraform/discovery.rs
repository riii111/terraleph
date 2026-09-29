use std::{fs, io, path::Path};

use crate::{
    app::{
        environments::{Environment, EnvironmentAvailability, EnvironmentIdentity},
        execution::Tool,
    },
    infra::CancellationToken,
};

use super::{
    command::ProcessRunner,
    configuration::{self, ExecutionLocation},
    workspace::read_workspace_with_arguments,
};

pub(crate) fn discover(
    root: &Path,
    tool: Tool,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
) -> io::Result<Vec<Environment>> {
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            directories.push(entry.path());
        }
    }
    directories.sort();
    let mut environments = Vec::new();
    for directory in directories {
        if let Some(availability) = inspect_directory(&directory, tool, cancellation, runner) {
            environments.push(Environment { tool, availability });
        }
    }
    Ok(environments)
}

fn inspect_directory(
    directory: &Path,
    tool: Tool,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
) -> Option<EnvironmentAvailability> {
    let result: io::Result<Option<EnvironmentAvailability>> = (|| {
        if !configuration::has_configuration(directory, tool)? {
            return Ok(None);
        }
        let directory = fs::canonicalize(directory)?;
        let configuration = configuration::read_configuration(&directory, tool, None)?;
        if !configuration.has_backend {
            return Ok(None);
        }
        if configuration.execution_location == ExecutionLocation::HcpCandidate {
            return Ok(Some(EnvironmentAvailability::ExcludedHcp { directory }));
        }
        let workspace = read_workspace_with_arguments(tool, &directory, &[], cancellation, runner)
            .map_err(|_| io::Error::other("cannot read the selected workspace"))?;
        Ok(Some(EnvironmentAvailability::Available(
            EnvironmentIdentity {
                directory,
                workspace,
            },
        )))
    })();
    match result {
        Ok(availability) => availability,
        Err(error) => Some(EnvironmentAvailability::Error {
            directory: directory.to_owned(),
            message: error.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::terraform::command::{ProcessOutput, ProcessStatus, RunningProcess};
    use std::{
        cell::RefCell,
        ffi::OsString,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "terraleph-discovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(fs::canonicalize(root).unwrap())
        }

        fn write(&self, path: &str, source: &str) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }

        fn discover(&self, tool: Tool, runner: &WorkspaceRunner) -> Vec<Environment> {
            discover(&self.0, tool, &CancellationToken::new(), runner).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[derive(Default)]
    struct WorkspaceRunner {
        calls: RefCell<Vec<(Tool, PathBuf)>>,
        fail: bool,
    }
    impl ProcessRunner for WorkspaceRunner {
        fn start(
            &self,
            tool: Tool,
            root: &Path,
            arguments: &[OsString],
        ) -> io::Result<Box<dyn RunningProcess>> {
            assert_eq!(arguments, ["workspace", "show"].map(OsString::from));
            self.calls.borrow_mut().push((tool, root.to_owned()));
            if self.fail {
                return Err(io::Error::other("must-not-leak-workspace-diagnostic"));
            }
            Ok(Box::new(WorkspaceProcess))
        }
    }
    struct WorkspaceProcess;
    impl RunningProcess for WorkspaceProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>> {
            Ok(Some(ProcessStatus::Exited(0)))
        }
        fn request_interrupt(&mut self) -> io::Result<()> {
            panic!("unexpected interrupt")
        }
        fn wait(&mut self) -> io::Result<ProcessStatus> {
            Ok(ProcessStatus::Exited(0))
        }
        fn collect_output(self: Box<Self>) -> io::Result<ProcessOutput> {
            Ok(ProcessOutput::new(
                b"selected-workspace\n".to_vec(),
                Vec::new(),
            ))
        }
    }

    #[test]
    fn direct_candidates_are_sorted_and_keep_tool_directory_and_selected_workspace() {
        let fixture = Fixture::new();
        fixture.write("z/main.tf", "terraform {\n backend \"s3\" {}\n}");
        fixture.write(
            "a/main.tf.json",
            r#"{"terraform":[{"backend":{"gcs":{}}}]}"#,
        );
        fixture.write(
            "module/main.tf",
            "resource \"terraform_data\" \"example\" {}",
        );
        fixture.write(
            "nested/deeper/main.tf",
            "terraform {\n backend \"local\" {}\n}",
        );
        fixture.write(
            "ignored/.hidden.tf",
            "terraform {\n backend \"local\" {}\n}",
        );
        fs::create_dir_all(fixture.0.join("directory/main.tf")).unwrap();
        let runner = WorkspaceRunner::default();

        let environments = fixture.discover(Tool::OpenTofu, &runner);

        assert_eq!(environments.len(), 2);
        for (environment, name) in environments.iter().zip(["a", "z"]) {
            assert_eq!(environment.tool, Tool::OpenTofu);
            assert_eq!(
                environment.availability,
                EnvironmentAvailability::Available(EnvironmentIdentity {
                    directory: fixture.0.join(name),
                    workspace: "selected-workspace".to_owned(),
                })
            );
        }
        assert_eq!(
            *runner.calls.borrow(),
            [
                (Tool::OpenTofu, fixture.0.join("a")),
                (Tool::OpenTofu, fixture.0.join("z"))
            ]
        );
    }

    #[test]
    fn hcp_candidates_are_retained_without_running_commands() {
        let fixture = Fixture::new();
        fixture.write("hcp/main.tf", "terraform {\n cloud {}\n}");
        let runner = WorkspaceRunner::default();

        let environments = fixture.discover(Tool::Terraform, &runner);

        assert_eq!(
            environments[0].availability,
            EnvironmentAvailability::ExcludedHcp {
                directory: fixture.0.join("hcp")
            }
        );
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn broken_candidates_are_errors_without_running_commands() {
        let fixture = Fixture::new();
        fixture.write("broken/main.tf", "terraform {");
        let runner = WorkspaceRunner::default();

        let environments = fixture.discover(Tool::Terraform, &runner);

        assert!(
            matches!(&environments[0].availability, EnvironmentAvailability::Error {directory, ..} if directory == &fixture.0.join("broken"))
        );
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn initialized_hcp_metadata_excludes_an_otherwise_local_candidate() {
        let fixture = Fixture::new();
        fixture.write("dev/main.tf", "terraform {\n backend \"s3\" {}\n}");
        fixture.write(
            "dev/.terraform/terraform.tfstate",
            r#"{"backend":{"type":"remote"}}"#,
        );
        let runner = WorkspaceRunner::default();

        let environments = fixture.discover(Tool::Terraform, &runner);

        assert!(matches!(
            environments[0].availability,
            EnvironmentAvailability::ExcludedHcp { .. }
        ));
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn opentofu_ignores_broken_shadowed_terraform_configuration() {
        let fixture = Fixture::new();
        fixture.write("dev/main.tf", "broken {");
        fixture.write("dev/main.tofu", "terraform {\n backend \"local\" {}\n}");
        let runner = WorkspaceRunner::default();

        assert!(fixture.discover(Tool::OpenTofu, &runner)[0].is_available());
        assert!(matches!(
            fixture.discover(Tool::Terraform, &runner)[0].availability,
            EnvironmentAvailability::Error { .. }
        ));
    }

    #[test]
    fn workspace_failure_is_an_error_without_exposing_cli_output() {
        let fixture = Fixture::new();
        fixture.write("dev/main.tf", "terraform {\n backend \"local\" {}\n}");
        let runner = WorkspaceRunner {
            fail: true,
            ..WorkspaceRunner::default()
        };

        let environments = fixture.discover(Tool::Terraform, &runner);

        assert_eq!(
            environments[0].availability,
            EnvironmentAvailability::Error {
                directory: fixture.0.join("dev"),
                message: "cannot read the selected workspace".to_owned(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_discovered() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outside = Fixture::new();
        outside.write("main.tf", "terraform {\n backend \"local\" {}\n}");
        symlink(&outside.0, fixture.0.join("linked")).unwrap();
        let runner = WorkspaceRunner::default();

        assert!(fixture.discover(Tool::Terraform, &runner).is_empty());
        assert!(runner.calls.borrow().is_empty());
    }
}
