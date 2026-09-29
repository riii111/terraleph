use std::{collections::BTreeMap, ffi::OsString, path::Path};

use serde_json::{Map, Value};

use crate::app::execution::Tool;
use crate::app::plan::{AttributeType, ProviderSchema, ProviderSchemas, ResourceSchema};
use crate::infra::CancellationToken;

use super::super::command::{
    ProcessRunner, TerraformCommand, TerraformExecutionError, interrupted_error, run_command,
};

const SUPPORTED_FORMAT_MAJOR: u64 = 1;

pub(crate) fn read_provider_schema_with_arguments(
    tool: Tool,
    root: &Path,
    global_arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
) -> Result<Option<ProviderSchemas>, TerraformExecutionError> {
    let mut arguments = global_arguments.to_vec();
    arguments.extend([
        OsString::from("providers"),
        OsString::from("schema"),
        OsString::from("-json"),
    ]);
    let output = match run_command(
        tool,
        root,
        TerraformCommand::ProvidersSchema,
        &arguments,
        cancellation,
        runner,
        None,
    ) {
        Ok(output) => output,
        Err(_error) if !cancellation.is_cancelled() => return Ok(None),
        Err(error) => return Err(error),
    };
    if output.interrupted {
        return Err(interrupted_error(tool, TerraformCommand::ProvidersSchema));
    }
    if !output.status.is_success() {
        return Ok(None);
    }
    Ok(parse_provider_schemas(&output.output.stdout))
}

fn parse_provider_schemas(input: &[u8]) -> Option<ProviderSchemas> {
    let document = serde_json::from_slice::<Value>(input).ok()?;
    let root = document.as_object()?;
    if format_major(root)? != SUPPORTED_FORMAT_MAJOR {
        return None;
    }
    let providers = root
        .get("provider_schemas")?
        .as_object()?
        .iter()
        .map(|(name, value)| parse_provider_schema(value).map(|schema| (name.clone(), schema)))
        .collect::<Option<BTreeMap<_, _>>>()?;
    Some(ProviderSchemas { providers })
}

fn parse_provider_schema(value: &Value) -> Option<ProviderSchema> {
    let provider = value.as_object()?;
    let resources = match provider.get("resource_schemas") {
        Some(value) => value
            .as_object()?
            .iter()
            .map(|(name, value)| parse_resource_schema(value).map(|schema| (name.clone(), schema)))
            .collect::<Option<BTreeMap<_, _>>>()?,
        None => BTreeMap::new(),
    };
    Some(ProviderSchema { resources })
}

fn parse_resource_schema(value: &Value) -> Option<ResourceSchema> {
    let block = value.as_object()?.get("block")?.as_object()?;
    let attributes = parse_attributes(block.get("attributes"))?;
    let block_types = parse_block_types(block.get("block_types"))?;
    Some(ResourceSchema {
        attributes,
        block_types,
    })
}

fn parse_attributes(value: Option<&Value>) -> Option<BTreeMap<String, AttributeType>> {
    let Some(value) = value else {
        return Some(BTreeMap::new());
    };
    value
        .as_object()?
        .iter()
        .map(|(name, value)| {
            parse_attribute_type(value.as_object()?).map(|kind| (name.clone(), kind))
        })
        .collect()
}

fn parse_attribute_type(attribute: &Map<String, Value>) -> Option<AttributeType> {
    if let Some(kind) = attribute.get("type") {
        return parse_type(kind);
    }
    parse_nested_type(attribute.get("nested_type")?)
}

fn parse_nested_type(value: &Value) -> Option<AttributeType> {
    let nested_type = value.as_object()?;
    let object = AttributeType::Object(parse_attributes(nested_type.get("attributes"))?);
    match nested_type.get("nesting_mode")?.as_str()? {
        "single" | "group" => Some(object),
        "list" => Some(AttributeType::List(Box::new(object))),
        "set" => Some(AttributeType::Set(Box::new(object))),
        "map" => Some(AttributeType::Map(Box::new(object))),
        _ => None,
    }
}

fn parse_block_types(value: Option<&Value>) -> Option<BTreeMap<String, AttributeType>> {
    let Some(value) = value else {
        return Some(BTreeMap::new());
    };
    value
        .as_object()?
        .iter()
        .map(|(name, value)| {
            let nested = value.as_object()?.get("block")?.as_object()?;
            let object = parse_attributes(nested.get("attributes"))?;
            Some((name.clone(), AttributeType::Object(object)))
        })
        .collect()
}

