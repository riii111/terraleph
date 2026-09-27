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

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description="Open a cloudless Terracotta demo.")
    parser.add_argument("mode", choices=("single", "multi"))
    return run_demo(parser.parse_args().mode)


def run_demo(mode):
    if not sys.stdin.isatty() or not sys.stdout.isatty():
        raise RuntimeError("The demo requires an interactive terminal")

    fixture = single_fixture if mode == "single" else multi_fixture
    print(
        f"Starting Terracotta {mode} demo. Ctrl-C to cancel.",
        file=sys.stderr,
        flush=True,
    )
    print("[1/3] Preparing local Terraform scenario...", file=sys.stderr, flush=True)
    directory = (
        single_fixture.setup() if mode == "single" else multi_fixture.setup(ready=True)
    )
    demo_root = None
    scenario = directory
    try:
        if mode == "single":
            # The directory name is the typed apply confirmation, so the demo
            # runs from a short fixed name instead of the random temp name.
            demo_root = Path(tempfile.mkdtemp(prefix="terracotta-demo-")).resolve()
            directory = directory.rename(demo_root / "demo")
        target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target" / "demo"))
        if not target.is_absolute():
            target = ROOT / target
        target = target.resolve()
        environment = (
            single_fixture.isolated_environment(directory)
            if mode == "single"
            else multi_fixture.environment()
        )
        environment.update({"CARGO_TARGET_DIR": str(target), "RUSTC_WRAPPER": ""})
        print(
            f"[2/3] Building Terracotta (cache: {target})...",
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
            target / "debug" / ("terracotta.exe" if os.name == "nt" else "terracotta")
        )
        command = [str(executable)]
        if mode == "single":
            install_apply_delay(directory, environment)
            environment.pop("TF_IN_AUTOMATION", None)
            print(
                "[3/3] Opening the single-environment Overview...",
                file=sys.stderr,
                flush=True,
            )
        else:
            environment.pop("CI", None)
            command.extend(("terraform", f"-chdir={directory}", "plan"))
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


def install_apply_delay(directory, environment):
    if os.name == "nt":
        return

    terraform = shutil.which("terraform")
    if terraform is None:
        raise RuntimeError("Required executable not found: terraform")
    wrapper_directory = directory / ".terraform" / "terracotta-demo-bin"
    wrapper_directory.mkdir(parents=True)
    wrapper = wrapper_directory / "terraform"
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
