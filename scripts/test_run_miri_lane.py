"""Keep Miri partitions complete and disjoint without weakening analyzer failures."""

import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("run_miri_lane", Path(__file__).with_name("run-miri-lane.py"))
LANE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LANE)


class MiriLaneTests(unittest.TestCase):
    def test_listing_reads_only_test_records(self):
        self.assertEqual(LANE.listed_tests("server::tests::one: test\nconfig::protocol::test: test\n\n2 tests, 0 benchmarks\n"),
                         ["config::protocol::test", "server::tests::one"])
        for output in ("", "0 tests, 0 benchmarks\n", "invalid: test\n",
                       "a::b: test\na::b: test\n", "a::bad path: test\n"):
            with self.subTest(output=output), self.assertRaises(ValueError):
                LANE.listed_tests(output)

    def test_nested_module_and_test_words_cannot_duplicate_other_lanes(self):
        names = ["config::protocol::tests::one", "protocol::tests::server_deadline",
                 "server::tests::auth", "websocket::metrics::tests::one"]
        modules = ["config", "protocol", "server", "websocket"]
        partitions = [LANE.select_tests(names, [module]) for module in modules]
        self.assertEqual([name for part in partitions for name in part], names)
        self.assertEqual(len(set(name for part in partitions for name in part)), len(names))
        self.assertEqual(LANE.select_tests(names, ["protocol"]), [names[1]])

    def test_module_without_library_tests_does_not_remove_other_tests(self):
        self.assertEqual(LANE.select_tests(["server::tests::one"], ["server", "no_unit_tests"]),
                         ["server::tests::one"])

    def test_bad_or_empty_module_selection_fails_closed(self):
        for modules in ([], ["server", "server"], ["server::"], ["unknown"], ["server;echo"]):
            with self.subTest(modules=modules), self.assertRaises(ValueError):
                LANE.select_tests(["server::tests::one"], modules)

    @patch.object(LANE.subprocess, "run")
    def test_listing_failure_never_runs_the_analyzer(self, run):
        run.return_value = subprocess.CompletedProcess([], 17, stdout="listing failed\n")
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / "selected.json"
            self.assertEqual(LANE.run_lane("pinned", ["server"], manifest), 17)
            self.assertFalse(manifest.exists())
        self.assertEqual(run.call_count, 1)

    @patch.object(LANE.subprocess, "run")
    def test_exact_selected_paths_and_analyzer_failure_are_preserved(self, run):
        run.side_effect = [subprocess.CompletedProcess([], 0, stdout="server::tests::one: test\nconfig::server::two: test\n"),
                           subprocess.CompletedProcess([], 23)]
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / "selected.json"
            self.assertEqual(LANE.run_lane("pinned", ["server"], manifest), 23)
            report = json.loads(manifest.read_text())
            self.assertEqual(report["listed_count"], 2)
            self.assertEqual(report["tests"], ["server::tests::one"])
        self.assertEqual(run.call_args_list[1].args[0][-2:], ["--exact", "server::tests::one"])
        self.assertNotIn("--ignored", run.call_args_list[1].args[0])


if __name__ == "__main__":
    unittest.main()
