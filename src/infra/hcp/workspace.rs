use std::{env, ffi::OsStr, fs, path::Path};

use serde_json::Value;

pub(super) struct Workspace {
    pub(super) hostname: String,
    pub(super) organization: String,
    pub(super) name: String,
    pub(super) token: Option<String>,
    pub(super) tags: Value,
    pub(super) project: Option<String>,
}

impl Workspace {
    pub(super) fn api_endpoint(&self, endpoint: &str) -> Result<String, String> {
        let base = self.api_base(endpoint)?;
        let mut url = format!(
            "{base}/organizations/{}/workspaces/{}",
            encode(&self.organization),
            encode(&self.name)
        );
        if self.project.is_some() {
            url.push_str("?include=project");
        }
        Ok(url)
    }

    pub(super) fn api_base(&self, endpoint: &str) -> Result<String, String> {
        let base = if endpoint.starts_with('/') {
            format!("https://{}{endpoint}", self.hostname)
        } else {
            endpoint.to_owned()
        };
        let uri = base
            .parse::<ureq::http::Uri>()
            .map_err(|_| invalid_backend())?;
        if uri.scheme_str() != Some("https")
            || uri.authority().map(ureq::http::uri::Authority::as_str)
                != Some(self.hostname.as_str())
            || uri.query().is_some()
        {
            return Err("HCP advertised an API outside the configured HTTPS host; no credentials were sent.".to_owned());
        }
        Ok(base.trim_end_matches('/').to_owned())
    }

    pub(super) fn browser_url(&self) -> String {
        format!(
            "https://{}/app/{}/workspaces/{}",
            self.hostname,
            encode(&self.organization),
            encode(&self.name)
        )
    }

    pub(super) fn validate_selection(&self, response: &Value) -> Result<(), String> {
        if response
            .pointer("/data/attributes/name")
            .and_then(Value::as_str)
            != Some(self.name.as_str())
        {
            return Err(
                "HCP returned a different workspace; no local plan was started.".to_owned(),
            );
        }
        if let Some(project) = &self.project {
            let id = response
                .pointer("/data/relationships/project/data/id")
                .and_then(Value::as_str);
            let matches = response
                .get("included")
                .and_then(Value::as_array)
                .is_some_and(|resources| {
                    resources.iter().any(|item| {
                        item.get("type").and_then(Value::as_str) == Some("projects")
                            && item.get("id").and_then(Value::as_str) == id
                            && item.pointer("/attributes/name").and_then(Value::as_str)
                                == Some(project.as_str())
                    })
                });
            if !matches {
                return Err("The selected HCP workspace does not match the configured project; select a matching workspace and retry.".to_owned());
            }
        }
        if let Some(tags) = self.tags.as_array() {
            let actual = response
                .pointer("/data/attributes/tag-names")
                .and_then(Value::as_array);
            if !tags
                .iter()
                .all(|tag| tag.is_string() && actual.is_some_and(|actual| actual.contains(tag)))
            {
                return Err("The selected HCP workspace does not match the configured tags; select a matching workspace and retry.".to_owned());
            }
        }
        Ok(())
    }
}

pub(super) fn read(root: &Path, data_dir: Option<&OsStr>) -> Result<Workspace, String> {
    let data_dir = data_dir
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| root.join(".terraform"), |dir| root.join(dir));
    let bytes = fs::read(data_dir.join("terraform.tfstate")).map_err(|_| invalid_backend())?;
    let backend: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_backend())?;
    let selected = match env::var("TF_WORKSPACE") {
        Ok(value) if !value.is_empty() => Some(value),
        Err(env::VarError::NotUnicode(_)) => return Err(invalid_backend()),
        _ => match fs::read_to_string(data_dir.join("environment")) {
            Ok(value) if !value.trim().is_empty() => Some(value.trim().to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Ok(_) | Err(_) => return Err(invalid_backend()),
        },
    };
    resolve(&backend, selected.as_deref(), |key| {
        env::var(key).ok().filter(|value| !value.is_empty())
    })
}

