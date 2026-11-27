#!/usr/bin/env python3
"""Run one command under a hard deadline and retire its whole process group.

GNU timeout signals its direct child, not every descendant. A descendant that
ignores SIGTERM and inherits the caller's stdout can keep a captured pipe open
after timeout returns, so the wrapped step appears to hang until the job limit.
This runner starts the command in a new session, captures its output to a log,
and always retires the entire process group after the direct child exits.
"""
import argparse
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

POLL_SECONDS = 0.25
KILL_WAIT_SECONDS = 5


def positive_seconds(value):
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return parsed


def grace_seconds(value):
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed < 0 or parsed > 300:
        raise argparse.ArgumentTypeError("must be between 0 and 300")
    return parsed


def group_exists(process_group):
    try:
        os.killpg(process_group, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def signal_group(process_group, signal_number):
    try:
        os.killpg(process_group, signal_number)
    except ProcessLookupError:
        pass


def wait_for_group_exit(process_group, seconds):
    deadline = time.monotonic() + seconds
    while group_exists(process_group):
        if time.monotonic() >= deadline:
            return False
        time.sleep(min(POLL_SECONDS, 0.05))
    return True


def retire_group(process, grace):
    """Terminate every process left in the command's session."""
    process_group = process.pid
    if not group_exists(process_group):
        return True
    signal_group(process_group, signal.SIGTERM)
    started = time.monotonic()
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    remaining = max(0, grace - (time.monotonic() - started))
    if wait_for_group_exit(process_group, remaining):
        return True
    signal_group(process_group, signal.SIGKILL)
    try:
        process.wait(timeout=KILL_WAIT_SECONDS)
    except subprocess.TimeoutExpired:
        pass
    return wait_for_group_exit(process_group, KILL_WAIT_SECONDS)


def read_log(path):
    with path.open("rb") as log:
        return log.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout-seconds", required=True, type=positive_seconds)
    parser.add_argument("--grace-seconds", default=10, type=grace_seconds)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--no-print-log", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    command = arguments.command[1:] if arguments.command[:1] == ["--"] else arguments.command
    if not command:
        parser.error("a command is required after --")
    try:
        arguments.log.parent.mkdir(parents=True, exist_ok=True)
        log = arguments.log.open("wb")
    except OSError as error:
        print(f"could not open command log {arguments.log}: {error}", file=sys.stderr)
        return 1

    interrupted = None

    def remember_interrupt(signal_number, _frame):
        nonlocal interrupted
        interrupted = signal_number

    previous_term = signal.signal(signal.SIGTERM, remember_interrupt)
    previous_interrupt = signal.signal(signal.SIGINT, remember_interrupt)
    exit_code = 1
    try:
        with log:
            try:
                process = subprocess.Popen(
                    command,
                    stdout=log,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
            except OSError as error:
                print(f"could not start command {command[0]!r}: {error}", file=sys.stderr)
                exit_code = 127
            else:
                deadline = time.monotonic() + arguments.timeout_seconds
                timed_out = False
                while True:
                    if interrupted is not None:
                        break
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        timed_out = True
                        break
                    try:
                        process.wait(timeout=min(POLL_SECONDS, remaining))
                        break
                    except subprocess.TimeoutExpired:
                        continue

                retired = retire_group(process, arguments.grace_seconds)
                if not retired:
                    print(
                        f"process group {process.pid} still exists after SIGKILL",
                        file=sys.stderr,
                    )

                if interrupted is not None:
                    exit_code = 128 + interrupted
                elif timed_out:
                    exit_code = 124
                elif process.returncode is not None:
                    exit_code = process.returncode
                elif not retired:
                    exit_code = 137
    finally:
        signal.signal(signal.SIGTERM, previous_term)
        signal.signal(signal.SIGINT, previous_interrupt)
        if not arguments.no_print_log:
            sys.stdout.buffer.write(read_log(arguments.log))
            sys.stdout.buffer.flush()
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
