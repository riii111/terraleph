use std::time::Instant;

use serde_json::{Map, Value};

use crate::app::execution::{
    Diagnostic, DiagnosticPoint, DiagnosticPosition, DiagnosticSeverity, DiagnosticSource,
    EventStream, ExecutionEvent, ExecutionEventKind, ExecutionSummary, ResourceAction,
    ResourceEvent, ResourceEventKind,
};

#[derive(Default)]
pub(crate) struct TerraformEventParser {
    stdout: super::line_buffer::LineBuffer,
    stderr: super::line_buffer::LineBuffer,
}

impl TerraformEventParser {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(
        &mut self,
        stream: EventStream,
        bytes: &[u8],
        received_at: Instant,
    ) -> Vec<ExecutionEvent> {
        let mut events = Vec::new();
        self.buffer_mut(stream).push(bytes, |line| {
            if let Some(event) = Self::parse_line(stream, line, received_at) {
                events.push(event);
            }
        });
        events
    }

    pub(crate) fn finish(
        &mut self,
        stream: EventStream,
        received_at: Instant,
    ) -> Vec<ExecutionEvent> {
        let line = self.buffer_mut(stream).finish();
        if line.is_empty() {
            return Vec::new();
        }
        Self::parse_line(stream, &line, received_at)
            .into_iter()
            .collect()
    }

    const fn buffer_mut(&mut self, stream: EventStream) -> &mut super::line_buffer::LineBuffer {
        match stream {
            EventStream::Stdout => &mut self.stdout,
            EventStream::Stderr => &mut self.stderr,
        }
    }

    fn parse_line(
        stream: EventStream,
        line: &[u8],
        received_at: Instant,
    ) -> Option<ExecutionEvent> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return None;
        }

        let text = String::from_utf8_lossy(line);
        let kind = serde_json::from_str::<Value>(&text).map_or_else(
            |_| ExecutionEventKind::Diagnostic(non_json_diagnostic(stream, text.into_owned())),
            |value| parse_json_event(stream, &value),
        );
        Some(ExecutionEvent { received_at, kind })
    }
}

fn parse_json_event(stream: EventStream, value: &Value) -> ExecutionEventKind {
    let Some(object) = value.as_object() else {
        return ExecutionEventKind::Diagnostic(unknown_event_diagnostic(stream, None, None, None));
    };
    let event_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let message = object
        .get("@message")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let severity = object
        .get("@level")
        .and_then(Value::as_str)
        .map(diagnostic_severity);

    match event_type.as_deref() {
        Some("diagnostic") => parse_diagnostic(object).map_or_else(
            || {
                ExecutionEventKind::Diagnostic(unknown_event_diagnostic(
                    stream, event_type, message, severity,
                ))
            },
            ExecutionEventKind::Diagnostic,
        ),
        Some("change_summary" | "summary") => parse_summary(object, message.clone()).map_or_else(
            || {
                ExecutionEventKind::Diagnostic(unknown_event_diagnostic(
                    stream, event_type, message, severity,
                ))
            },
            ExecutionEventKind::Summary,
        ),
        Some(
            "version"
            | "log"
            | "initializing_modules_message"
            | "outputs"
            | "test_abstract"
            | "test_file"
            | "test_run"
            | "test_cleanup"
            | "test_summary"
            | "test_plan"
            | "test_state"
            | "test_interrupt"
            | "planned_action_invocation",
        ) => ExecutionEventKind::Informational {
            event_type: event_type.expect("matched event type should be present"),
            message,
        },
        Some(event_type) => resource_event_kind(event_type)
            .and_then(|kind| {
                resource_address(object).map(|address| (kind, address, resource_action(object)))
            })
            .map_or_else(
                || {
                    ExecutionEventKind::Diagnostic(unknown_event_diagnostic(
                        stream,
                        Some(event_type.to_owned()),
                        message.clone(),
                        severity,
                    ))
                },
                |(kind, address, action)| {
                    ExecutionEventKind::Resource(ResourceEvent {
                        address,
                        kind,
                        action,
                        message: message.clone(),
                    })
                },
            ),
        None => ExecutionEventKind::Diagnostic(unknown_event_diagnostic(
            stream, None, message, severity,
        )),
    }
}

