"""Require complete, explicit Lean theorem axiom reports."""

import hashlib
import json
from pathlib import Path
import tempfile
import tomllib
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import verify


class AxiomAudit(unittest.TestCase):
    """Reject tool drift, missing reports, and unapproved proof assumptions."""

    def test_exact_tool_version_required(self):
        """Similar version strings must not satisfy the pinned-version check."""
        verify.check_version("hax\nversion=0.4.2\ncommit=example")
        for output in ("version=0.4.20", "version=0.4.2-dev", "prefix version=0.4.2", ""):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                verify.check_version(output)

    def test_capability_probe_rejects_unrelated_failures(self):
        """Only the known extraction defect permits the compatibility retry."""
        output = f"error: Capability/Extraction/Funs.lean:42:6: {verify.CAPABILITY_LIMIT}\n"
        self.assertIn("unsupported", verify.classify_capability(1, output))
        self.assertIn("no aggregation theorem", verify.classify_capability(0, ""))
        for extra in ("error: dependency failed\n", "error: Capability/Extraction/Funs.lean:43:2: syntax error\n"):
            with self.subTest(extra=extra), self.assertRaises(RuntimeError):
                verify.classify_capability(1, output + extra)
        with self.assertRaises(RuntimeError):
            verify.classify_capability(1, "download failed")

    def report(self, axioms="propext, Quot.sound, Classical.choice"):
        """Build complete named theorem reports with controlled axiom lists."""
        return "\n".join(
            f"'{name}' depends on axioms: [{axioms}]"
            for name in verify.THEOREMS["negotiation"]
        )

    @patch("builtins.print")
    def test_standard_axioms_and_closed_proofs_pass(self, _print):
        """Accept approved axioms and reports of proofs with no axioms."""
        verify.check_axioms(self.report(), "negotiation")
        verify.check_axioms("\n".join(
            f"'{name}' does not depend on any axioms"
            for name in verify.THEOREMS["negotiation"]
        ), "negotiation")

    def test_missing_or_wrong_theorem_report_fails(self):
        """Reject empty, partial, or unrelated theorem evidence."""
        for output in ("", self.report().splitlines()[0],
                       self.report().replace("explicit_client_contract", "unrelated")):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                verify.check_axioms(output, "negotiation")

    def test_placeholder_or_unreviewed_axiom_fails(self):
        """Proof holes and unapproved assumptions must fail the audit."""
        for axiom in ("sorryAx", "unchecked_model", "Lean.ofReduceBool"):
            with self.subTest(axiom=axiom), self.assertRaises(RuntimeError):
                verify.check_axioms(self.report(axiom), "negotiation")


