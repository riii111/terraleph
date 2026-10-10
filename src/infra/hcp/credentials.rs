use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::app::execution::Tool;

use hcl::Body;
use serde_json::Value;

pub(super) fn read(tool: Tool, hostname: &str) -> Result<String, String> {
    read_with(tool, hostname, |key| env::var_os(key))
}

fn read_with(
    tool: Tool,
    hostname: &str,
    environment: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<String, String> {
    let variable = format!("TF_TOKEN_{}", hostname.replace('.', "_").replace('-', "__"));
    if let Some(token) = environment(&variable)
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
    {
        return Ok(token);
    }
    if let Some(config) =
        environment("TF_CLI_CONFIG_FILE").or_else(|| environment("TERRAFORM_CONFIG"))
    {
        return token_from_file(Path::new(&config), hostname)?
            .ok_or_else(|| missing_credentials(tool, hostname));
    }
    let home = environment(if cfg!(windows) { "APPDATA" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| missing_credentials(tool, hostname))?;
    let legacy = home.join(if cfg!(windows) {
        "terraform.rc"
    } else {
        ".terraformrc"
    });
    let tofu = home.join(if cfg!(windows) { "tofu.rc" } else { ".tofurc" });
    let xdg = (tool == Tool::OpenTofu && !cfg!(windows))
        .then(|| {
            environment("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(|path| PathBuf::from(path).join("opentofu"))
        })
        .flatten();
    let config = if tool == Tool::OpenTofu {
        if tofu.exists() {
            tofu
        } else if legacy.exists() {
            legacy
        } else {
            xdg.as_ref()
                .map_or(tofu, |directory| directory.join("tofurc"))
        }
    } else {
        legacy
    };
    let mut token = token_from_file(&config, hostname)?;
    let mut directory = home.join(if cfg!(windows) {
        "terraform.d"
    } else {
        ".terraform.d"
    });
    if !directory.exists()
        && let Some(xdg) = xdg
    {
        directory = xdg;
    }
    let mut files = match fs::read_dir(directory) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "Could not read the Terraform CLI configuration directory.".to_owned())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            return Err("Could not read the Terraform CLI configuration directory.".to_owned());
        }
    };
    files.retain(|path| {
        path.extension().is_some_and(|ext| ext == "tfrc")
            || (path.extension().is_some_and(|ext| ext == "json")
                && path.file_stem().is_some_and(|stem| {
                    Path::new(stem).extension().is_some_and(|ext| ext == "tfrc")
                }))
    });
    files.sort();
    for path in files {
        if let Some(found) = token_from_file(&path, hostname)? {
            token = Some(found);
        }
    }
    token.ok_or_else(|| missing_credentials(tool, hostname))
}

fn token_from_file(path: &Path, hostname: &str) -> Result<Option<String>, String> {
    let source = match fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(
                "Could not read Terraform CLI credentials; check file permissions.".to_owned(),
            );
        }
    };
    token_from_source(
        &source,
        path.extension().is_some_and(|value| value == "json"),
        hostname,
    )
}

fn token_from_source(source: &str, json: bool, hostname: &str) -> Result<Option<String>, String> {
    let invalid = || "Could not parse Terraform CLI credentials.".to_owned();
    if json {
        let value: Value = serde_json::from_str(source).map_err(|_| invalid())?;
        return Ok(value
            .get("credentials")
            .and_then(|credentials| credentials.get(hostname))
            .and_then(|credentials| credentials.get("token"))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned));
    }
    let body: Body = hcl::from_str(source).map_err(|_| invalid())?;
    Ok(body
        .blocks()
        .filter(|block| block.identifier() == "credentials")
        .find(|block| {
            block
                .labels()
                .first()
                .is_some_and(|label| label.as_str() == hostname)
        })
        .and_then(|block| {
            block
                .body
                .attributes()
                .find(|attribute| attribute.key() == "token")
        })
        .and_then(|attribute| match attribute.expr() {
            hcl::Expression::String(token) if !token.is_empty() => Some(token.clone()),
            _ => None,
        }))
}