fn resource_event_kind(event_type: &str) -> Option<ResourceEventKind> {
    Some(match event_type {
        "refresh_start" => ResourceEventKind::RefreshStart,
        "refresh_complete" => ResourceEventKind::RefreshComplete,
        "apply_start" => ResourceEventKind::ApplyStart,
        "apply_progress" => ResourceEventKind::ApplyProgress,
        "apply_complete" => ResourceEventKind::ApplyComplete,
        "apply_errored" => ResourceEventKind::ApplyErrored,
        "provision_start" => ResourceEventKind::ProvisionStart,
        "provision_progress" => ResourceEventKind::ProvisionProgress,
        "provision_complete" => ResourceEventKind::ProvisionComplete,
        "provision_errored" => ResourceEventKind::ProvisionErrored,
        "import_start" => ResourceEventKind::ImportStart,
        "import_complete" => ResourceEventKind::ImportComplete,
        "ephemeral_op_start" => ResourceEventKind::EphemeralStart,
        "ephemeral_op_progress" => ResourceEventKind::EphemeralProgress,
        "ephemeral_op_complete" => ResourceEventKind::EphemeralComplete,
        "ephemeral_op_errored" => ResourceEventKind::EphemeralErrored,
        "resource_drift" => ResourceEventKind::ResourceDrift,
        "planned_change" => ResourceEventKind::PlannedChange,
        _ => return None,
    })
}

fn resource_address(object: &Map<String, Value>) -> Option<String> {
    ["hook", "change", "resource"]
        .iter()
        .find_map(|field| object.get(*field))
        .and_then(|value| resource_address_from_value(value, 0))
}

fn resource_action(object: &Map<String, Value>) -> Option<ResourceAction> {
    ["hook", "change", "resource"]
        .iter()
        .find_map(|field| object.get(*field))
        .and_then(|value| resource_action_from_value(value, 0))
}

fn resource_action_from_value(value: &Value, depth: usize) -> Option<ResourceAction> {
    if depth > 2 {
        return None;
    }
    let object = value.as_object()?;
    if let Some(action) = object.get("action").and_then(Value::as_str) {
        return Some(match action {
            "create" => ResourceAction::Create,
            "read" => ResourceAction::Read,
            "update" => ResourceAction::Update,
            "delete" => ResourceAction::Delete,
            "replace" => ResourceAction::Replace,
            _ => ResourceAction::Unknown(action.to_owned()),
        });
    }
    ["resource", "hook", "change"]
        .iter()
        .find_map(|field| object.get(*field))
        .and_then(|nested| resource_action_from_value(nested, depth + 1))
}

fn resource_address_from_value(value: &Value, depth: usize) -> Option<String> {
    if depth > 2 {
        return None;
    }
    let object = value.as_object()?;
    if let Some(address) = object.get("addr").and_then(Value::as_str) {
        return Some(address.to_owned());
    }
    ["resource", "hook", "change"]
        .iter()
        .find_map(|field| object.get(*field))
        .and_then(|nested| resource_address_from_value(nested, depth + 1))
}

fn parse_summary(object: &Map<String, Value>, message: Option<String>) -> Option<ExecutionSummary> {
    let changes = object
        .get("changes")
        .or_else(|| object.get("summary"))?
        .as_object()?;
    Some(ExecutionSummary {
        adds: optional_count(changes, "add"),
        changes: optional_count(changes, "change"),
        removes: optional_count(changes, "remove"),
        operation: changes
            .get("operation")
            .and_then(Value::as_str)
            .map(str::to_owned),
        message,
    })
}

fn optional_count(object: &Map<String, Value>, field: &str) -> Option<usize> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
}

fn parse_diagnostic(object: &Map<String, Value>) -> Option<Diagnostic> {
    let diagnostic = object.get("diagnostic")?.as_object()?;
    let summary = diagnostic
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| object.get("@message").and_then(Value::as_str))
        .unwrap_or("Terraform diagnostic")
        .to_owned();
    let detail = diagnostic
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(Diagnostic {
        severity: diagnostic
            .get("severity")
            .and_then(Value::as_str)
            .or_else(|| object.get("@level").and_then(Value::as_str))
            .map_or(DiagnosticSeverity::Unknown, diagnostic_severity),
        summary,
        detail,
        address: diagnostic
            .get("address")
            .and_then(Value::as_str)
            .or_else(|| object.get("address").and_then(Value::as_str))
            .map(str::to_owned),
        position: diagnostic.get("range").and_then(parse_position),
        source: DiagnosticSource::Terraform,
    })
}

