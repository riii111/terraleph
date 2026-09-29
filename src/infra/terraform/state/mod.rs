use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::Path,
};

use serde_json::{Map, Value};

use crate::app::{
    execution::Tool,
    plan::{RelationEndpoint, RelationEvidence, RelationSource, RelationUnresolvedReason},
};
use crate::infra::CancellationToken;

use super::address::{
    AddressIndex, ModuleAddressSegment, ResourceAddress, format_resource_address,
    parse_module_address, parse_resource_address,
};
use super::command::{ProcessRunner, TerraformCommand, TerraformExecutionError, run_successful};

// A state that is read but cannot be parsed is `None`.
pub(super) fn read_state_with_arguments(
    tool: Tool,
    root: &Path,
    global_arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
) -> Result<Option<Vec<RelationEvidence>>, TerraformExecutionError> {
    let mut arguments = global_arguments.to_vec();
    arguments.extend([OsString::from("state"), OsString::from("pull")]);
    let output = run_successful(
        tool,
        root,
        TerraformCommand::StatePull,
        &arguments,
        cancellation,
        runner,
        None,
    )?;
    Ok(parse_state(&output.stdout))
}

fn parse_state(input: &[u8]) -> Option<Vec<RelationEvidence>> {
    let state = serde_json::from_slice::<Value>(input).ok()?;
    let resources = state.as_object()?.get("resources")?.as_array()?;
    let mut instances = Vec::new();
    for resource in resources {
        let resource = resource.as_object()?;
        let module = match resource.get("module") {
            Some(Value::String(module)) => parse_module_address(module)?,
            Some(Value::Null) | None => Vec::new(),
            _ => return None,
        };
        let mode = required_string(resource, "mode")?;
        if !matches!(mode, "managed" | "data") {
            return None;
        }
        let resource_type = required_string(resource, "type")?;
        let name = required_string(resource, "name")?;
        let block_key = format_resource_address(
            &unindexed_module_path(&module),
            mode,
            resource_type,
            name,
            None,
        );
        for instance in resource.get("instances")?.as_array()? {
            let instance = instance.as_object()?;
            let index = match instance.get("index_key") {
                Some(Value::String(value)) => Some(AddressIndex::String(value.clone())),
                Some(Value::Number(value)) => {
                    Some(AddressIndex::Number(value.as_u64()?.to_string()))
                }
                Some(Value::Null) | None => None,
                _ => return None,
            };
            let dependencies = parse_dependencies(instance)?;
            let address =
                format_resource_address(&module, mode, resource_type, name, index.as_ref());
            instances.push(StateInstance {
                address,
                block_key: block_key.clone(),
                modules: module.clone(),
                index,
                dependencies,
            });
        }
    }
    Some(relations_for_state(&instances))
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    object.get(field)?.as_str()
}

fn parse_dependencies(instance: &Map<String, Value>) -> Option<Vec<String>> {
    match instance.get("dependencies") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(dependencies)) => dependencies
            .iter()
            .map(|dependency| dependency.as_str().map(str::to_owned))
            .collect(),
        _ => None,
    }
}

struct StateInstance {
    address: String,
    block_key: String,
    modules: Vec<super::address::ModuleAddressSegment>,
    index: Option<AddressIndex>,
    dependencies: Vec<String>,
}

fn unindexed_module_path(modules: &[ModuleAddressSegment]) -> Vec<ModuleAddressSegment> {
    modules
        .iter()
        .map(|module| ModuleAddressSegment {
            name: module.name.clone(),
            index: None,
        })
        .collect()
}

fn dependency_block_key(address: &ResourceAddress) -> String {
    format_resource_address(
        &unindexed_module_path(&address.modules),
        &address.mode,
        &address.resource_type,
        &address.name,
        None,
    )
}

fn module_path_matches(
    candidate: &[ModuleAddressSegment],
    selectors: &[ModuleAddressSegment],
) -> bool {
    candidate.len() == selectors.len()
        && selectors.iter().zip(candidate).all(|(selector, actual)| {
            selector.name == actual.name
                && (selector.index.is_none() || selector.index == actual.index)
        })
}

