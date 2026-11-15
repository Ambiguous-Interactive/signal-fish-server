"""Local wrappers must use scoped verification unless deep work is explicit."""

import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent


def bash_executable(platform=os.name):
    if platform != "nt":
        return "bash"
    for prefix in ("C:/Program Files/Git", "C:/Program Files (x86)/Git"):
        candidate = Path(prefix) / "bin/bash.exe"
        if candidate.is_file():
            return str(candidate)
    raise FileNotFoundError("Git Bash is required for Windows shell fixtures")


class IterationEntrypoints(unittest.TestCase):
    def run_wrapper(self, script, args=(), exit_code=0, nested=False, expected_alias=None):
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory) / "scripts"
            scripts.mkdir()
            (scripts / script).write_bytes(
                (ROOT / "scripts" / script).read_bytes().replace(b"\r\n", b"\n")
            )
            stub = (
                '#!/usr/bin/env bash\n[ \"$PWD\" -ef \"$EXPECTED_ROOT\" ] || exit 8\nprintf "ARG:%s\\n" "$@"\n'
                f"exit {exit_code}\n"
            )
            (scripts / "dev-loop.sh").write_bytes(stub.encode("utf-8"))
            expected_root = Path(directory).resolve().as_posix()
            if expected_alias == "same":
                expected_root += "/."
            elif expected_alias == "different":
                different = Path(directory) / "different"
                different.mkdir()
                expected_root = different.resolve().as_posix()
            shell = bash_executable()
            env = dict(os.environ, LC_ALL="C", EXPECTED_ROOT=expected_root)
            if Path(shell).is_absolute():
                path_key = next((key for key in env if key.upper() == "PATH"), "PATH")
                env[path_key] = str(Path(shell).parent) + os.pathsep + env.get(path_key, "")
            cwd = Path(directory) / "nested" if nested else Path(directory)
            cwd.mkdir(exist_ok=True)
            return subprocess.run(
                [shell, (scripts / script).as_posix(), *args],
                cwd=cwd, env=env, capture_output=True, text=True, check=False,
            )

    def test_windows_shell_selection_uses_git_bash(self):
        with patch.object(type(ROOT), "is_file") as exists:
            exists.side_effect = [False, True]
            self.assertEqual(bash_executable("nt").replace("\\", "/"), "C:/Program Files (x86)/Git/bin/bash.exe")
            self.assertEqual(exists.call_count, 2)
            exists.side_effect = [False, False]
            with self.assertRaisesRegex(FileNotFoundError, "Git Bash"):
                bash_executable("nt")

    def test_selected_shell_directory_reaches_nested_bash(self):
        shell = ROOT.parent / "selected-shell" / "bash"
        with patch.object(os, "environ", {"pAtH": "original-path", "KEEP": "value"}):
            with patch(__name__ + ".bash_executable", return_value=str(shell)):
                with patch.object(subprocess, "run") as run:
                    self.run_wrapper("test.sh")
        environment = run.call_args.kwargs["env"]
        self.assertEqual(environment["pAtH"], str(shell.parent) + os.pathsep + "original-path")
        self.assertEqual([key for key in environment if key.upper() == "PATH"], ["pAtH"])
        self.assertEqual(environment["KEEP"], "value")
        self.assertEqual(run.call_args.args[0][0], str(shell))

    def test_defaults_scope_and_propagate_failure(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, exit_code=7)
                self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
                self.assertEqual(result.stdout.splitlines(), ["ARG:--clippy", "ARG:--changed"])

    def test_nested_callers_use_checkout_root(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, nested=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_shell_fixture_works_with_windows_text_defaults(self):
        original_open = io.open

        def windows_open(file, mode="r", buffering=-1, encoding=None, errors=None, newline=None, **kwargs):
            if "w" in mode and "b" not in mode and newline is None:
                newline = "\r\n"
            return original_open(file, mode, buffering, encoding, errors, newline, **kwargs)

        with patch("io.open", windows_open):
            for script in ("test.sh", "run-local-ci.sh"):
                with self.subTest(script=script):
                    result = self.run_wrapper(script, exit_code=7)
                    self.assertEqual(result.returncode, 7, result.stdout + result.stderr)
                    self.assertEqual(result.stdout.splitlines(), ["ARG:--clippy", "ARG:--changed"])

    def test_fix_requires_deep_in_every_argument_order(self):
        for args in (["--fix"], ["--fast", "--fix"], ["--help", "--fix"]):
            with self.subTest(args=args):
                result = self.run_wrapper("run-local-ci.sh", args)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertIn("--deep --fix", result.stderr)
        for args in (["--deep", "--fix", "--help"], ["--fix", "--deep", "--help"]):
            with self.subTest(args=args):
                result = self.run_wrapper("run-local-ci.sh", args)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn("ARG:", result.stdout)

    def test_directory_identity_accepts_aliases_and_rejects_other_directories(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                same = self.run_wrapper(script, expected_alias="same")
                self.assertEqual(same.returncode, 0, same.stderr)
                other = self.run_wrapper(script, expected_alias="different")
                self.assertEqual(other.returncode, 8, other.stderr)
                self.assertNotIn("ARG:", other.stdout)

    def test_feature_flags_remain_scoped(self):
        result = self.run_wrapper("test.sh", ["--all-features"])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            ["ARG:--clippy", "ARG:--changed", "ARG:--all-features"],
        )

    def test_deep_help_does_not_dispatch_scoped_runner(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, ["--deep", "--help"])
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("Usage:", result.stdout)
                self.assertNotIn("ARG:", result.stdout)


if __name__ == "__main__":
    unittest.main()