class RequiredAggregation(unittest.TestCase):
    """Require aggregation contracts, exact external mapping, and real faults."""

    def test_all_production_mutation_anchors_are_unique(self):
        """Each actual backend mutation must change exactly one production decision."""
        definitions = (verify.negotiation_mutations(), verify.capability_mutations(),
                       verify.projection_mutations())
        self.assertEqual(sum(map(len, definitions)), 87)
        verify.check_mutation_anchors(verify.ROOT)

    def test_mutation_preflight_rejects_source_drift_before_external_commands(self):
        """Missing and duplicate anchors fail before preparation or any proof tool runs."""
        for definitions in (verify.negotiation_mutations(), verify.capability_mutations(),
                            verify.projection_mutations()):
            name, (source, before, _) = next(iter(definitions.items()))
            for mode in ("missing", "duplicate"):
                with self.subTest(name=name, mode=mode), tempfile.TemporaryDirectory() as tmp:
                    root = Path(tmp)
                    verify.copy_sources(root)
                    path = root / source
                    original = path.read_text()
                    path.write_text(original.replace(before, "") if mode == "missing"
                                    else original + "\n" + before)
                    with patch.object(verify, "ROOT", root), \
                            patch("sys.argv", ["verify.py", "--prepare", "--mutations"]), \
                            patch.object(verify, "run") as run, \
                            patch.object(verify, "prepare") as prepare:
                        with self.assertRaisesRegex(RuntimeError, "Mutation anchor changed:"):
                            verify.main()
                        run.assert_not_called()
                        prepare.assert_not_called()

    @patch("builtins.print")
    def test_every_required_scenario_needs_complete_approved_axiom_reports(self, _print):
        """Reject omitted aggregation contracts and proof holes in every scenario."""
        for scenario, theorems in verify.THEOREMS.items():
            self.assertTrue(theorems)
            report = "\n".join(f"'{name}' depends on axioms: [propext, Quot.sound]"
                               for name in theorems)
            with self.subTest(scenario=scenario):
                verify.check_axioms(report, scenario)
                for output in ("", "\n".join(report.splitlines()[:-1]),
                               report.replace("Quot.sound", "sorryAx")):
                    with self.assertRaises(RuntimeError):
                        verify.check_axioms(output, scenario)
        self.assertIn("capability", verify.THEOREMS)

    def test_projection_controls_reject_only_contract_diagnostics(self):
        """Never count projection compiler/tool failures as theorem rejection."""
        diagnostic = "error: Projection/Verification/ProofObligations.lean:42:6: unsolved goals\n"
        verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                     "projection", "version-floor")
        for output in (diagnostic.replace("Verification/ProofObligations", "Extraction/Funs"),
                       diagnostic + "error: dependency unavailable\n", ""):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=output),
                                             "projection", "version-floor")
        with self.assertRaises(RuntimeError):
            verify.check_proof_rejection(SimpleNamespace(returncode=0, stdout=diagnostic),
                                         "projection", "version-floor")

    def test_peer_faults_require_only_semantic_graph_failures(self):
        """Compiler errors and failed dependencies cannot count as protocol evidence."""
        diagnostic = "error: Capability/Verification/PeerEdges.lean:42:6: unsolved goals\n"
        for name in ("peer-self", "peer-recipient", "peer-capability", "peer-mesh", "peer-star", "peer-host", "peer-relay"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("PeerEdges.lean", "HostValidity.lean")),
                (1, diagnostic.replace("Verification/PeerEdges", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic + "error: dependency unavailable\n"),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_finalize_faults_require_only_semantic_composition_failures(self):
        """Source faults must compile and fail the complete-plan semantic bridge."""
        diagnostic = "error: Capability/Verification/Finalize.lean:42:6: unsolved goals\n"
        names = [name for name in verify.capability_mutations() if name.startswith("finalize-")]
        self.assertEqual(len(names), 5)
        for name in names:
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Finalize.lean", "Selection.lean")),
                (1, diagnostic.replace("Verification/Finalize", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_membership_faults_require_only_semantic_transition_failures(self):
        """Compiler errors and failed dependencies cannot count as protocol evidence."""
        diagnostic = "error: Capability/Verification/Membership.lean:42:6: unsolved goals\n"
        for name in ("membership-validity", "membership-pair", "membership-update", "membership-replan"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Membership.lean", "HostValidity.lean")),
                (1, diagnostic.replace("Verification/Membership", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic + "error: dependency unavailable\n"),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_resolution_faults_require_only_semantic_default_contract_failures(self):
        """Wrong-profile and fallback faults cannot pass on compiler or unrelated errors."""
        diagnostic = "error: Projection/Verification/Defaults.lean:42:6: unsolved goals\n"
        for name in ("default-version", "overwrite-profile-version"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "projection", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Defaults.lean", "ProofObligations.lean")),
                (1, diagnostic.replace("Verification/Defaults", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic + "error: dependency unavailable\n"),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "projection", name)

    def test_profile_faults_require_semantic_production_contract_failures(self):
        """Reject bad tools, bad extraction, and unrelated diagnostics for caller faults."""
        diagnostic = "error: Projection/Verification/Profiles.lean:42:6: unsolved goals\n"
        for name in ("server-floor", "profile-version", "profile-transports"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "projection", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Profiles.lean", "Defaults.lean")),
                (1, diagnostic.replace("Verification/Profiles", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "projection", name)

    def test_admission_faults_require_only_semantic_admission_failures(self):
        """Reject extraction, unknown names, unrelated proofs, and accepted source faults."""
        diagnostic = "error: Projection/Verification/Admission.lean:42:6: unsolved goals\n"
        for name in ("admission-version", "admission-topology", "admission-transport", "admission-host"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "projection", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Admission.lean", "Profiles.lean")),
                (1, diagnostic.replace("Verification/Admission", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "projection", name)

    def test_pregather_faults_require_only_semantic_gate_failures(self):
        """Each omitted gate must reject its own contract, after successful extraction."""
        diagnostic = "error: Projection/Verification/Pregather.lean:42:6: unsolved goals\n"
        names = [name for name in verify.projection_mutations() if name.startswith("pregather-")]
        self.assertEqual(len(names), 7)
        for name in names:
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "projection", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Pregather.lean", "Admission.lean")),
                (1, diagnostic.replace("Verification/Pregather", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "projection", name)

    def test_transport_status_faults_require_only_semantic_gate_failures(self):
        """Each changed state rule must reject its own contract, after successful extraction."""
        diagnostic = "error: Projection/Verification/TransportStatus.lean:42:6: unsolved goals\n"
        names = [name for name in verify.projection_mutations() if name.startswith("status-")]
        self.assertEqual(len(names), 6)
        for name in names:
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "projection", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("TransportStatus.lean", "Admission.lean")),
                (1, diagnostic.replace("Verification/TransportStatus", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "projection", name)

    def test_axis_faults_require_only_owning_semantic_failures(self):
        """Compiler failures and failures outside the changed rule never count as evidence."""
        names = [name for name in verify.projection_mutations() if name.startswith("axes-")]
        self.assertEqual(len(names), 7)
        for name in names:
            module = "CapabilityNegotiation" if name in ("axes-dedup", "axes-order") else "NegotiatedAxes"
            diagnostic = f"error: Projection/Verification/{module}.lean:42:6: unsolved goals\n"
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic), "projection", name)
            verify.check_proof_rejection(SimpleNamespace(returncode=1,
                stdout=diagnostic.replace("unsolved goals", "Tactic `rfl` failed")), "projection", name)
            for code, output in ((0, diagnostic), (1, diagnostic.replace(module, "Admission")),
                                 (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                                 (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                                 (1, diagnostic + "error: dependency unavailable\n"), (1, "")):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output), "projection", name)

    def test_host_faults_require_only_semantic_host_contract_rejection(self):
        """A UUID mapping/compiler error cannot count as a host-validity rejection."""
        diagnostic = "error: Capability/Verification/HostValidity.lean:42:6: unsolved goals\n"
        for name in ("host-identity", "host-capability", "host-topology", "host-missing"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("HostValidity.lean", "Members.lean")),
                (1, diagnostic.replace("Verification/HostValidity", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_executability_faults_require_only_semantic_contract_rejection(self):
        """Compiler, tool and unrelated proof errors cannot count as executability rejection."""
        diagnostic = "error: Capability/Verification/Executability.lean:42:6: unsolved goals\n"
        for name in ("executable-transport", "executable-capability", "executable-missing"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Executability.lean", "Members.lean")),
                (1, diagnostic.replace("Verification/Executability", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_election_faults_require_only_semantic_contract_rejection(self):
        """A changed iterator signature or mapping failure is not a rejected election fault."""
        diagnostic = "error: Capability/Verification/Election.lean:42:6: unsolved goals\n"
        for name in ("election-authority", "election-date", "election-uuid"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Election.lean", "ElectionOrder.lean")),
                (1, diagnostic.replace("Election.lean", "Selection.lean")),
                (1, diagnostic.replace("Verification/Election", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_eligible_faults_require_only_semantic_contract_rejection(self):
        """Filter compiler errors and unrelated election failures are not eligible-election evidence."""
        diagnostic = "error: Capability/Verification/EligibleElection.lean:42:6: unsolved goals\n"
        for name in ("eligible-capability", "eligible-endpoint", "eligible-authority"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("EligibleElection.lean", "Election.lean")),
                (1, diagnostic.replace("EligibleElection.lean", "ElectionFixtures.lean")),
                (1, diagnostic.replace("EligibleElection.lean", "Members.lean")),
                (1, diagnostic.replace("Verification/EligibleElection", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic.replace("unsolved goals", "Tactic `rewrite` failed")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_selection_faults_require_only_semantic_contract_rejection(self):
        """Neither compiler errors nor an unrelated failed proof establish a selector fault."""
        diagnostic = "error: Capability/Verification/Selection.lean:42:6: unsolved goals\n"
        for name in ("selection-mesh-order", "selection-host-order", "selection-exact-ceiling",
                     "selection-no-ceiling", "selection-switch", "selection-members",
                     "selection-execution", "selection-floor"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                         "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Selection.lean", "SelectionEvaluation.lean")),
                (1, diagnostic.replace("Selection.lean", "Executability.lean")),
                (1, diagnostic.replace("Verification/Selection", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic.replace("unsolved goals", "Type mismatch")),
                (1, diagnostic.replace("unsolved goals", "Tactic `rewrite` failed")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, ""),
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_member_faults_require_only_actual_member_contract_rejection(self):
        """Reject extraction and tool failures for the concrete getter and host controls."""
        diagnostic = "error: Capability/Verification/Members.lean:42:6: unsolved goals\n"
        names = ("member-getter", "member-support", "member-direct-endpoint", "member-webrtc-endpoint")
        for name in names:
            for failure in ("unsolved goals", "Type mismatch", "Tactic `rfl` failed"):
                with self.subTest(name=name, failure=failure):
                    verify.check_proof_rejection(SimpleNamespace(
                        returncode=1, stdout=diagnostic.replace("unsolved goals", failure)),
                        "capability", name)
            for code, output in (
                (0, diagnostic),
                (1, diagnostic.replace("Members.lean", "ConcreteCallbacks.lean")),
                (1, diagnostic.replace("Verification/Members", "Extraction/Funs")),
                (1, diagnostic.replace("unsolved goals", "unknown identifier")),
                (1, diagnostic + "error: dependency unavailable\n"),
                (1, diagnostic + "error: Capability/Extraction/Types.lean:5:1: Type mismatch\n"),
                (1, ""),
            ):
                with self.subTest(name=name, code=code, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                                 "capability", name)

    def test_all_production_inputs_are_copied_and_missing_input_fails(self):
        """Isolated faults and extraction need every source, with unchanged bytes."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "original"
            for name in verify.SOURCES:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(name)
            destination = root / "copy"
            with patch.object(verify, "ROOT", source):
                verify.copy_sources(destination)
                for name in verify.SOURCES:
                    self.assertEqual((destination / name).read_bytes(), (source / name).read_bytes())
                (source / verify.SOURCES[-1]).unlink()
                with self.assertRaises(FileNotFoundError):
                    verify.copy_sources(root / "incomplete")

    def test_required_mapping_and_import_must_match_reviewed_alias(self):
        """Reject a changed external iterator model or missing import."""
        with tempfile.TemporaryDirectory() as tmp:
            crate = Path(tmp)
            canonical = crate / "compatibility/IteratorAll.lean"
            canonical.parent.mkdir()
            canonical.write_text("upstream alias\n")
            project = crate / "proofs/capability/lean"
            target = project / "Capability/Compatibility.lean"
            target.parent.mkdir(parents=True)
            assumptions = project / "Capability/Assumptions/FunsExternal.lean"
            assumptions.parent.mkdir()
            target.write_text(canonical.read_text())
            assumptions.write_text("import Capability.Compatibility\nimport CoreModels\n")
            verify.check_iterator_alias(project, crate)
            target.write_text("independent replacement\n")
            with self.assertRaisesRegex(RuntimeError, "differs"):
                verify.check_iterator_alias(project, crate)
            target.write_text(canonical.read_text())
            for imports in ("import CoreModels\n", "import Capability.Compatibility\n" * 2):
                assumptions.write_text(imports)
                with self.assertRaisesRegex(RuntimeError, "missing or duplicated"):
                    verify.check_iterator_alias(project, crate)

    def test_only_contract_failures_count_as_rejected_mutations(self):
        """Distinguish contract rejections from extraction and compiler failures."""
        good = "error: Capability/Verification/ProofObligations.lean:42:6: unsolved goals\n"
        verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=good),
                                     "capability", "empty-room")
        semantic = good.replace("ProofObligations.lean", "CallbackSemantics.lean")
        verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=semantic),
                                     "capability", "extra-callback")
        with self.assertRaisesRegex(RuntimeError, "did not fail the callback semantics"):
            verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=good),
                                         "capability", "extra-callback")
        for code, output in (
            (0, good), (1, "dependency failed"),
            (1, good + "error: Capability/Extraction/Funs.lean:5:1: Type mismatch\n"),
            (1, good.replace("unsolved goals", "unknown identifier")),
            (1, good.replace("Capability/", "Negotiation/")),
            (1, semantic + "error: Capability/Verification/Callbacks.lean:5:1: Type mismatch\n"),
        ):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                             "capability", "empty-room")

    def test_concrete_fault_requires_concrete_contract_rejection(self):
        """Do not count another failed theorem or a callback compiler error as this control."""
        diagnostic = "error: Capability/Verification/ConcreteCallbacks.lean:42:6: unsolved goals\n"
        verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=diagnostic),
                                     "capability", "wrong-callback-transport")
        for code, output in (
            (0, diagnostic),
            (1, diagnostic.replace("ConcreteCallbacks.lean", "ProofObligations.lean")),
            (1, diagnostic.replace("Verification/ConcreteCallbacks", "Extraction/Funs")),
            (1, diagnostic + "error: dependency unavailable\n"),
            (1, diagnostic.replace("unsolved goals", "unknown identifier")),
            (1, diagnostic.replace("unsolved goals", "Type mismatch")),
            (1, diagnostic + "error: Capability/Verification/ConcreteCallbacks.lean:44:8: Type mismatch\n"),
            (1, diagnostic + "error: Capability/Verification/ConcreteCallbacks.lean:44:8: unknown identifier\n"),
            (1, diagnostic.replace("unsolved goals", "Tactic `subst` failed")),
        ):
            with self.subTest(code=code, output=output), self.assertRaises(RuntimeError):
                verify.check_proof_rejection(SimpleNamespace(returncode=code, stdout=output),
                                             "capability", "wrong-callback-transport")

    def test_evidence_walk_never_descends_into_caches(self):
        """Prevent large dependency cache scans before collecting proof evidence."""
        with tempfile.TemporaryDirectory() as tmp:
            project = Path(tmp)
            excluded = [project / name for name in (".lake", ".lake-host", "llbc")]
            for cache in excluded:
                cache.mkdir()
                (cache / "large-cache").write_text("not evidence")
            (project / "Proof.lean").write_text("contract")
            (project / "linked-cache").symlink_to(excluded[0], target_is_directory=True)
            (project / "linked-file").symlink_to(project / "Proof.lean")
            scandir = verify.os.scandir

            def guarded_scandir(path):
                """Fail if the walker opens any excluded directory."""
                self.assertNotIn(Path(path), excluded + [project / "linked-cache"])
                return scandir(path)

            with patch.object(verify.os, "scandir", guarded_scandir):
                self.assertEqual(verify.project_evidence_inputs(project), [project / "Proof.lean"])

    def test_required_verification_rejects_nondeterministic_extraction(self):
        """Reject unstable generated definitions before building any theorem."""
        for drift in (False, True):
            with self.subTest(drift=drift), tempfile.TemporaryDirectory() as tmp:
                crate = Path(tmp)
                generated = crate / "proofs/capability/lean/Capability/Extraction/Funs.lean"
                generated.parent.mkdir(parents=True)
                calls = []

                def extract(_crate, scenario):
                    """Emit stable generated text or a deliberate second-run change."""
                    self.assertEqual(scenario, "capability")
                    calls.append(scenario)
                    generated.write_text(str(len(calls)) if drift else "stable")

                with patch.object(verify, "extract", extract), \
                        patch.object(verify, "check_iterator_alias"), \
                        patch.object(verify, "share_prepared_dependencies"), \
                        patch.object(verify, "run") as run, \
                        patch.object(verify, "check_axioms") as audit:
                    if drift:
                        with self.assertRaisesRegex(RuntimeError, "Nondeterministic"):
                            verify.verify_body(crate, "capability")
                        run.assert_not_called()
                        audit.assert_not_called()
                    else:
                        verify.verify_body(crate, "capability")
                        self.assertEqual(run.call_count, 2)
                        audit.assert_called_once()
                self.assertEqual(len(calls), 2)


class NegotiationEvidence(unittest.TestCase):
    """Retain isolated source faults before their temporary trees are removed."""

    def exercise_mutants(self, root, *, fail_second_witness=False):
        """Run real copying and archiving around simulated extraction/Lean commands."""
        for name in verify.SOURCES:
            source = root / name
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_bytes((verify.ROOT / name).read_bytes())
        original = (root / "src/protocol_core.rs").read_bytes()
        crate = root / "formal/source-linked/lean"
        project = crate / "proofs/negotiation/lean"
        files = {
            "Cargo.toml": "crate manifest",
            "Cargo.lock": "dependency pins",
            "hax.toml": "extractor pins",
            "src/lib.rs": "production module imports",
            "verify.py": "verification runner",
            "proofs/negotiation/lean/lean-toolchain": "Lean pin",
            "proofs/negotiation/lean/lake-manifest.json": "Lean dependency pins",
            "proofs/negotiation/lean/Negotiation/Verification/ProofObligations.lean": "contracts",
        }
        for name, content in files.items():
            path = crate / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        (project / ".lake/packages").mkdir(parents=True)
        logs = root / "logs"
        logs.mkdir()
        commands = []
        isolated = []

        def record(command, output, code=0):
            """Give the real archive command logs and timing records to preserve."""
            commands.append(command)
            log = f"{len(commands):02d}.log"
            (logs / log).write_text(output)
            verify.RUNS.append({"command": command, "seconds": 0.125,
                                "exit_code": code, "log": log})
            return SimpleNamespace(returncode=code, stdout=output)

        def extract(copied):
            """Emit a body tied to this actual mutated production source."""
            isolated.append(copied.parents[2])
            body = copied / "proofs/negotiation/lean/Negotiation/Extraction/Funs.lean"
            body.parent.mkdir(parents=True)
            body.write_text((copied.parents[2] / "src/protocol_core.rs").read_text())
            record(["cargo", "hax", "extract", "negotiation", "--locked"], "extracted")

        def run(command, copied_project, *, expect_success=True):
            """Simulate compiled extraction, semantic rejection, and audited witness."""
            if command == ["lake", "build", "Negotiation.Extraction"]:
                self.assertTrue(expect_success)
                return record(command, "extracted module compiled")
            if command == ["lake", "build"]:
                self.assertFalse(expect_success)
                return record(command,
                              "error: Negotiation/Verification/ProofObligations.lean:1:1: unsolved goals\n", 1)
            self.assertEqual(command, ["lake", "env", "lean", "Counterexample.lean"])
            self.assertTrue(expect_success)
            self.assertTrue((copied_project / "Counterexample.lean").is_file())
            if fail_second_witness and len(isolated) == 2:
                record(command, "witness failed", 1)
                raise RuntimeError("witness failed")
            return record(command, "'mutation_returns_wrong_value' does not depend on any axioms")

        with patch.object(verify, "ROOT", root), patch.object(verify, "LOGS", logs), \
                patch.object(verify, "RUNS", []), patch.object(verify, "extract", extract), \
                patch.object(verify, "run", run), patch.object(verify, "verify") as restored:
            if fail_second_witness:
                with self.assertRaisesRegex(RuntimeError, "witness failed"):
                    verify.mutants(crate, names=["lower-clamp", "ignore-server-ceiling"])
                restored.assert_not_called()
            else:
                verify.mutants(crate, names=["lower-clamp", "ignore-server-ceiling"])
                restored.assert_called_once_with(crate)
        self.assertEqual((root / "src/protocol_core.rs").read_bytes(), original)
        self.assertEqual(len(isolated), 2)
        self.assertTrue(all(not tree.exists() for tree in isolated))
        return logs, files, original

    @patch("builtins.print")
    def test_both_negotiation_faults_retain_complete_hashed_evidence(self, _print):
        """Keep both mutated sources, generated bodies, witnesses, pins, and timed logs."""
        with tempfile.TemporaryDirectory() as tmp:
            logs, files, original = self.exercise_mutants(Path(tmp))
            archives = sorted((logs / "proofs").glob("negotiation-*"))
            self.assertEqual(len(archives), 2)
            for archive in archives:
                report = json.loads((archive / "results.json").read_text())
                self.assertEqual(report["status"], "proof rejected; concrete wrong result kernel-checked")
                self.assertEqual(report["required_theorems"], list(verify.THEOREMS["negotiation"]))
                source = archive / "src/protocol_core.rs"
                self.assertNotEqual(source.read_bytes(), original)
                copied = archive / "formal/source-linked/lean"
                self.assertEqual(source.read_bytes(),
                                 (copied / "proofs/negotiation/lean/Negotiation/Extraction/Funs.lean").read_bytes())
                for name, content in files.items():
                    self.assertEqual((copied / name).read_text(), content)
                self.assertIn("#print axioms mutation_returns_wrong_value",
                              (copied / "proofs/negotiation/lean/Counterexample.lean").read_text())
                self.assertEqual([run["exit_code"] for run in report["runs"]], [0, 0, 1, 0])
                self.assertEqual(sum(run["seconds"] for run in report["runs"]), 0.5)
                for run in report["runs"]:
                    self.assertTrue((archive / run["log"]).is_file())
                for name, digest in report["files_sha256"].items():
                    self.assertEqual(hashlib.sha256((archive / name).read_bytes()).hexdigest(), digest)
            mutations = [(archive / "src/protocol_core.rs").read_text() for archive in archives]
            self.assertIn("if version < minimum { minimum } else { version }", mutations[0])
            self.assertIn("if true {", mutations[1])

    @patch("builtins.print")
    def test_failed_witness_preserves_fault_and_does_not_claim_acceptance_or_restoration(self, _print):
        """A failed second witness must leave its source and logs without an accepted status."""
        with tempfile.TemporaryDirectory() as tmp:
            logs, _files, _original = self.exercise_mutants(Path(tmp), fail_second_witness=True)
            archives = sorted((logs / "proofs").glob("negotiation-*"))
            self.assertEqual(len(archives), 2)
            reports = [json.loads((archive / "results.json").read_text()) for archive in archives]
            self.assertEqual(reports[0]["status"], "proof rejected; concrete wrong result kernel-checked")
            self.assertEqual(reports[1]["status"], "negative control failed")
            self.assertEqual([run["exit_code"] for run in reports[1]["runs"]], [0, 0, 1, 1])
            failed = archives[1]
            self.assertIn("if true {", (failed / "src/protocol_core.rs").read_text())
            self.assertTrue((failed / "formal/source-linked/lean/proofs/negotiation/lean/Counterexample.lean").is_file())
            self.assertEqual((failed / reports[1]["runs"][-1]["log"]).read_text(), "witness failed")


class PreparedDependencies(unittest.TestCase):
    """Share exact prepared pins on fresh checkouts without replacing local data."""

    def scaffold(self, root):
        """Create compatible project pins and one prepared dependency package."""
        crate = root / "formal/source-linked/lean"
        prepared = crate / "proofs/negotiation/lean"
        project = crate / "proofs/capability/lean"
        manifest = {"name": "Negotiation", "packagesDir": ".lake/packages",
                    "version": "1.2.0", "packages": [{"name": "hax", "rev": "pinned"}]}
        for directory, name in ((prepared, "Negotiation"), (project, "Capability")):
            directory.mkdir(parents=True)
            (directory / "lake-manifest.json").write_text(json.dumps(dict(manifest, name=name)))
            (directory / "lean-toolchain").write_text("leanprover/lean4:v4.31.0\n")
            (directory / "lakefile.toml").write_text(
                f'name = "{name}"\n[[require]]\nname = "hax"\nrev = "pinned"\n')
        packages = prepared / ".lake/packages"
        packages.mkdir(parents=True)
        (packages / "prepared-model").write_text("cached library")
        return crate, prepared, project

    def test_clean_project_shares_packages_idempotently_and_preserves_pins(self):
        """A fresh capability project must reuse cached models without editing pins."""
        with tempfile.TemporaryDirectory() as tmp:
            crate, prepared, project = self.scaffold(Path(tmp))
            pins = {directory / name: (directory / name).read_bytes()
                    for directory in (prepared, project)
                    for name in ("lake-manifest.json", "lakefile.toml", "lean-toolchain")}
            self.assertFalse((project / ".lake").exists())
            verify.share_prepared_dependencies(crate, project)
            verify.share_prepared_dependencies(crate, project)
            shared = project / ".lake/packages"
            self.assertTrue(shared.is_symlink())
            self.assertEqual(shared.resolve(), (prepared / ".lake/packages").resolve())
            self.assertEqual((shared / "prepared-model").read_text(), "cached library")
            self.assertEqual({path: path.read_bytes() for path in pins}, pins)

    def test_incompatible_graph_toolchain_or_requirements_fail_before_linking(self):
        """Never share models when manifest, dependency requirement, or Lean pins differ."""
        for change in ("manifest", "packagesDir", "toolchain", "requirements", "missing-cache"):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as tmp:
                crate, prepared, project = self.scaffold(Path(tmp))
                if change in ("manifest", "packagesDir"):
                    path = project / "lake-manifest.json"
                    manifest = json.loads(path.read_text())
                    if change == "manifest":
                        manifest["packages"][0]["rev"] = "different"
                    else:
                        manifest["packagesDir"] = "elsewhere"
                    path.write_text(json.dumps(manifest))
                elif change == "toolchain":
                    (project / "lean-toolchain").write_text("leanprover/lean4:other")
                elif change == "requirements":
                    path = project / "lakefile.toml"
                    path.write_text(path.read_text().replace('rev = "pinned"', 'rev = "other"'))
                else:
                    (prepared / ".lake/packages/prepared-model").unlink()
                    (prepared / ".lake/packages").rmdir()
                with self.assertRaises(RuntimeError):
                    verify.share_prepared_dependencies(crate, project)
                self.assertFalse((project / ".lake").exists())

    def test_existing_unrelated_directory_and_links_are_preserved(self):
        """Refuse conflicting local caches, including broken links, without deleting them."""
        for kind in ("directory", "different-link", "broken-link"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                crate, _prepared, project = self.scaffold(root)
                destination = project / ".lake/packages"
                destination.parent.mkdir()
                if kind == "directory":
                    destination.mkdir()
                    (destination / "user-data").write_text("preserve")
                else:
                    other = root / "other"
                    if kind == "different-link":
                        other.mkdir()
                    destination.symlink_to(other, target_is_directory=True)
                with self.assertRaisesRegex(RuntimeError, "Preserving conflicting"):
                    verify.share_prepared_dependencies(crate, project)
                if kind == "directory":
                    self.assertEqual((destination / "user-data").read_text(), "preserve")
                else:
                    self.assertEqual(destination.readlink(), root / "other")

    def test_required_capability_build_uses_prepared_packages_on_clean_checkout(self):
        """The normal required proof path must link cached dependencies before lake runs."""
        with tempfile.TemporaryDirectory() as tmp:
            crate, prepared, project = self.scaffold(Path(tmp))
            generated = project / "Capability/Extraction/Funs.lean"
            generated.parent.mkdir(parents=True)
            generated.write_text("stable extracted production definition")

            def run(_command, _project):
                """Observe the real dependency link before each mocked Lean command."""
                self.assertEqual((project / ".lake/packages").resolve(),
                                 (prepared / ".lake/packages").resolve())
                self.assertTrue((project / ".lake/packages").is_symlink())
                return SimpleNamespace(stdout="")

            with patch.object(verify, "extract"), patch.object(verify, "check_iterator_alias"), \
                    patch.object(verify, "run", run), patch.object(verify, "check_axioms"):
                verify.verify_body(crate, "capability")


class CapabilityEvidence(unittest.TestCase):
    """Keep failed and accepted extraction evidence separate from proof claims."""

    @patch("builtins.print")
    def test_main_probe_summary_distinguishes_unrequested_failed_and_passed(self, _print):
        """A failed optional probe must remain failed in the overall report."""
        accepted = "extraction_typechecked; no aggregation theorem proved"
        for requested, fails, expected in (
            (False, False, "not run; no aggregation theorem proved"),
            (True, True, "probe failed; no aggregation theorem proved"),
            (True, False, accepted),
        ):
            with self.subTest(requested=requested, fails=fails), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                verify.copy_sources(root)
                crate = root / "formal/source-linked/lean"
                verify.shutil.copytree(verify.HERE, crate, ignore=verify.shutil.ignore_patterns(
                    ".lake*", "target", "llbc", "__pycache__", "*.log"))
                logs = root / "logs"
                argv = ["verify.py"] + (["--probe-capability"] if requested else [])
                with patch.object(verify, "ROOT", root), patch.object(verify, "HERE", crate), \
                        patch.object(verify, "LOGS", logs), patch.object(verify, "RUNS", []), \
                        patch.object(verify, "run", return_value=SimpleNamespace(stdout="version=0.4.2")), \
                        patch.object(verify, "verify"), patch.object(verify, "probe_capability") as probe, \
                        patch("sys.argv", argv):
                    probe.side_effect = RuntimeError("probe failed") if fails else None
                    probe.return_value = accepted
                    if fails:
                        with self.assertRaisesRegex(RuntimeError, "probe failed"):
                            verify.main()
                    else:
                        verify.main()
                    self.assertEqual(probe.call_count, int(requested))
                report = json.loads((logs / "results.json").read_text())
                self.assertEqual(report["capability_probe_status"], expected)
                self.assertEqual(report["completed"], not fails)
                self.assertTrue(report["source_unchanged"])
                self.assertTrue(report["locks_unchanged"])

    def test_probe_manifest_isolates_only_target_and_fails_closed(self):
        """Preserve tool pins, backend options and other scenarios; reject missing scope."""
        original = ('[tools]\naeneas = "pinned"\n[scenario.capability]\n'
                    'backend = "lean"\ninclude = [\n "first",\n "second",\n]\n'
                    'charon-args = ["--opaque=keep"]\n[scenario.other]\ninclude = ["other"]\n')
        with tempfile.TemporaryDirectory() as tmp:
            crate = Path(tmp)
            path = crate / "hax.toml"
            path.write_text(original)
            verify.isolate_capability_probe(crate)
            expected = tomllib.loads(original)
            expected["scenario"]["capability"]["include"] = [
                "signal_fish_proof_core::protocol_core::nonempty_all_support"]
            self.assertEqual(tomllib.loads(path.read_text()), expected)
            for malformed in (original.replace('include = [\n "first",\n "second",\n]\n', ''),
                              '[scenario.other]\ninclude = ["other"]\n'):
                path.write_text(malformed)
                with self.assertRaises(RuntimeError):
                    verify.isolate_capability_probe(crate)
                self.assertEqual(path.read_text(), malformed)

    @patch("builtins.print")
    def test_probe_retry_and_deterministic_regeneration(self, _print):
        """Check retry boundaries, stable bodies, and honest failure snapshots."""
        missing = f"error: Capability/Extraction/Funs.lean:42:6: {verify.CAPABILITY_LIMIT}\n"
        for name, responses, drift, error, shim, extractions in (
            ("supported", [(0, ""), (0, "")], False, None, False, 2),
            ("backport", [(1, missing), (0, ""), (0, "")], False, None, True, 2),
            ("broken-backport", [(1, missing), RuntimeError("mapped typecheck failed")],
             False, "mapped typecheck failed", True, 1),
            ("unrelated-failure", [(1, "error: dependency failed\n")],
             False, "Unexpected capability probe failure", False, 1),
            ("drift", [(0, "")], True, "nondeterministic extraction", False, 2),
            ("repeat-extract", [(0, "")], RuntimeError("repeat extraction failed"),
             "repeat extraction failed", False, 2),
            ("backport-repeat-extract", [(1, missing), (0, "")], RuntimeError("repeat extraction failed"),
             "repeat extraction failed", True, 2),
            ("repeat-build", [(0, ""), RuntimeError("repeat typecheck failed")],
             False, "repeat typecheck failed", False, 2),
            ("backport-repeat-build", [(1, missing), (0, ""), RuntimeError("repeat typecheck failed")],
             False, "repeat typecheck failed", True, 2),
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / "src").mkdir()
                source = root / "src/protocol_core.rs"
                source.write_text("production source\n")
                for source_path in verify.SOURCES:
                    if source_path != "src/protocol_core.rs":
                        (root / source_path).write_text(f"production input {source_path}\n")
                crate = root / "formal/source-linked/lean"
                crate.mkdir(parents=True)
                hax_source = (verify.HERE / "hax.toml").read_text()
                (crate / "hax.toml").write_text(hax_source)
                negotiation = crate / "proofs/negotiation/lean"
                negotiation.mkdir(parents=True)
                (negotiation / "lake-manifest.json").write_text("{}")
                (crate / "compatibility").mkdir()
                (crate / "compatibility/IteratorAll.lean").write_text("upstream alias\n")
                logs = root / "logs"
                calls = []

                # The probe calls both mocks within this subtest. No callback
                # call is deferred to the next loop iteration.
                def extract(copied, scenario):
                    """Emit stable source, a changed body, or a repeat-extraction fault."""
                    self.assertEqual(scenario, "capability")
                    calls.append(copied)
                    if len(calls) == 2 and isinstance(drift, Exception):
                        raise drift
                    expected = tomllib.loads(hax_source)
                    expected["scenario"]["capability"]["include"] = [
                        "signal_fish_proof_core::protocol_core::nonempty_all_support"]
                    self.assertEqual(tomllib.loads((copied / "hax.toml").read_text()), expected)
                    project = copied / "proofs/capability/lean/Capability"
                    (project / "Extraction").mkdir(parents=True, exist_ok=True)
                    body = "changed body\n" if drift and len(calls) == 2 else "generated body\n"
                    (project / "Extraction/Funs.lean").write_text(body)
                    assumptions = project / "Assumptions/FunsExternal.lean"
                    if not assumptions.exists():
                        assumptions.parent.mkdir()
                        assumptions.write_text("import CoreModels\n")
                    (project / "Assumptions/TypesExternal.lean").write_text("generated external declarations\n")

                responses_iter = iter(responses)

                def run(command, project, *, expect_success=True):
                    """Return this subtest's scripted build results in call order."""
                    self.assertEqual(command, ["lake", "build", "Capability.Extraction"])
                    result = next(responses_iter)
                    if isinstance(result, Exception):
                        self.assertTrue(expect_success)
                        raise result
                    return SimpleNamespace(returncode=result[0], stdout=result[1])

                with patch.object(verify, "ROOT", root), patch.object(verify, "LOGS", logs), \
                        patch.object(verify, "RUNS", []), patch.object(verify, "extract", extract), \
                        patch.object(verify, "run", side_effect=run) as builds, \
                        patch.object(verify, "install_iterator_alias", wraps=verify.install_iterator_alias) as install:
                    if error:
                        with self.assertRaisesRegex(RuntimeError, error):
                            verify.probe_capability(crate)
                    else:
                        status = verify.probe_capability(crate)
                        self.assertIn("no aggregation theorem proved", status)
                        self.assertEqual("backport" in status, shim)
                self.assertEqual(install.call_count, int(shim))
                self.assertEqual(len(calls), extractions)
                self.assertEqual(builds.call_count, len(responses))
                self.assertEqual(source.read_text(), "production source\n")
                self.assertEqual((crate / "hax.toml").read_text(), hax_source)
                archives = sorted((logs / "capability-probe").iterdir())
                self.assertEqual(len(archives), 2 if shim else 1)
                if error:
                    final_status = json.loads((archives[-1] / "results.json").read_text())["status"]
                    self.assertNotIn("extraction_typechecked", final_status)
                    if "repeat" in name:
                        self.assertEqual(final_status,
                                         "repeat extraction/typecheck failed; no aggregation theorem proved")
                generated = "formal/source-linked/lean/proofs/capability/lean/Capability"
                for archive in archives:
                    self.assertFalse(json.loads((archive / "results.json").read_text())["aggregation_theorem_proved"])
                    self.assertEqual((archive / generated / "Assumptions/TypesExternal.lean").read_text(),
                                     "generated external declarations\n")
                    if drift is not True:
                        self.assertEqual((archive / generated / "Extraction/Funs.lean").read_text(), "generated body\n")
                mapped = archives[-1] / generated / "Compatibility.lean"
                self.assertEqual(mapped.exists(), shim)
                if shim:
                    self.assertFalse((archives[0] / generated / "Compatibility.lean").exists())
                    self.assertEqual(mapped.read_text(), "upstream alias\n")
                    self.assertEqual((archives[-1] / generated / "Assumptions/FunsExternal.lean").read_text(),
                                     "import Capability.Compatibility\nimport CoreModels\n")

    @patch("builtins.print")
    def test_partial_extraction_is_retained_when_probe_raises(self, _print):
        """Retain partial generated source even when extraction raises."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            for name in verify.SOURCES:
                (root / name).write_text(f"production input {name}\n")
            crate = root / "formal/source-linked/lean"
            crate.mkdir(parents=True)
            (crate / "hax.toml").write_text((verify.HERE / "hax.toml").read_text())
            logs = root / "logs"

            def failed_extract(copied, _scenario):
                """Write a partial output before simulating an extractor failure."""
                generated = copied / "proofs/capability/lean/Capability/Extraction/Funs.lean"
                generated.parent.mkdir(parents=True)
                generated.write_text("partial generated source\n")
                raise RuntimeError("extractor failed")

            with patch.object(verify, "ROOT", root), patch.object(verify, "LOGS", logs), \
                    patch.object(verify, "RUNS", []), patch.object(verify, "extract", failed_extract):
                with self.assertRaisesRegex(RuntimeError, "extractor failed"):
                    verify.probe_capability(crate)
            reports = list((logs / "capability-probe").glob("*/results.json"))
            self.assertEqual(len(reports), 1)
            report = json.loads(reports[0].read_text())
            self.assertEqual(report["status"], "probe failed; no aggregation theorem proved")
            self.assertFalse(report["aggregation_theorem_proved"])
            self.assertIn("formal/source-linked/lean/proofs/capability/lean/Capability/Extraction/Funs.lean",
                          report["files_sha256"])

    def test_snapshots_retain_inputs_outputs_and_logs_without_caches(self):
        """Keep distinct hashed archives without build caches or symlink targets."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            tree = root / "reproducer"
            crate = tree / "formal/source-linked/lean"
            logs = root / "logs"
            logs.mkdir()
            files = {
                **{name: f"production input {name}\n" for name in verify.SOURCES},
                "formal/source-linked/lean/Cargo.toml": "cargo manifest\n",
                "formal/source-linked/lean/Cargo.lock": "cargo lock\n",
                "formal/source-linked/lean/hax.toml": "tool pins\n",
                "formal/source-linked/lean/compatibility/IteratorAll.lean": "upstream alias\n",
                "formal/source-linked/lean/compatibility/LICENSE": "upstream license\n",
                "formal/source-linked/lean/src/lib.rs": "production import\n",
                "formal/source-linked/lean/proofs/negotiation/lean/lake-manifest.json": "{}\n",
                "formal/source-linked/lean/proofs/negotiation/lean/lakefile.toml": "lean pins\n",
                "formal/source-linked/lean/proofs/negotiation/lean/lean-toolchain": "lean version\n",
                "formal/source-linked/lean/proofs/capability/lean/Capability/Extraction/Funs.lean": "generated body\n",
                "formal/source-linked/lean/proofs/capability/lean/Capability/Assumptions/TypesExternal.lean": "external member types\n",
                "formal/source-linked/lean/proofs/capability/lean/lake-manifest.json": "{}\n",
            }
            for relative, content in files.items():
                path = tree / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            project = crate / "proofs/capability/lean"
            (project / ".lake/build").mkdir(parents=True)
            (project / ".lake/build/cache").write_text("not evidence")
            (project / "cached-package").symlink_to(project / ".lake/build/cache")
            runs = [
                {"command": ["cargo", "hax", "--version"], "log": "01.log", "exit_code": 0},
                {"command": ["lake", "build", "Capability.Extraction"], "log": "02.log", "exit_code": 1},
            ]
            (logs / "01.log").write_text("version=0.4.2\n")
            (logs / "02.log").write_text("raw typecheck failure\n")
            with patch.object(verify, "LOGS", logs), patch.object(verify, "RUNS", runs):
                failed = verify.retain_capability_evidence(tree, crate, 1, "unsupported")
                passed = verify.retain_capability_evidence(tree, crate, 1, "extraction_typechecked")
            self.assertNotEqual(failed, passed)
            for archive in (failed, passed):
                report = json.loads((archive / "results.json").read_text())
                self.assertFalse(report["aggregation_theorem_proved"])
                self.assertEqual(set(report["files_sha256"]), set(files))
                for relative, content in files.items():
                    self.assertEqual((archive / relative).read_text(), content)
                    self.assertEqual(report["files_sha256"][relative],
                                     hashlib.sha256(content.encode()).hexdigest())
                self.assertEqual((archive / "commands/01.log").read_text(), "version=0.4.2\n")
                self.assertEqual((archive / "commands/02.log").read_text(), "raw typecheck failure\n")
                self.assertEqual(report["runs"][1]["exit_code"], 1)
            self.assertEqual(runs[1]["log"], "02.log")


    def test_required_member_evidence_retains_external_types_and_llbc(self):
        """Preserve member interpretation and extractor IR with source and proof hashes."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            crate = root / "formal/source-linked/lean"
            inputs = {name: f"production input {name}\n" for name in verify.SOURCES}
            prefix = "formal/source-linked/lean/proofs/capability/lean/"
            inputs.update({
                prefix + "Capability/Assumptions/TypesExternal.lean": "reviewed external types\n",
                prefix + "Capability/Extraction/Types.lean": "actual extracted member\n",
                prefix + "Capability/Verification/Members.lean": "member contracts\n",
                prefix + "llbc/signal_fish_proof_core.llbc": "raw extractor type information\n",
            })
            for relative, content in inputs.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
            with patch.object(verify, "LOGS", root / "logs"), patch.object(verify, "RUNS", []):
                archive = verify.retain_proof_evidence(root, crate, "capability", 0, "failed proof")
            report = json.loads((archive / "results.json").read_text())
            self.assertEqual(report["status"], "failed proof")
            self.assertEqual(report["required_theorems"], list(verify.THEOREMS["capability"]))
            self.assertEqual(set(report["files_sha256"]), set(inputs))
            for relative, content in inputs.items():
                (root / relative).unlink()
                self.assertEqual((archive / relative).read_text(), content)
                self.assertEqual(report["files_sha256"][relative],
                                 hashlib.sha256(content.encode()).hexdigest())



class FaultPartitions(unittest.TestCase):
    """Keep local selection explicit and require the full CI fault inventory."""

    def test_partitions_cover_every_fault_once(self):
        """Require disjoint, balanced partitions with no omitted production fault."""
        inventory = verify.mutation_inventory()
        full = [name for names in inventory.values() for name in names]
        parts = [verify.select_mutations(list(verify.THEOREMS), shard=(i, 4)) for i in range(4)]
        combined = [name for part in parts for names in part.values() for name in names]
        self.assertCountEqual(combined, full)
        self.assertEqual(len(combined), len(set(combined)))
        sizes = [sum(map(len, part.values())) for part in parts]
        self.assertEqual(max(sizes), (len(full) + len(parts) - 1) // len(parts))
        self.assertLessEqual(max(sizes) - min(sizes), 1)

    def test_handshake_faults_require_only_owning_semantic_failures(self):
        """Reject tool errors and unrelated proof failures as handshake fault evidence."""
        faults = [name for name in verify.negotiation_mutations() if name.startswith("handshake-")]
        self.assertEqual(len(faults), 5)
        for name in faults:
            good = SimpleNamespace(returncode=1, stdout=
                "error: Negotiation/Verification/Handshake.lean:25:1: unsolved goals\n")
            verify.check_proof_rejection(good, "negotiation", name)
            for output in (
                "error: Negotiation/Extraction/Funs.lean:1:1: unknown constant\n",
                "error: Negotiation/Verification/ProofObligations.lean:1:1: unsolved goals\n",
                "error: Negotiation/Verification/Handshake.lean:1:1: unknown constant\n",
                good.stdout + "error: Negotiation/Extraction/Types.lean:1:1: unknown constant\n",
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=output), "negotiation", name)
            with self.subTest(name=name, accepted=True), self.assertRaises(RuntimeError):
                verify.check_proof_rejection(SimpleNamespace(returncode=0, stdout=""), "negotiation", name)

    def test_bounds_faults_require_only_owning_semantic_failures(self):
        """A malformed extracted function is not a rejected version-bounds policy."""
        faults = [name for name in verify.negotiation_mutations() if name.startswith("bounds-")]
        self.assertEqual(len(faults), 4)
        for name in faults:
            good = SimpleNamespace(returncode=1, stdout=
                "error: Negotiation/Verification/Bounds.lean:25:1: unsolved goals\n")
            verify.check_proof_rejection(good, "negotiation", name)
            for output in (
                "error: Negotiation/Extraction/Funs.lean:1:1: unknown constant\n",
                "error: Negotiation/Verification/Handshake.lean:1:1: unsolved goals\n",
                "error: Negotiation/Verification/Bounds.lean:1:1: unknown constant\n",
                good.stdout + "error: Negotiation/Extraction/Types.lean:1:1: unknown constant\n",
            ):
                with self.subTest(name=name, output=output), self.assertRaises(RuntimeError):
                    verify.check_proof_rejection(SimpleNamespace(returncode=1, stdout=output), "negotiation", name)
            with self.subTest(name=name, accepted=True), self.assertRaises(RuntimeError):
                verify.check_proof_rejection(SimpleNamespace(returncode=0, stdout=""), "negotiation", name)

    def test_local_selection_and_invalid_selection(self):
        """Accept explicit local scope and reject unknown, repeated or invalid choices."""
        self.assertEqual(verify.select_mutations(["negotiation"], ["lower-clamp"]),
                         {"negotiation": ["lower-clamp"]})
        for names, shard in ((["missing"], None), (["lower-clamp"] * 2, None),
                             (["peer-self"], None), (None, (-1, 4)),
                             (None, (4, 4)), (None, (0, 0)),
                             (None, (0, len(verify.negotiation_mutations()) + 1))):
            with self.subTest(names=names, shard=shard), self.assertRaises(ValueError):
                verify.select_mutations(["negotiation"], names, shard)

    def reports(self, root):
        """Write complete current-input reports for all four fault partitions."""
        reports = []
        for index in range(4):
            part = root / f"source-linked-lean-{index}"
            part.mkdir()
            selected = verify.select_mutations(list(verify.THEOREMS), shard=(index, 4))
            report = {"completed": True, "source_unchanged": True, "locks_unchanged": True,
                      "hax_version": verify.VERSION,
                      "source_hashes": {name: hashlib.sha256((verify.ROOT / name).read_bytes()).hexdigest()
                                        for name in verify.SOURCES},
                      "proved_scenarios": list(verify.THEOREMS), "mutation_shard": [index, 4],
                      "selected_mutations": selected, "completed_mutations": selected,
                      "lock_hashes": verify.current_lock_hashes(),
                      "capability_probe_status": "extraction_typechecked; no aggregation theorem proved"}
            path = part / "results.json"
            path.write_text(json.dumps(report))
            reports.append((path, report))
        return reports

    @patch("builtins.print")
    def test_aggregate_accepts_only_complete_current_evidence(self, _print):
        """Reject missing, stale, incomplete and malformed shard evidence."""
        cases = {"completed": False, "source_unchanged": False, "locks_unchanged": False,
                 "hax_version": "other", "source_hashes": {}, "lock_hashes": {},
                 "proved_scenarios": ["negotiation"], "mutation_shard": [1, 4],
                 "selected_mutations": {}, "completed_mutations": {},
                 "capability_probe_status": "not typechecked"}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            reports = self.reports(root)
            with patch.object(verify, "current_lock_hashes", wraps=verify.current_lock_hashes) as hashes:
                verify.audit_shards(root, 4)
                hashes.assert_called_once_with()
            path, original = reports[0]
            for key, value in cases.items():
                with self.subTest(key=key):
                    path.write_text(json.dumps(dict(original, **{key: value})))
                    with self.assertRaises((RuntimeError, ValueError)):
                        verify.audit_shards(root, 4)
            for field in ("completed", "source_unchanged", "locks_unchanged"):
                for malformed in ("false", 1, None):
                    path.write_text(json.dumps(dict(original, **{field: malformed})))
                    with self.subTest(field=field, value=malformed), self.assertRaises(RuntimeError):
                        verify.audit_shards(root, 4)
            for malformed in ([[0], 4], [False, 4], [0, True], [0.0, 4], ["0", 4],
                              [-1, 4], [4, 4], [0, 5], [0], None):
                with self.subTest(shard=malformed):
                    path.write_text(json.dumps(dict(original, mutation_shard=malformed)))
                    with self.assertRaisesRegex(RuntimeError, "Invalid or duplicate shard"):
                        verify.audit_shards(root, 4)
            path.write_text(json.dumps(original))
            path.unlink()
            with self.assertRaisesRegex(RuntimeError, "Expected 4 shard reports"):
                verify.audit_shards(root, 4)

    def test_corrupt_or_unreadable_reports_have_path_diagnostics(self):
        """Reject corrupt JSON and non-object reports with clear, chained errors."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            path, _report = self.reports(root)[0]
            for contents, message, cause in (
                ("", "Unreadable shard report", ValueError),
                ("{", "Unreadable shard report", ValueError),
                ("[]", "Invalid shard report", None),
                ("null", "Invalid shard report", None),
                ("true", "Invalid shard report", None),
                ('"text"', "Invalid shard report", None),
                ("1", "Invalid shard report", None),
            ):
                path.write_text(contents)
                with self.subTest(contents=contents), self.assertRaisesRegex(RuntimeError, message) as error:
                    verify.audit_shards(root, 4)
                self.assertIn(str(path), str(error.exception))
                if cause is not None:
                    self.assertIsInstance(error.exception.__cause__, cause)
            with patch.object(Path, "read_text", side_effect=OSError("unreadable")):
                with self.assertRaisesRegex(RuntimeError, "Unreadable shard report") as error:
                    verify.audit_shards(root, 4)
                self.assertIsInstance(error.exception.__cause__, OSError)
                self.assertIn(str(path), str(error.exception))

    @patch("builtins.print")
    def test_main_dispatches_only_explicit_faults_and_records_scope(self, _print):
        """Check actual CLI dispatch and honest evidence for full and selected runs."""
        for arguments, scenarios, selected in (
            (["--mutations"], list(verify.THEOREMS), verify.mutation_inventory()),
            (["--scenario", "negotiation", "--mutations", "--mutation", "lower-clamp"],
             ["negotiation"], {"negotiation": ["lower-clamp"]}),
            (["--mutations", "--mutation-shard", "0/4"], list(verify.THEOREMS),
             verify.select_mutations(list(verify.THEOREMS), shard=(0, 4))),
        ):
            with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as tmp:
                with patch.object(verify, "LOGS", Path(tmp)), patch.object(verify, "RUNS", []), \
                        patch.object(verify, "run", return_value=SimpleNamespace(stdout="version=0.4.2")), \
                        patch.object(verify, "verify") as proof, \
                        patch.object(verify, "mutants") as negotiation, \
                        patch.object(verify, "capability_mutants") as capability, \
                        patch.object(verify, "projection_mutants") as projection, \
                        patch("sys.argv", ["verify.py", *arguments]):
                    verify.main()
                    self.assertEqual([call.args[1] for call in proof.call_args_list], scenarios)
                    for scenario, runner in (("negotiation", negotiation), ("capability", capability),
                                             ("projection", projection)):
                        if selected.get(scenario):
                            runner.assert_called_once_with(verify.HERE, names=selected[scenario])
                        else:
                            runner.assert_not_called()
                report = json.loads((Path(tmp) / "results.json").read_text())
                self.assertEqual(report["selected_mutations"], selected)
                self.assertEqual(report["completed_mutations"], selected)
                self.assertEqual(report["proved_scenarios"], scenarios)

    @patch("builtins.print")
    def test_main_reports_changed_and_missing_protected_inputs(self, _print):
        """Keep fail-closed verdicts and name the exact protected input that drifted."""
        for case in ("unchanged", "generated", "tool-pin", "missing-generated", "source", "missing-source"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                crate = root / "formal/source-linked/lean"
                generated = crate / "proofs/projection/lean/Projection/Extraction/Funs.lean"
                pin = crate / "hax.toml"
                for name in verify.SOURCES:
                    path = root / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text("original production source\n")
                generated.parent.mkdir(parents=True)
                generated.write_text("original generated source location\n")
                pin.write_text("original pinned tool\n")
                changed = (root / "src/protocol_core.rs" if "source" in case else
                           pin if case == "tool-pin" else generated)
                expected = [] if case == "unchanged" else [changed.relative_to(root).as_posix()]
                missing = case.startswith("missing-")

                def proof(_crate, _scenario):
                    if missing:
                        changed.unlink()
                    elif case != "unchanged":
                        changed.write_text("changed during extraction or proof work\n")

                logs = crate / "target/verification-logs"
                with patch.object(verify, "ROOT", root), patch.object(verify, "HERE", crate), \
                        patch.object(verify, "LOGS", logs), patch.object(verify, "RUNS", []), \
                        patch.object(verify, "lock_inputs", return_value=[pin, generated]), \
                        patch.object(verify, "run", return_value=SimpleNamespace(stdout="version=0.4.2")), \
                        patch.object(verify, "verify", side_effect=proof) as checked, \
                        patch("sys.argv", ["verify.py", "--scenario", "projection"]):
                    if case == "unchanged":
                        verify.main()
                    else:
                        with self.assertRaises(RuntimeError) as error:
                            verify.main()
                        self.assertIn(expected[0], str(error.exception))
                        if missing:
                            self.assertIn("(missing)", str(error.exception))
                    checked.assert_called_once_with(crate, "projection")
                report = json.loads((logs / "results.json").read_text())
                source_changed = "source" in case
                self.assertEqual(report["completed"], case == "unchanged")
                self.assertEqual(report["source_unchanged"], not source_changed)
                self.assertEqual(report["locks_unchanged"], case == "unchanged" or source_changed)
                self.assertEqual(report["changed_source_inputs"], expected if source_changed else [])
                self.assertEqual(report["changed_protected_inputs"], [] if source_changed else expected)
                self.assertEqual(report["missing_inputs"], expected if missing else [])

    def test_invalid_cli_selection_fails_before_proof_tools(self):
        """Reject invalid CLI scope before any external proof command starts."""
        for arguments in (["--mutation", "lower-clamp"], ["--mutation-shard", "0/4"],
                          ["--mutations", "--mutation-shard", "4/4"],
                          ["--mutations", "--mutation-shard", "bad"],
                          ["--mutations", "--mutation-shard", "0/4", "--scenario", "negotiation"],
                          ["--mutations", "--mutation", "missing"],
                          ["--scenario", "negotiation", "--scenario", "negotiation"]):
            with self.subTest(arguments=arguments), patch("sys.argv", ["verify.py", *arguments]), \
                    patch.object(verify, "run") as run, patch("sys.stderr"):
                with self.assertRaises(SystemExit) as error:
                    verify.main()
                self.assertEqual(error.exception.code, 2)
                run.assert_not_called()


class MutationWorkspaceReuse(unittest.TestCase):
    """Reuse build files without allowing one source fault to affect another."""

    def exercise(self, root, scenario, failure=None):
        """Observe isolation, retained evidence and cleanup through every failure seam."""
        originals = {name: f"{name} original\n" for name in verify.SOURCES}
        for name, content in originals.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        crate = root / "formal/source-linked/lean"
        project = crate / f"proofs/{scenario}/lean"
        project.mkdir(parents=True)
        witnesses = crate / "mutation-witnesses"
        witnesses.mkdir()
        changes = {
            "member-support": ("src/session_member.rs", "original", "first fault"),
            "host-topology": ("src/session_admission.rs", "original", "second fault"),
        }
        if scenario == "projection":
            changes = {
                "profile-version": ("src/session_capability.rs", "original", "first fault"),
                "pregather-topology": ("src/session_admission.rs", "original", "second fault"),
            }
        for name in changes:
            (witnesses / f"{name}.lean").write_text(name)
        (crate / "proofs/negotiation/lean/.lake/packages").mkdir(parents=True)
        visits = []
        archives = []
        resets = []
        copy_sources = verify.copy_sources

        def reset(tree):
            """Check that all inputs are restored, including the previous fault's file."""
            copy_sources(tree)
            self.assertEqual({name: (tree / name).read_text() for name in originals}, originals)
            resets.append(tree)

        def extract(copied, extracted_scenario):
            """Require exactly one current fault and a reused isolated build directory."""
            self.assertEqual(extracted_scenario, scenario)
            index = len(visits)
            tree = copied.parents[2]
            name = list(changes)[index]
            source, before, after = changes[name]
            expected = dict(originals)
            expected[source] = expected[source].replace(before, after)
            self.assertEqual({name: (tree / name).read_text() for name in originals}, expected)
            current = copied / f"proofs/{scenario}/lean"
            self.assertFalse((current / "Counterexample.lean").exists())
            marker = current / ".lake/build/cache-marker"
            if index == 0:
                self.assertFalse(marker.exists())
                marker.parent.mkdir()
                marker.write_text("build output")
            else:
                self.assertEqual(copied, visits[0])
                self.assertEqual(marker.read_text(), "build output")
            visits.append(copied)
            if index == 1 and failure == "extract":
                raise RuntimeError("failed extraction")

        def run(command, current, *, expect_success=True):
            """Preserve semantic rejection and witness checks after extraction builds."""
            if command[:3] == ["lake", "env", "lean"]:
                self.assertEqual((current / "Counterexample.lean").read_text().splitlines()[0],
                                 list(changes)[len(visits) - 1])
                if len(visits) == 2 and failure == "witness":
                    raise RuntimeError("failed witness")
                return SimpleNamespace(returncode=0, stdout=(
                    f"'{scenario.title()}.mutation_returns_wrong_value' does not depend on any axioms"))
            if not expect_success:
                module = "Members" if len(visits) == 1 else "HostValidity"
                if scenario == "projection":
                    module = "Profiles" if len(visits) == 1 else "Pregather"
                return SimpleNamespace(returncode=1, stdout=(
                    f"error: {scenario.title()}/Verification/{module}.lean:1:1: unsolved goals\n"))
            return SimpleNamespace(returncode=0, stdout="compiled")

        def retain(tree, copied, retained_scenario, first_run, status):
            """Snapshot the current failed or accepted fault before restoration."""
            self.assertEqual(retained_scenario, scenario)
            archives.append((status, {name: (tree / name).read_text() for name in originals}))
            if len(visits) == 2 and failure == "archive":
                raise RuntimeError("failed archive")

        with patch.object(verify, "ROOT", root), \
                patch.object(verify, f"{scenario}_mutations", return_value=changes), \
                patch.object(verify, "copy_sources", reset), \
                patch.object(verify, "extract", extract), \
                patch.object(verify, "check_iterator_alias"), \
                patch.object(verify, "run", run), \
                patch.object(verify, "retain_proof_evidence", retain), \
                patch.object(verify, "verify") as restored:
            if failure:
                with self.assertRaisesRegex(RuntimeError, f"failed {failure if failure != 'extract' else 'extraction'}"):
                    getattr(verify, f"{scenario}_mutants")(crate)
                restored.assert_not_called()
            else:
                getattr(verify, f"{scenario}_mutants")(crate)
                restored.assert_called_once_with(crate, scenario)
        self.assertEqual(len(visits), 2)
        self.assertEqual(len(resets), 4)
        self.assertEqual(len(archives), 2)
        self.assertEqual(archives[0][0], "proof rejected; concrete wrong result kernel-checked")
        self.assertEqual(archives[1][0], "negative control failed" if failure in ("extract", "witness")
                         else "proof rejected; concrete wrong result kernel-checked")
        for index, (_status, inputs) in enumerate(archives):
            name = list(changes)[index]
            source, before, after = changes[name]
            expected = dict(originals)
            expected[source] = expected[source].replace(before, after)
            self.assertEqual(inputs, expected)
        self.assertEqual({name: (root / name).read_text() for name in originals}, originals)
        self.assertTrue(all(not copied.exists() for copied in visits))

    @patch("builtins.print")
    def test_faults_share_only_build_state_and_keep_independent_evidence(self, _print):
        """Two faults in different production files must never accumulate."""
        for scenario in ("capability", "projection"):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory() as tmp:
                self.exercise(Path(tmp), scenario)

    @patch("builtins.print")
    def test_failed_extraction_witness_and_archive_restore_sources_and_remove_workspace(self, _print):
        """A failure keeps its fault snapshot and cannot claim restored proofs."""
        for scenario in ("capability", "projection"):
            for failure in ("extract", "witness", "archive"):
                with self.subTest(scenario=scenario, failure=failure), tempfile.TemporaryDirectory() as tmp:
                    self.exercise(Path(tmp), scenario, failure=failure)


if __name__ == "__main__":
    unittest.main()
