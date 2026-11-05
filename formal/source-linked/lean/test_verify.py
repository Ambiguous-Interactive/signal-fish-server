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


class CapabilityEvidence(unittest.TestCase):
    @patch("builtins.print")
    def test_probe_retry_and_deterministic_regeneration(self, _print):
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

                def extract(copied, scenario):
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
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "src").mkdir()
            (root / "src/protocol_core.rs").write_text("production source\n")
            crate = root / "formal/source-linked/lean"
            crate.mkdir(parents=True)
            (crate / "hax.toml").write_text("pinned tools\n")
            logs = root / "logs"

            def failed_extract(copied, _scenario):
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
