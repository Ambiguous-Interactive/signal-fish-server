"""Behavioral controls for the bounded command supervisor."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

RUNNER = Path(__file__).with_name("run-bounded-command.py")
REPO_ROOT = Path(__file__).resolve().parents[1]


def wait_for_pid_exit(pid, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.05)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    return False


class BoundedCommandTests(unittest.TestCase):
    def run_helper(self, command, timeout_seconds=2, grace_seconds=1, timeout=15):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "command.log"
            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    "--timeout-seconds",
                    str(timeout_seconds),
                    "--grace-seconds",
                    str(grace_seconds),
                    "--log",
                    str(log),
                    "--",
                    *command,
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=timeout,
            )
            return completed, log.read_text()

    def write_fixture(self, directory, name, source):
        path = Path(directory) / name
        path.write_text(textwrap.dedent(source))
        return path

    def test_timeout_retires_descendant_that_ignores_sigterm_and_holds_stdout(self):
        with tempfile.TemporaryDirectory() as directory:
            child_pid_file = Path(directory) / "child.pid"
            parent = self.write_fixture(
                directory,
                "hang_parent.py",
                f"""
                import subprocess
                import sys
                import time
                from pathlib import Path

                child = subprocess.Popen([
                    sys.executable,
                    "-c",
                    "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                    "print('descendant-alive', flush=True); time.sleep(60)",
                ])
                Path({str(child_pid_file)!r}).write_text(str(child.pid))
                print("parent-waiting", flush=True)
                time.sleep(60)
                """,
            )
            started = time.monotonic()
            completed, log = self.run_helper(
                [sys.executable, str(parent)],
                timeout_seconds=1,
                grace_seconds=1,
            )
            elapsed = time.monotonic() - started

            self.assertEqual(completed.returncode, 124, completed.stderr)
            self.assertLess(elapsed, 8, "the supervisor must not wait for the descendant")
            self.assertIn("parent-waiting", log)
            self.assertIn("descendant-alive", log)
            child_pid = int(child_pid_file.read_text())
            self.assertTrue(wait_for_pid_exit(child_pid), f"descendant {child_pid} survived")

    def test_success_retains_output_and_status(self):
        completed, log = self.run_helper(
            [sys.executable, "-c", "print('bounded-command-ok')"]
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("bounded-command-ok", log)
        self.assertIn("bounded-command-ok", completed.stdout)

    def test_failure_status_is_preserved(self):
        completed, log = self.run_helper(
            [sys.executable, "-c", "raise SystemExit(7)"]
        )
        self.assertEqual(completed.returncode, 7, completed.stderr)
        self.assertEqual(log, "")

    def test_parent_exit_retires_background_descendant(self):
        with tempfile.TemporaryDirectory() as directory:
            child_pid_file = Path(directory) / "child.pid"
            parent = self.write_fixture(
                directory,
                "background_parent.py",
                f"""
                import subprocess
                import sys
                from pathlib import Path

                child = subprocess.Popen([
                    sys.executable,
                    "-c",
                    "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)",
                ])
                Path({str(child_pid_file)!r}).write_text(str(child.pid))
                print("parent-exits", flush=True)
                """,
            )
            completed, log = self.run_helper(
                [sys.executable, str(parent)],
                timeout_seconds=10,
                grace_seconds=1,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("parent-exits", log)
            child_pid = int(child_pid_file.read_text())
            self.assertTrue(wait_for_pid_exit(child_pid), f"descendant {child_pid} survived")

    def test_sigterm_to_supervisor_retires_its_group(self):
        with tempfile.TemporaryDirectory() as directory:
            child_pid_file = Path(directory) / "child.pid"
            parent = self.write_fixture(
                directory,
                "hang_parent.py",
                f"""
                import subprocess
                import sys
                import time
                from pathlib import Path

                child = subprocess.Popen([
                    sys.executable,
                    "-c",
                    "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)",
                ])
                Path({str(child_pid_file)!r}).write_text(str(child.pid))
                print("parent-waiting", flush=True)
                time.sleep(60)
                """,
            )
            log = Path(directory) / "command.log"
            supervisor = subprocess.Popen(
                [
                    sys.executable,
                    str(RUNNER),
                    "--timeout-seconds",
                    "30",
                    "--grace-seconds",
                    "1",
                    "--log",
                    str(log),
                    "--no-print-log",
                    "--",
                    sys.executable,
                    str(parent),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            deadline = time.monotonic() + 5
            while not child_pid_file.exists() and time.monotonic() < deadline:
                time.sleep(0.05)
            self.assertTrue(child_pid_file.exists(), "fixture did not start")
            child_pid = int(child_pid_file.read_text())

            supervisor.send_signal(signal.SIGTERM)
            _, stderr = supervisor.communicate(timeout=10)

            self.assertEqual(supervisor.returncode, 143, stderr)
            self.assertTrue(wait_for_pid_exit(child_pid), f"descendant {child_pid} survived")

    def test_invalid_deadlines_are_rejected(self):
        for arguments, message in [
            (["--timeout-seconds", "0"], "at least 1"),
            (["--timeout-seconds", "1", "--grace-seconds", "301"], "between 0 and 300"),
        ]:
            with self.subTest(arguments=arguments):
                with tempfile.TemporaryDirectory() as directory:
                    completed = subprocess.run(
                        [
                            sys.executable,
                            str(RUNNER),
                            *arguments,
                            "--log",
                            str(Path(directory) / "command.log"),
                            "--",
                            sys.executable,
                            "-c",
                            "pass",
                        ],
                        stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE,
                        text=True,
                        timeout=10,
                    )
                self.assertEqual(completed.returncode, 2)
                self.assertIn(message, completed.stderr)

    def test_missing_executable_returns_127(self):
        completed, log = self.run_helper(["/definitely-not-a-real-command"])
        self.assertEqual(completed.returncode, 127)
        self.assertIn("could not start command", completed.stderr)
        self.assertEqual(log, "")

    def test_missing_command_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            completed = subprocess.run(
                [
                    sys.executable,
                    str(RUNNER),
                    "--timeout-seconds",
                    "1",
                    "--log",
                    str(Path(directory) / "command.log"),
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                timeout=10,
            )
        self.assertEqual(completed.returncode, 2)
        self.assertIn("a command is required", completed.stderr)


if __name__ == "__main__":
    unittest.main()
