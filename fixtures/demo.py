#!/usr/bin/env python3

import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from basic import plan as single_fixture
from environments import environment as multi_fixture
from cloudless import scenario as cloudless_fixture

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description="Open a cloudless Terraleph demo.")
    parser.add_argument(
        "mode", nargs="?", choices=("single", "multi", *cloudless_fixture.VIEWS)
    )
    parser.add_argument(
        "--list", action="store_true", help="List scenarios and verification points."
    )
    parser.add_argument("--tool", choices=("terraform", "tofu"), default="terraform")
    args = parser.parse_args()
    if args.list:
        for name, view in {
            "single": "単一環境のplanレビューとapply",
            "multi": "複数環境の選択とplanレビュー",
            **cloudless_fixture.VIEWS,
        }.items():
            print(f"{name}: {view}")
        return 0
    if args.mode is None:
        parser.error("a scenario or --list is required")
    return run_demo(args.mode, args.tool)


def run_demo(mode, tool="terraform"):
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        raise RuntimeError("The demo requires an interactive terminal")

    fixture = {"single": single_fixture, "multi": multi_fixture}.get(
        mode, cloudless_fixture
    )
    if mode in cloudless_fixture.VIEWS:
        print(cloudless_fixture.VIEWS[mode], file=sys.stderr, flush=True)
    print(
        f"Starting Terraleph {mode} demo. Ctrl-C to cancel.",
        file=sys.stderr,
        flush=True,
    )
    print("[1/3] Preparing local Terraform scenario...", file=sys.stderr, flush=True)
    if mode == "single":
        directory = single_fixture.setup(tool)
    elif mode == "multi":
        directory = multi_fixture.setup(tool, ready=True)
    else:
        directory = cloudless_fixture.setup(mode, tool)
    demo_root = None
    scenario = directory
    try:
        if mode == "single":
            # The directory name is the typed apply confirmation, so the demo
            # runs from a short fixed name instead of the random temp name.
            demo_root = Path(tempfile.mkdtemp(prefix="terraleph-demo-")).resolve()
            directory = directory.rename(demo_root / "demo")
        target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target" / "demo"))
        if not target.is_absolute():
            target = ROOT / target
        target = target.resolve()
        environment = (
            single_fixture.isolated_environment(directory)
            if mode not in ("multi", "diagnostics", "sensitive")
            else multi_fixture.environment()
        )
        environment.update({"CARGO_TARGET_DIR": str(target), "RUSTC_WRAPPER": ""})
        print(
            f"[2/3] Building Terraleph (cache: {target})...",
            file=sys.stderr,
            flush=True,
        )
        build = subprocess.run(
            ["cargo", "build", "--locked"],
            cwd=ROOT,
            env=environment,
            stdin=subprocess.DEVNULL,
        )
        if build.returncode:
            return build.returncode

        executable = (
            target / "debug" / ("terraleph.exe" if os.name == "nt" else "terraleph")
        )
        command = [str(executable)]
        if mode not in ("single", "multi"):
            if mode in ("lock", "stale"):
                cloudless_fixture.install_apply_hook(directory, tool, environment, mode)
            environment.pop("TF_IN_AUTOMATION", None)
            environment.pop("CI", None)
            command.extend((tool, "plan"))
            print(f"[3/3] Opening the {mode} scenario...", file=sys.stderr, flush=True)
        elif tool == "tofu" and mode == "single":
            command.extend((tool, "plan"))
        if mode == "single":
            install_apply_delay(directory, environment, tool)
            environment.pop("TF_IN_AUTOMATION", None)
            print(
                "[3/3] Opening the single-environment Overview...",
                file=sys.stderr,
                flush=True,
            )
        elif mode == "multi":
            environment.pop("CI", None)
            command.extend((tool, f"-chdir={directory}", "plan"))
            print(
                "[3/3] Opening the multi-environment Overview...",
                file=sys.stderr,
                flush=True,
            )
        return subprocess.run(command, cwd=directory, env=environment).returncode
    finally:
        if demo_root is not None:
            if directory != scenario:
                directory.rename(scenario)
            demo_root.rmdir()
        fixture.clean(scenario)


def install_apply_delay(directory, environment, tool="terraform"):
    if os.name == "nt":
        return

    terraform = shutil.which(tool)
    if terraform is None:
        raise RuntimeError(f"Required executable not found: {tool}")
    wrapper_directory = directory / ".terraform" / "terraleph-demo-bin"
    wrapper_directory.mkdir(parents=True)
    wrapper = wrapper_directory / tool
    wrapper.write_text(
        "#!/usr/bin/env python3\n"
        "import os,sys,time\n"
        "if len(sys.argv) > 1 and sys.argv[1] == 'apply': time.sleep(5)\n"
        f"os.execv({str(Path(terraform).resolve())!r}, "
        f"[{str(Path(terraform).resolve())!r}, *sys.argv[1:]])\n"
    )
    wrapper.chmod(0o700)
    environment["PATH"] = (
        str(wrapper_directory) + os.pathsep + environment.get("PATH", os.defpath)
    )


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError) as error:
        sys.exit(str(error))