fn parse_type(value: &Value) -> Option<AttributeType> {
    match value {
        Value::String(kind) => match kind.as_str() {
            "bool" => Some(AttributeType::Bool),
            "number" => Some(AttributeType::Number),
            "string" => Some(AttributeType::String),
            "dynamic" => Some(AttributeType::Dynamic),
            _ => None,
        },
        Value::Array(parts) => {
            let nested = || parts.get(1).and_then(parse_type).map(Box::new);
            match parts.first()?.as_str()? {
                "list" => Some(AttributeType::List(nested()?)),
                "set" => Some(AttributeType::Set(nested()?)),
                "map" => Some(AttributeType::Map(nested()?)),
                "tuple" => parts
                    .get(1)?
                    .as_array()?
                    .iter()
                    .map(parse_type)
                    .collect::<Option<Vec<_>>>()
                    .map(AttributeType::Tuple),
                "object" => parts
                    .get(1)?
                    .as_object()?
                    .iter()
                    .map(|(name, value)| parse_type(value).map(|value| (name.clone(), value)))
                    .collect::<Option<BTreeMap<_, _>>>()
                    .map(AttributeType::Object),
                _ => None,
            }
        }
        _ => None,
    }
}

fn format_major(root: &Map<String, Value>) -> Option<u64> {
    let version = root.get("format_version")?.as_str()?;
    version.split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_resource_types_and_omitted_sections_structurally() {
        let document = json!({
            "format_version": "1.0",
            "provider_schemas": {
                "registry.terraform.io/hashicorp/example": {
                    "resource_schemas": {
                        "example_resource": {
                            "block": {
                                "attributes": {
                                    "labels": {"type": ["map", "string"]},
                                    "nested": {"type": ["object", {"name": "string"}]},
                                    "tags": {"type": ["set", "string"]},
                                    "pair": {"type": ["tuple", ["string", "bool"]]},
                                    "framework_nested": {
                                        "nested_type": {
                                            "nesting_mode": "list",
                                            "attributes": {"name": {"type": "string"}}
                                        }
                                    },
                                    "framework_group": {
                                        "nested_type": {
                                            "nesting_mode": "group",
                                            "attributes": {"name": {"type": "string"}}
                                        }
                                    },
                                    "rules": {
                                        "nested_type": {
                                            "nesting_mode": "set",
                                            "attributes": {"port": {"type": "number"}}
                                        }
                                    }
                                },
                                "block_types": {
                                    "settings": {"nesting_mode": "list", "block": {"attributes": {}}}
                                }
                            }
                        },
                        "empty_resource": {"block": {}}
                    }
                },
                "without_resources": {}
            }
        });

        let schemas = parse_provider_schemas(document.to_string().as_bytes())
            .expect("provider schema should parse");

        let provider = &schemas.providers["registry.terraform.io/hashicorp/example"];
        let attributes = &provider.resources["example_resource"].attributes;
        assert!(attributes["labels"].is_simple_map());
        assert!(!attributes["nested"].is_simple_map());
        assert!(!attributes["framework_nested"].is_simple_map());
        assert!(!attributes["framework_group"].is_simple_map());
        assert_eq!(
            attributes["tags"],
            AttributeType::Set(Box::new(AttributeType::String))
        );
        assert_eq!(
            attributes["pair"],
            AttributeType::Tuple(vec![AttributeType::String, AttributeType::Bool])
        );
        assert_eq!(
            attributes["rules"],
            AttributeType::Set(Box::new(AttributeType::Object(BTreeMap::from([(
                "port".to_owned(),
                AttributeType::Number
            )]))))
        );
        assert!(matches!(
            provider.resources["example_resource"].block_types["settings"],
            AttributeType::Object(_)
        ));
        let empty = &provider.resources["empty_resource"];
        assert!(empty.attributes.is_empty());
        assert!(empty.block_types.is_empty());
        assert!(schemas.providers["without_resources"].resources.is_empty());
    }

    #[rstest]
    #[case::invalid_json("{")]
    #[case::missing_provider_schemas(r#"{"format_version":"1.0"}"#)]
    #[case::unsupported_format_major(r#"{"format_version":"2.0","provider_schemas":{}}"#)]
    #[case::invalid_format_version(r#"{"format_version":"one","provider_schemas":{}}"#)]
    fn malformed_or_unsupported_document_is_unavailable(#[case] input: &str) {
        assert!(parse_provider_schemas(input.as_bytes()).is_none());
    }

    #[rstest]
    #[case::missing_block(json!({}))]
    #[case::missing_attribute_type(json!({"block": {"attributes": {"name": {}}}}))]
    #[case::unknown_attribute_type(json!({"block": {"attributes": {"name": {"type": "text"}}}}))]
    #[case::unknown_nesting_mode(json!({"block": {"attributes": {"name": {
        "nested_type": {"nesting_mode": "tree", "attributes": {}}
    }}}}))]
    #[case::missing_nested_block(json!({"block": {"block_types": {"settings": {}}}}))]
    fn malformed_resource_schema_makes_schema_unavailable(#[case] resource: Value) {
        let document = json!({
            "format_version": "1.0",
            "provider_schemas": {"example": {"resource_schemas": {"example_resource": resource}}}
        });

        assert!(parse_provider_schemas(document.to_string().as_bytes()).is_none());
    }
}
