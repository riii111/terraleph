use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use crate::app::{
    environments::{Environment, EnvironmentAvailability},
    execution::Tool,
};

use super::configuration;

mod git;

use git::Listing;

const WALK_DEPTH: usize = 4;

pub(crate) struct Discovery {
    pub(crate) environments: Vec<Environment>,
    pub(crate) walk_limit: Option<usize>,
}

// Discovery reads files only. Running the tool, even for `workspace show`, would touch a candidate
// the user has not chosen yet.
pub(crate) fn discover(root: &Path, tool: Tool) -> io::Result<Discovery> {
    let (directories, walk_limit) = match git::list_configuration(root) {
        Listing::Directories(directories) => (directories, None),
        listing => {
            let walk = walk(root, WALK_DEPTH, matches!(listing, Listing::Empty))?;
            (walk.directories, walk.truncated.then_some(WALK_DEPTH))
        }
    };
    Ok(Discovery {
        environments: inspect_directories(&directories, tool),
        walk_limit,
    })
}

pub(crate) fn inspect_targets(directories: &[PathBuf], tool: Tool) -> io::Result<Vec<Environment>> {
    let mut seen = BTreeSet::new();
    let mut environments = Vec::new();
    for directory in directories {
        let canonical = fs::canonicalize(directory).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("--env-dir {}: {error}", directory.display()),
            )
        })?;
        if !canonical.is_dir() {
            return Err(io::Error::other(format!(
                "--env-dir {}: not a directory",
                directory.display()
            )));
        }
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if !configuration::has_configuration(&canonical, tool)? {
            return Err(io::Error::other(format!(
                "--env-dir {}: no configuration files for {} in this directory",
                directory.display(),
                tool.display_name()
            )));
        }
        let availability = match configuration::read_configuration(&canonical, tool, None) {
            Ok(_) => EnvironmentAvailability::Available {
                directory: canonical,
            },
            Err(error) => EnvironmentAvailability::Error {
                directory: canonical,
                message: error.to_string(),
            },
        };
        environments.push(Environment { tool, availability });
    }
    Ok(environments)
}

fn inspect_directories(directories: &[PathBuf], tool: Tool) -> Vec<Environment> {
    let mut environments = Vec::new();
    let mut module_sources = BTreeSet::new();
    for directory in directories {
        if let Some(availability) = inspect_directory(directory, tool) {
            environments.push(Environment { tool, availability });
        }
        if let Ok(sources) = configuration::local_module_sources(directory, tool) {
            module_sources.extend(
                sources
                    .iter()
                    .filter_map(|source| fs::canonicalize(source).ok()),
            );
        }
    }
    environments.retain(|environment| !module_sources.contains(environment.directory()));
    environments.sort_by(|left, right| left.directory().cmp(right.directory()));
    environments
}

struct Walk {
    directories: Vec<PathBuf>,
    truncated: bool,
}

// Symlinks are not followed, so they cannot repeat a candidate or loop.
fn walk(root: &Path, max_depth: usize, within_work_tree: bool) -> io::Result<Walk> {
    let mut directories = Vec::new();
    let mut truncated = false;
    let mut pending = child_directories(root)?
        .into_iter()
        .map(|directory| (directory, 1))
        .collect::<Vec<_>>();
    while let Some((directory, depth)) = pending.pop() {
        if within_work_tree && is_repository_root(&directory) {
            continue;
        }
        if let Ok(children) = child_directories(&directory) {
            if depth < max_depth {
                pending.extend(children.into_iter().map(|child| (child, depth + 1)));
            } else {
                truncated |= !children.is_empty();
            }
        }
        directories.push(directory);
    }
    Ok(Walk {
        directories,
        truncated,
    })
}

fn child_directories(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !is_hidden(&entry.file_name()) {
            children.push(entry.path());
        }
    }
    Ok(children)
}

fn is_hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

