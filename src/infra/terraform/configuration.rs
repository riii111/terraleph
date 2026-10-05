use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

use crate::app::execution::Tool;
use hcl::Body;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionLocation {
    Local,
    HcpCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Configuration {
    pub(crate) has_backend: bool,
    pub(crate) execution_location: ExecutionLocation,
}

pub(crate) fn has_configuration(root: &Path, tool: Tool) -> io::Result<bool> {
    Ok(!configuration_files(root, tool)?.is_empty())
}

// Only the root module's own calls are listed; nested calls are installed with their parent.
pub(crate) fn module_calls(root: &Path, tool: Tool) -> io::Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for path in configuration_files(root, tool)? {
        let source = fs::read_to_string(&path)?;
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let value: Value =
                serde_json::from_str(&source).map_err(|_| invalid_configuration())?;
            if let Some(modules) = value.get("module") {
                for_json_block(modules, |calls| {
                    names.extend(calls.keys().cloned());
                    Ok(())
                })?;
            }
        } else {
            let body: Body = hcl::from_str(&source).map_err(|_| invalid_configuration())?;
            names.extend(
                body.blocks()
                    .filter(|block| block.identifier() == "module")
                    .filter_map(|block| block.labels().first())
                    .map(|label| label.as_str().to_owned()),
            );
        }
    }
    Ok(names)
}

pub(crate) fn execution_location_for_tool(
    root: &Path,
    tool: Tool,
    data_dir: Option<&OsStr>,
) -> io::Result<ExecutionLocation> {
    Ok(read_configuration(root, tool, data_dir)?.execution_location)
}

pub(crate) fn read_configuration(
    root: &Path,
    tool: Tool,
    data_dir: Option<&OsStr>,
) -> io::Result<Configuration> {
    let mut configuration = Configuration {
        has_backend: false,
        execution_location: ExecutionLocation::Local,
    };
    let mut paths = configuration_files(root, tool)?;
    paths.sort_by_key(|path| is_override_file(path));
    for path in paths {
        let source = read_source_configuration(&path)?;
        if source.has_backend {
            if configuration.has_backend && !is_override_file(&path) {
                return Err(invalid_configuration());
            }
            configuration = source;
        }
    }
    let data_dir = data_dir
        .filter(|value| !value.is_empty())
        .map_or_else(|| root.join(".terraform"), |value| root.join(value));
    match fs::read(data_dir.join("terraform.tfstate")) {
        Ok(bytes) => {
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| invalid_configuration())?;
            let object = value.as_object().ok_or_else(invalid_configuration)?;
            if let Some(backend) = object.get("backend").filter(|value| !value.is_null()) {
                let kind = backend
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_configuration)?;
                if matches!(kind, "remote" | "cloud") {
                    configuration.execution_location = ExecutionLocation::HcpCandidate;
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(configuration)
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Terraform and OpenTofu only recognize their lowercase configuration extensions"
)]
fn configuration_files(root: &Path, tool: Tool) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        !path.is_dir()
            && path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| {
                    !name.starts_with('.') && !name.starts_with('#') && !name.ends_with('~')
                })
    });
    paths.sort();
    if tool == Tool::Terraform {
        return Ok(paths
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.ends_with(".tf") || name.ends_with(".tf.json"))
            })
            .collect());
    }

    let names = paths
        .iter()
        .filter_map(|path| path.file_name().and_then(OsStr::to_str))
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    Ok(paths
        .into_iter()
        .filter(|path| {
            let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                return false;
            };
            if name.ends_with(".tofu") || name.ends_with(".tofu.json") {
                return true;
            }
            if let Some(stem) = name.strip_suffix(".tf") {
                return !names.contains(&format!("{stem}.tofu"));
            }
            if let Some(stem) = name.strip_suffix(".tf.json") {
                return !names.contains(&format!("{stem}.tofu.json"));
            }
            false
        })
        .collect())
}

fn read_source_configuration(path: &Path) -> io::Result<Configuration> {
    let source = fs::read_to_string(path)?;
    let mut configuration = Configuration {
        has_backend: false,
        execution_location: ExecutionLocation::Local,
    };
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let value: Value = serde_json::from_str(&source).map_err(|_| invalid_configuration())?;
        read_json_configuration(&value, &mut configuration)?;
    } else {
        let body: Body = hcl::from_str(&source).map_err(|_| invalid_configuration())?;
        read_hcl_configuration(&body, &mut configuration)?;
    }
    Ok(configuration)
}

