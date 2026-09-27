#!/usr/bin/env python3
"""Verify the Terraform JSON and state shapes used by relationship collection."""

import argparse
import json
import shutil
import subprocess
import tempfile
from pathlib import Path


FIXTURE_ROOT = Path(__file__).resolve().parent


def run(tool: str, root: Path, *arguments: str) -> str:
    result = subprocess.run(
        [tool, f"-chdir={root}", *arguments],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        raise RuntimeError(f"{Path(tool).name} {arguments[0]} failed ({result.returncode})")
    return result.stdout


def references(value: object) -> list[str]:
    found: list[str] = []
    if isinstance(value, dict):
        refs = value.get("references")
        if isinstance(refs, list):
            found.extend(item for item in refs if isinstance(item, str))
        for key, child in value.items():
            if key != "references":
                found.extend(references(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(references(child))
    return found


def resources(module: dict[str, object]) -> dict[str, dict[str, object]]:
    result = {
        str(resource["name"]): resource
        for resource in module.get("resources", [])
        if isinstance(resource, dict) and isinstance(resource.get("name"), str)
    }
    for call in module.get("module_calls", {}).values():
        if isinstance(call, dict) and isinstance(call.get("module"), dict):
            result.update(resources(call["module"]))
    return result


def value_addresses(module: dict[str, object]) -> set[str]:
    found = {
        str(resource["address"])
        for resource in module.get("resources", [])
        if isinstance(resource, dict) and isinstance(resource.get("address"), str)
    }
    for child in module.get("child_modules", []):
        if isinstance(child, dict):
            found.update(value_addresses(child))
    return found


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(f"relationship fixture failed: {message}")


def verify(tool: str) -> str:
    version = json.loads(run(tool, Path.cwd(), "version", "-json"))["terraform_version"]
    with tempfile.TemporaryDirectory(prefix="terraleph-relations-") as temporary:
        root = Path(temporary)
        shutil.copytree(FIXTURE_ROOT, root, dirs_exist_ok=True, ignore=shutil.ignore_patterns("scenario.py", "README.md"))

        run(tool, root, "init", "-input=false", "-no-color", "-backend=false")
        initial_plan = root / "initial.tfplan"
        run(tool, root, "plan", "-input=false", "-no-color", f"-out={initial_plan}")
        initial_document = json.loads(run(tool, root, "show", "-json", str(initial_plan)))
        require(initial_document.get("prior_state") is None, "a new plan has no prior state")
        run(tool, root, "apply", "-input=false", "-no-color", "-auto-approve", str(initial_plan))

        (root / "stateful.tf").unlink()
        saved_plan = root / "review.tfplan"
        run(tool, root, "plan", "-input=false", "-no-color", f"-out={saved_plan}")
        document = json.loads(run(tool, root, "show", "-json", str(saved_plan)))
        state = json.loads(run(tool, root, "state", "pull"))

        config = document["configuration"]["root_module"]
        config_resources = resources(config)
        required_resources = {
            "source",
            "other_source",
            "counted",
            "foreach",
            "direct",
            "explicit",
            "indexed",
            "mixed",
            "root_variable",
            "conditional",
            "count_expression",
            "foreach_expression",
            "inside",
            "module_dependent",
            "module_output_consumer",
            "merged_evidence",
        }
        require(required_resources <= config_resources.keys(), "configuration addresses are present")

        indexed_refs = references(config_resources["indexed"].get("expressions"))
        require("terraform_data.counted[1].id" in indexed_refs, "the exact instance reference is recorded")
        require("terraform_data.counted[1]" in indexed_refs, "the indexed containing reference is recorded")
        require("terraform_data.counted" in indexed_refs, "the block containing reference is recorded")
        require(
            config_resources["explicit"].get("depends_on") == ["terraform_data.source"],
            "explicit depends_on is recorded",
        )
        require(
            config_resources["module_dependent"].get("depends_on") == ["module.child"],
            "module depends_on is recorded",
        )
        require("terraform_data.source.id" in references(config_resources["direct"].get("expressions")), "direct references are recorded")
        mixed_refs = references(config_resources["mixed"].get("expressions"))
        require("terraform_data.source.id" in mixed_refs and "local.unresolved" in mixed_refs, "known and local references coexist")
        require("var.root_only" in references(config_resources["root_variable"].get("expressions")), "root variable references remain explicit")
        conditional_refs = references(config_resources["conditional"].get("expressions"))
        require(
            "terraform_data.source.id" in conditional_refs
            and "terraform_data.other_source.id" in conditional_refs,
            "both conditional branches are recorded",
        )
        require(
            "terraform_data.source.input" in references(config_resources["count_expression"].get("count_expression")),
            "count expression references are recorded",
        )
        require(
            "terraform_data.source.input" in references(config_resources["foreach_expression"].get("for_each_expression")),
            "for_each expression references are recorded",
        )

        child_call = config["module_calls"]["child"]
        require(
            "terraform_data.source.id" in references(child_call.get("expressions")),
            "module call input references are recorded",
        )
        child = child_call["module"]
        require("var.input" in references(child["resources"][0].get("expressions")), "child variable reference is recorded")
        require("terraform_data.inside.id" in references(child["outputs"]["output"]), "child output references are recorded")
        require("module.child.output" in references(config_resources["module_output_consumer"].get("expressions")), "module output use is recorded")

        planned_addresses = value_addresses(document["planned_values"]["root_module"])
        require("terraform_data.counted[0]" in planned_addresses, "count instance zero is present")
        require("terraform_data.counted[1]" in planned_addresses, "count instance one is present")
        require('terraform_data.foreach["first"]' in planned_addresses, "for_each keys are preserved")
        require("module.child.terraform_data.inside" in planned_addresses, "module paths are preserved")

        state_addresses = {
            f"{resource.get('module') + '.' if resource.get('module') else ''}{resource['type']}.{resource['name']}"
            for resource in state["resources"]
        }
        require("terraform_data.state_target" not in config_resources, "deleted resources are absent from configuration")
        require(
            {"terraform_data.state_target", "terraform_data.state_dependent"} <= state_addresses,
            "deleted resources remain identifiable in current state",
        )
        dependent_state = next(
            resource
            for resource in state["resources"]
            if resource["name"] == "state_dependent"
        )
        dependencies = [
            dependency
            for instance in dependent_state["instances"]
            for dependency in instance.get("dependencies", [])
        ]
        require("terraform_data.state_target" in dependencies, "state dependencies survive configuration deletion")

        prior_addresses = {
            resource["address"]
            for resource in document["prior_state"]["values"]["root_module"]["resources"]
        }
        require("terraform_data.state_target" in prior_addresses, "the saved plan records prior state")

        merged_refs = references(config_resources["merged_evidence"].get("expressions"))
        merged_state = next(
            resource
            for resource in state["resources"]
            if resource["name"] == "merged_evidence"
        )
        merged_dependencies = [
            dependency
            for instance in merged_state["instances"]
            for dependency in instance.get("dependencies", [])
        ]
        require("terraform_data.source.id" in merged_refs, "configuration evidence remains on the merged node")
        require("terraform_data.source" in merged_dependencies, "state evidence remains on the merged node")
        return version


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tool", choices=("terraform", "tofu"), default="terraform")
    arguments = parser.parse_args()
    tool = shutil.which(arguments.tool)
    if not tool:
        raise SystemExit(f"{arguments.tool} was not found on PATH")
    try:
        version = verify(tool)
    except (KeyError, OSError, RuntimeError, TypeError, ValueError) as error:
        raise SystemExit(str(error)) from None
    print(f"PASS {arguments.tool} {version}: relationship JSON and state fixture")


if __name__ == "__main__":
    main()
