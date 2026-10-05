#!/usr/bin/env python3
"""Verify real cloudless plan, state, diagnostics and apply failures without a TTY."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import time

import scenario


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def execute(directory, tool, *args):
    return subprocess.run(
        [tool, *args],
        cwd=directory,
        env=scenario.basic.isolated_environment(directory),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=120,
    )


def accept(name, tool):
    start = time.monotonic()
    directory = scenario.setup(name, tool)
    try:
        if name == "diagnostics":
            for kind, expected, message in (
                ("warning", 0, "Synthetic check warning"),
                ("error", 1, "Synthetic precondition error"),
            ):
                result = execute(
                    directory / kind, tool, "plan", "-input=false", "-no-color"
                )
                require(
                    result.returncode == expected
                    and message in result.stdout + result.stderr,
                    f"{kind} diagnostic missing: {result.stdout}{result.stderr}",
                )
        else:
            plan_directory = directory / "dev" if name == "sensitive" else directory
            result = execute(plan_directory, tool, "show", "-json", "review.tfplan")
            require(result.returncode == 0, result.stderr)
            plan = json.loads(result.stdout)
            changes = {
                item["address"]: item["change"] for item in plan["resource_changes"]
            }
            if name == "sensitive":
                nested = changes["terraform_data.nested"]
                require(
                    scenario.basic.contains_true(nested["before_sensitive"])
                    and scenario.basic.contains_true(nested["after_sensitive"]),
                    "Nested sensitivity missing",
                )
                require(
                    changes["random_password.credential"]["before_sensitive"].get(
                        "result"
                    )
                    is True,
                    "Password sensitivity missing",
                )
                require(
                    plan["output_changes"]["secret"]["after_sensitive"] is True
                    and plan["output_changes"]["nested"]["after_sensitive"] is True,
                    "Output sensitivity missing",
                )
                require(
                    plan["variables"]["secret"]["value"]
                    == "synthetic-secret-never-use",
                    "Sensitive input missing",
                )
            elif name == "large":
                require(
                    len(changes) == 300
                    and all(
                        change["actions"] == ["update"] for change in changes.values()
                    ),
                    "Expected 300 updates",
                )
                require(
                    all(
                        len(change["after"]["input"]["description"]) > 1000
                        for change in changes.values()
                    ),
                    "Long attributes missing",
                )
            elif name == "failure":
                result = execute(
                    directory, tool, "apply", "-json", "-input=false", "review.tfplan"
                )
                require(
                    result.returncode != 0
                    and "synthetic apply failure" in result.stdout + result.stderr,
                    "Apply failure missing",
                )
                events = [json.loads(line) for line in result.stdout.splitlines()]
                completed = {
                    event.get("hook", {}).get("resource", {}).get("addr")
                    for event in events
                    if event.get("type") == "apply_complete"
                }
                failed = {
                    event.get("hook", {}).get("resource", {}).get("addr")
                    for event in events
                    if event.get("type") == "apply_errored"
                }
                require(
                    {f"terraform_data.success[{n}]" for n in range(3)} <= completed
                    and "terraform_data.failure" in failed,
                    "Success/failure progress events missing",
                )
            else:
                env = scenario.basic.isolated_environment(directory)
                scenario.install_apply_hook(directory, tool, env, name)
                result = subprocess.run(
                    [tool, "apply", "-input=false", "-no-color", "review.tfplan"],
                    cwd=directory,
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=120,
                )
                message = (
                    "Error acquiring the state lock"
                    if name == "lock"
                    else "Saved plan is stale"
                )
                require(
                    result.returncode != 0 and message in result.stdout + result.stderr,
                    f"Expected {message}: {result.stdout}{result.stderr}",
                )
                if name == "lock":
                    retry = execute(
                        directory, tool, "plan", "-input=false", "-no-color"
                    )
                    require(
                        retry.returncode == 0,
                        "Lock holder was not released: " + retry.stderr,
                    )
                else:
                    state = json.loads(execute(directory, tool, "state", "pull").stdout)
                    inputs = {
                        item["name"]: item["instances"][0]["attributes"]["input"][
                            "value"
                        ]
                        for item in state["resources"]
                    }
                    require(
                        inputs == {"api": "baseline", "drift": "external-change"},
                        "Stale plan changed state",
                    )
        print(f"PASS {tool} {name} ({time.monotonic() - start:.1f}s)")
    finally:
        scenario.clean(directory)


def accept_tui(name, tool, binary):
    directory = scenario.setup(name, tool)
    try:
        env = scenario.basic.isolated_environment(directory)
        if name in ("sensitive", "diagnostics"):
            env.pop("TF_DATA_DIR", None)
        env.pop("TF_IN_AUTOMATION", None)
        env.pop("CI", None)
        if name in ("lock", "stale"):
            scenario.install_apply_hook(directory, tool, env, name)
        driver = scenario.FIXTURES.parents[1] / "tests/support/cli/pty_driver.py"
        result = subprocess.run(
            [
                sys.executable,
                str(driver),
                str(binary),
                str(directory),
                "120",
                "40",
                f"cloudless_apply_{name}"
                if name in ("failure", "lock", "stale")
                else f"cloudless_{name}",
                tool,
                "plan",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        require(result.returncode == 0, result.stdout + result.stderr)
        lines = result.stdout.splitlines()
        expected_exit = 1 if name in ("failure", "lock", "stale", "diagnostics") else 0
        require(
            f"exit={expected_exit}" in lines
            and "restored=true" in lines
            and "cursor_restored=true" in lines,
            "CLI failure or terminal restoration missing: " + result.stdout,
        )
        if "sensitive_value_visible=true" in lines:
            print(
                f"OBSERVED {tool} sensitive: synthetic secret appeared in terminal output"
            )
        print(f"PASS {tool} {name} real CLI: expected exit and terminal restored")
    finally:
        scenario.clean(directory)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("names", nargs="*", choices=tuple(scenario.VIEWS))
    parser.add_argument("--tool", choices=("terraform", "tofu"), default="terraform")
    parser.add_argument(
        "--binary",
        type=Path,
        help="Also check real CLI apply failures and terminal restoration (POSIX).",
    )
    args = parser.parse_args()
    for name in args.names or scenario.VIEWS:
        accept(name, args.tool)
        if args.binary:
            accept_tui(name, args.tool, args.binary.resolve(strict=True))


if __name__ == "__main__":
    main()
