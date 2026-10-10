mod credentials;
mod workspace;

use std::{ffi::OsStr, path::Path, time::Duration};

use serde_json::Value;
use ureq::Agent;

use crate::{app::execution::Tool, infra::CancellationToken};

pub(crate) enum ExecutionCheckError {
    Remote(String),
    Failed(String),
}

pub(crate) fn require_local(
    tool: Tool,
    root: &Path,
    data_dir: Option<&OsStr>,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionCheckError> {
    let target = workspace::read(root, data_dir).map_err(ExecutionCheckError::Failed)?;
    let token = match &target.token {
        Some(token) => token.clone(),
        None => credentials::read(tool, &target.hostname).map_err(ExecutionCheckError::Failed)?,
    };
    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let result = check(&agent, &target, &token, cancellation);
    if cancellation.is_cancelled() {
        return Err(ExecutionCheckError::Failed(
            "HCP execution check was interrupted.".to_owned(),
        ));
    }
    result
}

fn check(
    agent: &Agent,
    target: &workspace::Workspace,
    token: &str,
    cancellation: &CancellationToken,
) -> Result<(), ExecutionCheckError> {
    check_with(target, token, cancellation, &mut |url, token| {
        if cancellation.is_cancelled() {
            return Err("HCP execution check was interrupted.".to_owned());
        }
        request_json(agent, url, token)
    })
}

fn check_with(
    target: &workspace::Workspace,
    token: &str,
    cancellation: &CancellationToken,
    get: &mut impl FnMut(&str, Option<&str>) -> Result<Value, String>,
) -> Result<(), ExecutionCheckError> {
    if cancellation.is_cancelled() {
        return Err(ExecutionCheckError::Failed(
            "HCP execution check was interrupted.".to_owned(),
        ));
    }
    let discovery = get(
        &format!("https://{}/.well-known/terraform.json", target.hostname),
        None,
    )
    .map_err(ExecutionCheckError::Failed)?;
    let endpoint = discovery
        .get("tfe.v2")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ExecutionCheckError::Failed(
                "The configured host does not advertise the HCP workspace API.".to_owned(),
            )
        })?;
    let api_base = target
        .api_base(endpoint)
        .map_err(ExecutionCheckError::Failed)?;
    let endpoint = target
        .api_endpoint(endpoint)
        .map_err(ExecutionCheckError::Failed)?;
    let response = get(&endpoint, Some(token)).map_err(ExecutionCheckError::Failed)?;
    target
        .validate_selection(&response)
        .map_err(ExecutionCheckError::Failed)?;
    if let Some(tags) = target.tags.as_object().filter(|tags| !tags.is_empty()) {
        let id = response
            .pointer("/data/id")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ExecutionCheckError::Failed("HCP did not return a workspace identifier.".to_owned())
            })?;
        let bindings = get(
            &format!(
                "{api_base}/workspaces/{}/effective-tag-bindings",
                workspace::encode(id)
            ),
            Some(token),
        )
        .map_err(ExecutionCheckError::Failed)?;
        let actual = bindings.get("data").and_then(Value::as_array);
        if !tags.iter().all(|(key, value)| {
            value.is_string()
                && actual.is_some_and(|items| {
                    items.iter().any(|item| {
                        item.pointer("/attributes/key").and_then(Value::as_str)
                            == Some(key.as_str())
                            && item.pointer("/attributes/value") == Some(value)
                    })
                })
        }) {
            return Err(ExecutionCheckError::Failed("The selected HCP workspace does not match the configured tag bindings; select a matching workspace and retry.".to_owned()));
        }
    }
    match execution_mode(&response).map_err(ExecutionCheckError::Failed)? {
        "local" => Ok(()),
        mode => Err(ExecutionCheckError::Remote(format!(
            "This workspace uses HCP {mode} execution. Run and review its plan in HCP Terraform ({}) instead of creating a local saved plan.",
            target.browser_url()
        ))),
    }
}

