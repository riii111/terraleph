
import os
import pty
import re
import select
import signal
import fcntl
import struct
import sys
import termios
import time

_, binary, root, columns_arg, rows_arg, scenario = sys.argv[:6]
columns = int(columns_arg)
rows = int(rows_arg)
command = sys.argv[6:]
pid, fd = pty.fork()
if pid == 0:
    if scenario in ("hangup_before_review", "signal_hangup_ignored"):
        # Like nohup: an ignored disposition survives exec, so closing the terminal sends nothing
        # and Terraleph must notice the closed terminal on its own.
        signal.signal(signal.SIGHUP, signal.SIG_IGN)
    os.environ["TERM"] = "xterm-256color"
    os.execv(
        "/bin/sh",
        [
            "sh",
            "-c",
            'stty rows "$1" cols "$2"; cd "$3"; shift 3; exec "$@"',
            "sh",
            rows_arg,
            columns_arg,
            root,
            binary,
            *command,
        ],
    )

output = bytearray()
observed = []
# Ratatui hides the cursor as the last write of every frame, and no screen here places a cursor.
FRAME_END = b"\x1b[?25l"


class Screen:
    def __init__(self, columns, rows):
        self.columns = columns
        self.rows = rows
        self.cells = [[" "] * columns for _ in range(rows)]
        self.row = 0
        self.column = 0
        self.saved = (0, 0)
        self.pending = bytearray()

    def resize(self, columns, rows):
        cells = [[" "] * columns for _ in range(rows)]
        for row in range(min(self.rows, rows)):
            for column in range(min(self.columns, columns)):
                cells[row][column] = self.cells[row][column]
        self.columns = columns
        self.rows = rows
        self.cells = cells
        self.row = min(self.row, rows - 1)
        self.column = min(self.column, columns - 1)

    def feed(self, data):
        self.pending.extend(data)
        while self.pending:
            if self.pending[0] == 0x1b:
                if len(self.pending) < 2:
                    return
                if self.pending[1] == ord("["):
                    final = next(
                        (index for index, value in enumerate(self.pending[2:], 2)
                         if 0x40 <= value <= 0x7e),
                        None,
                    )
                    if final is None:
                        return
                    sequence = bytes(self.pending[2:final])
                    command = chr(self.pending[final])
                    del self.pending[:final + 1]
                    self.csi(sequence.decode("ascii", "ignore"), command)
                    continue
                if self.pending[1] == ord("]"):
                    # Terminals consume an OSC string, ended by BEL or ST, without drawing it.
                    end = next(
                        (index for index in range(2, len(self.pending))
                         if self.pending[index] == 0x07
                         or self.pending[index:index + 2] == b"\x1b\\"),
                        None,
                    )
                    if end is None:
                        return
                    del self.pending[:end + (1 if self.pending[end] == 0x07 else 2)]
                    continue
                del self.pending[:2]
                continue

            value = self.pending[0]
            if value == 0x0d:
                del self.pending[:1]
                self.column = 0
                continue
            if value == 0x0a:
                del self.pending[:1]
                self.row = min(self.row + 1, self.rows - 1)
                continue
            if value == 0x08:
                del self.pending[:1]
                self.column = max(self.column - 1, 0)
                continue
            if value < 0x20 or value == 0x7f:
                del self.pending[:1]
                continue

            character = None
            for length in range(1, min(4, len(self.pending)) + 1):
                try:
                    character = bytes(self.pending[:length]).decode("utf-8")
                    break
                except UnicodeDecodeError as error:
                    if error.reason == "unexpected end of data" and length == len(self.pending):
                        return
            if character is None:
                character = "�"
                length = 1
            del self.pending[:length]
            self.put(character)

    def put(self, character):
        if self.row >= self.rows:
            return
        if self.column >= self.columns:
            self.column = 0
            self.row = min(self.row + 1, self.rows - 1)
        self.cells[self.row][self.column] = character
        self.column = min(self.column + 1, self.columns)

    def csi(self, parameters, command):
        private = parameters.startswith("?")
        if private:
            parameters = parameters[1:]
        values = []
        for value in parameters.split(";") if parameters else []:
            try:
                values.append(int(value) if value else 1)
            except ValueError:
                values.append(1)

        if command in ("H", "f"):
            self.row = max((values[0] if values else 1) - 1, 0)
            self.column = max((values[1] if len(values) > 1 else 1) - 1, 0)
        elif command == "G":
            self.column = max((values[0] if values else 1) - 1, 0)
        elif command == "d":
            self.row = max((values[0] if values else 1) - 1, 0)
        elif command == "A":
            self.row = max(self.row - (values[0] if values else 1), 0)
        elif command == "B":
            self.row = min(self.row + (values[0] if values else 1), self.rows - 1)
        elif command == "C":
            self.column = min(self.column + (values[0] if values else 1), self.columns)
        elif command == "D":
            self.column = max(self.column - (values[0] if values else 1), 0)
        elif command == "J":
            if not values or values[0] == 2:
                self.cells = [[" "] * self.columns for _ in range(self.rows)]
        elif command == "K":
            mode = values[0] if values else 0
            start = 0 if mode == 2 else self.column if mode == 0 else 0
            end = self.columns if mode in (0, 2) else self.column + 1
            for column in range(start, min(end, self.columns)):
                self.cells[self.row][column] = " "
        elif command == "s":
            self.saved = (self.row, self.column)
        elif command == "u":
            self.row, self.column = self.saved

    def text(self):
        return "\n".join("".join(row) for row in self.cells)


