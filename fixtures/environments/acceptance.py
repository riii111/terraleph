#!/usr/bin/env python3
"""Run the cloudless multi-environment PTY acceptance with a real CLI."""

import argparse
from collections import Counter
import json
import os
from pathlib import Path
import shutil
import subprocess

import environment

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--tool", choices=("terraform", "tofu"), default="terraform")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    executable = shutil.which(args.tool)
    if executable is None:
        raise RuntimeError(f"Missing {args.tool}")
    directory = environment.setup(args.tool)
    try:
        before = {
            name: (directory / name / "terraform.tfstate").read_bytes()
            for name in environment.NAMES
        }
        support = directory / ".acceptance"
        support.mkdir()
        owned = support / "plans"
        owned.mkdir()
        wrappers = support / "bin"
        wrappers.mkdir()
        log = support / "commands.jsonl"
        gate = support / "release-plan"
        wrapper = wrappers / args.tool
        wrapper.write_text(
            "#!/usr/bin/env python3\n"
            "import json,os,subprocess,sys,time\n"
            "from pathlib import Path\n"
            f"with open({str(log)!r}, 'a') as log: log.write(json.dumps([Path.cwd().name, *sys.argv[1:]]) + '\\n')\n"
            f"gate = Path({str(gate)!r})\n"
            "if Path.cwd().name == 'prod' and sys.argv[1] == 'plan':\n"
            "    count_file = gate.parent / 'prod-plan-count'\n"
            "    count = int(count_file.read_text()) + 1 if count_file.exists() else 1\n"
            "    count_file.write_text(str(count))\n"
            "    if count == 1:\n"
            "        deadline = time.monotonic() + 60\n"
            "        while not gate.exists():\n"
            "            if time.monotonic() > deadline: sys.exit('PTY did not release the plan')\n"
            "            time.sleep(0.05)\n"
            f"    result = subprocess.run([{executable!r}, *sys.argv[1:]])\n"
            "    gate.with_name(f'{gate.name}-{count}').write_text(str(result.returncode))\n"
            "    sys.exit(result.returncode)\n"
            f"os.execv({executable!r}, [{executable!r}, *sys.argv[1:]])\n"
        )
        wrapper.chmod(0o700)
        env = environment.environment()
        env["PATH"] = str(wrappers) + os.pathsep + env["PATH"]
        env["TMPDIR"] = str(owned)
        env["TERRALEPH_REAL_PLAN_GATE"] = str(gate)
        result = subprocess.run(
            [
                "python3",
                str(ROOT / "tests/support/cli/pty_driver.py"),
                str(binary),
                str(directory),
                "120",
                "40",
                "env_real",
                args.tool,
                "plan",
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=120,
        )
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)
        output_lines = result.stdout.splitlines()
        if (
            "exit=0" not in output_lines
            or "restored=true" not in output_lines
            or "cursor_restored=true" not in output_lines
        ):
            raise RuntimeError(result.stdout)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        prod_plan_results = [
            int((support / f"release-plan-{index}").read_text()) for index in (1, 2)
        ]
        plans = Counter(call[0] for call in calls if call[1] == "plan")
        inits = Counter(call[0] for call in calls if call[1] == "init")
        assert plans == {"dev": 1, "prod": 2, "stg": 1}, plans
        assert prod_plan_results[0] != 0, prod_plan_results
        assert prod_plan_results[1] == 2, prod_plan_results
        assert inits == {"dev": 1, "prod": 1, "stg": 1}, inits
        assert not any(call[1] == "apply" for call in calls)
        assert not list(owned.iterdir()), list(owned.iterdir())
        assert before == {
            name: (directory / name / "terraform.tfstate").read_bytes()
            for name in environment.NAMES
        }
        print(
            f"{args.tool}: init 3, plan dev=1/prod=2/stg=1; state unchanged; temporary plans removed"
        )
        print(result.stdout.strip())
    finally:
        environment.clean(directory)
        print("Scenario removed")


if __name__ == "__main__":
    main()
