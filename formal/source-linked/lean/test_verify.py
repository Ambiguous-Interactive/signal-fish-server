"""Require complete, explicit Lean theorem axiom reports."""

import unittest
from unittest.mock import patch

import verify


class AxiomAudit(unittest.TestCase):
    def test_exact_tool_version_required(self):
        verify.check_version("hax\nversion=0.4.2\ncommit=example")
        for output in ("version=0.4.20", "version=0.4.2-dev", "prefix version=0.4.2", ""):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                verify.check_version(output)

    def test_capability_probe_rejects_unrelated_failures(self):
        output = f"error: Capability/Extraction/Funs.lean:42:6: {verify.CAPABILITY_LIMIT}\n"
        self.assertIn("unsupported", verify.classify_capability(1, output))
        self.assertIn("no aggregation theorem", verify.classify_capability(0, ""))
        for extra in ("error: dependency failed\n", "error: Capability/Extraction/Funs.lean:43:2: syntax error\n"):
            with self.subTest(extra=extra), self.assertRaises(RuntimeError):
                verify.classify_capability(1, output + extra)
        with self.assertRaises(RuntimeError):
            verify.classify_capability(1, "download failed")

    def report(self, axioms="propext, Quot.sound, Classical.choice"):
        return "\n".join(
            f"'{name}' depends on axioms: [{axioms}]"
            for name in verify.THEOREMS["negotiation"]
        )

    @patch("builtins.print")
    def test_standard_axioms_and_closed_proofs_pass(self, _print):
        verify.check_axioms(self.report(), "negotiation")
        verify.check_axioms("\n".join(
            f"'{name}' does not depend on any axioms"
            for name in verify.THEOREMS["negotiation"]
        ), "negotiation")

    def test_missing_or_wrong_theorem_report_fails(self):
        for output in ("", self.report().splitlines()[0],
                       self.report().replace("explicit_client_contract", "unrelated")):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                verify.check_axioms(output, "negotiation")

    def test_placeholder_or_unreviewed_axiom_fails(self):
        for axiom in ("sorryAx", "unchecked_model", "Lean.ofReduceBool"):
            with self.subTest(axiom=axiom), self.assertRaises(RuntimeError):
                verify.check_axioms(self.report(axiom), "negotiation")



if __name__ == "__main__":
    unittest.main()