fn diagnostic_severity(severity: &str) -> DiagnosticSeverity {
    match severity {
        "error" => DiagnosticSeverity::Error,
        "warning" | "warn" => DiagnosticSeverity::Warning,
        "info" => DiagnosticSeverity::Info,
        _ => DiagnosticSeverity::Unknown,
    }
}

fn parse_position(value: &Value) -> Option<DiagnosticPosition> {
    let range = value.as_object()?;
    let filename = range.get("filename").and_then(Value::as_str)?.to_owned();
    Some(DiagnosticPosition {
        filename,
        start: parse_point(range.get("start")?)?,
        end: parse_point(range.get("end")?)?,
    })
}

fn parse_point(value: &Value) -> Option<DiagnosticPoint> {
    let point = value.as_object()?;
    Some(DiagnosticPoint {
        line: point.get("line").and_then(Value::as_u64)?,
        column: point.get("column").and_then(Value::as_u64)?,
        byte: point.get("byte").and_then(Value::as_u64),
    })
}

const fn non_json_diagnostic(stream: EventStream, text: String) -> Diagnostic {
    Diagnostic {
        severity: DiagnosticSeverity::Unknown,
        summary: text,
        detail: None,
        address: None,
        position: None,
        source: DiagnosticSource::NonJson { stream },
    }
}