// A submodule's `.git` is a file, so `is_dir` would miss it.
fn is_repository_root(directory: &Path) -> bool {
    fs::symlink_metadata(directory.join(".git")).is_ok()
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    const BACKEND: &str = "terraform {\n backend \"local\" {}\n}";

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
            discover(&self.0, tool).unwrap().environments
        }

        fn git(&self, arguments: &[&str]) {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.0)
                .args(arguments)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {arguments:?} failed");
        }

        fn directories(environments: &[Environment]) -> Vec<&Path> {
            environments.iter().map(Environment::directory).collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    mod discovery {
        use super::*;

        #[test]
        fn candidates_are_sorted_and_keep_tool_and_directory() {
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
            fixture.write("ignored/.hidden.tf", BACKEND);
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
        fn the_walk_outside_git_stops_at_its_depth_and_reports_the_limit() {
            let fixture = Fixture::new();
            fixture.write("live/aws/prod/main.tf", BACKEND);
            fixture.write("live/gcp/prod/main.tf", BACKEND);
            fixture.write("live/gcp/prod/bootstrap/main.tf", BACKEND);
            fixture.write("a/b/c/d/too-deep/main.tf", BACKEND);

            let discovery = discover(&fixture.0, Tool::Terraform).unwrap();

            assert_eq!(
                Fixture::directories(&discovery.environments),
                [
                    fixture.0.join("live/aws/prod"),
                    fixture.0.join("live/gcp/prod"),
                    fixture.0.join("live/gcp/prod/bootstrap"),
                ]
            );
            assert_eq!(discovery.walk_limit, Some(WALK_DEPTH));
        }

        #[test]
        fn a_walk_that_reaches_every_directory_reports_no_limit() {
            let fixture = Fixture::new();
            fixture.write("a/b/c/dev/main.tf", BACKEND);
            fixture.write("a/b/c/dev/.terraform/modules/x/main.tf", BACKEND);

            let discovery = discover(&fixture.0, Tool::Terraform).unwrap();

            assert_eq!(
                Fixture::directories(&discovery.environments),
                [fixture.0.join("a/b/c/dev")]
            );
            assert_eq!(discovery.walk_limit, None);
        }

        #[test]
        fn generated_and_hidden_directories_are_skipped() {
            let fixture = Fixture::new();
            fixture.write("dev/main.tf", BACKEND);
            fixture.write("dev/.terraform/modules/vpc/main.tf", BACKEND);
            fixture.write(".git/hooks/main.tf", BACKEND);
            fixture.write(".terragrunt-cache/x/main.tf", BACKEND);

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(Fixture::directories(&environments), [fixture.0.join("dev")]);
        }

        #[test]
        fn locally_called_modules_are_not_candidates_even_with_a_backend() {
            let fixture = Fixture::new();
            fixture.write(
                "envs/prod/main.tf",
                "terraform {\n backend \"s3\" {}\n}\nmodule \"app\" { source = \"../../modules/app\" }",
            );
            fixture.write(
                "modules/app/main.tf",
                "terraform {\n backend \"s3\" {}\n}\nmodule \"db\" { source = \"../db\" }",
            );
            fixture.write("modules/db/main.tf", BACKEND);
            fixture.write("modules/plain/main.tf", "variable \"name\" {}");
            fixture.write("modules/shared/main.tf", BACKEND);
            fixture.write(
                "envs/stg/main.tf.json",
                &serde_json::json!({
                    "terraform": {"backend": {"s3": {}}},
                    "module": {"shared": {"source": fixture.0.join("modules/shared")}},
                })
                .to_string(),
            );

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(
                Fixture::directories(&environments),
                [fixture.0.join("envs/prod"), fixture.0.join("envs/stg")]
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
        fn hcp_candidates_wait_for_the_selected_workspace_execution_check() {
            let fixture = Fixture::new();
            fixture.write("hcp/main.tf", "terraform {\n cloud {}\n}");

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(
                environments[0].availability,
                EnvironmentAvailability::Available {
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
        fn initialized_hcp_metadata_remains_available_for_execution_check() {
            let fixture = Fixture::new();
            fixture.write("dev/main.tf", "terraform {\n backend \"s3\" {}\n}");
            fixture.write(
                "dev/.terraform/terraform.tfstate",
                r#"{"backend":{"type":"remote"}}"#,
            );

            let environments = fixture.discover(Tool::Terraform);

            assert!(matches!(
                environments[0].availability,
                EnvironmentAvailability::Available { .. }
            ));
        }

        #[test]
        fn opentofu_ignores_broken_shadowed_terraform_configuration() {
            let fixture = Fixture::new();
            fixture.write("dev/main.tf", "broken {");
            fixture.write("dev/main.tofu", BACKEND);

            assert!(fixture.discover(Tool::OpenTofu)[0].is_available());
            assert!(matches!(
                fixture.discover(Tool::Terraform)[0].availability,
                EnvironmentAvailability::Error { .. }
            ));
        }

        #[cfg(unix)]
        #[test]
        fn symlinked_directories_are_neither_repeated_nor_followed_into_loops() {
            use std::os::unix::fs::symlink;

            let fixture = Fixture::new();
            let outside = Fixture::new();
            outside.write("main.tf", BACKEND);
            fixture.write("live/dev/main.tf", BACKEND);
            symlink(&outside.0, fixture.0.join("linked")).unwrap();
            symlink(fixture.0.join("live"), fixture.0.join("live/dev/loop")).unwrap();
            symlink(fixture.0.join("live/dev"), fixture.0.join("alias")).unwrap();

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(
                Fixture::directories(&environments),
                [fixture.0.join("live/dev")]
            );
        }
    }

    mod git_work_tree {
        use super::*;

        fn repository() -> Fixture {
            let fixture = Fixture::new();
            fixture.git(&["init", "--quiet"]);
            fixture
        }

        #[test]
        fn tracked_and_untracked_candidates_are_found_at_any_depth_without_a_limit() {
            let fixture = repository();
            fixture.write("infra/terraform/gcp/envs/prod/asia/main.tf", BACKEND);
            fixture.write(
                "infra/terraform/gcp/envs/stg/main.tf.json",
                r#"{"terraform":[{"backend":{"gcs":{}}}]}"#,
            );
            fixture.git(&["add", "infra/terraform/gcp/envs/prod"]);

            let discovery = discover(&fixture.0, Tool::Terraform).unwrap();

            assert_eq!(
                Fixture::directories(&discovery.environments),
                [
                    fixture.0.join("infra/terraform/gcp/envs/prod/asia"),
                    fixture.0.join("infra/terraform/gcp/envs/stg"),
                ]
            );
            assert_eq!(discovery.walk_limit, None);
        }

        #[test]
        fn ignored_hidden_and_deleted_directories_are_not_candidates() {
            let fixture = repository();
            fixture.write(".gitignore", "vendor/\n");
            fixture.write("envs/dev/main.tf", BACKEND);
            fixture.write("vendor/stack/main.tf", BACKEND);
            fixture.write("envs/dev/.cache/main.tf", BACKEND);
            fixture.write("envs/old/main.tf", BACKEND);
            fixture.git(&["add", "envs/old"]);
            fs::remove_dir_all(fixture.0.join("envs/old")).unwrap();

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(
                Fixture::directories(&environments),
                [fixture.0.join("envs/dev")]
            );
        }

        #[test]
        fn a_search_from_a_subdirectory_lists_only_below_it() {
            let fixture = repository();
            fixture.write("live/prod/main.tf", BACKEND);
            fixture.write("other/prod/main.tf", BACKEND);

            let discovery = discover(&fixture.0.join("live"), Tool::Terraform).unwrap();

            assert_eq!(
                Fixture::directories(&discovery.environments),
                [fixture.0.join("live/prod")]
            );
        }

        #[test]
        fn configuration_that_git_ignores_entirely_is_still_walked() {
            let fixture = repository();
            fixture.write(".gitignore", "*\n");
            fixture.write("scratch/dev/main.tf", BACKEND);

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(
                Fixture::directories(&environments),
                [fixture.0.join("scratch/dev")]
            );
        }

        #[test]
        fn the_walk_in_a_work_tree_without_configuration_skips_submodules() {
            let library = repository();
            library.write("dev/main.tf", BACKEND);
            library.git(&["add", "."]);
            library.git(&[
                "-c",
                "user.name=Terraleph",
                "-c",
                "user.email=terraleph@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "library",
            ]);
            let fixture = repository();
            let source = library.0.to_str().unwrap();
            fixture.git(&[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "--quiet",
                "add",
                source,
                "sub",
            ]);
            fixture.write("scratch/stg/main.tf", BACKEND);
            fixture.write(".gitignore", "scratch/\n");

            let environments = fixture.discover(Tool::Terraform);

            assert_eq!(
                Fixture::directories(&environments),
                [fixture.0.join("scratch/stg")]
            );
        }

        #[test]
        fn configuration_listed_only_in_hidden_directories_does_not_fall_back_to_the_walk() {
            let fixture = repository();
            fixture.write(".gitignore", "ignored/\n");
            fixture.write(".hidden/main.tf", BACKEND);
            fixture.write("ignored/dev/main.tf", BACKEND);

            let discovery = discover(&fixture.0, Tool::Terraform).unwrap();

            assert!(discovery.environments.is_empty());
            assert_eq!(discovery.walk_limit, None);
        }
    }

    mod targets {
        use super::*;

        #[test]
        fn named_directories_are_candidates_without_a_backend_or_despite_module_calls() {
            let fixture = Fixture::new();
            fixture.write("stack/main.tf", "resource \"terraform_data\" \"x\" {}");
            fixture.write("hcp/main.tf", "terraform {\n cloud {}\n}");
            fixture.write("broken/main.tf", "terraform {");

            let environments = inspect_targets(
                &[
                    fixture.0.join("stack"),
                    fixture.0.join("hcp"),
                    fixture.0.join("broken"),
                ],
                Tool::Terraform,
            )
            .unwrap();

            assert_eq!(
                environments[0].availability,
                EnvironmentAvailability::Available {
                    directory: fixture.0.join("stack")
                }
            );
            assert!(matches!(
                environments[1].availability,
                EnvironmentAvailability::Available { .. }
            ));
            assert!(matches!(
                environments[2].availability,
                EnvironmentAvailability::Error { .. }
            ));
        }

        #[cfg(unix)]
        #[test]
        fn the_same_directory_named_twice_or_through_a_symlink_is_one_candidate() {
            use std::os::unix::fs::symlink;

            let fixture = Fixture::new();
            fixture.write("dev/main.tf", BACKEND);
            symlink(fixture.0.join("dev"), fixture.0.join("alias")).unwrap();

            let environments = inspect_targets(
                &[
                    fixture.0.join("dev"),
                    fixture.0.join("alias"),
                    fixture.0.join("dev/."),
                ],
                Tool::Terraform,
            )
            .unwrap();

            assert_eq!(Fixture::directories(&environments), [fixture.0.join("dev")]);
        }

        #[test]
        fn a_directory_without_configuration_is_rejected_with_its_name() {
            let fixture = Fixture::new();
            fixture.write("empty/README.md", "");

            let missing = inspect_targets(&[fixture.0.join("missing")], Tool::Terraform);
            let empty = inspect_targets(&[fixture.0.join("empty")], Tool::OpenTofu);

            assert!(missing.unwrap_err().to_string().contains("missing"));
            let error = empty.unwrap_err().to_string();
            assert!(error.contains("empty") && error.contains("tofu"), "{error}");
        }
    }
}
