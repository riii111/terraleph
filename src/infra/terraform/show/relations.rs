use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::app::plan::{
    ConfigurationRelationStatus, PlanRelations, RelationEndpoint, RelationEvidence, RelationSource,
    RelationUnresolvedReason,
};

use super::super::address::{
    ModuleAddressSegment, ResourceAddress, ResourceReference, format_resource_address,
    parse_reference, parse_resource_address,
};

pub(super) fn parse_configuration(
    document: &Value,
    planned_addresses: &BTreeSet<String>,
    deleted_addresses: &BTreeSet<String>,
) -> PlanRelations {
    let has_prior_state = document
        .get("prior_state")
        .is_some_and(|state| !state.is_null());
    let Some(root) = document
        .as_object()
        .and_then(|root| root.get("configuration"))
        .and_then(Value::as_object)
        .and_then(|configuration| configuration.get("root_module"))
    else {
        return PlanRelations::from_saved_plan(
            ConfigurationRelationStatus::Unavailable,
            Vec::new(),
            has_prior_state,
        );
    };

    let mut tree = ConfigurationTree::default();
    tree.collect_module(root, &[], None);
    let mut evidence = Vec::new();
    for (path, module) in &tree.modules {
        for resource in &module.resources {
            let source_addresses = source_addresses_for(
                path,
                &resource.identity,
                planned_addresses,
                deleted_addresses,
            );
            for (dependent, context) in source_addresses {
                evidence.extend(tree.relations_for_resource(
                    path,
                    resource,
                    &dependent,
                    &context,
                    planned_addresses,
                ));
            }
        }
    }
    evidence.sort();
    evidence.dedup();

    let status = if tree.partial {
        ConfigurationRelationStatus::Partial
    } else {
        ConfigurationRelationStatus::Available
    };
    PlanRelations::from_saved_plan(status, evidence, has_prior_state)
}

