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
import tomllib

VERSION = "0.4.2"
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
THEOREMS = {"negotiation": (
    "Negotiation.explicit_client_contract",
    "Negotiation.omitted_client_contract",
    "Negotiation.below_floor_preserved",
), "capability": (
    "Capability.empty_contract",
    "Capability.all_members_contract",
    "Capability.boolean_identity_contract",
    "Capability.nonempty_outcomes",
    "Capability.callback_trace_exists",
    "Capability.callback_trace_contract",
    "Capability.callback_order_outcomes",
    "Capability.callback_failure_outcomes",
    "Capability.concrete_callback_contract",
    "Capability.concrete_aggregation_contract",
    "Capability.concrete_aggregation_outcomes",
), "projection": (
    "Projection.support_contract",
    "Projection.projection_contract",
    "Projection.projected_support_contract",
    "Projection.support_outcomes",
    "Projection.known_profile_contract",
    "Projection.absent_profile_contract",
    "Projection.resolved_projection_contract",
    "Projection.resolved_support_contract",
    "Projection.absent_support_contract",
    "Projection.resolution_outcomes",
)}
CAPABILITY_LIMIT = "Unknown constant `CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all`"
ALLOWED_AXIOMS = {"propext", "Quot.sound", "Classical.choice"}
SOURCES = ("src/protocol_core.rs", "src/session_capability.rs")
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
    check_axiom_reports(output, THEOREMS[scenario])


def check_axiom_reports(output, theorems):
    """Audit named contracts and concrete fault witnesses with the same policy."""
    for theorem in theorems:
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


def verify_body(crate, scenario="negotiation"):
    """Regenerate the helper, build its proofs, and audit their axioms."""
    extract(crate, scenario)
    project = crate / f"proofs/{scenario}/lean"
    if scenario != "negotiation":
        share_prepared_dependencies(crate, project)
    if scenario == "capability":
        check_iterator_alias(project, crate)
    # Check that extraction remains deterministic before accepting a proof.
    generated = project / scenario.title() / "Extraction"
    before = {path.relative_to(generated): path.read_bytes() for path in generated.rglob("*.lean")}
    extract(crate, scenario)
    after = {path.relative_to(generated): path.read_bytes() for path in generated.rglob("*.lean")}
    if not before or before != after:
        raise RuntimeError(f"Nondeterministic {scenario} extraction")
    run(["lake", "build"], project)
    report = run(["lake", "env", "lean", "Audit.lean"], project)
    check_axioms(report.stdout, scenario)


def verify(crate, scenario="negotiation"):
    """Keep complete evidence for successful and failed required checks."""
    first_run = len(RUNS)
    status = "verification failed"
    try:
        verify_body(crate, scenario)
        status = "proofs accepted; axioms audited; regeneration identical"
    finally:
        retain_proof_evidence(crate.parents[2], crate, scenario, first_run, status)


def share_prepared_dependencies(crate, project):
    """Share prepared packages only for the exact same pinned dependency graph.

    Preserve unrelated local directories and links. A conflicting cache needs
    an explicit local decision; the runner must never replace or delete it.
    """
    prepared = crate / "proofs/negotiation/lean"
    manifests = []
    for candidate in (prepared, project):
        manifest = json.loads((candidate / "lake-manifest.json").read_text())
        manifest.pop("name", None)
        manifests.append(manifest)
    if manifests[0] != manifests[1] or manifests[0].get("packagesDir") != ".lake/packages":
        raise RuntimeError("Cannot share Lean packages: pinned dependency manifests differ")
    requirements = [tomllib.loads((candidate / "lakefile.toml").read_text()).get("require")
                    for candidate in (prepared, project)]
    if not requirements[0] or requirements[0] != requirements[1]:
        raise RuntimeError("Cannot share Lean packages: pinned Lake requirements differ")
    if (prepared / "lean-toolchain").read_bytes() != (project / "lean-toolchain").read_bytes():
        raise RuntimeError("Cannot share Lean packages: pinned Lean toolchains differ")
    packages = prepared / ".lake/packages"
    if not packages.is_dir():
        raise RuntimeError("Prepared Lean packages are missing; run with --prepare first")
    destination = project / ".lake/packages"
    if destination.exists() or destination.is_symlink():
        if not destination.is_symlink() or destination.resolve() != packages.resolve():
            raise RuntimeError(f"Preserving conflicting Lean package directory or link: {destination}")
        return
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.symlink_to(packages.resolve(), target_is_directory=True)


