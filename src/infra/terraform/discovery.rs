use std::{fs, io, path::Path};

use crate::app::{
    environments::{Environment, EnvironmentAvailability},
    execution::Tool,
};

use super::configuration::{self, ExecutionLocation};

// Discovery reads files only. Running the tool, even for `workspace show`, would touch a candidate
// the user has not chosen yet.
pub(crate) fn discover(root: &Path, tool: Tool) -> io::Result<Vec<Environment>> {
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
        if let Some(availability) = inspect_directory(&directory, tool) {
            environments.push(Environment { tool, availability });
        }
    }
    Ok(environments)
}

fn inspect_directory(directory: &Path, tool: Tool) -> Option<EnvironmentAvailability> {
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
        Ok(Some(EnvironmentAvailability::Available { directory }))
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
    use std::{
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

        fn discover(&self, tool: Tool) -> Vec<Environment> {
            discover(&self.0, tool).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn direct_candidates_are_sorted_and_keep_tool_and_directory() {
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

        let environments = fixture.discover(Tool::OpenTofu);

        assert_eq!(
            environments,
            ["a", "z"].map(|name| Environment {
                tool: Tool::OpenTofu,
                availability: EnvironmentAvailability::Available {
                    directory: fixture.0.join(name),
                },
            })
        );
    }

    #[test]
    fn uninitialized_candidates_are_available_without_a_selected_workspace() {
        let fixture = Fixture::new();
        fixture.write("dev/main.tf", "terraform {\n backend \"s3\" {}\n}");
        fixture.write("prod/main.tf", "terraform {\n backend \"s3\" {}\n}");

        let environments = fixture.discover(Tool::Terraform);

        assert!(environments.iter().all(Environment::is_available));
        assert!(!fixture.0.join("dev/.terraform").exists());
    }

    #[test]
    fn hcp_candidates_are_retained_as_excluded() {
        let fixture = Fixture::new();
        fixture.write("hcp/main.tf", "terraform {\n cloud {}\n}");

        let environments = fixture.discover(Tool::Terraform);

        assert_eq!(
            environments[0].availability,
            EnvironmentAvailability::ExcludedHcp {
                directory: fixture.0.join("hcp")
            }
        );
    }

    #[test]
    fn broken_candidates_are_errors() {
        let fixture = Fixture::new();
        fixture.write("broken/main.tf", "terraform {");

        let environments = fixture.discover(Tool::Terraform);

        assert!(
            matches!(&environments[0].availability, EnvironmentAvailability::Error {directory, ..} if directory == &fixture.0.join("broken"))
        );
    }

    #[test]
    fn initialized_hcp_metadata_excludes_an_otherwise_local_candidate() {
        let fixture = Fixture::new();
        fixture.write("dev/main.tf", "terraform {\n backend \"s3\" {}\n}");
        fixture.write(
            "dev/.terraform/terraform.tfstate",
            r#"{"backend":{"type":"remote"}}"#,
        );

        let environments = fixture.discover(Tool::Terraform);

        assert!(matches!(
            environments[0].availability,
            EnvironmentAvailability::ExcludedHcp { .. }
        ));
    }

    #[test]
    fn opentofu_ignores_broken_shadowed_terraform_configuration() {
        let fixture = Fixture::new();
        fixture.write("dev/main.tf", "broken {");
        fixture.write("dev/main.tofu", "terraform {\n backend \"local\" {}\n}");

        assert!(fixture.discover(Tool::OpenTofu)[0].is_available());
        assert!(matches!(
            fixture.discover(Tool::Terraform)[0].availability,
            EnvironmentAvailability::Error { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_not_discovered() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new();
        let outside = Fixture::new();
        outside.write("main.tf", "terraform {\n backend \"local\" {}\n}");
        symlink(&outside.0, fixture.0.join("linked")).unwrap();

        assert!(fixture.discover(Tool::Terraform).is_empty());
    }
}