fn resolve(
    metadata: &Value,
    selected: Option<&str>,
    environment: impl Fn(&str) -> Option<String>,
) -> Result<Workspace, String> {
    let kind = metadata
        .pointer("/backend/type")
        .and_then(Value::as_str)
        .ok_or_else(invalid_backend)?;
    let config = metadata
        .pointer("/backend/config")
        .ok_or_else(invalid_backend)?;
    if !matches!(kind, "remote" | "cloud") {
        return Err(invalid_backend());
    }
    let configured = |key: &str| {
        config
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let hostname = configured("hostname")
        .or_else(|| {
            (kind == "cloud")
                .then(|| environment("TF_CLOUD_HOSTNAME"))
                .flatten()
        })
        .unwrap_or_else(|| "app.terraform.io".to_owned())
        .to_ascii_lowercase();
    if !hostname
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
    {
        return Err(invalid_backend());
    }
    let organization = configured("organization")
        .or_else(|| {
            (kind == "cloud")
                .then(|| environment("TF_CLOUD_ORGANIZATION"))
                .flatten()
        })
        .ok_or_else(invalid_backend)?;
    let token = configured("token");
    let workspaces = config
        .get("workspaces")
        .and_then(|value| {
            value
                .as_array()
                .and_then(|array| array.first())
                .or(Some(value))
        })
        .unwrap_or(&Value::Null);
    let tags = decode_tags(
        workspaces
            .get("tags")
            .filter(|_| kind == "cloud")
            .unwrap_or(&Value::Null),
    )?;
    let has_tags = tags.as_array().is_some_and(|tags| !tags.is_empty())
        || tags.as_object().is_some_and(|tags| !tags.is_empty());
    let project = workspaces
        .get("project")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            (kind == "cloud")
                .then(|| environment("TF_CLOUD_PROJECT"))
                .flatten()
        });
    let project = project.filter(|_| has_tags);
    let name = workspace_name(kind, workspaces, selected)?;
    Ok(Workspace {
        hostname,
        organization,
        name,
        token,
        tags,
        project,
    })
}

// Terraform's dynamic cty attributes retain both the concrete type and value in backend metadata.
fn decode_tags(encoded: &Value) -> Result<Value, String> {
    if encoded.is_null() {
        return Ok(Value::Null);
    }
    let invalid = || {
        "The initialized HCP workspace tags have invalid type information; run init again before planning.".to_owned()
    };
    let tags = encoded.get("value").ok_or_else(invalid)?;
    let descriptor = encoded
        .get("type")
        .and_then(Value::as_array)
        .filter(|descriptor| descriptor.len() == 2)
        .ok_or_else(invalid)?;
    let valid = match descriptor[0].as_str() {
        Some("tuple") => descriptor[1].as_array().is_some_and(|types| {
            types.iter().all(|ty| ty == "string")
                && (tags.is_null()
                    || tags.as_array().is_some_and(|values| {
                        values.len() == types.len() && values.iter().all(Value::is_string)
                    }))
        }),
        Some("list" | "set") => {
            descriptor[1] == "string"
                && (tags.is_null()
                    || tags
                        .as_array()
                        .is_some_and(|values| values.iter().all(Value::is_string)))
        }
        Some("object") => descriptor[1].as_object().is_some_and(|types| {
            types.values().all(|ty| ty == "string")
                && (tags.is_null()
                    || tags.as_object().is_some_and(|values| {
                        values.len() == types.len()
                            && types
                                .keys()
                                .all(|key| values.get(key).is_some_and(Value::is_string))
                    }))
        }),
        Some("map") => {
            descriptor[1] == "string"
                && (tags.is_null()
                    || tags
                        .as_object()
                        .is_some_and(|values| values.values().all(Value::is_string)))
        }
        _ => false,
    };
    if valid {
        Ok(tags.clone())
    } else {
        Err(invalid())
    }
}

fn workspace_name(
    kind: &str,
    workspaces: &Value,
    selected: Option<&str>,
) -> Result<String, String> {
    let fixed = workspaces
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let prefix = workspaces
        .get("prefix")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let name = if let Some(name) = fixed {
        if let Some(selected) = selected {
            let expected = if kind == "remote" { "default" } else { name };
            if selected != expected {
                return Err("The selected Terraform workspace does not match the initialized HCP workspace; run init/workspace select and retry.".to_owned());
            }
        }
        name.to_owned()
    } else if kind == "remote" {
        let selected = selected.unwrap_or("default");
        let prefix = prefix.ok_or_else(invalid_backend)?;
        if selected == "default" {
            return Err("Select a non-default Terraform workspace for this remote backend prefix, then retry.".to_owned());
        }
        format!("{prefix}{selected}")
    } else {
        selected
            .ok_or_else(|| {
                "The HCP workspace has not been selected; run terraform workspace select and retry."
                    .to_owned()
            })?
            .to_owned()
    };
    Ok(name)
}