fn unknown_event_diagnostic(
    stream: EventStream,
    event_type: Option<String>,
    message: Option<String>,
    severity: Option<DiagnosticSeverity>,
) -> Diagnostic {
    Diagnostic {
        severity: severity.unwrap_or(DiagnosticSeverity::Unknown),
        summary: message.unwrap_or_else(|| "Unknown Terraform event".to_owned()),
        detail: event_type
            .as_deref()
            .map(|event_type| format!("Event type: {event_type}")),
        address: None,
        position: None,
        source: DiagnosticSource::UnknownEvent { stream, event_type },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    #[test]
    fn parses_split_refresh_events_by_resource_address() {
        let mut parser = TerraformEventParser::new();
        let first = br#"{"@message":"aws_vpc.main: Refreshing state...","type":"refresh_start","hook":{"resource":{"addr":"aws_vpc.main"}}}
"#;
        let second = br#"{"type":"refresh_complete","hook":{"resource":{"addr":"aws_vpc.main"}}}
"#;

        let split = first.len() / 2;
        assert!(
            parser
                .push(EventStream::Stdout, &first[..split], Instant::now())
                .is_empty()
        );
        let first_events = parser.push(EventStream::Stdout, &first[split..], Instant::now());
        assert_eq!(first_events.len(), 1);
        assert_eq!(
            first_events[0].kind,
            ExecutionEventKind::Resource(ResourceEvent {
                address: "aws_vpc.main".to_owned(),
                kind: ResourceEventKind::RefreshStart,
                action: None,
                message: Some("aws_vpc.main: Refreshing state...".to_owned()),
            })
        );
        let events = parser.push(EventStream::Stdout, second, Instant::now());
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].kind,
            ExecutionEventKind::Resource(ResourceEvent {
                address: "aws_vpc.main".to_owned(),
                kind: ResourceEventKind::RefreshComplete,
                action: None,
                message: None,
            })
        );
    }

    #[test]
    fn preserves_hook_action_for_apply_progress() {
        let mut parser = TerraformEventParser::new();
        let events = parser.push(
            EventStream::Stdout,
            br#"{"type":"apply_complete","hook":{"resource":{"addr":"aws_vpc.main"},"action":"update"}}
"#,
            Instant::now(),
        );

        assert!(matches!(
            &events[0].kind,
            ExecutionEventKind::Resource(ResourceEvent {
                action: Some(ResourceAction::Update),
                kind: ResourceEventKind::ApplyComplete,
                ..
            })
        ));
    }

    #[test]
    fn preserves_interleaved_streams_and_waits_for_a_complete_line() {
        let mut parser = TerraformEventParser::new();
        let stdout = br#"{"type":"refresh_start","hook":{"resource":{"addr":"aws_vpc.main"}}}
"#;
        let stderr = b"provider warning";
        let first_at = Instant::now();
        let stdout_complete_at = first_at + Duration::from_millis(20);
        let stderr_finished_at = first_at + Duration::from_millis(30);

        assert!(
            parser
                .push(EventStream::Stdout, &stdout[..20], first_at)
                .is_empty()
        );
        assert!(
            parser
                .push(
                    EventStream::Stderr,
                    stderr,
                    first_at + Duration::from_millis(10)
                )
                .is_empty()
        );
        let stdout_events = parser.push(EventStream::Stdout, &stdout[20..], stdout_complete_at);
        let stderr_events = parser.finish(EventStream::Stderr, stderr_finished_at);

        assert_eq!(stdout_events.len(), 1);
        assert!(matches!(
            stdout_events[0].kind,
            ExecutionEventKind::Resource(ResourceEvent {
                kind: ResourceEventKind::RefreshStart,
                ..
            })
        ));
        assert_eq!(stdout_events[0].received_at, stdout_complete_at);
        assert_eq!(stderr_events.len(), 1);
        assert!(matches!(
            &stderr_events[0].kind,
            ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Unknown,
                source: DiagnosticSource::NonJson {
                    stream: EventStream::Stderr
                },
                summary,
                ..
            }) if summary == "provider warning"
        ));
        assert_eq!(stderr_events[0].received_at, stderr_finished_at);
    }

    #[test]
    fn preserves_events_across_crlf_utf8_chunks_and_eof() {
        let mut parser = TerraformEventParser::new();
        let timestamp = Instant::now();
        let event = json!({
            "type": "version",
            "@message": "初期化しました"
        })
        .to_string();
        let input = format!("\r\n警告\r\n{event}\r\n未完了").into_bytes();
        let mut events = Vec::new();

        for chunk in input.chunks(2) {
            events.extend(parser.push(EventStream::Stdout, chunk, timestamp));
        }
        events.extend(parser.finish(EventStream::Stdout, timestamp));

        assert_eq!(events.len(), 3);
        assert!(matches!(
            &events[0].kind,
            ExecutionEventKind::Diagnostic(diagnostic)
                if diagnostic.summary == "警告"
                    && diagnostic.source
                        == DiagnosticSource::NonJson {
                            stream: EventStream::Stdout,
                        }
        ));
        assert!(matches!(
            &events[1].kind,
            ExecutionEventKind::Informational {
                event_type,
                message: Some(message),
            } if event_type == "version" && message == "初期化しました"
        ));
        assert!(matches!(
            &events[2].kind,
            ExecutionEventKind::Diagnostic(diagnostic)
                if diagnostic.summary == "未完了"
                    && diagnostic.detail.is_none()
                    && diagnostic.source
                        == DiagnosticSource::NonJson {
                            stream: EventStream::Stdout,
                        }
        ));
    }

    #[test]
    fn parses_summary_and_diagnostic_position_without_inventing_progress_total() {
        let mut parser = TerraformEventParser::new();
        let summary = json!({
            "type": "change_summary",
            "@message": "Plan: 2 to add, 2 to change, 2 to destroy.",
            "changes": {"add": 2, "change": 2, "remove": 2, "operation": "plan"}
        })
        .to_string();
        let diagnostic = json!({
            "type": "diagnostic",
            "@level": "error",
            "diagnostic": {
                "severity": "error",
                "summary": "Invalid value",
                "detail": "The value is not valid.",
                "address": "terraform_data.api",
                "range": {
                    "filename": "main.tf",
                    "start": {"line": 4, "column": 2, "byte": 20},
                    "end": {"line": 4, "column": 8, "byte": 26}
                }
            }
        })
        .to_string();

        let summary_event = parser.push(
            EventStream::Stdout,
            format!("{summary}\n").as_bytes(),
            Instant::now(),
        );
        let diagnostic_event = parser.push(
            EventStream::Stdout,
            format!("{diagnostic}\n").as_bytes(),
            Instant::now(),
        );
        assert_eq!(
            summary_event[0].kind,
            ExecutionEventKind::Summary(ExecutionSummary {
                adds: Some(2),
                changes: Some(2),
                removes: Some(2),
                operation: Some("plan".to_owned()),
                message: Some("Plan: 2 to add, 2 to change, 2 to destroy.".to_owned()),
            })
        );
        let ExecutionEventKind::Diagnostic(diagnostic) = &diagnostic_event[0].kind else {
            panic!("expected a diagnostic event");
        };
        assert_eq!(diagnostic.summary, "Invalid value");
        assert_eq!(
            diagnostic.detail.as_deref(),
            Some("The value is not valid.")
        );
        assert_eq!(diagnostic.address.as_deref(), Some("terraform_data.api"));
        assert_eq!(
            diagnostic
                .position
                .as_ref()
                .map(|position| position.filename.as_str()),
            Some("main.tf")
        );
    }

    #[test]
    fn preserves_long_non_json_diagnostic_text() {
        let mut parser = TerraformEventParser::new();
        let long_text = "x".repeat(100_000);
        let event = json!({
            "type": "version",
            "@message": "after long diagnostic"
        })
        .to_string();
        let input = format!("{long_text}\n{event}\n").into_bytes();
        let mut events = Vec::new();
        for chunk in input.chunks(8 * 1024) {
            events.extend(parser.push(EventStream::Stderr, chunk, Instant::now()));
        }

        let ExecutionEventKind::Diagnostic(diagnostic) = &events[0].kind else {
            panic!("expected a stderr diagnostic");
        };
        assert_eq!(diagnostic.summary.len(), long_text.len());
        assert!(diagnostic.detail.is_none());
        assert!(matches!(
            &events[1].kind,
            ExecutionEventKind::Informational {
                event_type,
                message: Some(message),
            } if event_type == "version" && message == "after long diagnostic"
        ));
    }

    #[test]
    fn classifies_unusable_json_events_as_diagnostics() {
        struct DiagnosticCase {
            name: &'static str,
            input: &'static str,
            expected_summary: &'static str,
            expected_detail: Option<&'static str>,
            expected_event_type: Option<&'static str>,
        }

        for case in [
            DiagnosticCase {
                name: "unknown_type",
                input: r#"{"@message":"Future event occurred","type":"future_event"}"#,
                expected_summary: "Future event occurred",
                expected_detail: Some("Event type: future_event"),
                expected_event_type: Some("future_event"),
            },
            DiagnosticCase {
                name: "missing_type",
                input: r#"{"@message":"Event type is missing"}"#,
                expected_summary: "Event type is missing",
                expected_detail: None,
                expected_event_type: None,
            },
            DiagnosticCase {
                name: "malformed_diagnostic",
                input: r#"{"@message":"Malformed diagnostic","type":"diagnostic"}"#,
                expected_summary: "Malformed diagnostic",
                expected_detail: Some("Event type: diagnostic"),
                expected_event_type: Some("diagnostic"),
            },
        ] {
            let mut parser = TerraformEventParser::new();
            let events = parser.push(
                EventStream::Stdout,
                format!("{}\n", case.input).as_bytes(),
                Instant::now(),
            );

            let ExecutionEventKind::Diagnostic(diagnostic) = &events[0].kind else {
                panic!(
                    "case {}: expected an unusable JSON event diagnostic",
                    case.name
                );
            };
            assert_eq!(
                diagnostic.summary, case.expected_summary,
                "case: {}",
                case.name
            );
            assert_eq!(
                diagnostic.detail.as_deref(),
                case.expected_detail,
                "case: {}",
                case.name
            );
            assert_eq!(
                diagnostic.source,
                DiagnosticSource::UnknownEvent {
                    stream: EventStream::Stdout,
                    event_type: case.expected_event_type.map(str::to_owned),
                },
                "case: {}",
                case.name
            );
            assert_eq!(
                diagnostic.severity,
                DiagnosticSeverity::Unknown,
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn treats_known_non_resource_messages_as_informational() {
        let mut parser = TerraformEventParser::new();
        let events = parser.push(
            EventStream::Stdout,
            br#"{"@message":"Terraform 1.9.0","type":"version"}
"#,
            Instant::now(),
        );

        let ExecutionEventKind::Informational {
            event_type,
            message: Some(message),
        } = &events[0].kind
        else {
            panic!("expected a known informational event");
        };
        assert_eq!(event_type, "version");
        assert_eq!(message, "Terraform 1.9.0");

        let events = parser.push(
            EventStream::Stdout,
            br#"{"@level":"info","@message":"provider output","type":"log"}
{"@message":"action invoked","type":"planned_action_invocation","invocation":{"action_addr":{"addr":"action.example.main"}}}
"#,
            Instant::now(),
        );
        assert!(matches!(
            &events[0].kind,
            ExecutionEventKind::Informational { event_type, .. }
                if event_type == "log"
        ));
        assert!(matches!(
            &events[1].kind,
            ExecutionEventKind::Informational { event_type, .. }
                if event_type == "planned_action_invocation"
        ));
    }
}
