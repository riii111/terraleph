#!/usr/bin/env python3

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


FIXTURES = Path(__file__).resolve().parent
MARKER = ".terraleph-basic-scenario"
EXPECTED_ACTIONS = {
    "terraform_data.api": ["update"],
    "terraform_data.worker": ["delete", "create"],
    "terraform_data.old": ["delete"],
    "terraform_data.new": ["create"],
    "terraform_data.pending": ["update"],
}


def setup(tool="terraform"):
    for executable in ("git", tool):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Required executable not found: {executable}")

    directory = new_scenario("terraleph-basic-")
    environment = isolated_environment(directory)
    try:
        apply_baseline(
            directory,
            environment,
            tool,
            FIXTURES / "baseline",
            "test: establish applied baseline",
        )

        shutil.copyfile(FIXTURES / "changes" / "pending.tf", directory / "pending.tf")
        run(directory, environment, "git", "add", "pending.tf")
        commit(directory, environment, "test: leave committed change unapplied")
        shutil.copyfile(FIXTURES / "changes" / "main.tf", directory / "main.tf")
        save_plan(directory, environment, tool, validate_basic_plan)
    except BaseException:
        shutil.rmtree(directory)
        raise

    print("Verified: create 1 / update 2 / replace 1 / delete 1", file=sys.stderr)
    return directory


def contains_true(value):
    if value is True:
        return True
    if isinstance(value, dict):
        return any(contains_true(item) for item in value.values())
    if isinstance(value, list):
        return any(contains_true(item) for item in value)
    return False


def validate_group_expansion_plan(plan):
    if not isinstance(plan, dict):
        raise RuntimeError("Terraform plan JSON is not an object")
    resource_changes = plan.get("resource_changes")
    if not isinstance(resource_changes, list):
        raise RuntimeError("Terraform plan has no resource_changes list")

    changes = [
        resource
        for resource in resource_changes
        if isinstance(resource, dict)
        and isinstance(resource.get("change"), dict)
        and resource["change"].get("actions") != ["no-op"]
    ]
    addresses = {"time_sleep.server[0]", "time_sleep.server[1]"}
    if (
        len(changes) != 2
        or {resource.get("address") for resource in changes} != addresses
    ):
        raise RuntimeError("Expected changes only for time_sleep.server[0] and [1]")

    expected_attributes = {"create_duration", "destroy_duration"}
    for resource in changes:
        address = resource["address"]
        change = resource["change"]
        if change.get("actions") != ["update"]:
            raise RuntimeError(f"Expected an in-place update for {address}")
        if any(
            contains_true(change.get(field))
            for field in ("after_unknown", "before_sensitive", "after_sensitive")
        ):
            raise RuntimeError(
                f"Plan contains unknown or sensitive values for {address}"
            )

        before = change.get("before")
        after = change.get("after")
        if not isinstance(before, dict) or not isinstance(after, dict):
            raise RuntimeError(
                f"Plan has no comparable before/after values for {address}"
            )
        changed_attributes = {
            name
            for name in before.keys() | after.keys()
            if before.get(name) != after.get(name)
        }
        if changed_attributes != expected_attributes:
            raise RuntimeError(
                f"Unexpected changed attributes for {address}: {sorted(changed_attributes)}"
            )
        if any(
            before.get(name) != "0s" or after.get(name) != "1s"
            for name in expected_attributes
        ):
            raise RuntimeError(f"Unexpected duration values for {address}")


def setup_group_expansion(tool="terraform"):
    for executable in ("git", tool):
        if shutil.which(executable) is None:
            raise RuntimeError(f"Required executable not found: {executable}")

    directory = new_scenario("terraleph-basic-group-")
    environment = isolated_environment(directory)
    try:
        apply_baseline(
            directory,
            environment,
            tool,
            FIXTURES / "group-expansion" / "baseline.tf",
            "test: establish repeated-resource baseline",
        )
        shutil.copyfile(
            FIXTURES / "group-expansion" / "changes.tf", directory / "main.tf"
        )
        save_plan(directory, environment, tool, validate_group_expansion_plan)
    except BaseException:
        shutil.rmtree(directory)
        raise

    print("Verified: 2 known in-place time_sleep updates", file=sys.stderr)
    return directory


