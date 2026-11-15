"""Local wrappers must use scoped verification unless deep work is explicit."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class IterationEntrypoints(unittest.TestCase):
    def run_wrapper(self, script, args=(), exit_code=0, nested=False, expected_alias=None):
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory) / "scripts"
            scripts.mkdir()
            shutil.copyfile(ROOT / "scripts" / script, scripts / script)
            (scripts / "dev-loop.sh").write_text(
                '#!/usr/bin/env bash\n[ \"$PWD\" -ef \"$EXPECTED_ROOT\" ] || exit 8\nprintf "ARG:%s\\n" "$@"\n'
                f"exit {exit_code}\n"
            )
            expected_root = str(Path(directory).resolve())
            if expected_alias == "same":
                expected_root += os.sep + "."
            elif expected_alias == "different":
                different = Path(directory) / "different"
                different.mkdir()
                expected_root = str(different.resolve())
            env = dict(os.environ, LC_ALL="C", EXPECTED_ROOT=expected_root)
            cwd = Path(directory) / "nested" if nested else Path(directory)
            cwd.mkdir(exist_ok=True)
            return subprocess.run(
                ["bash", (scripts / script).as_posix(), *args],
                cwd=cwd, env=env, capture_output=True, text=True, check=False,
            )

    def test_defaults_scope_and_propagate_failure(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, exit_code=7)
                self.assertEqual(result.returncode, 7, result.stderr)
                self.assertEqual(result.stdout.splitlines(), ["ARG:--clippy", "ARG:--changed"])

    def test_nested_callers_use_checkout_root(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, nested=True)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_fix_requires_deep_in_every_argument_order(self):
        for args in (["--fix"], ["--fast", "--fix"], ["--help", "--fix"]):
            with self.subTest(args=args):
                result = self.run_wrapper("run-local-ci.sh", args)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("--deep --fix", result.stderr)
        for args in (["--deep", "--fix", "--help"], ["--fix", "--deep", "--help"]):
            with self.subTest(args=args):
                result = self.run_wrapper("run-local-ci.sh", args)
                self.assertEqual(result.returncode, 0, result.stderr)
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
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines(),
            ["ARG:--clippy", "ARG:--changed", "ARG:--all-features"],
        )

    def test_deep_help_does_not_dispatch_scoped_runner(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, ["--deep", "--help"])
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("Usage:", result.stdout)
                self.assertNotIn("ARG:", result.stdout)


if __name__ == "__main__":
    unittest.main()