def check_iterator_alias(project, crate):
    """Required proofs must use the exact reviewed alias and import it once."""
    if (project / "Capability/Compatibility.lean").read_bytes() != (crate / "compatibility/IteratorAll.lean").read_bytes():
        raise RuntimeError("Required capability alias differs from reviewed backport")
    imports = (project / "Capability/Assumptions/FunsExternal.lean").read_text().splitlines()
    if imports.count("import Capability.Compatibility") != 1:
        raise RuntimeError("Required capability alias import missing or duplicated")


def check_proof_rejection(result, scenario, name):
    """Reject a source fault only when the extracted module compiled first.

    Build failures outside the contract file are tool failures, not successful
    negative controls. A separately checked concrete witness is also required.
    """
    if result.returncode == 0:
        raise RuntimeError(f"Proof accepted source mutation: {name}")
    module = scenario.title()
    targets = (f"{module}/Verification/ProofObligations.lean:",)
    if scenario == "projection":
        targets += ("Projection/Verification/Defaults.lean:",)
    if scenario == "capability":
        targets += ("Capability/Verification/CallbackSemantics.lean:",
                    "Capability/Verification/ConcreteCallbacks.lean:")
    errors = re.findall(r"^error: (.+)$", result.stdout, re.MULTILINE)
    locations = [error for error in errors if error not in ("Lean exited with code 1", "build failed")]
    if not locations or any(not error.startswith(targets) for error in locations):
        raise RuntimeError(f"Mutation failed outside the proof: {name}\n{result.stdout}")
    if name == "extra-callback" and not any(
            error.startswith("Capability/Verification/CallbackSemantics.lean:")
            for error in locations):
        raise RuntimeError(f"Mutation did not fail the callback semantics: {name}\n{result.stdout}")
    if name == "wrong-callback-transport" and not all(
            error.startswith("Capability/Verification/ConcreteCallbacks.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the concrete callback proof: {name}\n{result.stdout}")
    if name in ("default-version", "overwrite-profile-version") and not all(
            error.startswith("Projection/Verification/Defaults.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the profile resolution proof: {name}\n{result.stdout}")
    if not any(marker in result.stdout for marker in (
            "unsolved goals", "Type mismatch", "type mismatch", "Tactic `")):
        raise RuntimeError(f"Mutation did not fail a proof obligation: {name}\n{result.stdout}")


def retain_proof_evidence(tree, crate, scenario, first_run, status):
    """Archive source, generated definitions, contracts, pins, and raw commands."""
    destination = LOGS / "proofs" / f"{scenario}-{time.time_ns()}"
    destination.mkdir(parents=True)
    project = crate / f"proofs/{scenario}/lean"
    inputs = [tree / name for name in SOURCES]
    inputs.extend(crate / name for name in ("Cargo.toml", "Cargo.lock", "hax.toml", "src/lib.rs"))
    inputs.extend(sorted((crate / "compatibility").glob("*")))
    inputs.extend(sorted((crate / "mutation-witnesses").glob("*.lean")))
    if project.exists():
        inputs.extend(project_evidence_inputs(project))
    return retain_snapshot(tree, inputs, destination, first_run, {
        "hax_version": VERSION, "status": status, "scenario": scenario,
        "required_theorems": THEOREMS[scenario],
    })


def project_evidence_inputs(project):
    """Prune caches before walking so evidence collection stays small and fast."""
    result = []
    for directory, folders, files in os.walk(project):
        folders[:] = sorted(name for name in folders
                            if not name.startswith(".lake") and name != "llbc"
                            and not (Path(directory) / name).is_symlink())
        result.extend(Path(directory) / name for name in sorted(files)
                      if not (Path(directory) / name).is_symlink())
    return result


def retain_snapshot(tree, inputs, destination, first_run, metadata):
    """Hash immutable evidence and preserve logs without build caches."""
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
        **metadata, "files_sha256": hashes, "runs": records,
    }, indent=2) + "\n")
    return destination


def copy_sources(tree):
    """Copy every actual production input into an isolated verification tree."""
    for name in SOURCES:
        destination = tree / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / name, destination)


