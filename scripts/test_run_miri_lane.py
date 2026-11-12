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
        self.assertEqual(LANE.listed_tests("server::tests::über: test\n"), ["server::tests::über"])
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

    def test_qualified_subpartition_keeps_every_test_once(self):
        names = ["server::session_policy_tests::one", "server::session_policy_tests::nested::two",
                 "server::session_policy_tests_extra::three", "server::tests::four",
                 "config::server::session_policy_tests::five"]
        server = LANE.select_tests(names, ["server"], ["server::session_policy_tests"])
        policy = LANE.select_tests(names, ["server::session_policy_tests"])
        other = LANE.select_tests(names, ["config"])
        self.assertEqual(policy, names[:2])
        self.assertEqual(server, names[2:4])
        self.assertEqual(sorted(server + policy + other), sorted(names))
        self.assertFalse(set(server) & set(policy))

    def test_exclusions_reject_invalid_or_unrelated_paths(self):
        for exclusions in (["server::"], ["server"], ["other::tests"],
                           ["server::tests", "server::tests"], ["server::bad-path"]):
            with self.subTest(exclusions=exclusions), self.assertRaises(ValueError):
                LANE.select_tests(["server::tests::one"], ["server"], exclusions)

    def test_two_heavy_children_and_near_prefix_siblings_keep_one_owner(self):
        names = ["server::ready_state_tests::one", "server::session_policy_tests::two",
                 "server::ready_state_tests_extra::three", "server::session_policy_tests_extra::four",
                 "server::tests::five"]
        children = ["server::ready_state_tests", "server::session_policy_tests"]
        partitions = [LANE.select_tests(names, ["server"], children)]
        partitions.extend(LANE.select_tests(names, [child]) for child in children)
        self.assertEqual(partitions[0], names[2:])
        self.assertEqual(sorted(name for part in partitions for name in part), sorted(names))

    def test_three_heavy_server_children_keep_every_path_once(self):
        children = ["server::ready_state_tests", "server::session_policy_tests",
                    "server::room_service_tests"]
        names = [child + "::one" for child in children]
        names.extend(child + "_extra::two" for child in children)
        names.append("server::tests::three")
        partitions = [LANE.select_tests(names, ["server"], children)]
        partitions.extend(LANE.select_tests(names, [child]) for child in children)
        flattened = [name for part in partitions for name in part]
        self.assertEqual(sorted(flattened), sorted(names))
        self.assertEqual(len(set(flattened)), len(names))
        self.assertEqual(partitions[0], names[3:])

    def test_excluding_all_tests_fails_closed(self):
        with self.assertRaises(ValueError):
            LANE.select_tests(["server::tests::one"], ["server"], ["server::tests"])

    def test_bad_or_empty_module_selection_fails_closed(self):
        for modules in ([], ["server", "server"], ["server::"], ["unknown"], ["server;echo"],
                        ["server::::tests"], ["::server"], ["server::9tests"]):
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

    @patch.object(LANE.subprocess, "run")
    def test_qualified_exclusion_manifest_and_failure_exit_are_preserved(self, run):
        run.side_effect = [subprocess.CompletedProcess([], 0, stdout=
                           "server::session_policy_tests::one: test\nserver::tests::two: test\n"),
                           subprocess.CompletedProcess([], 29)]
        with tempfile.TemporaryDirectory() as tmp:
            manifest = Path(tmp) / "selected.json"
            self.assertEqual(LANE.run_lane("pinned", ["server"], manifest,
                                           ["server::session_policy_tests"]), 29)
            report = json.loads(manifest.read_text())
            self.assertEqual(report["tests"], ["server::tests::two"])
            self.assertEqual(report["exclude_modules"], ["server::session_policy_tests"])
            self.assertEqual(report["module_counts"], {"server": 1})
        self.assertEqual(run.call_args_list[1].args[0][-2:], ["--exact", "server::tests::two"])


if __name__ == "__main__":
    unittest.main()
