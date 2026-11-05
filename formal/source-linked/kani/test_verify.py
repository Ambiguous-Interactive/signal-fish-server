"""Fail-closed checks for the pinned Kani result reader."""

import copy
import unittest

import verify


def report():
    return {
        "metadata": {"kani_version": verify.VERSION},
        "verification_results": {"results": [
            {
                "harness_id": name,
                "status": "Success",
                "checks": ([
                    {"category": "cover", "description": cover, "status": "Satisfied"}
                    for cover in sorted(verify.COVERS[name])
                ] + [
                    {"category": "assertion", "description": contract, "status": "Success"}
                    for contract in sorted(verify.CONTRACTS[name])
                ]),
            }
            for name in sorted(verify.HARNESSES)
        ]},
    }


class ResultValidation(unittest.TestCase):
    def test_complete_production_proofs_pass(self):
        verify.check_report(report(), verify.HARNESSES)

    def test_missing_or_unreachable_witness_fails(self):
        for status in ("Unreachable", "Unsatisfiable", "Undetermined", "Failure"):
            with self.subTest(status=status):
                candidate = report()
                candidate["verification_results"]["results"][1]["checks"][0]["status"] = status
                with self.assertRaises(RuntimeError):
                    verify.check_report(candidate, verify.HARNESSES)
        candidate = report()
        candidate["verification_results"]["results"][1]["checks"].pop(0)
        with self.assertRaises(RuntimeError):
            verify.check_report(candidate, verify.HARNESSES)

    def test_empty_or_duplicate_harness_results_fail(self):
        candidate = report()
        results = candidate["verification_results"]["results"]
        for replacement in ([], results + [copy.deepcopy(results[0])], results[:-1]):
            with self.subTest(count=len(replacement)):
                candidate["verification_results"]["results"] = replacement
                with self.assertRaises(RuntimeError):
                    verify.check_report(candidate, verify.HARNESSES)

    def test_tool_version_drift_fails(self):
        candidate = report()
        candidate["metadata"]["kani_version"] = "0.0.0"
        with self.assertRaises(RuntimeError):
            verify.check_report(candidate, verify.HARNESSES)

    def test_named_contract_must_be_present_and_reachable(self):
        candidate = report()
        candidate["verification_results"]["results"][1]["checks"][-1]["status"] = "Unreachable"
        with self.assertRaises(RuntimeError):
            verify.check_report(candidate, verify.HARNESSES)

    def test_obligations_cannot_move_to_a_different_harness(self):
        candidate = report()
        results = candidate["verification_results"]["results"]
        results[0]["checks"].extend(results[1]["checks"])
        results[1]["checks"] = []
        with self.assertRaises(RuntimeError):
            verify.check_report(candidate, verify.HARNESSES)

    def test_unknown_status_rejects_otherwise_valid_negative(self):
        for status in ("Undetermined", "SolverError", "FutureStatus"):
            candidate = {
                "metadata": {"kani_version": verify.VERSION},
                "verification_results": {"results": [{
                    "harness_id": "mutant", "status": "Failure",
                    "checks": [
                        {"category": "assertion", "description": "client ceiling", "status": "Failure"},
                        {"category": "assertion", "description": "other", "status": status},
                    ],
                }]},
            }
            with self.subTest(status=status), self.assertRaises(RuntimeError):
                verify.check_report(candidate, {"mutant"}, "client ceiling")

    def test_mutant_requires_named_assertion_failure(self):
        for category, description, status, accepted in [
            ("assertion", '"client ceiling"', "Failure", True),
            ("assertion", "different property", "Failure", False),
            ("unwinding_assertion", "client ceiling", "Failure", False),
            ("assertion", "client ceiling", "Success", False),
            ("unsupported_construct", "client ceiling", "Failure", False),
        ]:
            with self.subTest(category=category, description=description, status=status):
                candidate = {
                    "metadata": {"kani_version": verify.VERSION},
                    "verification_results": {"results": [{
                        "harness_id": "mutant", "status": "Failure",
                        "checks": [{"category": category, "description": description, "status": status}],
                    }]},
                }
                if accepted:
                    verify.check_report(candidate, {"mutant"}, "client ceiling")
                else:
                    with self.assertRaises(RuntimeError):
                        verify.check_report(candidate, {"mutant"}, "client ceiling")


if __name__ == "__main__":
    unittest.main()