fn missing_credentials(tool: Tool, hostname: &str) -> String {
    let executable = tool.display_name();
    format!(
        "No {executable} CLI token is available for {hostname}; run {executable} login or set the host's TF_TOKEN variable, then retry. Credential helpers are not supported by the execution-mode check."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::json(r#"{"credentials":{"app.terraform.io":{"token":"synthetic"}}}"#, true)]
    #[case::hcl("credentials \"app.terraform.io\" {\n token = \"synthetic\"\n}", false)]
    fn reads_the_token_for_the_configured_host(#[case] source: &str, #[case] json: bool) {
        assert_eq!(
            token_from_source(source, json, "app.terraform.io")
                .unwrap()
                .as_deref(),
            Some("synthetic")
        );
        assert_eq!(
            token_from_source(source, json, "other.example").unwrap(),
            None
        );
    }

    #[test]
    fn malformed_credentials_do_not_echo_their_contents() {
        let error = token_from_source("{synthetic-secret", true, "app.terraform.io").unwrap_err();

        assert!(!error.contains("synthetic-secret"));
    }

    #[test]
    fn login_credentials_override_default_config_but_an_explicit_config_skips_them() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(if cfg!(windows) {
            "terraform.rc"
        } else {
            ".terraformrc"
        });
        fs::write(
            &config,
            "credentials \"app.terraform.io\" {\n token = \"old\"\n}",
        )
        .unwrap();
        let directory = home.path().join(if cfg!(windows) {
            "terraform.d"
        } else {
            ".terraform.d"
        });
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("credentials.tfrc.json"),
            r#"{"credentials":{"app.terraform.io":{"token":"login"}}}"#,
        )
        .unwrap();
        let environment = |key: &str| {
            matches!(key, "HOME" | "APPDATA").then(|| home.path().as_os_str().to_owned())
        };

        assert_eq!(
            read_with(Tool::Terraform, "app.terraform.io", environment).unwrap(),
            "login"
        );
        let explicit = |key: &str| {
            if key == "TF_CLI_CONFIG_FILE" {
                Some(config.as_os_str().to_owned())
            } else {
                environment(key)
            }
        };
        assert_eq!(
            read_with(Tool::Terraform, "app.terraform.io", explicit).unwrap(),
            "old"
        );
        let token = |key: &str| {
            if key == "TF_TOKEN_app_terraform_io" {
                Some("environment".into())
            } else {
                explicit(key)
            }
        };
        assert_eq!(
            read_with(Tool::Terraform, "app.terraform.io", token).unwrap(),
            "environment"
        );
    }

    #[test]
    fn opentofu_prefers_its_own_config_over_the_legacy_file() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join(if cfg!(windows) {
                "terraform.rc"
            } else {
                ".terraformrc"
            }),
            "credentials \"app.terraform.io\" {\n token = \"legacy\"\n}",
        )
        .unwrap();
        fs::write(
            home.path()
                .join(if cfg!(windows) { "tofu.rc" } else { ".tofurc" }),
            "credentials \"app.terraform.io\" {\n token = \"tofu\"\n}",
        )
        .unwrap();

        let token = read_with(Tool::OpenTofu, "app.terraform.io", |key| {
            matches!(key, "HOME" | "APPDATA").then(|| home.path().as_os_str().to_owned())
        })
        .unwrap();

        assert_eq!(token, "tofu");
    }

    #[cfg(not(windows))]
    #[test]
    fn opentofu_selects_xdg_config_and_credentials_independently_with_home_precedence() {
        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let directory = xdg.path().join("opentofu");
        fs::create_dir(&directory).unwrap();
        let config = directory.join("tofurc");
        fs::write(
            &config,
            "credentials \"app.terraform.io\" {\n token = \"xdg-config\"\n}",
        )
        .unwrap();
        fs::write(
            directory.join("credentials.tfrc.json"),
            r#"{"credentials":{"app.terraform.io":{"token":"xdg-login"}}}"#,
        )
        .unwrap();
        let environment = |key: &str| match key {
            "HOME" => Some(home.path().as_os_str().to_owned()),
            "XDG_CONFIG_HOME" => Some(xdg.path().as_os_str().to_owned()),
            _ => None,
        };

        assert_eq!(
            read_with(Tool::OpenTofu, "app.terraform.io", environment).unwrap(),
            "xdg-login"
        );
        let explicit = |key: &str| {
            if key == "TF_CLI_CONFIG_FILE" {
                Some(config.as_os_str().to_owned())
            } else {
                environment(key)
            }
        };
        assert_eq!(
            read_with(Tool::OpenTofu, "app.terraform.io", explicit).unwrap(),
            "xdg-config"
        );
        fs::create_dir(home.path().join(".terraform.d")).unwrap();
        assert_eq!(
            read_with(Tool::OpenTofu, "app.terraform.io", environment).unwrap(),
            "xdg-config"
        );
        fs::write(
            home.path().join(".terraformrc"),
            "credentials \"app.terraform.io\" {\n token = \"legacy-config\"\n}",
        )
        .unwrap();
        assert_eq!(
            read_with(Tool::OpenTofu, "app.terraform.io", environment).unwrap(),
            "legacy-config"
        );
        fs::write(
            home.path().join(".tofurc"),
            "credentials \"app.terraform.io\" {\n token = \"home-config\"\n}",
        )
        .unwrap();
        assert_eq!(
            read_with(Tool::OpenTofu, "app.terraform.io", environment).unwrap(),
            "home-config"
        );
    }
}
