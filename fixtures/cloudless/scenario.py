#!/usr/bin/env python3
"""Disposable cloudless scenarios and real apply-time state conflicts."""

import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from basic import plan as basic

FIXTURES = Path(__file__).resolve().parent
VIEWS = {
    "sensitive": "Pで両環境をplanし、Plan Review・Overview・Compare・コピーで機微値が秘匿されることを確認してください。",
    "large": "300件の変更をスクロール・フィルタ・グループ展開し、長い属性値の描画を確認してください。",
    "failure": "applyして、時間差の成功と途中の失敗、ログ、終了コード、端末の復元を確認してください。",
    "lock": "applyしてstate lock取得エラーを確認してください。競合プロセスは自動終了します。",
    "stale": "applyしてsaved planのstaleエラーを確認してください。再planせず失敗することを確認してください。",
    "diagnostics": "Pで両環境をplanし、warning環境のcheck警告とerror環境のpreconditionエラーを確認してください。",
}
MARKER = ".terraleph-cloudless-scenario"


def setup(name, tool="terraform"):
    if shutil.which(tool) is None:
        raise RuntimeError(f"Required executable not found: {tool}")
    directory = Path(tempfile.mkdtemp(prefix="terraleph-cloudless-")).resolve()
    (directory / MARKER).write_text(str(directory) + "\n")
    try:
        if name == "diagnostics":
            for kind in ("warning", "error"):
                child = directory / kind
                child.mkdir()
                shutil.copyfile(FIXTURES / "diagnostics.tf", child / "main.tf")
                (child / "terraform.tfvars").write_text(
                    f"valid = {str(kind == 'warning').lower()}\n"
                )
                initialize(child, tool)
        else:
            shutil.copyfile(FIXTURES / f"{name}.tf", directory / "main.tf")
            initialize(directory, tool)
            env = basic.isolated_environment(directory)
            if name in ("sensitive", "large", "stale"):
                basic.run(
                    directory,
                    env,
                    tool,
                    "apply",
                    "-auto-approve",
                    "-input=false",
                    "-no-color",
                    "-var=revision=baseline",
                )
            basic.run(
                directory,
                env,
                tool,
                "plan",
                "-input=false",
                "-no-color",
                "-out=review.tfplan",
            )
        if name == "sensitive":
            dev = directory / "dev"
            dev.mkdir()
            for path in list(directory.iterdir()):
                if path != dev and path.name != MARKER:
                    path.rename(dev / path.name)
            shutil.copytree(dev, directory / "prod")
            (directory / "prod/terraform.tfvars").write_text(
                'secret = "synthetic-prod-secret-never-use"\n'
            )
        return directory
    except BaseException:
        shutil.rmtree(directory)
        raise


def initialize(directory, tool):
    basic.run(
        directory,
        basic.isolated_environment(directory),
        tool,
        "init",
        "-input=false",
        "-no-color",
    )


def clean(directory):
    if directory.is_symlink():
        raise RuntimeError("Refusing a symlink scenario")
    directory = directory.resolve()
    marker = directory / MARKER
    if (
        not directory.name.startswith("terraleph-cloudless-")
        or marker.is_symlink()
        or not marker.is_file()
        or marker.read_text() != str(directory) + "\n"
    ):
        raise RuntimeError("Not a scenario created by this script")
    shutil.rmtree(directory)


def install_apply_hook(directory, tool, environment, name):
    executable = shutil.which(tool)
    if executable is None:
        raise RuntimeError(f"Required executable not found: {tool}")
    wrappers = directory / ".demo-bin"
    wrappers.mkdir()
    wrapper = wrappers / tool
    wrapper.write_text(
        f"#!{sys.executable}\n"
        "import os,signal,sys\n"
        "def stop(number, frame): raise SystemExit(128 + number)\n"
        "signal.signal(signal.SIGTERM, stop)\n"
        "signal.signal(signal.SIGINT, stop)\n"
        f"sys.path.insert(0, {str(FIXTURES)!r})\n"
        "from scenario import apply_with_conflict\n"
        "if len(sys.argv) > 1 and sys.argv[1] == 'apply':\n"
        f"    sys.exit(apply_with_conflict({str(directory)!r}, {str(Path(executable).resolve())!r}, sys.argv[1:], {name!r}))\n"
        f"os.execv({executable!r}, [{executable!r}, *sys.argv[1:]])\n"
    )
    wrapper.chmod(0o700)
    environment["PATH"] = (
        str(wrappers) + os.pathsep + environment.get("PATH", os.defpath)
    )


def apply_with_conflict(directory, tool, arguments, name):
    directory = Path(directory)
    env = basic.isolated_environment(directory)
    if name == "stale":
        basic.run(
            directory,
            env,
            tool,
            "apply",
            "-auto-approve",
            "-input=false",
            "-no-color",
            "-target=terraform_data.drift",
            "-var=revision=external-change",
        )
        return subprocess.run([tool, *arguments], cwd=directory, env=env).returncode

    ready = directory / "lock-ready"
    ready.unlink(missing_ok=True)
    log = directory / "lock-holder.log"
    with log.open("w") as output:
        holder = subprocess.Popen(
            [
                tool,
                "apply",
                "-auto-approve",
                "-input=false",
                "-no-color",
                "-target=terraform_data.lock_holder",
            ],
            cwd=directory,
            env=env,
            stdout=output,
            stderr=output,
            start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 30
            while not ready.exists():
                if holder.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(
                        "State lock holder did not become ready: " + log.read_text()
                    )
                time.sleep(0.05)
            return subprocess.run([tool, *arguments], cwd=directory, env=env).returncode
        finally:
            # The provisioner and tool share this owned group; wait before removing state.
            try:
                os.killpg(holder.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                holder.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(holder.pid, signal.SIGKILL)
                holder.wait()