fn request_json(agent: &Agent, url: &str, token: Option<&str>) -> Result<Value, String> {
    let mut request = agent.get(url).header("Accept", "application/vnd.api+json");
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    // Do not expose request errors or response bodies: a backend token or service may put
    // credentials in them. Status codes still distinguish authentication and permissions.
    let mut response = request.call().map_err(|_| {
        "Could not contact HCP Terraform; check connectivity and TLS trust, then retry.".to_owned()
    })?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!(
            "HCP execution mode could not be checked (HTTP {status}); check the selected workspace and credentials, then retry."
        ));
    }
    response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_json()
        .map_err(|_| "HCP returned an invalid workspace API response.".to_owned())
}

fn execution_mode(response: &Value) -> Result<&str, String> {
    match response
        .pointer("/data/attributes/execution-mode")
        .and_then(Value::as_str)
    {
        Some(mode @ ("local" | "remote" | "agent")) => Ok(mode),
        _ => Err(
            "HCP did not return a recognized execution mode; no local plan was started.".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use serde_json::json;

    #[rstest]
    #[case::missing(json!({"data":{"attributes":{}}}))]
    #[case::unknown(json!({"data":{"attributes":{"execution-mode":"future"}}}))]
    #[case::null(json!({"data":{"attributes":{"execution-mode":null}}}))]
    fn cannot_assume_local_execution(#[case] response: Value) {
        assert!(execution_mode(&response).is_err());
    }

    #[test]
    fn workspace_modes_allow_only_local_plans_and_remote_modes_include_hcp_guidance() {
        let target = workspace::Workspace {
            hostname: "app.terraform.io".to_owned(),
            organization: "team".to_owned(),
            name: "stg".to_owned(),
            token: None,
            tags: Value::Null,
            project: None,
        };
        for mode in ["local", "remote", "agent"] {
            let mut requests = Vec::new();

            let result = check_with(
                &target,
                "synthetic-token",
                &CancellationToken::default(),
                &mut |url, token| {
                    requests.push((url.to_owned(), token.map(str::to_owned)));
                    Ok(if token.is_none() {
                        json!({"tfe.v2":"/api/v2/"})
                    } else {
                        json!({"data":{"attributes":{"name":"stg","execution-mode":mode}}})
                    })
                },
            );

            assert_eq!(
                requests,
                [
                    (
                        "https://app.terraform.io/.well-known/terraform.json".to_owned(),
                        None
                    ),
                    (
                        "https://app.terraform.io/api/v2/organizations/team/workspaces/stg"
                            .to_owned(),
                        Some("synthetic-token".to_owned())
                    ),
                ]
            );
            if mode == "local" {
                assert!(result.is_ok());
            } else {
                let Err(ExecutionCheckError::Remote(message)) = result else {
                    panic!("expected HCP exclusion")
                };
                assert!(
                    message.contains(mode)
                        && message.contains("https://app.terraform.io/app/team/workspaces/stg")
                );
            }
        }
    }

    #[test]
    fn unsafe_discovery_never_receives_a_token() {
        let target = workspace::Workspace {
            hostname: "app.terraform.io".to_owned(),
            organization: "team".to_owned(),
            name: "stg".to_owned(),
            token: None,
            tags: Value::Null,
            project: None,
        };
        let mut requests = 0;

        let result = check_with(
            &target,
            "synthetic-token",
            &CancellationToken::default(),
            &mut |_, token| {
                requests += 1;
                assert!(token.is_none());
                Ok(json!({"tfe.v2":"https://other.example/api/v2/"}))
            },
        );

        assert!(matches!(result, Err(ExecutionCheckError::Failed(_))));
        assert_eq!(requests, 1);
    }

    #[test]
    fn api_errors_expose_status_but_never_response_bodies_or_tokens() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
            thread,
        };

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            assert!(
                String::from_utf8(request)
                    .unwrap()
                    .contains("Bearer synthetic-token")
            );
            socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 16\r\nConnection: close\r\n\r\nsynthetic-secret").unwrap();
        });
        let agent: Agent = Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .into();

        let error = request_json(
            &agent,
            &format!("http://{address}/workspace"),
            Some("synthetic-token"),
        )
        .unwrap_err();

        server.join().unwrap();
        assert!(error.contains("HTTP 403"));
        assert!(!error.contains("synthetic-secret") && !error.contains("synthetic-token"));
    }
}
