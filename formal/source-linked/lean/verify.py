#!/usr/bin/env python3
"""Regenerate Lean from production Rust, check proofs, and test source mutants."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

VERSION = "0.4.2"
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
THEOREMS = {"negotiation": (
    "Negotiation.explicit_client_contract",
    "Negotiation.omitted_client_contract",
    "Negotiation.below_floor_preserved",
)}
CAPABILITY_LIMIT = "Unknown constant `CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all`"
ALLOWED_AXIOMS = {"propext", "Quot.sound", "Classical.choice"}
LOGS = HERE / "target/verification-logs"
COMMAND_COUNT = 0
RUNS = []


def run(command, cwd, *, expect_success=True):
    """Record command output and timing; reject unexpected nonzero exits."""
    global COMMAND_COUNT
    started = time.monotonic()
    env = dict(os.environ, RUST_LOG="error")
    LOGS.mkdir(parents=True, exist_ok=True)
    COMMAND_COUNT += 1
    log_path = LOGS / f"{COMMAND_COUNT:02d}.log"
    with log_path.open("w") as log:
        result = subprocess.run(command, cwd=cwd, env=env, text=True,
                                stdout=log, stderr=subprocess.STDOUT,
                                timeout=1200, check=False)
    result.stdout = log_path.read_text()
    elapsed = round(time.monotonic() - started, 3)
    record = {"command": command, "seconds": elapsed, "exit_code": result.returncode,
              "log": f"{COMMAND_COUNT:02d}.log"}
    RUNS.append(record)
    print(json.dumps(record), flush=True)
    if expect_success and result.returncode:
        raise RuntimeError(result.stdout)
    return result


def check_version(output):
    """Require the exact cargo-hax version before extracting source."""
    if f"version={VERSION}" not in output.splitlines():
        raise RuntimeError(f"Expected cargo-hax {VERSION}")


def check_axioms(output, scenario):
    """Require each named theorem's report and reject unapproved axioms."""
    for theorem in THEOREMS[scenario]:
        empty = f"'{theorem}' does not depend on any axioms"
        if empty in output:
            continue
        match = re.search(re.escape(f"'{theorem}' depends on axioms:")
                          + r"\s*\[([^]]*)\]", output)
        if match is None:
            raise RuntimeError(f"Missing axiom report for {theorem}:\n{output}")
        axioms = {item.strip() for item in match.group(1).split(",") if item.strip()}
        if axioms - ALLOWED_AXIOMS:
            raise RuntimeError(f"Unapproved axioms for {theorem}: {sorted(axioms)}")
    print(output.strip(), flush=True)


def extract(crate, scenario="negotiation"):
    """Extract the selected production helper with a stable trailing newline."""
    run(["cargo", "hax", "extract", scenario, "--locked"], crate)
    project = crate / f"proofs/{scenario}/lean"
    for source in project.glob("*/Extraction/*.lean"):
        source.write_text(source.read_text().rstrip() + "\n")


def verify(crate, scenario="negotiation"):
    """Regenerate the helper, build its proofs, and audit their axioms."""
    extract(crate, scenario)
    project = crate / f"proofs/{scenario}/lean"
    run(["lake", "build"], project)
    report = run(["lake", "env", "lean", "Audit.lean"], project)
    check_axioms(report.stdout, scenario)