screen = Screen(columns, rows)


def child_status():
    waited, status = os.waitpid(pid, os.WNOHANG)
    if waited == 0:
        return None
    return os.waitstatus_to_exitcode(status)


def read_available():
    ready, _, _ = select.select([fd], [], [], 0.1)
    if not ready:
        return
    try:
        chunk = os.read(fd, 8192)
        output.extend(chunk)
        screen.feed(chunk)
    except OSError:
        pass


def wait_screen(predicate, name, description, timeout=20):
    before = screen.text()
    deadline = time.time() + timeout
    while time.time() < deadline:
        current = screen.text()
        if current != before and predicate(current):
            observed.append(name)
            return
        read_available()
        current = screen.text()
        if current != before and predicate(current):
            observed.append(name)
            return
        if child_status() is not None:
            break
    raise RuntimeError(
        f"missing {name}: {description!r}; screen={screen.text()!r}; head={bytes(output)[:4000]!r}; tail={bytes(output)[-1200:]!r}"
    )


def wait_new(marker, name, timeout=20):
    wait_screen(lambda current: marker in current, name, marker, timeout)


def observe_current_or_wait(marker, name, timeout=20):
    if marker in screen.text():
        observed.append(name)
        return
    wait_new(marker, name, timeout)


def wait_copy_notice(name, description):
    notices = ("Copied.", "Sent to terminal clipboard.", "Copy failed.")
    wait_screen(lambda current: any(notice in current for notice in notices), name, description)
    # Without a system clipboard, as on a headless runner, the copy reaches the terminal as OSC 52.
    if "Sent to terminal clipboard." in screen.text() and b"\x1b]52;c;" not in output:
        raise RuntimeError(f"{name} did not send OSC 52; tail={bytes(output)[-1200:]!r}")


def wait_parts(markers, name, timeout=20):
    wait_screen(lambda current: all(marker in current for marker in markers), name, markers, timeout)


def wait_environment(name, status, timeout=20, row_only=False):
    def matches(current):
        lines = current.splitlines()
        for index, line in enumerate(lines):
            if ("[x]" in line or "[ ]" in line) and name in line:
                if index + 1 < len(lines):
                    status_line = lines[index + 1].split("││", 1)[0]
                    if status in status_line:
                        return True
            if not row_only and not ("[x]" in line or "[ ]" in line):
                for part in line.split("│"):
                    if name in part and status in part:
                        return True
        return False

    if matches(screen.text()):
        observed.append(f"{name}_{status.lower()}")
    else:
        wait_screen(matches, f"{name} {status}", f"{name} {status}", timeout)


def wait_sidebar_statuses(statuses, timeout=20):
    expected = list(statuses)

    def matches(current):
        lines = current.splitlines()
        actual = [
            lines[index + 1].split("││", 1)[0].strip("│ ")
            for index, line in enumerate(lines[:-1])
            if "[x]" in line or "[ ]" in line
        ]
        return all(
            sum(status in line for line in actual) >= expected.count(status)
            for status in set(expected)
        )

    if matches(screen.text()):
        observed.append("sidebar_statuses")
    else:
        wait_screen(matches, "sidebar_statuses", expected, timeout)


