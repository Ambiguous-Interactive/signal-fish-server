"""Local wrappers must use scoped verification unless deep work is explicit."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class IterationEntrypoints(unittest.TestCase):
    def run_wrapper(self, script, args=(), exit_code=0):
        with tempfile.TemporaryDirectory() as directory:
            scripts = Path(directory) / "scripts"
            scripts.mkdir()
            shutil.copyfile(ROOT / "scripts" / script, scripts / script)
            (scripts / "dev-loop.sh").write_text(
                '#!/usr/bin/env bash\nprintf "ARG:%s\\n" "$@"\n'
                f"exit {exit_code}\n"
            )
            env = dict(os.environ, LC_ALL="C")
            return subprocess.run(
                ["bash", str(scripts / script), *args],
                cwd=directory, env=env, capture_output=True, text=True, check=False,
            )

    def test_defaults_scope_and_propagate_failure(self):
        for script in ("test.sh", "run-local-ci.sh"):
            with self.subTest(script=script):
                result = self.run_wrapper(script, exit_code=7)
                self.assertEqual(result.returncode, 7, result.stderr)
                self.assertEqual(result.stdout.splitlines(), ["ARG:--clippy", "ARG:--changed"])

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
