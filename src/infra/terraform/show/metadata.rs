use serde_json::{Map, Value};

use crate::app::execution::SensitiveValue;
use crate::app::review::PlanMetadata;

pub(super) fn metadata_from_document(
    root: &Map<String, Value>,
    detailed_exit_has_changes: bool,
) -> PlanMetadata {
    let errored = root.get("errored").and_then(Value::as_bool) == Some(true);
    let applyable = !errored
        && root
            .get("applyable")
            .and_then(Value::as_bool)
            .unwrap_or(detailed_exit_has_changes);

    let tool_version = root
        .get("terraform_version")
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_owned);

    PlanMetadata::new(applyable)
        .with_tool_version(tool_version)
        .with_sensitive_values(sensitive_values(root))
}

fn sensitive_values(root: &Map<String, Value>) -> Vec<SensitiveValue> {
    let mut values = Vec::new();
    for resource in root
        .get("resource_changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
    {
        let Some(change) = resource.get("change").and_then(Value::as_object) else {
            continue;
        };
        for (value_field, mask_field) in
            [("before", "before_sensitive"), ("after", "after_sensitive")]
        {
            if let (Some(value), Some(mask)) = (change.get(value_field), change.get(mask_field)) {
                collect_masked_values(value, mask, &mut values);
            }
        }
    }
    if let Some(outputs) = root.get("output_changes").and_then(Value::as_object) {
        for output in outputs.values().filter_map(Value::as_object) {
            let change = output
                .get("change")
                .and_then(Value::as_object)
                .unwrap_or(output);
            if change.get("sensitive") == Some(&Value::Bool(true))
                && let Some(value) = change.get("value").or_else(|| change.get("after"))
            {
                collect_scalar_values(value, &mut values);
            }
            for (value_field, mask_field) in
                [("before", "before_sensitive"), ("after", "after_sensitive")]
            {
                if let (Some(value), Some(mask)) = (change.get(value_field), change.get(mask_field))
                {
                    collect_masked_values(value, mask, &mut values);
                }
            }
        }
    }
    values.retain(|value| match value {
        SensitiveValue::Text(value) | SensitiveValue::Number(value) => !value.is_empty(),
        SensitiveValue::Bool(_) => true,
    });
    values.sort();
    values.dedup();
    values
}

fn collect_masked_values(value: &Value, mask: &Value, values: &mut Vec<SensitiveValue>) {
    match mask {
        Value::Bool(true) => collect_scalar_values(value, values),
        Value::Object(mask) => {
            let Some(value) = value.as_object() else {
                return;
            };
            for (key, mask) in mask {
                if let Some(value) = value.get(key) {
                    collect_masked_values(value, mask, values);
                }
            }
        }
        Value::Array(mask) => {
            let Some(value) = value.as_array() else {
                return;
            };
            for (value, mask) in value.iter().zip(mask) {
                collect_masked_values(value, mask, values);
            }
        }
        _ => {}
    }
}

fn collect_scalar_values(value: &Value, values: &mut Vec<SensitiveValue>) {
    match value {
        Value::String(value) => values.push(SensitiveValue::Text(value.clone())),
        Value::Number(value) => values.push(SensitiveValue::Number(value.to_string())),
        Value::Bool(value) => values.push(SensitiveValue::Bool(*value)),
        Value::Array(values_array) => {
            for value in values_array {
                collect_scalar_values(value, values);
            }
        }
        Value::Object(values_object) => {
            for value in values_object.values() {
                collect_scalar_values(value, values);
            }
        }
        Value::Null => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parse_metadata(document: &Value, detailed_exit_has_changes: bool) -> PlanMetadata {
        let root = document.as_object().expect("plan JSON root is an object");
        metadata_from_document(root, detailed_exit_has_changes)
    }

    #[test]
    fn applyability_follows_errored_then_applyable_then_detailed_exit_code() {
        struct ApplyableCase {
            name: &'static str,
            document: Value,
            detailed_exit_has_changes: bool,
            expected: bool,
        }

        for case in [
            ApplyableCase {
                name: "errored_overrides_applyable",
                document: json!({"format_version": "1.0", "errored": true, "applyable": true}),
                detailed_exit_has_changes: true,
                expected: false,
            },
            ApplyableCase {
                name: "applyable_false_overrides_exit_code",
                document: json!({"format_version": "1.0", "applyable": false}),
                detailed_exit_has_changes: true,
                expected: false,
            },
            ApplyableCase {
                name: "applyable_true_overrides_exit_code",
                document: json!({"format_version": "1.0", "applyable": true}),
                detailed_exit_has_changes: false,
                expected: true,
            },
            ApplyableCase {
                name: "missing_applyable_uses_exit_code_changes",
                document: json!({"format_version": "1.0"}),
                detailed_exit_has_changes: true,
                expected: true,
            },
            ApplyableCase {
                name: "missing_applyable_uses_exit_code_no_changes",
                document: json!({"format_version": "1.0"}),
                detailed_exit_has_changes: false,
                expected: false,
            },
        ] {
            let metadata = parse_metadata(&case.document, case.detailed_exit_has_changes);

            assert_eq!(metadata.applyable(), case.expected, "case: {}", case.name);
        }
    }

    #[test]
    fn reads_the_tool_version_only_when_it_is_a_non_empty_string() {
        struct VersionCase {
            name: &'static str,
            document: Value,
            expected: Option<&'static str>,
        }

        for case in [
            VersionCase {
                name: "string",
                document: json!({"format_version": "1.0", "terraform_version": "1.9.0"}),
                expected: Some("1.9.0"),
            },
            VersionCase {
                name: "missing",
                document: json!({"format_version": "1.0"}),
                expected: None,
            },
            VersionCase {
                name: "empty",
                document: json!({"format_version": "1.0", "terraform_version": ""}),
                expected: None,
            },
            VersionCase {
                name: "not_a_string",
                document: json!({"format_version": "1.0", "terraform_version": 1}),
                expected: None,
            },
        ] {
            let metadata = parse_metadata(&case.document, true);

            assert_eq!(
                metadata.tool_version(),
                case.expected,
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn extracts_sensitive_scalars_without_debug_leaks() {
        let document = json!({
            "format_version": "1.0",
            "applyable": true,
            "resource_changes": [
                {
                    "address": "terraform_data.api",
                    "change": {
                        "actions": ["update"],
                        "before": {"token": "old-secret"},
                        "before_sensitive": {"token": true},
                        "after": {"token": "new-secret"},
                        "after_sensitive": {"token": true}
                    }
                }
            ],
            "output_changes": {
                "endpoint": {"change": {
                    "actions": ["update"],
                    "before": "previous-output-secret",
                    "before_sensitive": true,
                    "after": "output-secret",
                    "after_sensitive": true
                }}
            }
        });

        let metadata = parse_metadata(&document, true);

        assert_eq!(
            metadata.sensitive_values(),
            [
                SensitiveValue::Text("new-secret".to_owned()),
                SensitiveValue::Text("old-secret".to_owned()),
                SensitiveValue::Text("output-secret".to_owned()),
                SensitiveValue::Text("previous-output-secret".to_owned()),
            ]
        );
        let debug = format!("{metadata:?}");
        assert!(!debug.contains("old-secret"));
        assert!(!debug.contains("new-secret"));
        assert!(!debug.contains("output-secret"));
        assert!(!debug.contains("previous-output-secret"));
    }
}