def wait_file(path, name, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if os.path.isfile(path) and os.path.getsize(path) > 0:
            observed.append(name)
            return
        read_available()
    raise RuntimeError(f"missing {name}: {path!r}; head={bytes(output)[:4000]!r}")


def plan_status_is_above_plan_text(current):
    markers = ("Changes", "terraform_data.api")
    positions = [current.find(marker) for marker in markers]
    if any(position < 0 for position in positions):
        return False
    return positions[0] < positions[1]


def wait_review(name, timeout=20):
    # The Overview also shows [2] Changes above the address; only the review footer offers a apply.
    wait_screen(
        lambda current: plan_status_is_above_plan_text(current) and "a apply" in current,
        name,
        "review header above terraform_data.api with a apply",
        timeout,
    )


def wait_frame(marker, name, timeout=20):
    wait_screen(
        lambda current: marker in current and output.endswith(FRAME_END),
        name,
        f"{marker} with a completed frame",
        timeout,
    )


def wait_redraw(frames, name, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if output.count(FRAME_END) > frames:
            observed.append(name)
            return
        read_available()
        if child_status() is not None:
            break
    raise RuntimeError(
        f"missing {name}: frame after {frames}; screen={screen.text()!r}; tail={bytes(output)[-1200:]!r}"
    )


def wait_exit(timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        status = child_status()
        if status is not None:
            return drain_after_exit(status)
        read_available()
    raise RuntimeError(
        f"child did not exit; screen={screen.text()!r}; observed={observed!r}; head={bytes(output)[:4000]!r}; tail={bytes(output)[-1200:]!r}"
    )


def send_key(key):
    os.write(fd, key)
    time.sleep(0.1)


def assert_screen_unchanged(name, timeout=1):
    before = screen.text()
    deadline = time.time() + timeout
    while time.time() < deadline:
        read_available()
        if screen.text() != before:
            raise RuntimeError(f"screen changed while {name}")
        if child_status() is not None:
            raise RuntimeError(f"child exited while {name}")
        time.sleep(0.05)
    observed.append(name)


def stacked_overview_panes(current):
    lines = current.splitlines()
    changes = next((index for index, line in enumerate(lines) if "[2] Changes" in line), None)
    relations = next((index for index, line in enumerate(lines) if "[3] Relations" in line), None)
    # Side-by-side panes share heading rows, so row ranges below would mix the two panes.
    if changes is None or relations is None or changes >= relations:
        raise RuntimeError(f"Overview panes are not stacked vertically; screen={current!r}")
    return lines[changes + 1:relations], lines[relations + 1:]


def assert_first_overview_row_selected(name):
    current = screen.text()
    changes, relations = stacked_overview_panes(current)
    if not any(line.startswith("│> ") and "terraform_data.api" in line for line in changes):
        raise RuntimeError(f"first Changes row is not selected; screen={current!r}")
    if not any(line.startswith("│> ") and "terraform_data.api" in line for line in relations):
        raise RuntimeError(f"relation of the first row is not selected; screen={current!r}")
    observed.append(name)


def send_text(text):
    for character in text:
        send_key(character.encode())


def quit_with_enter():
    send_key(b"q")
    wait_screen(
        lambda current: "Quit Terraleph?" in current or "Quit?" in current,
        "quit_confirmation",
        "quit confirmation",
    )
    send_key(b"\r")
    return wait_exit()


def drain_after_exit(status):
    while True:
        ready, _, _ = select.select([fd], [], [], 0.2)
        if not ready:
            return status
        try:
            chunk = os.read(fd, 8192)
        except OSError:
            return status
        if not chunk:
            return status
        output.extend(chunk)
        screen.feed(chunk)


def resize(columns, rows):
    # The size change already signals the child. A second SIGWINCH can share a crossterm read
    # with the next key, and crossterm then leaves that key unreported until more input arrives.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
    screen.resize(columns, rows)


TERMINATION_SIGNALS = {"hup": signal.SIGHUP, "int": signal.SIGINT, "term": signal.SIGTERM}


def send_termination_signal(scenario):
    os.kill(pid, TERMINATION_SIGNALS[scenario.rsplit("_", 1)[1]])
    observed.append("signal_sent")


def release_plan():
    open(os.path.join(root, "release-plan"), "w").close()


def wait_exit_after_hangup(timeout=20, after_close=None):
    # Closing the master is what a terminal emulator or `tmux kill-server` does; the kernel
    # then delivers SIGHUP to the session unless it is ignored, and no further output can be read.
    os.close(fd)
    observed.append("terminal_closed")
    if after_close:
        after_close()
    deadline = time.time() + timeout
    while time.time() < deadline:
        status = child_status()
        if status is not None:
            return status
        time.sleep(0.05)
    raise RuntimeError("child did not exit after hangup")


def kill_child():
    try:
        os.killpg(pid, signal.SIGKILL)
    except OSError:
        try:
            os.kill(pid, signal.SIGKILL)
        except OSError:
            pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass


try:
    if scenario in ("full_text", "user_output", "cli_args", "detailed"):
        wait_review("plan_text", timeout=30)
        observe_current_or_wait("3/", "plan_position")
        exit_code = quit_with_enter()
    elif scenario.startswith("env_"):
        if scenario == "env_child_interrupt":
            exit_code = wait_exit()
        elif scenario.startswith("env_signal_"):
            wait_environment("a-ready", "Ready")
            wait_environment("z-slow", "Running")
            wait_file(os.environ["TERRALEPH_FAKE_PID_PATH"], "active_process")
            send_termination_signal(scenario)
            exit_code = wait_exit()
        elif scenario in ("env_partial", "env_cancel"):
            wait_environment("a-ready", "Ready")
            wait_environment("z-slow", "Running")
            send_key(b"v")
            wait_new("terraform_data.api", "ready_review_while_running")
            send_key(b"0")
            wait_new("a-ready", "back_to_environments")
            if scenario == "env_cancel":
                wait_file(os.environ["TERRALEPH_FAKE_PID_PATH"], "active_process")
                send_key(b"q")
                wait_new("Stop acquiring", "cancel_confirmation")
                send_key(b"\x1b")
                wait_new("a-ready ✓ Ready", "acquisition_continues_after_cancel")
                with open(os.environ["TERRALEPH_FAKE_PID_PATH"]) as pid_file:
                    active_pid = int(pid_file.read().strip())
                os.kill(active_pid, 0)
                send_key(b"q")
                wait_new("Stop acquiring", "cancel_confirmation_reopened")
                send_key(b"\r")
                exit_code = wait_exit()
            else:
                open(os.path.join(root, "z-slow/release-plan"), "w").close()
                wait_environment("z-slow", "Ready")
                exit_code = quit_with_enter()
        elif scenario == "env_example":
            wait_parts(["Ready: 2/3", "Error", "~ 20"], "example_comparison")
            send_key(b"]")
            send_key(b"r")
            wait_parts(["Ready: 3/3", "~ 200"], "example_retry")
            exit_code = quit_with_enter()
        elif scenario == "env_real":
            wait_environment("dev", "Ready", row_only=True)
            wait_environment("prod", "Running", row_only=True)
            send_key(b"v")
            wait_new("terraform_data.api", "real_review_while_running")
            send_key(b"\x1b")
            wait_new("Address", "real_back_to_matrix")
            open(os.environ["TERRALEPH_REAL_PLAN_GATE"], "w").close()
            wait_environment("prod", "Error", row_only=True)
            send_key(b"]")
            send_key(b"]")
            send_key(b"\r")
            wait_new("prod: Error", "real_error_diagnostic")
            send_key(b"\x1b")
            wait_screen(
                lambda current: "prod: Error" not in current and "Address" in current,
                "real_error_dialog_closed",
                "closed prod error dialog",
            )
            with open(os.path.join(root, "prod/retry.auto.tfvars"), "w") as repair:
                repair.write('release = "new"\n')
            send_key(b"r")
            wait_file(
                os.environ["TERRALEPH_REAL_PLAN_GATE"] + "-2",
                "prod_retry_plan_complete",
            )
            wait_sidebar_statuses(["Ready", "Ready", "Ready"])
            send_key(b"v")
            wait_parts(["terraform_data.api", "prod"], "real_retried_plan_review")
            send_key(b"0")
            wait_new("Address", "real_complete_comparison")
            exit_code = quit_with_enter()
        elif scenario == "env_apply":
            for name in ("a-dev", "b-stg"):
                wait_environment(name, "Ready")
            send_key(b"]")
            send_key(b"v")
            wait_review("env_plan_detail")
            send_key(b"a")
            wait_new("Apply this reviewed plan?", "env_apply_confirmation")
            send_text("yes")
            send_key(b"\r")
            wait_parts(["Apply complete", "terraform_data.api"], "env_apply_success", timeout=60)
            exit_code = quit_with_enter()
        elif scenario == "env_default_matrix":
            for name in ("a-dev", "b-stg", "c-prod"):
                wait_environment(name, "Ready")
            observe_current_or_wait("Same change across envs: 2 patterns", "default_matrix_summary")
            send_key(b"2")
            send_key(b" ")
            observe_current_or_wait("terraform_data.server[*]", "default_matrix")
            observed.extend(("default_matrix_summary", "default_matrix"))
            exit_code = quit_with_enter()
        elif scenario == "env_relations":
            for name in ("a-dev", "b-stg", "c-prod"):
                wait_environment(name, "Ready")
            send_key(b"3")
            wait_new("* [3] Relations", "relations_pane")
            send_key(b"]")
            wait_new("b-stg · whole env", "relations_environment_switched")
            send_key(b"\r")
            wait_new(
                '# terraform_data.api will be updated in-place',
                "relations_raw_plan",
            )
            send_key(b"0")
            wait_new("b-stg · whole env", "relations_overview_restored")
            exit_code = quit_with_enter()
        elif scenario == "env_matrix":
            for name in ("a-dev", "b-stg", "c-prod"):
                wait_environment(name, "Ready")
                if name != "c-prod":
                    send_key(b"]")
            send_key(b"[")
            send_key(b"[")
            send_key(b"2")
            if rows < 20:
                send_key(b"f")
            send_key(b"/")
            wait_new("Filter:", "matrix_filter")
            send_text("[198]")
            send_key(b"\r")
            send_key(b" ")
            send_key(b"j")
            send_key(b"]")
            send_key(b"v")
            wait_parts(
                ["terraform_data.server[0]", "Esc overview"],
                "matrix_full_plan",
            )
            send_key(b"]")
            wait_new("c-prod", "matrix_digit_environment")
            send_key(b"\x1b")
            wait_parts(["Filter: /[198]", "server[198]"], "restored_matrix_selection")
            exit_code = quit_with_enter()
        elif scenario == "env_many":
            for _ in range(11):
                send_key(b"]")
            wait_environment("env-11", "Ready")
            send_key(b"v")
            wait_parts(
                ["terraform_data.api", "env-11", "Esc overview"], "twelfth_environment"
            )
            send_key(b"[")
            wait_new("env-10", "eleventh_environment")
            send_key(b"0")
            wait_new("blank: absent", "restored_last_column")
            exit_code = quit_with_enter()
        elif scenario == "env_show_failure":
            wait_environment("a-ready", "Ready")
            wait_environment("b-error", "Error")
            send_key(b"]")
            send_key(b"\r")
            wait_parts(["show output could not be parsed", "synthetic plan warning"], "warning_and_failure")
            send_key(b"\x1b")
            exit_code = quit_with_enter()
        elif scenario == "env_retry":
            wait_environment("a-ready", "Ready")
            wait_environment("b-error", "Error")
            send_key(b"]")
            send_key(b"\r")
            # The Overview also shows the diagnostic, so wait for the dialog's own key hint.
            wait_parts(["Missing required variable", "Esc close"], "error_diagnostic")
            send_key(b"\x1b")
            wait_screen(
                lambda current: "a-ready" in current and "Esc close" not in current,
                "error_dialog_closed",
                "dialog closed",
            )
            send_key(b"r")
            wait_environment("b-error", "Ready")
            exit_code = quit_with_enter()
        else:
            if scenario in ("env_init_failure", "env_reinit_failure"):
                wait_environment("a-ready", "Ready")
                wait_environment("b-other", "Error")
            elif scenario == "env_excluded":
                wait_environment("a-ready", "Ready")
                wait_environment("b-other", "Excluded")
            elif scenario == "env_detailed":
                wait_sidebar_statuses(["Ready", "Ready"])
            else:
                wait_environment("a-ready", "Ready")
                wait_environment("b-other", "Ready")
            if scenario in ("env_init_failure", "env_reinit_failure"):
                send_key(b"]")
                observe_current_or_wait("Error", "failed_environment")
            if scenario == "env_detailed":
                send_key(b"c")
                wait_new("chosen-production", "selected_workspace")
                send_key(b"\x1b")
            exit_code = quit_with_enter()
    elif scenario == "filter_navigation":
        wait_review("plan_text", timeout=30)
        observe_current_or_wait("3/", "plan_position")
        send_key(b"/")
        wait_new("/ ", "filter_input")
        send_text("api")
        wait_new(" matches", "filter_matches")
        send_key(b"\r")
        wait_new("y copy all", "filter_confirmed")
        send_key(b"?")
        wait_new("Help", "filter_help")
        send_key(b"?")
        wait_new("y copy all", "filter_help_closed")
        send_key(b"c")
        wait_new("Execution directory", "filter_context")
        send_key(b"\x1b")
        wait_new("y copy all", "filter_context_closed")
        send_key(b"y")
        wait_copy_notice("filter_copy", "copy notice after filtering")
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "filter_apply_confirmation")
        send_key(b"\x1b")
        wait_screen(
            lambda current: "Apply this reviewed plan?" not in current
            and "y copy all" in current,
            "filter_apply_cancelled",
            "filtered review after closing apply confirmation",
        )
        send_key(b"q")
        wait_new("Quit Terraleph?", "filter_quit_confirmation")
        send_key(b"\x1b")
        wait_new("y copy all", "filter_quit_cancelled")
        send_key(b"n")
        send_key(b"N")
        send_key(b"\x1b")
        wait_screen(
            lambda current: "Plan | Filter" not in current
            and "terraform_data.api" in current,
            "filter_cleared",
            "full review after Escape",
        )
        exit_code = quit_with_enter()
    elif scenario == "overview_navigation":
        wait_review("plan_text", timeout=30)
        send_key(b"s")
        wait_parts(
            ["Ready", "[2] Changes", "[3] Relations", "terraform_data.server[*]", "Repeated: 2"],
            "overview_opened",
        )
        send_key(b"3")
        wait_new("* [3] Relations", "overview_relations_focused")
        send_key(b"f")
        wait_screen(
            lambda current: "[3] Relations" in current and "[2] Changes" not in current,
            "overview_relations_maximized",
            "maximized Relations pane",
        )
        send_key(b"\x1b")
        wait_screen(
            lambda current: "[2] Changes" in current
            and "[3] Relations" in current
            and "* [3] Relations" in current,
            "overview_split_restored",
            "split Overview with Relations focus",
        )
        send_key(b"\r")
        wait_review("overview_relations_raw")
        send_key(b"\x1b")
        wait_screen(
            lambda current: "[2] Changes" in current
            and "[3] Relations" in current
            and "* [3] Relations" in current,
            "overview_relations_restored",
            "Overview after Relations raw-plan roundtrip",
        )
        send_key(b"2")
        wait_new("* [2] Changes", "overview_changes_focused")
        send_key(b"j")
        send_key(b"j")
        send_key(b" ")
        wait_new('terraform_data.server["one"]', "overview_expanded")
        send_key(b"\r")
        wait_new("terraform_data.server[\"one\"]", "overview_raw_block")
        send_key(b"/")
        wait_new("/ ", "overview_raw_filter_input")
        send_text("api")
        wait_new(" matches", "overview_raw_filter_matches")
        send_key(b"\x1b")
        wait_new("/ filter", "overview_raw_filter_cancelled")
        send_key(b"/")
        wait_new("/ ", "overview_raw_filter_input_again")
        send_text("api")
        wait_new(" matches", "overview_raw_filter_matches_again")
        send_key(b"\r")
        wait_new("y copy all", "overview_raw_filter_confirmed")
        send_key(b"\x1b")
        wait_new("Ready", "overview_restored")
        send_key(b"/")
        wait_new("Filter: /", "overview_filter_input")
        send_text("two")
        send_key(b"\r")
        wait_new("display only", "overview_filtered")
        send_key(b"v")
        wait_review("overview_full_plan")
        exit_code = quit_with_enter()
    elif scenario == "default_overview":
        wait_parts(
            ["Ready", "[2] Changes", "[3] Relations", "terraform_data.server[*]", "q quit"],
            "default_overview",
        )
        assert_first_overview_row_selected("default_overview_first_row_selected")
        send_key(b"q")
        send_key(b"\r")
        exit_code = wait_exit()
    elif scenario in ("plan_apply", "default_apply"):
        if scenario == "default_apply":
            wait_parts(["[2] Changes", "[3] Relations"], "default_overview")
            send_key(b"v")
        wait_parts(["terraform_data.api", "a apply"], "plan_apply_offered", timeout=30)
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "apply_confirmation")
        send_text("yes")
        send_key(b"\r")
        wait_parts(["Apply complete", "terraform_data.api"], "apply_success", timeout=60)
        exit_code = quit_with_enter()
    elif scenario == "default_ci":
        wait_new("Usage: terraleph", "default_help")
        exit_code = wait_exit()
    elif scenario == "unsupported_default":
        observed.append("unsupported_default")
        exit_code = wait_exit()
    elif scenario == "demo":
        wait_new("Opening the single-environment Overview...", "demo_tui", timeout=120)
        wait_parts(["Change Address", "terraform_data.api"], "demo_overview")
        send_key(b"q")
        send_key(b"\r")
        exit_code = wait_exit()
    elif scenario == "demo_apply":
        wait_new("Opening the", "demo_tui", timeout=300)
        if "multi" in command:
            for name in ("dev", "stg", "prod"):
                wait_environment(name, "Ready", timeout=300)
            send_key(b"]")
        else:
            wait_parts(["Change Address", "terraform_data.api"], "demo_overview", timeout=300)
        send_key(b"v")
        wait_new("a apply", "demo_plan_detail")
        send_key(b"a")
        wait_parts(["Apply this reviewed plan?", "To confirm, type"], "demo_apply_confirmation")
        send_text(re.search(r'To confirm, type "([^"]+)" below', screen.text()).group(1))
        send_key(b"\r")
        wait_new("Apply complete", "demo_apply_success", timeout=300)
        exit_code = quit_with_enter()
    elif scenario == "basic_workflow":
        wait_review("plan_text", timeout=30)
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "apply_confirmation")
        wait_new("Type ", "apply_target_prompt")
        send_text(os.path.basename(os.path.normpath(root)))
        send_key(b"\r")
        wait_parts(["Apply complete", "terraform_data.api"], "apply_result", timeout=60)
        send_key(b"y")
        exit_code = quit_with_enter()
    elif scenario == "no_changes":
        observed.append("no_changes")
        exit_code = wait_exit()
    elif scenario.startswith("signal_review_"):
        wait_review("plan_text", timeout=30)
        send_termination_signal(scenario)
        exit_code = wait_exit()
    elif scenario.startswith("plan_signal_group_"):
        wait_file(os.environ["TERRALEPH_FAKE_PID_PATH"], "terraform_started")
        os.killpg(pid, TERMINATION_SIGNALS[scenario.rsplit("_", 1)[1]])
        observed.append("signal_sent")
        exit_code = wait_exit()
    elif scenario.startswith("plan_signal_parent_"):
        wait_file(os.environ["TERRALEPH_FAKE_PID_PATH"], "terraform_started")
        send_termination_signal(scenario)
        time.sleep(0.3)
        release_plan()
        exit_code = wait_exit()
    elif scenario == "hangup_before_review":
        wait_file(os.environ["TERRALEPH_FAKE_PID_PATH"], "terraform_started")
        exit_code = wait_exit_after_hangup(after_close=release_plan)
    elif scenario in ("signal_hangup", "signal_hangup_ignored"):
        wait_review("plan_text", timeout=30)
        exit_code = wait_exit_after_hangup()
    elif scenario.startswith("signal_apply_"):
        wait_review("plan_text", timeout=30)
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "apply_confirmation")
        send_text("yes")
        send_key(b"\r")
        wait_new("Applying...", "apply_started")
        send_key(b"v")
        wait_new("Applying saved plan...", "apply_logs_open")
        send_termination_signal(scenario)
        exit_code = wait_exit()
    elif scenario in (
        "apply_success",
        "apply_failure",
        "apply_interrupt",
        "apply_log_view",
        "apply_mapping",
    ):
        wait_review("plan_text", timeout=30)
        if scenario == "apply_success":
            send_key(b"/")
            wait_new("/ ", "apply_filter_input")
            send_text("not-present")
            observe_current_or_wait("No matches", "apply_filter_no_matches")
            send_key(b"\r")
            wait_new("y copy all", "apply_filter_confirmed")
        send_key(b"?")
        wait_new("Help", "plan_help")
        send_key(b"?")
        wait_new("a apply", "plan_help_closed")
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "apply_confirmation")
        send_key(b"?")
        wait_new("Apply help", "apply_help")
        send_key(b"?")
        wait_new("Apply this reviewed plan?", "apply_help_closed")
        send_key(b"\t")
        wait_new("Execution directory", "apply_context")
        send_key(b"\x1b")
        wait_new("Apply this reviewed plan?", "apply_context_closed")
        send_text("yes")
        send_key(b"\r")
        wait_new("Applying...", "apply_started")
        if scenario == "apply_log_view":
            send_key(b"\t")
            wait_screen(
                lambda current: "Targets" in current and "Targets *" not in current,
                "apply_logs_focused",
                "Targets panel not focused",
            )
            send_key(b"\t")
            wait_screen(
                lambda current: "Targets *" in current,
                "apply_targets_focused",
                "Targets panel focused",
            )
            send_key(b"\t")
            wait_screen(
                lambda current: "Targets" in current and "Targets *" not in current,
                "apply_logs_refocused",
                "Targets panel not focused",
            )
            wait_parts(
                ["Apply complete", "terraform_data.api", "y yank result"],
                "apply_success",
                timeout=60,
            )
            exit_code = quit_with_enter()
        elif scenario in ("apply_success", "apply_mapping"):
            wait_parts(["Apply complete", "terraform_data.api"], "apply_success")
            exit_code = quit_with_enter()
        elif scenario == "apply_failure":
            wait_parts(["Apply failed", "synthetic apply failure"], "apply_failure")
            exit_code = quit_with_enter()
        else:
            send_key(b"v")
            wait_new("Applying saved plan...", "apply_logs_open")
            send_key(b"\x03")
            wait_parts(["Stopping apply", "Apply interrupted"], "apply_interrupted")
            exit_code = quit_with_enter()
    elif scenario == "apply_resize":
        wait_review("plan_text", timeout=30)
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "apply_confirmation")
        for typed in ("y", "ye", "yes"):
            send_key(typed[-1].encode())
            wait_new(f"> {typed}|", f"apply_input_{typed}")
        resize(24, 6)
        wait_frame("Terminal too small", "apply_confirmation_narrow")
        frames = output.count(FRAME_END)
        send_key(b"\r")
        wait_redraw(frames, "apply_narrow_enter_redrawn")
        if "Resize or press Esc to" not in screen.text():
            raise RuntimeError(f"narrow Enter left the apply confirmation; screen={screen.text()!r}")
        resize(100, 24)
        wait_parts(
            ['To confirm, type "yes" below.', "> yes|"],
            "apply_confirmation_resized",
        )
        send_key(b"\r")
        wait_new("Applying...", "apply_started")
        send_key(b"\r")
        observed.append("apply_second_enter")
        wait_new("Apply complete", "apply_result", timeout=60)
        exit_code = quit_with_enter()
    elif scenario in ("apply_no", "apply_escape"):
        wait_review("plan_text", timeout=30)
        send_key(b"a")
        wait_new("Apply this reviewed plan?", "apply_confirmation")
        send_text("no") if scenario == "apply_no" else send_key(b"\x1b")
        if scenario == "apply_no":
            send_key(b"\r")
            wait_new('Does not match "yes".', "apply_invalid_input")
            send_key(b"\x1b")
        wait_screen(
            lambda current: (
                'To confirm, type "yes" below.' not in current
                and "Apply this reviewed plan?" not in current
                and "terraform_data.api" in current
            ),
            "plan_restored",
            "review screen after cancelling apply",
        )
        exit_code = quit_with_enter()
    elif scenario == "diagnostic_success":
        wait_screen(
            plan_status_is_above_plan_text,
            "plan_status_and_text",
            (
                "Changes",
                "terraform_data.api",
            ),
        )
        exit_code = quit_with_enter()
    elif scenario == "quit_confirmation":
        wait_review("plan_text", timeout=30)
        send_key(b"q")
        wait_new("Quit Terraleph?", "quit_confirmation")
        send_key(b"q")
        assert_screen_unchanged("quit_repeat")
        send_key(b"\x1b")
        wait_new("q quit", "quit_cancelled")
        send_key(b"\x03")
        wait_new("Quit Terraleph?", "quit_ctrl_c")
        send_key(b"\x1b")
        wait_new("q quit", "quit_ctrl_c_cancelled")
        send_key(b"q")
        wait_new("Quit Terraleph?", "quit_confirmation_again")
        send_key(b"y")
        wait_copy_notice("quit_copy", "copy notice after cancelling quit confirmation")
        send_key(b"q")
        wait_new("Quit Terraleph?", "quit_confirmation_after_copy")
        send_key(b"\r")
        exit_code = wait_exit()
    elif scenario == "empty_filter_quit":
        wait_review("plan_text", timeout=30)
        send_key(b"/")
        wait_new("/ ", "empty_filter_input")
        send_text("not-present")
        observe_current_or_wait("No matches", "empty_filter_no_matches")
        send_key(b"\r")
        wait_new("y copy all", "empty_filter_confirmed")
        send_key(b"\x03")
        wait_new("Quit Terraleph?", "empty_filter_ctrl_c")
        send_key(b"\x1b")
        wait_screen(
            lambda current: "Quit Terraleph?" not in current
            and "Filter: /not-present" in current
            and "No matching changes." in current,
            "empty_filter_ctrl_c_cancelled",
            "filtered review after cancelling quit confirmation",
        )
        send_key(b"\x03")
        wait_new("Quit Terraleph?", "empty_filter_ctrl_c_again")
        send_key(b"\r")
        exit_code = wait_exit()
    elif scenario == "failure":
        observed.append("failed")
        exit_code = wait_exit()
    elif scenario == "interrupt":
        wait_file(os.environ["TERRALEPH_FAKE_PID_PATH"], "terraform_started")
        send_key(b"\x03")
        observed.append("interrupt_requested")
        exit_code = wait_exit()
    elif scenario == "narrow":
        wait_new("Terminal too small", "narrow")
        resize(100, 24)
        wait_review("resized")
        exit_code = quit_with_enter()
    elif scenario == "panic":
        exit_code = wait_exit()
    else:
        raise RuntimeError(f"unknown scenario: {scenario}")
    restored = b"\x1b[?1049l" in output
    cursor_restored = b"\x1b[?25h" in output
    print(f"exit={exit_code}")
    print(f"restored={str(restored).lower()}")
    print(f"cursor_restored={str(cursor_restored).lower()}")
    print("observed=" + ",".join(observed))
except BaseException as error:
    kill_child()
    print(f"driver_error={error!r}")
    print(bytes(output)[-1200:].decode("utf-8", "replace"))
    raise
