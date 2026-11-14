"""Behavioral controls for fast/deep verification selection."""
import importlib.util
import json
from pathlib import Path
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("ci_cohort", Path(__file__).with_name("ci-cohort.py"))
cohort = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cohort)


class SelectionTests(unittest.TestCase):
    def test_capacity_qualification_is_deep_or_affected(self):
        self.assertNotIn("capacity_runner", cohort.runtime_targets())
        self.assertFalse(cohort.classify(["src/server.rs"])["capacity"])
        self.assertFalse(cohort.classify(["docs/index.md"])["capacity"])
        for source in ["tests/capacity_runner/main.rs", "tests/capacity_runner/oracle.rs",
                       "benches/capacity.rs", "scripts/run-capacity.sh", "Cargo.toml",
                       "Cargo.lock", "build.rs", "rust-toolchain.toml", ".github/workflows/ci.yml"]:
            with self.subTest(source=source):
                self.assertTrue(cohort.classify([source])["capacity"])

    def test_deep_runtime_keeps_all_root_targets(self):
        for event in ["schedule", "workflow_dispatch"]:
            with self.subTest(event=event):
                with mock.patch("sys.argv", ["ci-cohort.py", "--runtime", "--dry-run", "--event", event]):
                    with mock.patch("builtins.print") as output:
                        cohort.main()
                command = json.loads(output.call_args.args[0])
                self.assertNotIn("--lib", command)
                self.assertNotIn("--test", command)
        manifest = cohort.tomllib.loads((cohort.ROOT / "Cargo.toml").read_text())
        self.assertIn("capacity_runner", {target["name"] for target in manifest["test"]})

    def test_capacity_job_keeps_complete_suite_and_required_result(self):
        workflow = (cohort.ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("  capacity-qualification:", 1)[1].split("  required-result:", 1)[0]
        self.assertIn("needs.quick-check.outputs.capacity == 'true'", job)
        self.assertIn("--test capacity_runner --no-tests fail", job)
        self.assertNotIn("-E ", job)
        self.assertIn("capacity-qualification", cohort.REQUIRED)
        docs = (cohort.ROOT / ".github/workflows/doc-validation.yml").read_text()
        self.assertIn('      - "build.rs"', docs.split("concurrency:", 1)[0])

    def test_capacity_result_can_skip_only_when_unselected(self):
        results = {name: {"result": "success"} for name in cohort.REQUIRED}
        results["capacity-qualification"] = {"result": "skipped"}
        self.assertEqual(cohort.failed_results(results, "true", "true", "false"), [])
        for selected in ["true", ""]:
            self.assertIn("capacity-qualification", cohort.failed_results(results, "true", "true", selected))
        for status in ["failure", "cancelled"]:
            results["capacity-qualification"] = {"result": status}
            self.assertIn("capacity-qualification", cohort.failed_results(results, "true", "true", "false"))
        results.pop("capacity-qualification")
        self.assertIn("capacity-qualification", cohort.failed_results(results, "true", "true", "false"))

    def test_core_source_ownership_compiles_every_extracted_input(self):
        self.assertEqual(cohort.LEAN, cohort.CORE_SOURCE_IMPORTS)
        for source in cohort.LEAN:
            self.assertEqual(cohort.source_owner(source), "formal/source-linked/lean/Cargo.toml")
        self.assertEqual(cohort.source_owner("src/server.rs"), "Cargo.toml")
        with mock.patch.object(cohort, "CORE_SOURCE_IMPORTS", set()):
            for source in cohort.LEAN:
                self.assertEqual(cohort.source_owner(source), "Cargo.toml")
        with mock.patch.object(Path, "read_text", return_value='#[cfg(feature = "future")]'):
            for source in cohort.LEAN:
                self.assertEqual(cohort.source_owner(source), "Cargo.toml")

    def test_quick_gate_does_not_compile_twice(self):
        workflow = (cohort.ROOT / ".github/workflows/ci.yml").read_text()
        commands = workflow.split("  quick-check:", 1)[1].split("  ci-matrix:", 1)[0]
        self.assertNotIn("cargo check", commands)

    def test_required_results_reject_missing_failed_and_cancelled_checks(self):
        results = {name: {"result": "success"} for name in cohort.REQUIRED}
        self.assertEqual(cohort.failed_results(results, "true", "true"), [])
        for status in ["failure", "cancelled", "skipped"]:
            results["nextest"] = {"result": status}
            self.assertIn("nextest", cohort.failed_results(results, "true", "true"))
        results.pop("nextest")
        self.assertIn("nextest", cohort.failed_results(results, "false", "false"))

    def test_only_unselected_specialists_can_skip(self):
        results = {name: {"result": "success"} for name in cohort.REQUIRED}
        results["package-validation"] = {"result": "skipped"}
        self.assertEqual(cohort.failed_results(results, "true", "false"), [])
        self.assertIn("package-validation", cohort.failed_results(results, "true", "true"))
        self.assertIn("package-validation", cohort.failed_results(results, "true", ""))

    def test_unknown_inputs_keep_conservative_coverage(self):
        self.assertTrue(all(cohort.classify(["new-subsystem/model.rs"]).values()))
        self.assertTrue(all(cohort.classify(["new-build-config"]).values()))

    def test_extractor_inventory_is_the_selection_source_of_truth(self):
        sources = cohort.extracted_sources('SOURCES = ("src/new_core.rs",)')
        self.assertEqual(sources, {"src/new_core.rs"})
        with self.assertRaises(ValueError):
            cohort.extracted_sources("SOMETHING_ELSE = ()")

    def test_runtime_change_does_not_repeat_fault_campaign(self):
        self.assertFalse(cohort.classify(["src/server.rs"])["proof_faults"])
        self.assertFalse(cohort.classify(["src/server.rs"])["lean"])
        self.assertTrue(cohort.classify(["src/server.rs"])["fuzz"])

    def test_every_extracted_source_rechecks_positive_proofs(self):
        for source in cohort.LEAN:
            with self.subTest(source=source):
                self.assertTrue(cohort.classify([source])["lean"])
                self.assertFalse(cohort.classify([source])["proof_faults"])

    def test_proof_harness_and_toolchain_require_fault_controls(self):
        for source in ["formal/source-linked/lean/verify.py", "Cargo.lock", "rust-toolchain.toml", ".github/workflows/formal-verification.yml"]:
            with self.subTest(source=source):
                self.assertTrue(cohort.classify([source])["proof_faults"])

    def test_packaging_and_privacy_inputs_cannot_skip(self):
        for source in ["Cargo.toml", "build.rs", "src/lib.rs", "src/websocket/mod.rs", "tests/ui/private.rs"]:
            with self.subTest(source=source):
                self.assertTrue(cohort.classify([source])["packaging"])

    def test_nested_builds_are_not_in_runtime_compilation(self):
        self.assertTrue(cohort.runtime_targets())
        self.assertFalse(set(cohort.runtime_targets()) & cohort.NESTED)


if __name__ == "__main__":
    unittest.main()