fn relations_for_state(instances: &[StateInstance]) -> Vec<RelationEvidence> {
    let addresses = instances
        .iter()
        .map(|instance| instance.address.as_str())
        .collect::<BTreeSet<_>>();
    let mut instances_by_block = BTreeMap::<&str, Vec<&StateInstance>>::new();
    for instance in instances {
        instances_by_block
            .entry(&instance.block_key)
            .or_default()
            .push(instance);
    }
    let mut relations = Vec::new();
    for instance in instances {
        let dependent = RelationEndpoint::Instance(instance.address.clone());
        for dependency in &instance.dependencies {
            let Some(address) = parse_resource_address(dependency) else {
                relations.push(RelationEvidence::unresolved(
                    dependent.clone(),
                    RelationSource::State,
                    RelationUnresolvedReason::InvalidState,
                ));
                continue;
            };
            if let Some(index) = address.index.as_ref() {
                let full = address.full();
                if addresses.contains(full.as_str()) {
                    relations.push(RelationEvidence::resolved(
                        dependent.clone(),
                        RelationEndpoint::Instance(full),
                        RelationSource::State,
                    ));
                    continue;
                }
                let block_key = dependency_block_key(&address);
                let mut targets = instances_by_block
                    .get(block_key.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|candidate| {
                        candidate.index.as_ref() == Some(index)
                            && module_path_matches(&candidate.modules, &address.modules)
                    });
                match (targets.next(), targets.next()) {
                    (Some(target), None) => relations.push(RelationEvidence::resolved(
                        dependent.clone(),
                        RelationEndpoint::Instance(target.address.clone()),
                        RelationSource::State,
                    )),
                    (Some(_), Some(_)) => relations.push(RelationEvidence::resolved(
                        dependent.clone(),
                        RelationEndpoint::Block(full),
                        RelationSource::State,
                    )),
                    _ => relations.push(RelationEvidence::unresolved(
                        dependent.clone(),
                        RelationSource::State,
                        RelationUnresolvedReason::MissingAddress,
                    )),
                }
                continue;
            }
            let block = address.block();
            let block_key = dependency_block_key(&address);
            let mut targets = instances_by_block
                .get(block_key.as_str())
                .into_iter()
                .flatten()
                .filter(|candidate| module_path_matches(&candidate.modules, &address.modules));
            match (targets.next(), targets.next()) {
                (Some(target), None) if target.address == block => {
                    relations.push(RelationEvidence::resolved(
                        dependent.clone(),
                        RelationEndpoint::Instance(target.address.clone()),
                        RelationSource::State,
                    ));
                }
                (None, _) => relations.push(RelationEvidence::unresolved(
                    dependent.clone(),
                    RelationSource::State,
                    RelationUnresolvedReason::MissingAddress,
                )),
                _ => relations.push(RelationEvidence::resolved(
                    dependent.clone(),
                    RelationEndpoint::Block(block),
                    RelationSource::State,
                )),
            }
        }
    }
    relations.sort();
    relations.dedup();
    relations
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        io,
        path::PathBuf,
        rc::Rc,
    };

    use serde_json::json;

    use crate::infra::terraform::command::{ProcessOutput, ProcessStatus, RunningProcess};

    use super::*;

    #[expect(
        clippy::needless_pass_by_value,
        reason = "JSON fixture values are serialized directly into the parser input"
    )]
    fn parse(value: Value) -> Option<Vec<RelationEvidence>> {
        parse_state(value.to_string().as_bytes())
    }

    struct FakeRunner {
        output: Vec<u8>,
        status: ProcessStatus,
        calls: RefCell<Vec<(Tool, PathBuf, Vec<OsString>)>>,
        cancellation: Option<CancellationToken>,
        waits: Rc<Cell<usize>>,
        interrupts: Rc<Cell<usize>>,
    }

    struct FakeProcess {
        output: Vec<u8>,
        status: ProcessStatus,
        cancellation: Option<CancellationToken>,
        waits: Rc<Cell<usize>>,
        interrupts: Rc<Cell<usize>>,
    }

    impl ProcessRunner for FakeRunner {
        fn start(
            &self,
            tool: Tool,
            root: &Path,
            arguments: &[OsString],
        ) -> io::Result<Box<dyn RunningProcess>> {
            self.calls
                .borrow_mut()
                .push((tool, root.to_owned(), arguments.to_vec()));
            Ok(Box::new(FakeProcess {
                output: self.output.clone(),
                status: self.status,
                cancellation: self.cancellation.clone(),
                waits: Rc::clone(&self.waits),
                interrupts: Rc::clone(&self.interrupts),
            }))
        }
    }

    impl RunningProcess for FakeProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>> {
            if let Some(cancellation) = &self.cancellation {
                cancellation.cancel();
                Ok(None)
            } else {
                Ok(Some(self.status))
            }
        }

        fn request_interrupt(&mut self) -> io::Result<()> {
            self.interrupts.set(self.interrupts.get() + 1);
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessStatus> {
            self.waits.set(self.waits.get() + 1);
            Ok(ProcessStatus::Signaled)
        }

        fn collect_output(self: Box<Self>) -> io::Result<ProcessOutput> {
            Ok(ProcessOutput::new(self.output, Vec::new()))
        }
    }

    fn runner(output: &[u8], status: ProcessStatus) -> FakeRunner {
        FakeRunner {
            output: output.to_vec(),
            status,
            calls: RefCell::new(Vec::new()),
            cancellation: None,
            waits: Rc::new(Cell::new(0)),
            interrupts: Rc::new(Cell::new(0)),
        }
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "JSON fixture values are moved into the synthetic state document"
    )]
    fn instance(index_key: Option<Value>, dependencies: Value) -> Value {
        let mut value = json!({"dependencies": dependencies});
        if let Some(index_key) = index_key {
            value["index_key"] = index_key;
        }
        value
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "JSON fixture values are moved into the synthetic state document"
    )]
    fn resource(module: Option<&str>, name: &str, instances: Vec<Value>) -> Value {
        let mut resource = json!({
            "mode": "managed",
            "type": "terraform_data",
            "name": name,
            "instances": instances,
        });
        if let Some(module) = module {
            resource["module"] = json!(module);
        }
        resource
    }

    #[test]
    fn reads_only_state_addresses_and_classifies_exact_and_block_dependencies() {
        let state = json!({
            "resources": [
                resource(None, "single", vec![{
                    let mut instance = instance(None, json!([]));
                    instance["attributes"] = json!({"token": "synthetic-secret"});
                    instance
                }]),
                resource(None, "many", vec![
                    instance(Some(json!(0)), json!([])),
                    instance(Some(json!(1)), json!([])),
                ]),
                resource(None, "dependent", vec![instance(None, json!([
                    "terraform_data.single", "terraform_data.many", "terraform_data.many[1]"
                ]))]),
            ]
        });

        let relations = parse(state).expect("state should parse");

        assert_eq!(relations.len(), 3);
        assert!(relations.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "terraform_data.single" && target.is_instance()
            })
        }));
        assert!(relations.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "terraform_data.many" && !target.is_instance()
            })
        }));
        assert!(relations.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "terraform_data.many[1]" && target.is_instance()
            })
        }));
        assert!(!format!("{relations:?}").contains("attributes"));
        assert!(!format!("{relations:?}").contains("synthetic-secret"));
    }

    #[test]
    fn resolves_state_block_dependencies_with_omitted_repeated_module_indexes() {
        let state = json!({
            "resources": [
                resource(Some("module.child[0]"), "inside", vec![instance(None, json!([]))]),
                resource(Some("module.child[1]"), "inside", vec![instance(None, json!([]))]),
                resource(None, "waiter", vec![instance(None, json!([
                    "module.child.terraform_data.inside"
                ]))]),
            ]
        });

        let relations = parse(state).expect("repeated module block dependency should parse");

        assert!(relations.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.waiter"
                && edge.referenced.as_ref().is_some_and(|target| {
                    target.address() == "module.child.terraform_data.inside"
                        && !target.is_instance()
                })
        }));
        assert!(!relations.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.waiter"
                && edge.unresolved == Some(RelationUnresolvedReason::MissingAddress)
        }));
    }

    #[test]
    fn retains_module_instance_addresses_and_marks_unresolvable_state_format_unavailable() {
        let state = json!({
            "resources": [
                resource(Some("module.child[\"blue\"]"), "source", vec![instance(None, json!([]))]),
                resource(None, "dependent", vec![instance(None, json!(["module.child[\"blue\"].terraform_data.source"]))]),
            ]
        });

        let relations = parse(state).expect("state should parse");

        assert!(relations.iter().any(|edge| {
            edge.referenced.as_ref().is_some_and(|target| {
                target.address() == "module.child[\"blue\"].terraform_data.source"
                    && target.is_instance()
            })
        }));
        assert!(parse(json!({"resources": [{"instances": "bad"}]})).is_none());
        for index in [json!(1.0), json!(1.5), json!(-1)] {
            assert!(parse(json!({
                "resources": [resource(None, "invalid_index", vec![instance(Some(index), json!([]))])]
            })).is_none());
        }
        let mut unknown_mode = resource(None, "unknown_mode", vec![instance(None, json!([]))]);
        unknown_mode["mode"] = json!("future");
        assert!(parse(json!({"resources": [unknown_mode]})).is_none());
        assert!(
            parse(json!({"resources": []}))
                .expect("empty state is valid")
                .is_empty()
        );
    }

    #[test]
    fn malformed_dependency_identifiers_remain_unresolved() {
        let state = json!({
            "resources": [
                resource(None, "single", vec![instance(None, json!([]))]),
                resource(None, "dependent", vec![instance(None, json!([
                    "terraform_data.single.id"
                ]))]),
            ]
        });

        let relations = parse(state).expect("state shape should parse");

        assert!(relations.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.dependent"
                && edge.source == RelationSource::State
                && edge.unresolved == Some(RelationUnresolvedReason::InvalidState)
        }));
        assert!(!relations.iter().any(|edge| {
            edge.dependent.address() == "terraform_data.dependent"
                && edge
                    .referenced
                    .as_ref()
                    .is_some_and(|target| target.address() == "terraform_data.single")
        }));
    }

    #[test]
    fn runs_state_pull_with_the_plan_root_and_global_arguments() {
        let runner = runner(br#"{"resources":[]}"#, ProcessStatus::Exited(0));
        let root = Path::new("/workspace with spaces");
        let arguments = [OsString::from("-chdir=/workspace with spaces")];

        let Ok(Some(relations)) = read_state_with_arguments(
            Tool::OpenTofu,
            root,
            &arguments,
            &CancellationToken::new(),
            &runner,
        ) else {
            panic!("state should be read");
        };

        assert!(relations.is_empty());
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, Tool::OpenTofu);
        assert_eq!(calls[0].1, root);
        assert_eq!(
            calls[0].2,
            [
                OsString::from("-chdir=/workspace with spaces"),
                OsString::from("state"),
                OsString::from("pull"),
            ]
        );
    }

    #[test]
    fn state_pull_failure_does_not_expose_process_output() {
        let runner = runner(b"synthetic-secret state data", ProcessStatus::Exited(1));

        let Err(error) = read_state_with_arguments(
            Tool::Terraform,
            Path::new("/workspace"),
            &[],
            &CancellationToken::new(),
            &runner,
        ) else {
            panic!("failed state pull should be an execution error");
        };

        assert!(!format!("{error}").contains("synthetic-secret"));
    }

    #[test]
    fn successful_state_pull_with_malformed_output_is_invalid_format() {
        let runner = runner(br#"{"resources":"synthetic"}"#, ProcessStatus::Exited(0));

        let result = read_state_with_arguments(
            Tool::Terraform,
            Path::new("/workspace"),
            &[],
            &CancellationToken::new(),
            &runner,
        );

        assert!(matches!(result, Ok(None)));
    }

    #[test]
    fn cancellation_interrupts_and_reaps_state_pull() {
        let cancellation = CancellationToken::new();
        let mut runner = runner(b"synthetic-secret", ProcessStatus::Exited(0));
        runner.cancellation = Some(cancellation.clone());

        let result = read_state_with_arguments(
            Tool::Terraform,
            Path::new("/workspace"),
            &[],
            &cancellation,
            &runner,
        );

        assert!(matches!(result, Err(error) if error.is_interrupted()));
        assert_eq!(runner.interrupts.get(), 1);
        assert_eq!(runner.waits.get(), 1);
    }
}