fn invalid_backend() -> String {
    "Could not identify the initialized HCP workspace; run init and select the workspace, then retry.".to_owned()
}

pub(super) fn encode(value: &str) -> String {
    use std::fmt::Write;

    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::super::{ExecutionCheckError, check_with};
    use super::*;
    use crate::infra::CancellationToken;
    use rstest::rstest;
    use serde_json::json;

    #[test]
    fn resolves_names_prefixes_and_selected_cloud_workspaces() {
        for (kind, workspaces, selected, expected) in [
            ("remote", json!({"name":"stg"}), Some("default"), "stg"),
            ("remote", json!({"prefix":"team-"}), Some("stg"), "team-stg"),
            ("cloud", json!({"name":"stg"}), Some("stg"), "stg"),
            (
                "cloud",
                json!({"tags":{"type":["tuple",["string"]],"value":["team"]}}),
                Some("stg"),
                "stg",
            ),
        ] {
            let metadata = json!({"backend":{"type":kind,"config":{"organization":"team","workspaces":[workspaces]}}});

            let workspace = resolve(&metadata, selected, |_| None).unwrap();

            assert_eq!(workspace.name, expected);
        }
    }

    #[test]
    fn refuses_a_selected_workspace_that_differs_from_the_fixed_mapping() {
        let metadata = json!({"backend":{"type":"cloud","config":{"organization":"team","workspaces":{"name":"stg"}}}});

        assert!(resolve(&metadata, Some("prod"), |_| None).is_err());
    }

    #[test]
    fn refuses_cross_host_and_insecure_service_endpoints() {
        let workspace = Workspace {
            hostname: "app.terraform.io".to_owned(),
            organization: "team".to_owned(),
            name: "stg".to_owned(),
            token: None,
            tags: Value::Null,
            project: None,
        };

        assert!(
            workspace
                .api_endpoint("https://other.example/api/v2/")
                .is_err()
        );
        assert!(
            workspace
                .api_endpoint("http://app.terraform.io/api/v2/")
                .is_err()
        );
        assert_eq!(
            workspace.api_endpoint("/api/v2/").unwrap(),
            "https://app.terraform.io/api/v2/organizations/team/workspaces/stg"
        );
    }

    #[test]
    fn cloud_environment_fills_only_omitted_settings_and_remote_never_uses_it() {
        let environment = |key: &str| match key {
            "TF_CLOUD_HOSTNAME" => Some("enterprise.example".to_owned()),
            "TF_CLOUD_ORGANIZATION" => Some("team".to_owned()),
            "TF_CLOUD_PROJECT" => Some("platform".to_owned()),
            _ => None,
        };
        let config = json!({"backend":{"type":"cloud","config":{"hostname":"", "organization":"", "workspaces":{"tags":{"type":["tuple",["string"]],"value":["team"]}}}}});

        let cloud = resolve(&config, Some("stg"), environment).unwrap();

        assert_eq!(cloud.hostname, "enterprise.example");
        assert_eq!(cloud.organization, "team");
        assert_eq!(cloud.project.as_deref(), Some("platform"));
        let explicit = json!({"backend":{"type":"cloud","config":{"hostname":"explicit.example","organization":"explicit", "workspaces":{"tags":{"type":["tuple",["string"]],"value":["team"]},"project":"explicit-project"}}}});
        let cloud = resolve(&explicit, Some("stg"), environment).unwrap();
        assert_eq!(
            (
                cloud.hostname.as_str(),
                cloud.organization.as_str(),
                cloud.project.as_deref()
            ),
            ("explicit.example", "explicit", Some("explicit-project"))
        );
        let remote = json!({"backend":{"type":"remote","config":{"workspaces":{"name":"stg"}}}});
        assert!(resolve(&remote, Some("default"), environment).is_err());
    }

    #[test]
    fn prefix_mapping_requires_a_nondefault_workspace() {
        let metadata = json!({"backend":{"type":"remote","config":{"organization":"team","workspaces":{"prefix":"team-"}}}});

        assert!(resolve(&metadata, None, |_| None).is_err());
        assert!(resolve(&metadata, Some("default"), |_| None).is_err());
    }

    #[test]
    fn cloud_selection_must_match_tags_and_project() {
        let metadata = json!({"backend":{"type":"cloud","config":{"organization":"team","workspaces":{"tags":{"type":["tuple",["string"]],"value":["team"]},"project":"platform"}}}});
        let workspace = resolve(&metadata, Some("stg"), |_| None).unwrap();
        let response = json!({
            "data":{"attributes":{"name":"stg","tag-names":["team"]},"relationships":{"project":{"data":{"id":"prj-test"}}}},
            "included":[{"type":"projects","id":"prj-test","attributes":{"name":"platform"}}],
        });

        assert!(workspace.validate_selection(&response).is_ok());
        let mut wrong_tags = response.clone();
        wrong_tags["data"]["attributes"]["tag-names"] = json!(["other"]);
        assert!(workspace.validate_selection(&wrong_tags).is_err());
        let mut wrong_project = response;
        wrong_project["included"][0]["attributes"]["name"] = json!("other");
        assert!(workspace.validate_selection(&wrong_project).is_err());
    }

    #[test]
    fn typed_backend_tags_select_matching_workspaces_before_checking_their_execution_mode() {
        for (case, encoded) in [
            (
                "tuple",
                json!({"type":["tuple",["string"]],"value":["team"]}),
            ),
            (
                "object",
                json!({"type":["object",{"team":"string"}],"value":{"team":"platform"}}),
            ),
        ] {
            let metadata = json!({"backend":{"type":"cloud","config":{"organization":"team","workspaces":{"tags":encoded}}}});
            let target = resolve(&metadata, Some("stg"), |_| None).unwrap();
            for mode in ["local", "remote", "agent"] {
                for matching in [true, false] {
                    let result = check_with(
                        &target,
                        "synthetic-token",
                        &CancellationToken::default(),
                        &mut |url, token| {
                            Ok(if token.is_none() {
                                json!({"tfe.v2":"/api/v2/"})
                            } else if url.ends_with("effective-tag-bindings") {
                                let value = if matching { "platform" } else { "other" };
                                json!({"data":[{"attributes":{"key":"team","value":value}}]})
                            } else {
                                let tag = if matching { "team" } else { "other" };
                                json!({"data":{"id":"ws-synthetic","attributes":{"name":"stg","tag-names":[tag],"execution-mode":mode}}})
                            })
                        },
                    );

                    match (matching, mode, result) {
                        (true, "local", Ok(()))
                        | (true, "remote" | "agent", Err(ExecutionCheckError::Remote(_)))
                        | (false, _, Err(ExecutionCheckError::Failed(_))) => {}
                        _ => panic!("unexpected decision: {case}, {mode}, matching={matching}"),
                    }
                }
            }
        }
    }

    #[rstest]
    #[case::list(json!({"type":["list","string"],"value":["team"]}))]
    #[case::set(json!({"type":["set","string"],"value":["team"]}))]
    fn homogeneous_collection_types_decode_as_tag_names(#[case] encoded: Value) {
        assert_eq!(decode_tags(&encoded).unwrap(), json!(["team"]));
    }

    #[test]
    fn homogeneous_map_type_decodes_as_tag_bindings() {
        assert_eq!(
            decode_tags(&json!({"type":["map","string"],"value":{"team":"platform"}})).unwrap(),
            json!({"team":"platform"})
        );
    }

    #[rstest]
    #[case::missing_type(json!({"value":["team"]}))]
    #[case::missing_value(json!({"type":["list","string"]}))]
    #[case::invalid_descriptor(json!({"type":"list","value":["team"]}))]
    #[case::non_string_type(json!({"type":["list","number"],"value":["team"]}))]
    #[case::non_string_value(json!({"type":["list","string"],"value":[1]}))]
    #[case::wrong_tuple_size(json!({"type":["tuple",["string","string"]],"value":["team"]}))]
    #[case::wrong_object_key(json!({"type":["object",{"team":"string"}],"value":{"other":"platform"}}))]
    fn invalid_tag_metadata_stops_workspace_resolution(#[case] tags: Value) {
        let metadata = json!({"backend":{"type":"cloud","config":{"organization":"team","workspaces":{"tags":tags}}}});

        assert!(resolve(&metadata, Some("stg"), |_| None).is_err());
    }
}