fn read_hcl_configuration(body: &Body, configuration: &mut Configuration) -> io::Result<()> {
    for terraform in body
        .blocks()
        .filter(|block| block.identifier() == "terraform")
    {
        if !terraform.labels().is_empty() {
            return Err(invalid_configuration());
        }
        for block in terraform.body.blocks() {
            match block.identifier() {
                "backend" => {
                    if block.labels().len() != 1 {
                        return Err(invalid_configuration());
                    }
                    record_backend(configuration, block.labels()[0].as_str())?;
                }
                "cloud" => {
                    if !block.labels().is_empty() {
                        return Err(invalid_configuration());
                    }
                    record_backend(configuration, "cloud")?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn read_json_configuration(value: &Value, configuration: &mut Configuration) -> io::Result<()> {
    let object = value.as_object().ok_or_else(invalid_configuration)?;
    if let Some(terraform) = object.get("terraform") {
        for_json_block(terraform, |block| {
            if let Some(cloud) = block.get("cloud") {
                for_json_block(cloud, |_| record_backend(configuration, "cloud"))?;
            }
            if let Some(backend) = block.get("backend") {
                if backend.as_array().is_some_and(Vec::is_empty) {
                    return Err(invalid_configuration());
                }
                for_json_block(backend, |backends| {
                    if backends.is_empty() {
                        return Err(invalid_configuration());
                    }
                    for (kind, body) in backends {
                        for_json_block(body, |_| record_backend(configuration, kind))?;
                    }
                    Ok(())
                })?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

fn record_backend(configuration: &mut Configuration, kind: &str) -> io::Result<()> {
    if configuration.has_backend {
        return Err(invalid_configuration());
    }
    configuration.has_backend = true;
    configuration.execution_location = if matches!(kind, "remote" | "cloud") {
        ExecutionLocation::HcpCandidate
    } else {
        ExecutionLocation::Local
    };
    Ok(())
}

fn is_override_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    let name = name.strip_suffix(".json").unwrap_or(name);
    let stem = name
        .strip_suffix(".tf")
        .or_else(|| name.strip_suffix(".tofu"));
    stem.is_some_and(|stem| stem == "override" || stem.ends_with("_override"))
}

fn for_json_block(
    value: &Value,
    mut read: impl FnMut(&serde_json::Map<String, Value>) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(blocks) = value.as_array() {
        for block in blocks {
            read(block.as_object().ok_or_else(invalid_configuration)?)?;
        }
        Ok(())
    } else {
        read(value.as_object().ok_or_else(invalid_configuration)?)
    }
}

fn invalid_configuration() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "cannot determine Terraform execution location",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(name: &str, source: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "terraleph-config-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("create fixture");
            fs::write(root.join(name), source).expect("write configuration");
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("clean fixture");
        }
    }

    fn execution_location(root: &Path, data_dir: Option<&OsStr>) -> io::Result<ExecutionLocation> {
        super::execution_location_for_tool(root, Tool::Terraform, data_dir)
    }

    #[rstest]
    #[case::cloud("main.tf", "terraform {\n cloud {}\n}")]
    #[case::remote("main.tf", "terraform {\n backend \"remote\" {}\n}")]
    #[case::json_cloud("main.tf.json", r#"{"terraform":{"cloud":{}}}"#)]
    #[case::json_remote("main.tf.json", r#"{"terraform":[{"backend":{"remote":{}}}]}"#)]
    fn hcp_configuration_is_a_candidate(#[case] name: &str, #[case] source: &str) {
        let fixture = Fixture::new(name, source);
        assert_eq!(
            execution_location(&fixture.0, None).unwrap(),
            ExecutionLocation::HcpCandidate
        );
    }

    #[rstest]
    #[case::s3("main.tf", "terraform {\n backend \"s3\" {}\n}")]
    #[case::gcs("main.tf.json", r#"{"terraform":{"backend":{"gcs":{}}}}"#)]
    #[case::comments(
        "main.tf",
        "# terraform { cloud {} }\n/* backend \"remote\" {} */\nlocals { text = \"cloud\" }"
    )]
    #[case::strings("main.tf.json", r#"{"locals":{"text":"terraform {\n cloud {}\n}"}}"#)]
    fn local_configuration_does_not_detect_cloud_words(#[case] name: &str, #[case] source: &str) {
        let fixture = Fixture::new(name, source);
        assert_eq!(
            execution_location(&fixture.0, None).unwrap(),
            ExecutionLocation::Local
        );
    }

    #[rstest]
    #[case::hcl("main.tf", "terraform {")]
    #[case::json("main.tf.json", "{")]
    #[case::json_shape("main.tf.json", r#"{"terraform":true}"#)]
    #[case::json_backend_missing_label("main.tf.json", r#"{"terraform":{"backend":{}}}"#)]
    #[case::json_backend_missing_label_array("main.tf.json", r#"{"terraform":{"backend":[]}}"#)]
    #[case::hcl_backend_missing_label("main.tf", "terraform {\n backend {}\n}")]
    #[case::json_backend_body("main.tf.json", r#"{"terraform":{"backend":{"s3":true}}}"#)]
    #[case::json_cloud_body("main.tf.json", r#"{"terraform":{"cloud":true}}"#)]
    fn broken_configuration_is_indeterminate(#[case] name: &str, #[case] source: &str) {
        let fixture = Fixture::new(name, source);
        assert!(execution_location(&fixture.0, None).is_err());
    }

    #[test]
    fn initialized_backend_uses_selected_data_directory_and_rejects_corruption() {
        let fixture = Fixture::new("main.tf", "");
        let data = fixture.0.join("custom data");
        fs::create_dir(&data).unwrap();
        let state = data.join("terraform.tfstate");
        for (kind, expected) in [
            ("remote", ExecutionLocation::HcpCandidate),
            ("cloud", ExecutionLocation::HcpCandidate),
            ("s3", ExecutionLocation::Local),
            ("gcs", ExecutionLocation::Local),
        ] {
            fs::write(&state, format!(r#"{{"backend":{{"type":"{kind}"}}}}"#)).unwrap();
            assert_eq!(
                execution_location(&fixture.0, Some(OsStr::new("custom data"))).unwrap(),
                expected
            );
            assert_eq!(
                execution_location(&fixture.0, Some(data.as_os_str())).unwrap(),
                expected
            );
        }
        fs::write(&state, "broken").unwrap();
        assert!(execution_location(&fixture.0, Some(data.as_os_str())).is_err());
    }

    #[rstest]
    #[case::hcl(
        "main.tf",
        "terraform {\n  backend \"s3\" {}\n}\n",
        "main.tofu",
        "terraform {\n  cloud {}\n}\n"
    )]
    #[case::json(
        "main.tf.json",
        r#"{"terraform":{"backend":{"s3":{}}}}"#,
        "main.tofu.json",
        r#"{"terraform":{"cloud":{}}}"#
    )]
    fn opentofu_prefers_tofu_configuration_over_same_named_terraform_file(
        #[case] terraform_name: &str,
        #[case] terraform_source: &str,
        #[case] tofu_name: &str,
        #[case] tofu_source: &str,
    ) {
        let fixture = Fixture::new(terraform_name, terraform_source);
        fs::write(fixture.0.join(tofu_name), tofu_source).unwrap();

        assert_eq!(
            execution_location_for_tool(&fixture.0, Tool::OpenTofu, None).unwrap(),
            ExecutionLocation::HcpCandidate
        );
    }

    #[test]
    fn same_named_terraform_file_is_excluded_only_by_its_exact_tofu_counterpart() {
        struct SelectionCase {
            name: &'static str,
            tool: Tool,
            files: &'static [&'static str],
            expected: &'static [&'static str],
        }

        for case in [
            SelectionCase {
                name: "hcl_repeated_extension_with_counterpart",
                tool: Tool::OpenTofu,
                files: &["a.tf.tf", "a.tf.tofu"],
                expected: &["a.tf.tofu"],
            },
            SelectionCase {
                name: "hcl_repeated_extension_with_unrelated_tofu",
                tool: Tool::OpenTofu,
                files: &["a.tf.tf", "a.tofu"],
                expected: &["a.tf.tf", "a.tofu"],
            },
            SelectionCase {
                name: "hcl_repeated_extension_without_counterpart",
                tool: Tool::OpenTofu,
                files: &["a.tf.tf"],
                expected: &["a.tf.tf"],
            },
            SelectionCase {
                name: "json_repeated_extension_with_counterpart",
                tool: Tool::OpenTofu,
                files: &["a.tf.json.tf.json", "a.tf.json.tofu.json"],
                expected: &["a.tf.json.tofu.json"],
            },
            SelectionCase {
                name: "json_repeated_extension_with_unrelated_tofu",
                tool: Tool::OpenTofu,
                files: &["a.tf.json.tf.json", "a.tofu.json"],
                expected: &["a.tf.json.tf.json", "a.tofu.json"],
            },
            SelectionCase {
                name: "json_repeated_extension_without_counterpart",
                tool: Tool::OpenTofu,
                files: &["a.tf.json.tf.json"],
                expected: &["a.tf.json.tf.json"],
            },
            SelectionCase {
                name: "terraform_ignores_tofu_files",
                tool: Tool::Terraform,
                files: &[
                    "a.tf.tf",
                    "a.tf.tofu",
                    "a.tf.json.tf.json",
                    "a.tf.json.tofu.json",
                ],
                expected: &["a.tf.json.tf.json", "a.tf.tf"],
            },
        ] {
            let fixture = Fixture::new(case.files[0], "");
            for file in &case.files[1..] {
                fs::write(fixture.0.join(file), "").unwrap();
            }

            let selected = configuration_files(&fixture.0, case.tool).unwrap();

            let selected = selected
                .iter()
                .map(|path| path.file_name().and_then(OsStr::to_str).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(selected, case.expected, "case: {}", case.name);
        }
    }

    #[rstest]
    #[case::hcl("main.tf", "terraform {\n backend \"local\" {}\n backend \"s3\" {}\n}")]
    #[case::json("main.tf.json", r#"{"terraform":{"backend":{"local":[{},{}]}}}"#)]
    #[case::cloud("main.tf.json", r#"{"terraform":{"cloud":[{},{}]}}"#)]
    #[case::conflicting("main.tf.json", r#"{"terraform":{"cloud":{},"backend":{"local":{}}}}"#)]
    fn multiple_backend_or_cloud_blocks_are_invalid(#[case] name: &str, #[case] source: &str) {
        let fixture = Fixture::new(name, source);

        assert!(execution_location(&fixture.0, None).is_err());
    }

    #[test]
    fn multiple_normal_files_cannot_define_backends() {
        let fixture = Fixture::new("main.tf", "terraform {\n backend \"local\" {}\n}");
        fs::write(
            fixture.0.join("second.tf.json"),
            r#"{"terraform":{"backend":{"local":{}}}}"#,
        )
        .unwrap();

        assert!(execution_location(&fixture.0, None).is_err());
    }

    #[rstest]
    #[case::terraform_hcl(
        Tool::Terraform,
        "override.tf",
        "terraform {\n backend \"local\" {}\n}"
    )]
    #[case::terraform_json(
        Tool::Terraform,
        "a_override.tf.json",
        r#"{"terraform":{"backend":{"local":{}}}}"#
    )]
    #[case::tofu_hcl(
        Tool::OpenTofu,
        "override.tofu",
        "terraform {\n backend \"local\" {}\n}"
    )]
    #[case::tofu_json(
        Tool::OpenTofu,
        "a_override.tofu.json",
        r#"{"terraform":{"backend":{"local":{}}}}"#
    )]
    fn override_files_replace_the_normal_backend_after_loading_normal_files(
        #[case] tool: Tool,
        #[case] name: &str,
        #[case] source: &str,
    ) {
        let fixture = Fixture::new("z.tf", "terraform {\n cloud {}\n}");
        fs::write(fixture.0.join(name), source).unwrap();

        assert_eq!(
            execution_location_for_tool(&fixture.0, tool, None).unwrap(),
            ExecutionLocation::Local
        );
    }

    #[rstest]
    #[case::cloud(r#"{"terraform":{"cloud":[]}}"#)]
    #[case::backend(r#"{"terraform":{"backend":{"local":[]}}}"#)]
    fn empty_labeled_block_arrays_do_not_declare_a_backend(#[case] source: &str) {
        let fixture = Fixture::new("main.tf.json", source);

        assert!(
            !read_configuration(&fixture.0, Tool::Terraform, None)
                .unwrap()
                .has_backend
        );
    }
}