def new_scenario(prefix):
    directory = Path(tempfile.mkdtemp(prefix=prefix)).resolve()
    (directory / MARKER).write_text(str(directory) + "\n")
    return directory


def apply_baseline(directory, environment, tool, baseline, message):
    if baseline.is_dir():
        sources = baseline.glob("*.tf")
        for source in sources:
            shutil.copyfile(source, directory / source.name)
    else:
        shutil.copyfile(baseline, directory / "main.tf")

    (directory / ".gitignore").write_text(
        ".terraform/\n*.tfstate\n*.tfstate.*\n*.tfplan\n*.tfplan.json\n"
        ".terraform.lock.hcl\n" + MARKER + "\n"
    )
    run(directory, environment, "git", "init", "--initial-branch=main")
    run(directory, environment, "git", "add", ".")
    commit(directory, environment, message)
    run(directory, environment, tool, "init", "-input=false", "-no-color")
    run(
        directory,
        environment,
        tool,
        "apply",
        "-auto-approve",
        "-input=false",
        "-no-color",
    )


def save_plan(directory, environment, tool, validate):
    run(
        directory,
        environment,
        tool,
        "plan",
        "-input=false",
        "-no-color",
        "-out=review.tfplan",
    )
    plan_json = run(directory, environment, tool, "show", "-json", "review.tfplan")
    validate(json.loads(plan_json))
    changed = run(directory, environment, "git", "diff", "--name-only", "HEAD")
    if changed.splitlines() != ["main.tf"]:
        raise RuntimeError(f"Unexpected Git changes: {changed}")
    (directory / "review.tfplan.json").write_text(plan_json)


def validate_basic_plan(plan):
    actual = {
        change["address"]: change["change"]["actions"]
        for change in plan["resource_changes"]
        if change["change"]["actions"] != ["no-op"]
    }
    if actual != EXPECTED_ACTIONS:
        raise RuntimeError(f"Unexpected plan actions: {actual}")


def isolated_environment(directory):
    # Ambient Git and CLI options must not redirect this disposable scenario.
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("GIT_", "TF_", "TOFU_"))
    }
    environment.update(
        {
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "TF_IN_AUTOMATION": "1",
            "TF_DATA_DIR": str(directory / ".terraform"),
            "TF_CLI_CONFIG_FILE": os.devnull,
            "CHECKPOINT_DISABLE": "1",
        }
    )
    return environment


def run(directory, environment, *command):
    print("Running: " + " ".join(command), file=sys.stderr, flush=True)
    result = subprocess.run(
        command,
        cwd=directory,
        env=environment,
        text=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode:
        raise RuntimeError(result.stdout + result.stderr)
    return result.stdout


def commit(directory, environment, message):
    run(
        directory,
        environment,
        "git",
        "-c",
        "user.name=Terraleph Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "-c",
        f"core.hooksPath={os.devnull}",
        "commit",
        "-m",
        message,
    )


def clean(directory):
    if directory.is_symlink():
        raise RuntimeError("Refusing to remove a symlink")
    directory = directory.resolve()
    marker = directory / MARKER
    if (
        not directory.name.startswith("terraleph-basic-")
        or not marker.is_file()
        or marker.is_symlink()
        or marker.read_text() != str(directory) + "\n"
    ):
        raise RuntimeError("Not a scenario directory created by this script")
    shutil.rmtree(directory)
    print(f"Removed: {directory}")


def accept(tool):
    for create in (setup, setup_group_expansion):
        directory = create(tool)
        try:
            print(f"Verified {directory.name} with {tool}")
        finally:
            clean(directory)


def main():
    parser = argparse.ArgumentParser(
        description="Verify plans and provide fixtures to integration tests."
    )
    commands = parser.add_subparsers(dest="command", required=True)
    acceptance = commands.add_parser("accept")
    acceptance.add_argument("--tool", choices=("terraform", "tofu"), required=True)
    tests = commands.add_parser("test", help="Manage fixtures for integration tests.")
    test_commands = tests.add_subparsers(dest="test_command", required=True)
    test_commands.add_parser("setup")
    clean_command = test_commands.add_parser("clean")
    clean_command.add_argument("directory", type=Path)
    args = parser.parse_args()

    if args.command == "accept":
        accept(args.tool)
    elif args.test_command == "setup":
        print(setup())
    else:
        clean(args.directory)


if __name__ == "__main__":
    main()
