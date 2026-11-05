"""Require complete, explicit Lean theorem axiom reports."""

import hashlib
import json
from pathlib import Path
import tempfile
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
                crate = root / "formal/source-linked/lean"
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
                    project = copied / "proofs/capability/lean/Capability"
                    (project / "Extraction").mkdir(parents=True, exist_ok=True)
                    body = "changed body\n" if drift and len(calls) == 2 else "generated body\n"
                    (project / "Extraction/Funs.lean").write_text(body)
                    assumptions = project / "Assumptions/FunsExternal.lean"
                    if not assumptions.exists():
                        assumptions.parent.mkdir()
                        assumptions.write_text("import CoreModels\n")

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
            (root / "src/protocol_core.rs").write_text("production source\n")
            crate = root / "formal/source-linked/lean"
            crate.mkdir(parents=True)
            (crate / "hax.toml").write_text("pinned tools\n")
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
                "src/protocol_core.rs": "production source\n",
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


if __name__ == "__main__":
    unittest.main()