def mutants(crate):
    """Reject isolated source faults, check their wrong results, then restore green."""
    source = ROOT / "src/protocol_core.rs"
    original = source.read_text()
    changes = {
        "lower-clamp": ("Some(version) => version,",
                        "Some(version) => if version < minimum { minimum } else { version },"),
        "ignore-server-ceiling": ("if requested < maximum {", "if true {"),
    }
    for name, (before, after) in changes.items():
        if original.count(before) != 1:
            raise RuntimeError(f"Mutation anchor changed: {name}")
        with tempfile.TemporaryDirectory(prefix=f"signal-fish-lean-{name}-") as tmp:
            tree = Path(tmp)
            (tree / "src").mkdir()
            (tree / "src/protocol_core.rs").write_text(original.replace(before, after))
            copied = tree / "formal/source-linked/lean"
            shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
                ".lake*", "target", "llbc", "__pycache__", "*.log", "capability"))
            project = copied / "proofs/negotiation/lean"
            (project / ".lake").mkdir()
            (project / ".lake/packages").symlink_to(
                crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
            extract(copied)
            run(["lake", "build", "Negotiation.Extraction"], project)
            result = run(["lake", "build"], project, expect_success=False)
            if result.returncode == 0:
                raise RuntimeError(f"Proof accepted source mutation: {name}")
            if "Negotiation/Verification/ProofObligations.lean:" not in result.stdout:
                raise RuntimeError(f"Mutation failed outside the proof: {name}\n{result.stdout}")
            if not any(marker in result.stdout for marker in (
                "unsolved goals", "Type mismatch", "type mismatch", "Tactic `")):
                raise RuntimeError(f"Mutation did not fail a proof obligation: {name}\n{result.stdout}")
            client, minimum, maximum = (2, 3, 3) if name == "lower-clamp" else (3, 2, 2)
            witness = "import Negotiation.Extraction\nopen CoreModels Aeneas Aeneas.Std\nopen signal_fish_proof_core\n"
            witness += f"theorem mutation_returns_wrong_value : protocol_core.negotiate_protocol_version (.Some (U16.ofNat {client})) (U16.ofNat {minimum}) (U16.ofNat {maximum}) = RustM.ok (U16.ofNat 3) := by rfl\n"
            (project / "Counterexample.lean").write_text(witness)
            run(["lake", "env", "lean", "Counterexample.lean"], project)
            print(f"REJECTED {name}: proof failed and extracted result is 3 where contract requires 2", flush=True)
    verify(crate)


def classify_capability(returncode, output):
    """Accept a typecheck or the known alias defect; reject unrelated failures."""
    if returncode == 0:
        return "extraction_typechecked; no aggregation theorem proved"
    errors = re.findall(r"^error: (.+)$", output, re.MULTILINE)
    expected = re.compile(
        r"Capability/Extraction/Funs\.lean:\d+:\d+: ("
        + re.escape(CAPABILITY_LIMIT) + r"|expected a product type, got)$")
    if CAPABILITY_LIMIT in output and errors and all(
        error in ("Lean exited with code 1", "build failed") or expected.fullmatch(error)
        for error in errors
    ):
        return "unsupported: pinned library lacks concrete Iterator::all alias"
    raise RuntimeError(f"Unexpected capability probe failure:\n{output}")


def retain_capability_evidence(tree, crate, first_run, status):
    """Keep the small source reproducer and raw commands after its tempdir ends."""
    destination = LOGS / "capability-probe" / str(time.time_ns())
    destination.mkdir(parents=True)
    inputs = [tree / "src/protocol_core.rs"]
    inputs.extend(crate / name for name in ("Cargo.toml", "Cargo.lock", "hax.toml", "src/lib.rs"))
    inputs.extend(sorted((crate / "compatibility").glob("*")))
    negotiation = crate / "proofs/negotiation/lean"
    inputs.extend(negotiation / name for name in (
        "lake-manifest.json", "lakefile.toml", "lean-toolchain"))
    project = crate / "proofs/capability/lean"
    if project.exists():
        inputs.extend(path for path in project.rglob("*")
                      if path.is_file() and not path.is_symlink()
                      and not any(part.startswith(".lake") for part in path.relative_to(project).parts))
    hashes = {}
    for source in inputs:
        if not source.is_file():
            continue
        relative = source.relative_to(tree)
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        hashes[str(relative)] = hashlib.sha256(target.read_bytes()).hexdigest()
    records = [dict(record) for record in RUNS[first_run:]]
    versions = [record for record in RUNS[:first_run]
                if record["command"] == ["cargo", "hax", "--version"]]
    if versions:
        records.insert(0, dict(versions[-1]))
    for record in records:
        target = destination / "commands" / record["log"]
        target.parent.mkdir(exist_ok=True)
        shutil.copyfile(LOGS / record["log"], target)
        record["log"] = str(target.relative_to(destination))
    (destination / "results.json").write_text(json.dumps({
        "hax_version": VERSION, "status": status,
        "aggregation_theorem_proved": False,
        "files_sha256": hashes, "runs": records,
    }, indent=2) + "\n")
    return destination


def probe_capability(crate):
    """Check aggregation extraction and regeneration without claiming a theorem.

    Retry only the known alias defect with the upstream backport. Retain both
    baseline and retry evidence before deleting the isolated source copy.
    """
    with tempfile.TemporaryDirectory(prefix="signal-fish-lean-capability-") as tmp:
        tree = Path(tmp)
        (tree / "src").mkdir()
        shutil.copyfile(ROOT / "src/protocol_core.rs", tree / "src/protocol_core.rs")
        copied = tree / "formal/source-linked/lean"
        shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
            ".lake*", "target", "llbc", "__pycache__", "*.log", "capability"))
        first_run = len(RUNS)
        status = "probe failed; no aggregation theorem proved"
        try:
            extract(copied, "capability")
            project = copied / "proofs/capability/lean"
            manifest = json.loads((crate / "proofs/negotiation/lean/lake-manifest.json").read_text())
            manifest["name"] = "Capability"
            (project / "lake-manifest.json").write_text(json.dumps(manifest))
            (project / ".lake").mkdir()
            (project / ".lake/packages").symlink_to(
                crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
            result = run(["lake", "build", "Capability.Extraction"], project, expect_success=False)
            status = classify_capability(result.returncode, result.stdout)
            if result.returncode:
                retain_capability_evidence(tree, copied, first_run, status)
                status = "compatibility typecheck failed; no aggregation theorem proved"
                install_iterator_alias(project, copied)
                run(["lake", "build", "Capability.Extraction"], project)
                status = "extraction_typechecked with upstream alias backport; no aggregation theorem proved"
            generated = project / "Capability/Extraction"
            accepted_status = status
            status = "repeat extraction/typecheck failed; no aggregation theorem proved"
            before = {path.relative_to(generated): path.read_bytes()
                      for path in generated.rglob("*.lean")}
            extract(copied, "capability")
            after = {path.relative_to(generated): path.read_bytes()
                     for path in generated.rglob("*.lean")}
            if not before or before != after:
                status = "nondeterministic extraction; no aggregation theorem proved"
                raise RuntimeError(status)
            run(["lake", "build", "Capability.Extraction"], project)
            status = accepted_status
            print(f"CAPABILITY PROBE: {status}", flush=True)
            return status
        finally:
            evidence = retain_capability_evidence(tree, copied, first_run, status)
            print(f"CAPABILITY EVIDENCE: {evidence.relative_to(LOGS)}", flush=True)


def install_iterator_alias(project, crate):
    """Import only a concrete alias of the pinned upstream iterator model."""
    shutil.copyfile(crate / "compatibility/IteratorAll.lean",
                    project / "Capability/Compatibility.lean")
    assumptions = project / "Capability/Assumptions/FunsExternal.lean"
    assumptions.write_text("import Capability.Compatibility\n" + assumptions.read_text())


def prepare():
    """Fetch the pinned dependencies and only their required Mathlib cache modules."""
    project = HERE / "proofs/negotiation/lean"
    run(["lake", "env", "true"], project)
    packages = project / ".lake/packages"
    modules = set()
    for dependency in (packages / "aeneas/backends/lean", packages / "hax"):
        for source in dependency.rglob("*.lean"):
            if ".lake" in source.relative_to(dependency).parts:
                continue
            modules.update(re.findall(r"^import (Mathlib\.[A-Za-z0-9_.]+)",
                                      source.read_text(), re.MULTILINE))
    if not modules:
        raise RuntimeError("Could not identify required Mathlib cache modules")
    run(["lake", "exe", "cache", "get", *sorted(modules)], project)


def main():
    """Run selected checks and record results while guarding source and tool pins."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mutations", action="store_true")
    parser.add_argument("--probe-capability", action="store_true",
                        help="classify harder-target extraction support; never claims an aggregation proof")
    parser.add_argument("--prepare", action="store_true",
                        help="fetch pinned dependencies and only required Mathlib cache modules")
    args = parser.parse_args()
    version = run(["cargo", "hax", "--version"], HERE)
    check_version(version.stdout)
    source = ROOT / "src/protocol_core.rs"
    source_before = source.read_bytes()
    lock_paths = [HERE / "Cargo.lock", HERE / "hax.toml"]
    lock_paths.extend(sorted((HERE / "compatibility").glob("*")))
    for scenario in THEOREMS:
        project = HERE / f"proofs/{scenario}/lean"
        lock_paths.extend(project / name for name in (
            "lake-manifest.json", "lakefile.toml", "lean-toolchain"))
    locks_before = {path: path.read_bytes() for path in lock_paths}
    digest = hashlib.sha256(source_before).hexdigest()
    print(json.dumps({"production_source_sha256": digest}), flush=True)
    completed = False
    proved_scenarios = []
    capability_status = "not run; no aggregation theorem proved"
    try:
        if args.prepare:
            prepare()
        verify(HERE)
        proved_scenarios.append("negotiation")
        if args.mutations:
            mutants(HERE)
        if args.probe_capability:
            capability_status = probe_capability(HERE)
        completed = True
    finally:
        unchanged = source.read_bytes() == source_before
        locks_unchanged = all(path.read_bytes() == value for path, value in locks_before.items())
        LOGS.mkdir(parents=True, exist_ok=True)
        (LOGS / "results.json").write_text(json.dumps({
            "hax_version": VERSION, "source_sha256": digest,
            "completed": completed and unchanged and locks_unchanged,
            "source_unchanged": unchanged, "locks_unchanged": locks_unchanged,
            "proved_scenarios": proved_scenarios, "capability_status": capability_status,
            "runs": RUNS,
        }, indent=2) + "\n")
        if not unchanged:
            raise RuntimeError("Production source changed during verification")
        if not locks_unchanged:
            raise RuntimeError("Verification changed a pinned tool or dependency file")
    print("PASS: current production source verified; source unchanged", flush=True)


if __name__ == "__main__":
    main()