def projection_mutants(crate):
    """Reject version and membership faults only after extraction compiles."""
    original = (ROOT / "src/session_capability.rs").read_text()
    changes = {
        "version-floor": ("capabilities.version >= 3", "capabilities.version >= 2"),
        "transport-projection": ("direct_transport: transports.contains(&Transport::Direct)",
                                 "direct_transport: transports.contains(&Transport::Relay)"),
        "default-version": ("version: minimum,", "version: 3,"),
        "overwrite-profile-version": ("Some(capabilities) => capabilities,",
                                      "Some(capabilities) => SessionCapabilities { version: minimum, ..capabilities },"),
    }
    for name, (before, after) in changes.items():
        if original.count(before) != 1:
            raise RuntimeError(f"Mutation anchor changed: {name}")
        with tempfile.TemporaryDirectory(prefix=f"signal-fish-lean-{name}-") as tmp:
            tree = Path(tmp)
            copy_sources(tree)
            (tree / "src/session_capability.rs").write_text(original.replace(before, after))
            copied = tree / "formal/source-linked/lean"
            shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
                ".lake*", "target", "llbc", "__pycache__", "*.log"))
            project = copied / "proofs/projection/lean"
            (project / ".lake").mkdir()
            (project / ".lake/packages").symlink_to(
                crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
            first_run = len(RUNS)
            status = "negative control failed"
            try:
                extract(copied, "projection")
                run(["lake", "build", "Projection.Extraction"], project)
                target = ["Projection.Verification.Defaults"] if name in (
                    "default-version", "overwrite-profile-version") else []
                result = run(["lake", "build", *target], project, expect_success=False)
                check_proof_rejection(result, "projection", name)
                witness = (crate / f"mutation-witnesses/{name}.lean").read_text()
                witness += "\n#print axioms Projection.mutation_returns_wrong_value\n"
                (project / "Counterexample.lean").write_text(witness)
                report = run(["lake", "env", "lean", "Counterexample.lean"], project)
                check_axiom_reports(report.stdout, ("Projection.mutation_returns_wrong_value",))
                status = "proof rejected; concrete wrong result kernel-checked"
                print(f"REJECTED {name}: {status}", flush=True)
            finally:
                retain_proof_evidence(tree, copied, "projection", first_run, status)
    verify(crate, "projection")


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
            copy_sources(tree)
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
            check_proof_rejection(result, "negotiation", name)
            client, minimum, maximum = (2, 3, 3) if name == "lower-clamp" else (3, 2, 2)
            witness = "import Negotiation.Extraction\nopen CoreModels Aeneas Aeneas.Std\nopen signal_fish_proof_core\n"
            witness += f"theorem mutation_returns_wrong_value : protocol_core.negotiate_protocol_version (.Some (U16.ofNat {client})) (U16.ofNat {minimum}) (U16.ofNat {maximum}) = RustM.ok (U16.ofNat 3) := by rfl\n"
            witness += "\n#print axioms mutation_returns_wrong_value\n"
            (project / "Counterexample.lean").write_text(witness)
            report = run(["lake", "env", "lean", "Counterexample.lean"], project)
            check_axiom_reports(report.stdout, ("mutation_returns_wrong_value",))
            print(f"REJECTED {name}: proof failed and extracted result is 3 where contract requires 2", flush=True)
    verify(crate)


def capability_mutants(crate):
    """Reject result and callback-state faults with kernel-checked results."""
    changes = {
        "empty-room": ("!members.is_empty() && members.iter().all(supports)",
                       "members.iter().all(supports)"),
        "skip-member": ("members.iter().all(supports)",
                        "if members.len() == 1 { true } else { members.iter().all(supports) }"),
        "extra-callback": ("!members.is_empty() && members.iter().all(supports)",
                           "!members.is_empty() && { let mut supports = supports; "
                           "supports(&members[0]) && members.iter().all(supports) }"),
    }
    changes["wrong-callback-transport"] = (
        "supports_session(capabilities(member), topology, transport)",
        "supports_session(capabilities(member), topology, "
        "match transport { Transport::Direct => Transport::Relay, other => other })",
    )
    for name, (before, after) in changes.items():
        source = "src/session_capability.rs" if name == "wrong-callback-transport" else "src/protocol_core.rs"
        original = (ROOT / source).read_text()
        if original.count(before) != 1:
            raise RuntimeError(f"Mutation anchor changed: {name}")
        with tempfile.TemporaryDirectory(prefix=f"signal-fish-lean-{name}-") as tmp:
            tree = Path(tmp)
            copy_sources(tree)
            (tree / source).write_text(original.replace(before, after))
            copied = tree / "formal/source-linked/lean"
            shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
                ".lake*", "target", "llbc", "__pycache__", "*.log"))
            project = copied / "proofs/capability/lean"
            (project / ".lake").mkdir()
            (project / ".lake/packages").symlink_to(
                crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
            first_run = len(RUNS)
            status = "negative control failed"
            try:
                extract(copied, "capability")
                check_iterator_alias(project, copied)
                run(["lake", "build", "Capability.Extraction"], project)
                target = {
                    "extra-callback": ["Capability.Verification.CallbackSemantics"],
                    "wrong-callback-transport": ["Capability.Verification.ConcreteCallbacks"],
                }.get(name, [])
                result = run(["lake", "build", *target], project, expect_success=False)
                check_proof_rejection(result, "capability", name)
                run(["lake", "build", "Capability.Verification.Callbacks"], project)
                witness = (crate / f"mutation-witnesses/{name}.lean").read_text()
                witness += "\n#print axioms Capability.mutation_returns_wrong_value\n"
                (project / "Counterexample.lean").write_text(witness)
                report = run(["lake", "env", "lean", "Counterexample.lean"], project)
                check_axiom_reports(report.stdout, ("Capability.mutation_returns_wrong_value",))
                status = "proof rejected; concrete wrong result kernel-checked"
                print(f"REJECTED {name}: {status}", flush=True)
            finally:
                retain_proof_evidence(tree, copied, "capability", first_run, status)
    verify(crate, "capability")


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
    inputs = [tree / name for name in SOURCES]
    inputs.extend(crate / name for name in ("Cargo.toml", "Cargo.lock", "hax.toml", "src/lib.rs"))
    inputs.extend(sorted((crate / "compatibility").glob("*")))
    negotiation = crate / "proofs/negotiation/lean"
    inputs.extend(negotiation / name for name in (
        "lake-manifest.json", "lakefile.toml", "lean-toolchain"))
    project = crate / "proofs/capability/lean"
    if project.exists():
        inputs.extend(project_evidence_inputs(project))
    return retain_snapshot(tree, inputs, destination, first_run, {
        "hax_version": VERSION, "status": status,
        "aggregation_theorem_proved": False,
    })


def probe_capability(crate):
    """Check aggregation extraction and regeneration without claiming a theorem.

    Retry only the known alias defect with the upstream backport. Retain both
    baseline and retry evidence before deleting the isolated source copy.
    """
    with tempfile.TemporaryDirectory(prefix="signal-fish-lean-capability-") as tmp:
        tree = Path(tmp)
        copy_sources(tree)
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
    sources_before = {ROOT / name: (ROOT / name).read_bytes() for name in SOURCES}
    lock_paths = [HERE / "Cargo.lock", HERE / "hax.toml"]
    lock_paths.extend(sorted((HERE / "compatibility").glob("*")))
    lock_paths.extend(sorted((HERE / "mutation-witnesses").glob("*.lean")))
    for scenario in THEOREMS:
        project = HERE / f"proofs/{scenario}/lean"
        lock_paths.extend(project / name for name in (
            "lake-manifest.json", "lakefile.toml", "lean-toolchain"))
    locks_before = {path: path.read_bytes() for path in lock_paths}
    digest = hashlib.sha256(source_before).hexdigest()
    print(json.dumps({"production_source_sha256": digest}), flush=True)
    completed = False
    proved_scenarios = []
    capability_probe_status = "not run; no aggregation theorem proved"
    try:
        if args.prepare:
            prepare()
        for scenario in THEOREMS:
            verify(HERE, scenario)
            proved_scenarios.append(scenario)
        if args.mutations:
            mutants(HERE)
            capability_mutants(HERE)
            projection_mutants(HERE)
        if args.probe_capability:
            capability_probe_status = probe_capability(HERE)
        completed = True
    finally:
        unchanged = all(path.read_bytes() == value for path, value in sources_before.items())
        locks_unchanged = all(path.read_bytes() == value for path, value in locks_before.items())
        LOGS.mkdir(parents=True, exist_ok=True)
        (LOGS / "results.json").write_text(json.dumps({
            "hax_version": VERSION, "source_sha256": digest,
            "completed": completed and unchanged and locks_unchanged,
            "source_unchanged": unchanged, "locks_unchanged": locks_unchanged,
            "source_hashes": {str(path.relative_to(ROOT)): hashlib.sha256(value).hexdigest()
                              for path, value in sources_before.items()},
            "proved_scenarios": proved_scenarios, "capability_probe_status": capability_probe_status,
            "runs": RUNS,
        }, indent=2) + "\n")
        if not unchanged:
            raise RuntimeError("Production source changed during verification")
        if not locks_unchanged:
            raise RuntimeError("Verification changed a pinned tool or dependency file")
    print("PASS: current production source verified; source unchanged", flush=True)


if __name__ == "__main__":
    main()