fn append_evidence(
    evidence: &mut Vec<RelationEvidence>,
    dependent: &RelationEndpoint,
    source: RelationSource,
    resolved: Resolution,
) {
    evidence.extend(
        resolved
            .targets
            .into_iter()
            .map(|referenced| RelationEvidence::resolved(dependent.clone(), referenced, source)),
    );
    evidence.extend(
        resolved
            .issues
            .into_iter()
            .map(|reason| RelationEvidence::unresolved(dependent.clone(), source, reason)),
    );
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResourceIdentity {
    mode: String,
    resource_type: String,
    name: String,
}

impl ResourceIdentity {
    fn from_object(object: &Map<String, Value>) -> Option<Self> {
        Some(Self {
            mode: object.get("mode")?.as_str()?.to_owned(),
            resource_type: object.get("type")?.as_str()?.to_owned(),
            name: object.get("name")?.as_str()?.to_owned(),
        })
    }

    fn matches(&self, address: &ResourceAddress) -> bool {
        self.mode == address.mode
            && self.resource_type == address.resource_type
            && self.name == address.name
    }

    fn format(
        &self,
        modules: &[ModuleAddressSegment],
        index: Option<&super::super::address::AddressIndex>,
    ) -> String {
        format_resource_address(modules, &self.mode, &self.resource_type, &self.name, index)
    }
}

struct ResourceDefinition {
    identity: ResourceIdentity,
    expressions: Vec<Vec<String>>,
    depends_on: Vec<String>,
    invalid: bool,
}

#[derive(Clone)]
struct ParentCall {
    parent_path: Vec<String>,
    call_name: String,
}

#[derive(Clone)]
struct ModuleCall {
    child_path: Vec<String>,
    inputs: BTreeMap<String, Vec<Vec<String>>>,
    expressions: Vec<Vec<String>>,
    depends_on: Vec<String>,
    repeated: bool,
    invalid: bool,
}

struct ModuleDefinition {
    resources: Vec<ResourceDefinition>,
    calls: BTreeMap<String, ModuleCall>,
    outputs: BTreeMap<String, (Vec<Vec<String>>, bool)>,
    variables_with_default: BTreeSet<String>,
    parent: Option<ParentCall>,
}

#[derive(Default)]
struct ConfigurationTree {
    modules: BTreeMap<Vec<String>, ModuleDefinition>,
    partial: bool,
}

impl ConfigurationTree {
    fn relations_for_resource(
        &self,
        path: &[String],
        resource: &ResourceDefinition,
        dependent: &RelationEndpoint,
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
    ) -> Vec<RelationEvidence> {
        let mut evidence = Vec::new();
        if resource.invalid {
            evidence.push(RelationEvidence::unresolved(
                dependent.clone(),
                RelationSource::Configuration,
                RelationUnresolvedReason::InvalidConfiguration,
            ));
        }
        let mut stack = BTreeSet::new();
        for group in &resource.expressions {
            let resolved = self.resolve_group(group, context, known_addresses, &mut stack);
            append_evidence(
                &mut evidence,
                dependent,
                RelationSource::Configuration,
                resolved,
            );
        }
        for reference in &resource.depends_on {
            let resolved = self.resolve_reference(reference, context, known_addresses, &mut stack);
            append_evidence(
                &mut evidence,
                dependent,
                RelationSource::Configuration,
                resolved,
            );
        }
        for (depth, call_name) in path.iter().enumerate() {
            let parent_path = path[..depth].to_vec();
            let Some(call) = self
                .modules
                .get(&parent_path)
                .and_then(|parent_module| parent_module.calls.get(call_name))
            else {
                continue;
            };
            let parent_context = ResolutionContext {
                modules: context.modules.iter().take(depth).cloned().collect(),
            };
            if call.invalid {
                evidence.push(RelationEvidence::unresolved(
                    dependent.clone(),
                    RelationSource::Configuration,
                    RelationUnresolvedReason::InvalidConfiguration,
                ));
            }
            for group in &call.expressions {
                let resolved =
                    self.resolve_group(group, &parent_context, known_addresses, &mut stack);
                append_evidence(
                    &mut evidence,
                    dependent,
                    RelationSource::Configuration,
                    resolved,
                );
            }
            for reference in &call.depends_on {
                let resolved =
                    self.resolve_reference(reference, &parent_context, known_addresses, &mut stack);
                append_evidence(
                    &mut evidence,
                    dependent,
                    RelationSource::Configuration,
                    resolved,
                );
            }
        }
        evidence
    }

    fn collect_module(&mut self, value: &Value, path: &[String], parent: Option<ParentCall>) {
        let Some(object) = value.as_object() else {
            self.partial = true;
            return;
        };
        let resources = self.parse_resources(object);
        let outputs = self.parse_outputs(object);
        let variables_with_default = self.parse_variables(object);
        let calls = self.parse_calls(object, path);
        self.modules.insert(
            path.to_vec(),
            ModuleDefinition {
                resources,
                calls: calls.clone(),
                outputs,
                variables_with_default,
                parent,
            },
        );
        for (name, call) in calls {
            let Some(call_value) = object
                .get("module_calls")
                .and_then(Value::as_object)
                .and_then(|calls| calls.get(&name))
            else {
                self.partial = true;
                continue;
            };
            let Some(child_module) = call_value.as_object().and_then(|call| call.get("module"))
            else {
                self.partial = true;
                continue;
            };
            self.collect_module(
                child_module,
                &call.child_path,
                Some(ParentCall {
                    parent_path: path.to_vec(),
                    call_name: name,
                }),
            );
        }
    }

    fn parse_resources(&mut self, module: &Map<String, Value>) -> Vec<ResourceDefinition> {
        let Some(resources) = module.get("resources") else {
            return Vec::new();
        };
        let Some(resources) = resources.as_array() else {
            self.partial = true;
            return Vec::new();
        };
        let mut parsed = Vec::new();
        for resource in resources {
            let Some(resource) = resource.as_object() else {
                self.partial = true;
                continue;
            };
            let Some(identity) = ResourceIdentity::from_object(resource) else {
                self.partial = true;
                continue;
            };
            let mut expressions = Vec::new();
            let mut invalid = false;
            if let Some(value) = resource.get("expressions") {
                invalid |= !collect_expression_map(value, &mut expressions);
            }
            for key in ["count_expression", "for_each_expression"] {
                if let Some(value) = resource.get(key) {
                    invalid |=
                        !value.is_object() || !collect_expression_groups(value, &mut expressions);
                }
            }
            let depends_on = parse_string_array(resource.get("depends_on"), &mut invalid);
            self.partial |= invalid;
            parsed.push(ResourceDefinition {
                identity,
                expressions,
                depends_on,
                invalid,
            });
        }
        parsed
    }

    fn parse_outputs(
        &mut self,
        module: &Map<String, Value>,
    ) -> BTreeMap<String, (Vec<Vec<String>>, bool)> {
        let Some(outputs) = module.get("outputs") else {
            return BTreeMap::new();
        };
        let Some(outputs) = outputs.as_object() else {
            self.partial = true;
            return BTreeMap::new();
        };
        outputs
            .iter()
            .map(|(name, output)| {
                let mut groups = Vec::new();
                let valid = output
                    .as_object()
                    .and_then(|output| output.get("expression"))
                    .is_some_and(|expression| {
                        expression.is_object() && collect_expression_groups(expression, &mut groups)
                    });
                if !valid {
                    self.partial = true;
                }
                (name.clone(), (groups, !valid))
            })
            .collect()
    }

    fn parse_variables(&mut self, module: &Map<String, Value>) -> BTreeSet<String> {
        let Some(variables) = module.get("variables") else {
            return BTreeSet::new();
        };
        let Some(variables) = variables.as_object() else {
            self.partial = true;
            return BTreeSet::new();
        };
        variables
            .iter()
            .filter_map(|(name, value)| {
                let Some(value) = value.as_object() else {
                    self.partial = true;
                    return None;
                };
                value.contains_key("default").then(|| name.clone())
            })
            .collect()
    }

    fn parse_calls(
        &mut self,
        module: &Map<String, Value>,
        path: &[String],
    ) -> BTreeMap<String, ModuleCall> {
        let Some(calls) = module.get("module_calls") else {
            return BTreeMap::new();
        };
        let Some(calls) = calls.as_object() else {
            self.partial = true;
            return BTreeMap::new();
        };
        calls
            .iter()
            .filter_map(|(name, value)| {
                let Some(call) = value.as_object() else {
                    self.partial = true;
                    return None;
                };
                let Some(child) = call.get("module") else {
                    self.partial = true;
                    return None;
                };
                if !child.is_object() {
                    self.partial = true;
                    return None;
                }
                let mut child_path = path.to_vec();
                child_path.push(name.clone());
                let mut inputs = BTreeMap::new();
                let mut invalid = false;
                if let Some(expressions) = call.get("expressions") {
                    if let Some(expressions) = expressions.as_object() {
                        for (input, expression) in expressions {
                            let mut groups = Vec::new();
                            invalid |= !expression.is_object()
                                || !collect_expression_groups(expression, &mut groups);
                            inputs.insert(input.clone(), groups);
                        }
                    } else {
                        invalid = true;
                    }
                }
                let mut declaration_expressions = Vec::new();
                for key in ["count_expression", "for_each_expression"] {
                    if let Some(expression) = call.get(key) {
                        invalid |= !expression.is_object()
                            || !collect_expression_groups(expression, &mut declaration_expressions);
                    }
                }
                let mut depends_on = parse_string_array(call.get("depends_on"), &mut invalid);
                depends_on.sort();
                let repeated = call.contains_key("count_expression")
                    || call.contains_key("for_each_expression");
                Some((
                    name.clone(),
                    ModuleCall {
                        child_path,
                        inputs,
                        expressions: declaration_expressions,
                        depends_on,
                        repeated,
                        invalid,
                    },
                ))
            })
            .collect()
    }

    fn resolve_group(
        &self,
        references: &[String],
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
        stack: &mut BTreeSet<String>,
    ) -> Resolution {
        let parsed = references
            .iter()
            .map(|reference| parse_reference(reference))
            .collect::<Vec<_>>();
        let specific_blocks = parsed
            .iter()
            .filter_map(|reference| match reference {
                Some(ResourceReference::Resource(address)) if address.index.is_some() => {
                    Some(address.block())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let specific_modules = parsed
            .iter()
            .filter_map(|reference| match reference {
                Some(ResourceReference::ModuleOutput { modules, .. }) => Some(modules.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let mut resolution = Resolution::default();
        for parsed in parsed {
            if let Some(ResourceReference::Resource(address)) = &parsed
                && address.index.is_none()
                && specific_blocks.contains(&address.block())
            {
                continue;
            }
            if let Some(ResourceReference::Module { modules }) = &parsed
                && specific_modules.contains(modules)
            {
                continue;
            }
            let Some(parsed) = parsed else {
                resolution
                    .issues
                    .insert(RelationUnresolvedReason::MissingAddress);
                continue;
            };
            resolution.merge(self.resolve_parsed_reference(
                parsed,
                context,
                known_addresses,
                stack,
            ));
        }
        resolution
    }

    fn resolve_reference(
        &self,
        reference: &str,
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
        stack: &mut BTreeSet<String>,
    ) -> Resolution {
        let Some(reference) = parse_reference(reference) else {
            return Resolution::issue(RelationUnresolvedReason::MissingAddress);
        };
        self.resolve_parsed_reference(reference, context, known_addresses, stack)
    }

    fn resolve_parsed_reference(
        &self,
        reference: ResourceReference,
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
        stack: &mut BTreeSet<String>,
    ) -> Resolution {
        match reference {
            ResourceReference::Resource(address) => {
                let Some((module_path, target_context, module_issue)) =
                    self.target_context(context, &address.modules, false)
                else {
                    return Resolution::issue(RelationUnresolvedReason::MissingAddress);
                };
                if let Some(issue) = module_issue {
                    return Resolution::issue(issue);
                }
                let identity = ResourceIdentity {
                    mode: address.mode.clone(),
                    resource_type: address.resource_type.clone(),
                    name: address.name.clone(),
                };
                if !self
                    .modules
                    .get(&module_path)
                    .is_some_and(|module| module.resources.iter().any(|r| r.identity == identity))
                {
                    return Resolution::issue(RelationUnresolvedReason::MissingAddress);
                }
                Self::endpoint_for_reference(
                    &identity,
                    &target_context,
                    address.index.as_ref(),
                    known_addresses,
                )
            }
            ResourceReference::ModuleOutput { modules, output } => {
                self.resolve_module_output(&modules, &output, context, known_addresses, stack)
            }
            ResourceReference::Module { modules } => {
                let Some((module_path, target_context, module_issue)) =
                    self.target_context(context, &modules, true)
                else {
                    return Resolution::issue(RelationUnresolvedReason::MissingAddress);
                };
                if let Some(issue) = module_issue {
                    return Resolution::issue(issue);
                }
                let mut resolution = Resolution::default();
                self.collect_module_resource_blocks(
                    &module_path,
                    &target_context,
                    known_addresses,
                    &mut resolution,
                );
                if resolution.targets.is_empty() && resolution.issues.is_empty() {
                    resolution
                        .issues
                        .insert(RelationUnresolvedReason::MissingAddress);
                }
                resolution
            }
            ResourceReference::Variable(name) => {
                self.resolve_variable(&name, context, known_addresses, stack)
            }
            ResourceReference::Local(_) => Resolution::issue(RelationUnresolvedReason::LocalValue),
            ResourceReference::Meta => Resolution::default(),
        }
    }

    fn resolve_variable(
        &self,
        name: &str,
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
        stack: &mut BTreeSet<String>,
    ) -> Resolution {
        let module_path = context.module_names();
        let Some(module) = self.modules.get(&module_path) else {
            return Resolution::issue(RelationUnresolvedReason::MissingAddress);
        };
        let Some(parent) = &module.parent else {
            return Resolution::issue(RelationUnresolvedReason::Variable);
        };
        let Some(call) = self
            .modules
            .get(&parent.parent_path)
            .and_then(|parent_module| parent_module.calls.get(&parent.call_name))
        else {
            return Resolution::issue(RelationUnresolvedReason::MissingAddress);
        };
        let Some(groups) = call.inputs.get(name) else {
            if module.variables_with_default.contains(name) {
                return Resolution::default();
            }
            return Resolution::issue(RelationUnresolvedReason::Variable);
        };
        let key = format!("variable:{}:{name}", format_path(&module_path));
        if !stack.insert(key.clone()) {
            return Resolution::issue(RelationUnresolvedReason::CyclicReference);
        }
        let parent_context = context.parent();
        let mut resolution = Resolution::default();
        for group in groups {
            resolution.merge(self.resolve_group(group, &parent_context, known_addresses, stack));
        }
        stack.remove(&key);
        resolution
    }

    fn resolve_module_output(
        &self,
        selectors: &[ModuleAddressSegment],
        output: &str,
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
        stack: &mut BTreeSet<String>,
    ) -> Resolution {
        let Some((module_path, child_context, issue)) =
            self.target_context(context, selectors, false)
        else {
            return Resolution::issue(RelationUnresolvedReason::MissingAddress);
        };
        if let Some(issue) = issue {
            return Resolution::issue(issue);
        }
        let Some((groups, invalid)) = self
            .modules
            .get(&module_path)
            .and_then(|module| module.outputs.get(output))
        else {
            return Resolution::issue(RelationUnresolvedReason::MissingAddress);
        };
        if *invalid {
            return Resolution::issue(RelationUnresolvedReason::InvalidConfiguration);
        }
        let key = format!("output:{}:{output}", child_context.format_path());
        if !stack.insert(key.clone()) {
            return Resolution::issue(RelationUnresolvedReason::CyclicReference);
        }
        let mut resolution = Resolution::default();
        for group in groups {
            resolution.merge(self.resolve_group(group, &child_context, known_addresses, stack));
        }
        stack.remove(&key);
        resolution
    }

    fn collect_module_resource_blocks(
        &self,
        module_path: &[String],
        context: &ResolutionContext,
        known_addresses: &BTreeSet<String>,
        resolution: &mut Resolution,
    ) {
        let Some(module) = self.modules.get(module_path) else {
            resolution
                .issues
                .insert(RelationUnresolvedReason::MissingAddress);
            return;
        };
        for resource in &module.resources {
            resolution.merge(Self::endpoint_for_reference(
                &resource.identity,
                context,
                None,
                known_addresses,
            ));
        }
        for (name, call) in &module.calls {
            if call.invalid {
                resolution
                    .issues
                    .insert(RelationUnresolvedReason::InvalidConfiguration);
            }
            let mut child_context = context.clone();
            child_context.modules.push(ModuleAddressSegment {
                name: name.clone(),
                index: None,
            });
            self.collect_module_resource_blocks(
                &call.child_path,
                &child_context,
                known_addresses,
                resolution,
            );
        }
    }

    fn target_context(
        &self,
        context: &ResolutionContext,
        selectors: &[ModuleAddressSegment],
        allow_repeated_block: bool,
    ) -> Option<(
        Vec<String>,
        ResolutionContext,
        Option<RelationUnresolvedReason>,
    )> {
        let mut path = context.module_names();
        let mut modules = context.modules.clone();
        for selector in selectors {
            let parent_path = path.clone();
            let call = self.modules.get(&parent_path)?.calls.get(&selector.name)?;
            if call.invalid {
                return Some((
                    call.child_path.clone(),
                    context.clone(),
                    Some(RelationUnresolvedReason::InvalidConfiguration),
                ));
            }
            if call.repeated != selector.index.is_some()
                && !(allow_repeated_block && call.repeated && selector.index.is_none())
            {
                return Some((
                    call.child_path.clone(),
                    context.clone(),
                    Some(if call.repeated {
                        RelationUnresolvedReason::AmbiguousModule
                    } else {
                        RelationUnresolvedReason::MissingAddress
                    }),
                ));
            }
            path.push(selector.name.clone());
            modules.push(selector.clone());
        }
        Some((path, ResolutionContext { modules }, None))
    }

    fn endpoint_for_reference(
        identity: &ResourceIdentity,
        context: &ResolutionContext,
        index: Option<&super::super::address::AddressIndex>,
        known_addresses: &BTreeSet<String>,
    ) -> Resolution {
        if context.modules.iter().any(|module| module.index.is_some())
            && !known_addresses
                .iter()
                .filter_map(|address| parse_resource_address(address))
                .any(|address| module_path_matches(&address.modules, &context.modules))
        {
            return Resolution::issue(RelationUnresolvedReason::MissingAddress);
        }
        let address = identity.format(&context.modules, index);
        if index.is_some() {
            if known_addresses.contains(&address) {
                Resolution::target(RelationEndpoint::Instance(address))
            } else {
                Resolution::issue(RelationUnresolvedReason::MissingAddress)
            }
        } else {
            Resolution::target(RelationEndpoint::Block(address))
        }
    }
}

fn module_path_matches(address: &[ModuleAddressSegment], prefix: &[ModuleAddressSegment]) -> bool {
    address.len() >= prefix.len()
        && prefix.iter().zip(address).all(|(expected, actual)| {
            expected.name == actual.name
                && (expected.index.is_none() || expected.index == actual.index)
        })
}

#[derive(Default)]
struct Resolution {
    targets: BTreeSet<RelationEndpoint>,
    issues: BTreeSet<RelationUnresolvedReason>,
}

impl Resolution {
    fn target(endpoint: RelationEndpoint) -> Self {
        Self {
            targets: BTreeSet::from([endpoint]),
            issues: BTreeSet::new(),
        }
    }

    fn issue(reason: RelationUnresolvedReason) -> Self {
        Self {
            targets: BTreeSet::new(),
            issues: BTreeSet::from([reason]),
        }
    }

    fn merge(&mut self, other: Self) {
        self.targets.extend(other.targets);
        self.issues.extend(other.issues);
    }
}

#[derive(Clone)]
struct ResolutionContext {
    modules: Vec<ModuleAddressSegment>,
}

impl ResolutionContext {
    fn module_names(&self) -> Vec<String> {
        self.modules
            .iter()
            .map(|module| module.name.clone())
            .collect()
    }

    fn parent(&self) -> Self {
        let mut modules = self.modules.clone();
        modules.pop();
        Self { modules }
    }

    fn format_path(&self) -> String {
        format_path(&self.module_names())
    }
}

fn format_path(path: &[String]) -> String {
    path.join("/")
}

fn source_addresses_for(
    path: &[String],
    identity: &ResourceIdentity,
    planned_addresses: &BTreeSet<String>,
    deleted_addresses: &BTreeSet<String>,
) -> Vec<(RelationEndpoint, ResolutionContext)> {
    let mut matches = planned_addresses
        .iter()
        .filter_map(|address| parse_resource_address(address))
        .filter(|address| {
            address.module_names().eq(path.iter().map(String::as_str)) && identity.matches(address)
        })
        .map(|address| {
            let context = ResolutionContext {
                modules: address.modules.clone(),
            };
            (RelationEndpoint::Instance(address.full()), context)
        })
        .collect::<Vec<_>>();
    let has_deleted_instances = matches.is_empty()
        && deleted_addresses
            .iter()
            .filter_map(|address| parse_resource_address(address))
            .any(|address| {
                address.module_names().eq(path.iter().map(String::as_str))
                    && identity.matches(&address)
            });
    if matches.is_empty() && !has_deleted_instances {
        let modules = path
            .iter()
            .map(|name| ModuleAddressSegment {
                name: name.clone(),
                index: None,
            })
            .collect::<Vec<_>>();
        let endpoint = RelationEndpoint::Block(identity.format(&modules, None));
        matches.push((endpoint, ResolutionContext { modules }));
    }
    matches.sort_by(|left, right| left.0.cmp(&right.0));
    matches.dedup_by(|left, right| left.0 == right.0);
    matches
}

fn collect_expression_groups(value: &Value, groups: &mut Vec<Vec<String>>) -> bool {
    let mut valid = true;
    match value {
        Value::Object(object) => {
            if let Some(references) = object.get("references") {
                if let Some(references) = references.as_array() {
                    let mut group = Vec::new();
                    for reference in references {
                        if let Some(reference) = reference.as_str() {
                            group.push(reference.to_owned());
                        } else {
                            valid = false;
                        }
                    }
                    groups.push(group);
                } else {
                    valid = false;
                }
            }
            for (key, value) in object {
                if key != "references" && key != "constant_value" {
                    valid &= collect_expression_groups(value, groups);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                valid &= collect_expression_groups(item, groups);
            }
        }
        _ => {}
    }
    valid
}

fn collect_expression_map(value: &Value, groups: &mut Vec<Vec<String>>) -> bool {
    let Some(expressions) = value.as_object() else {
        return false;
    };
    let mut valid = true;
    for expression in expressions.values() {
        if expression.is_object() {
            valid &= collect_expression_groups(expression, groups);
        } else {
            valid = false;
        }
    }
    valid
}

fn parse_string_array(value: Option<&Value>, invalid: &mut bool) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        *invalid = true;
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            item.as_str().map(str::to_owned).or_else(|| {
                *invalid = true;
                None
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::app::plan::StateRelationStatus;
    use serde_json::json;

    use super::*;

    #[expect(
        clippy::needless_pass_by_value,
        reason = "JSON fixture values are borrowed to build parser assertions"
    )]
    fn parse(document: Value, addresses: &[&str]) -> PlanRelations {
        parse_configuration(
            &document,
            &addresses
                .iter()
                .map(|address| (*address).to_owned())
                .collect(),
            &BTreeSet::new(),
        )
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "JSON fixture values are moved into the synthetic configuration document"
    )]
    fn resource(address: &str, name: &str, expressions: Value, depends_on: Value) -> Value {
        json!({
            "address": address,
            "mode": "managed",
            "type": "terraform_data",
            "name": name,
            "expressions": expressions,
            "depends_on": depends_on,
        })
    }

    #[test]
    fn retains_direct_and_explicit_dependencies_and_suppresses_containing_block_reference() {
        let document = json!({
            "prior_state": {"values": {"root_module": null}},
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.source", "source", json!({}), json!([])),
                resource("terraform_data.other", "other", json!({}), json!([])),
                resource("terraform_data.dependent", "dependent", json!({
                    "input": {"references": [
                        "terraform_data.source[1].id",
                        "terraform_data.source[1]",
                        "terraform_data.source"
                    ]}
                }), json!(["terraform_data.other"]))
            ]}}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.source[0]",
                "terraform_data.source[1]",
                "terraform_data.other",
                "terraform_data.dependent",
            ],
        );

        assert_eq!(relations.configuration.len(), 2);
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.dependent"
                && edge.referenced.as_ref().is_some_and(|target| {
                    target.address() == "terraform_data.source[1]" && target.is_instance()
                })
        }));
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.dependent"
                && edge.referenced.as_ref().is_some_and(|target| {
                    target.address() == "terraform_data.other" && !target.is_instance()
                })
        }));
        assert!(!relations.configuration.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "terraform_data.source" && !target.is_instance()
            })
        }));
    }

    #[test]
    fn keeps_the_block_reference_of_a_separate_expression() {
        let document = json!({
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.source", "source", json!({}), json!([])),
                resource("terraform_data.dependent", "dependent", json!({
                    "input": {"references": [
                        "terraform_data.source[1].id",
                        "terraform_data.source[1]",
                        "terraform_data.source"
                    ]},
                    "separate": {"references": ["terraform_data.source"]}
                }), json!([]))
            ]}}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.source[0]",
                "terraform_data.source[1]",
                "terraform_data.dependent",
            ],
        );

        assert_eq!(relations.configuration.len(), 2);
        assert!(relations.configuration.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "terraform_data.source[1]" && target.is_instance()
            })
        }));
        assert!(relations.configuration.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "terraform_data.source" && !target.is_instance()
            })
        }));
    }

    #[test]
    fn keeps_known_and_unresolved_references_on_the_same_resource() {
        let document = json!({
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.source", "source", json!({}), json!([])),
                resource("terraform_data.mixed", "mixed", json!({
                    "input": {"references": ["terraform_data.source.id", "local.unavailable"]}
                }), json!([]))
            ]}}
        });

        let relations = parse(document, &["terraform_data.source", "terraform_data.mixed"]);

        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.mixed"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "terraform_data.source")
        }));
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.mixed"
                && edge.unresolved == Some(RelationUnresolvedReason::LocalValue)
        }));
    }

    #[test]
    fn records_each_repeated_source_and_leaves_root_variables_unresolved() {
        let document = json!({
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.source", "source", json!({}), json!([])),
                {
                    "address": "terraform_data.counted",
                    "mode": "managed",
                    "type": "terraform_data",
                    "name": "counted",
                    "count_expression": {"references": ["terraform_data.source.input", "terraform_data.source"]},
                    "expressions": {},
                    "depends_on": [],
                },
                resource("terraform_data.consumer", "consumer", json!({
                    "input": {"references": ["var.root_only", "local.not_in_json"]}
                }), json!([]))
            ]}}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.source",
                "terraform_data.counted[0]",
                "terraform_data.counted[1]",
                "terraform_data.consumer",
            ],
        );

        for address in ["terraform_data.counted[0]", "terraform_data.counted[1]"] {
            assert!(relations.configuration.iter().any(|edge| {
                edge.dependent.address() == address
                    && edge.referenced.as_ref().is_some_and(|target| {
                        target.address() == "terraform_data.source" && !target.is_instance()
                    })
            }));
        }
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::Variable)
        }));
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::LocalValue)
        }));
    }

    #[test]
    fn collects_references_from_both_branches_of_a_conditional_expression() {
        let document = json!({
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.left", "left", json!({}), json!([])),
                resource("terraform_data.right", "right", json!({}), json!([])),
                resource("terraform_data.conditional", "conditional", json!({
                    "input": {
                        "condition": {"references": ["var.choose"]},
                        "true_result": {"references": ["terraform_data.left.id", "terraform_data.left"]},
                        "false_result": {"references": ["terraform_data.right.id", "terraform_data.right"]}
                    }
                }), json!([]))
            ]}}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.left",
                "terraform_data.right",
                "terraform_data.conditional",
            ],
        );

        for target in ["terraform_data.left", "terraform_data.right"] {
            assert!(relations.configuration.iter().any(|edge| {
                edge.dependent.address() == "terraform_data.conditional"
                    && edge.referenced.as_ref().is_some_and(|endpoint| {
                        endpoint.address() == target && !endpoint.is_instance()
                    })
            }));
        }
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.conditional"
                && edge.unresolved == Some(RelationUnresolvedReason::Variable)
        }));
    }

    #[test]
    fn marks_malformed_relationship_fields_partial_without_rejecting_plan_data() {
        let document = json!({
            "format_version": "1.0",
            "resource_changes": [],
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.consumer", "consumer", json!({
                    "input": {"references": "not-an-array"}
                }), json!([]))
            ]}}
        });

        let (_, _, analysis) = super::super::json::parse_plan_json_with_metadata(
            document.to_string().as_bytes(),
            false,
        )
        .expect("malformed relationship data should not reject the plan");

        assert_eq!(
            analysis.configuration_status,
            ConfigurationRelationStatus::Partial
        );
        assert!(analysis.configuration.iter().any(|edge| {
            edge.unresolved == Some(RelationUnresolvedReason::InvalidConfiguration)
        }));
    }

    #[test]
    fn marks_consumers_of_malformed_module_outputs_unresolved() {
        let document = json!({
            "format_version": "1.0",
            "resource_changes": [],
            "configuration": {"root_module": {
                "resources": [resource("terraform_data.consumer", "consumer", json!({
                    "input": {"references": ["module.child.output"]}
                }), json!([]))],
                "module_calls": {"child": {
                    "module": {
                        "resources": [resource("terraform_data.inside", "inside", json!({}), json!([]))],
                        "outputs": {"output": {"expression": {"references": "malformed"}}}
                    }
                }}
            }}
        });

        let (_, _, analysis) = super::super::json::parse_plan_json_with_metadata(
            document.to_string().as_bytes(),
            false,
        )
        .expect("malformed relationship data should not reject the plan");

        assert_eq!(
            analysis.configuration_status,
            ConfigurationRelationStatus::Partial
        );
        assert!(analysis.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::InvalidConfiguration)
        }));
    }

    #[test]
    fn distinguishes_plan_without_prior_state_from_state_not_yet_collected() {
        let without_state = parse(
            json!({"configuration": {"root_module": {"resources": []}}}),
            &[],
        );
        let with_state = parse(
            json!({
                "prior_state": {"values": {"root_module": {"resources": []}}},
                "configuration": {"root_module": {"resources": []}}
            }),
            &[],
        );

        assert_eq!(
            without_state.state_status,
            StateRelationStatus::NoPriorState
        );
        assert_eq!(with_state.state_status, StateRelationStatus::NotCollected);
        assert_eq!(
            with_state
                .with_state(StateRelationStatus::Unavailable, Vec::new())
                .state_status,
            StateRelationStatus::Unavailable
        );
    }

    #[test]
    fn resolves_module_inputs_and_outputs_without_reading_values() {
        let document = json!({
            "configuration": {"root_module": {
                "resources": [
                    resource("terraform_data.input_source", "input_source", json!({}), json!([])),
                    resource("terraform_data.output_consumer", "output_consumer", json!({
                    "input": {"references": ["module.child.output[0]", "module.child.output", "module.child"]}
                    }), json!([])),
                    resource("terraform_data.module_dependent", "module_dependent", json!({}), json!(["module.child"]))
                ],
                "module_calls": {"child": {
                    "expressions": {"input": {"references": ["terraform_data.input_source.id", "terraform_data.input_source"]}},
                    "module": {
                        "resources": [resource("terraform_data.inside", "inside", json!({
                            "input": {"references": ["var.input"]}
                        }), json!([])), resource("terraform_data.unused", "unused", json!({}), json!([]))],
                        "variables": {"input": {}},
                        "outputs": {"output": {"expression": {"references": ["terraform_data.inside.id", "terraform_data.inside"]}}}
                    }
                }}
            }}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.input_source",
                "module.child.terraform_data.inside",
                "module.child.terraform_data.unused",
                "terraform_data.output_consumer",
                "terraform_data.module_dependent",
            ],
        );

        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "module.child.terraform_data.inside"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "terraform_data.input_source")
        }));
        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.output_consumer"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "module.child.terraform_data.inside")
        }));
        assert!(!relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.output_consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::MissingAddress)
        }));
        assert!(!relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.output_consumer"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "module.child.terraform_data.unused")
        }));
        for target in [
            "module.child.terraform_data.inside",
            "module.child.terraform_data.unused",
        ] {
            assert!(relations.configuration.iter().any(|edge| {
                edge.dependent.address() == "terraform_data.module_dependent"
                    && edge.referenced.as_ref().is_some_and(|endpoint| {
                        endpoint.address() == target && !endpoint.is_instance()
                    })
            }));
        }
    }

    #[test]
    fn rejects_ambiguous_repeated_module_outputs_and_marks_missing_configuration() {
        let document = json!({
            "configuration": {"root_module": {
                "resources": [resource("terraform_data.consumer", "consumer", json!({
                    "input": {"references": ["module.child.output"]}
                }), json!([]))],
                "module_calls": {"child": {
                    "count_expression": {"constant_value": 2},
                    "module": {"resources": [], "outputs": {"output": {"expression": {"references": ["terraform_data.child.id"]}}}}
                }}
            }}
        });

        let relations = parse(document, &["terraform_data.consumer"]);

        assert!(
            relations
                .configuration
                .iter()
                .any(|edge| { edge.unresolved == Some(RelationUnresolvedReason::AmbiguousModule) })
        );
        assert_eq!(
            parse(json!({"format_version": "1.0"}), &[]).configuration_status,
            ConfigurationRelationStatus::Unavailable
        );
    }

    #[test]
    fn excludes_deleted_instances_from_configuration_relationships() {
        let document = json!({
            "format_version": "1.2",
            "prior_state": {"values": {"root_module": {"resources": [
                {"address": "terraform_data.source"},
                {"address": "terraform_data.counted[0]"},
                {"address": "terraform_data.counted[1]"}
            ]}}},
            "planned_values": {"root_module": {"resources": [
                {"address": "terraform_data.source"},
                {"address": "terraform_data.counted[0]"}
            ]}},
            "resource_changes": [{
                "address": "terraform_data.counted[1]",
                "mode": "managed",
                "change": {"actions": ["delete"]}
            }],
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.source", "source", json!({}), json!([])),
                resource("terraform_data.counted", "counted", json!({
                    "input": {"references": ["terraform_data.source.id"]}
                }), json!([]))
            ]}}
        });

        let (_, _, analysis) = super::super::json::parse_plan_json_with_metadata(
            document.to_string().as_bytes(),
            false,
        )
        .expect("plan with a deleted count instance should parse");

        assert!(analysis.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.counted[0]"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "terraform_data.source")
        }));
        assert!(
            !analysis
                .configuration
                .iter()
                .any(|edge| { edge.dependent.address() == "terraform_data.counted[1]" })
        );
    }

    #[test]
    fn omits_configuration_roots_for_blocks_with_only_deleted_instances() {
        let document = json!({
            "format_version": "1.2",
            "prior_state": {"values": {"root_module": {"resources": [
                {"address": "terraform_data.source"},
                {"address": "terraform_data.counted[0]"}
            ]}}},
            "planned_values": {"root_module": {"resources": [
                {"address": "terraform_data.source"}
            ]}},
            "resource_changes": [{
                "address": "terraform_data.counted[0]",
                "mode": "managed",
                "change": {"actions": ["delete"]}
            }],
            "configuration": {"root_module": {"resources": [
                resource("terraform_data.source", "source", json!({}), json!([])),
                resource("terraform_data.counted", "counted", json!({
                    "input": {"references": ["terraform_data.source.id"]}
                }), json!([]))
            ]}}
        });

        let (_, _, analysis) = super::super::json::parse_plan_json_with_metadata(
            document.to_string().as_bytes(),
            false,
        )
        .expect("zero-count plan should parse");

        assert!(!analysis.configuration.iter().any(|edge| {
            matches!(
                edge.dependent.address(),
                "terraform_data.counted" | "terraform_data.counted[0]"
            )
        }));
    }

    #[test]
    fn does_not_read_reference_shaped_data_from_constant_values() {
        let relations = parse(
            json!({
                "configuration": {"root_module": {"resources": [
                    resource("terraform_data.source", "source", json!({}), json!([])),
                    resource("terraform_data.consumer", "consumer", json!({
                        "input": {"constant_value": {"references": ["terraform_data.source"]}}
                    }), json!([]))
                ]}}
            }),
            &["terraform_data.source", "terraform_data.consumer"],
        );

        assert!(!relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "terraform_data.source")
        }));
    }

    #[test]
    fn propagates_module_call_expressions_and_ancestor_dependencies_to_child_resources() {
        let document = json!({
            "configuration": {"root_module": {
                "resources": [
                    resource("terraform_data.expression_source", "expression_source", json!({}), json!([])),
                    resource("terraform_data.explicit_source", "explicit_source", json!({}), json!([]))
                ],
                "module_calls": {"outer": {
                    "count_expression": {"references": [
                        "terraform_data.expression_source.input",
                        "terraform_data.expression_source"
                    ]},
                    "depends_on": ["terraform_data.explicit_source"],
                    "module": {
                        "resources": [resource("terraform_data.inside", "inside", json!({}), json!([]))],
                        "module_calls": {"inner": {
                            "module": {
                                "resources": [resource("terraform_data.deep", "deep", json!({}), json!([]))]
                            }
                        }}
                    }
                }}
            }}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.expression_source",
                "terraform_data.explicit_source",
                "module.outer[0].terraform_data.inside",
                "module.outer[0].module.inner.terraform_data.deep",
            ],
        );

        for dependent in [
            "module.outer[0].terraform_data.inside",
            "module.outer[0].module.inner.terraform_data.deep",
        ] {
            for referenced in [
                "terraform_data.expression_source",
                "terraform_data.explicit_source",
            ] {
                assert!(relations.configuration.iter().any(|edge| {
                    edge.dependent.address() == dependent
                        && edge
                            .referenced
                            .as_ref()
                            .is_some_and(|target| target.address() == referenced)
                }));
            }
        }
    }

    #[test]
    fn resolves_repeated_module_block_dependencies_to_block_endpoints() {
        let document = json!({
            "configuration": {"root_module": {
                "resources": [resource("terraform_data.consumer", "consumer", json!({}), json!(["module.child"]))],
                "module_calls": {"child": {
                    "count_expression": {"constant_value": 2},
                    "module": {
                        "resources": [resource("terraform_data.inside", "inside", json!({}), json!([]))]
                    }
                }}
            }}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.consumer",
                "module.child[0].terraform_data.inside",
                "module.child[1].terraform_data.inside",
            ],
        );

        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.referenced.as_ref().is_some_and(|target| {
                    target.address() == "module.child.terraform_data.inside"
                        && !target.is_instance()
                })
        }));
        assert!(!relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::AmbiguousModule)
        }));
    }

    #[test]
    fn resolves_repeated_child_modules_beneath_an_indexed_module_block() {
        let document = json!({
            "configuration": {"root_module": {
                "resources": [resource("terraform_data.consumer", "consumer", json!({}), json!(["module.outer[0]"]))],
                "module_calls": {"outer": {
                    "count_expression": {"constant_value": 2},
                    "module": {
                        "module_calls": {"inner": {
                            "count_expression": {"constant_value": 1},
                            "module": {
                                "resources": [resource("terraform_data.inside", "inside", json!({}), json!([]))]
                            }
                        }}
                    }
                }}
            }}
        });

        let relations = parse(
            document,
            &[
                "terraform_data.consumer",
                "module.outer[0].module.inner[0].terraform_data.inside",
            ],
        );

        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.referenced.as_ref().is_some_and(|target| {
                    target.address() == "module.outer[0].module.inner.terraform_data.inside"
                        && !target.is_instance()
                })
        }));
        assert!(!relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::MissingAddress)
        }));
    }

    #[test]
    fn rejects_module_instance_keys_missing_from_the_plan_addresses() {
        let document = json!({
            "configuration": {"root_module": {
                "resources": [resource("terraform_data.consumer", "consumer", json!({
                    "input": {"references": ["module.child[9].output"]}
                }), json!([]))],
                "module_calls": {"child": {
                    "count_expression": {"constant_value": 1},
                    "module": {
                        "resources": [resource("terraform_data.inside", "inside", json!({}), json!([]))],
                        "outputs": {"output": {"expression": {"references": [
                            "terraform_data.inside.id",
                            "terraform_data.inside"
                        ]}}}
                    }
                }}
            }}
        });

        let relations = parse(
            document,
            &[
                "module.child[0].terraform_data.inside",
                "terraform_data.consumer",
            ],
        );

        assert!(relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer"
                && edge.unresolved == Some(RelationUnresolvedReason::MissingAddress)
        }));
        assert!(!relations.configuration.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.consumer" && edge.referenced.is_some()
        }));
    }
}
