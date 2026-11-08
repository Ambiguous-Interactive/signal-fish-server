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
    "Negotiation.handshake_contract",
    "Negotiation.handshake_safety_contract",
    "Negotiation.handshake_accepts_contract",
    "Negotiation.handshake_refuses_contract",
    "Negotiation.handshake_outcomes",
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
    "Capability.member_support_contract",
    "Capability.member_getter_contract",
    "Capability.member_aggregation_contract",
    "Capability.member_host_contract",
    "Capability.member_host_inputs",
    "Capability.member_host_outcomes",
    "Capability.any_iterator_contract",
    "Capability.host_match_contract",
    "Capability.host_validity_contract",
    "Capability.host_validity_witness",
    "Capability.host_validity_outcomes",
    "Capability.executable_callback_contract",
    "Capability.executability_contract",
    "Capability.direct_executable_witness",
    "Capability.executability_outcomes",
    "Capability.array_find_contract",
    "Capability.selector_evaluation",
    "Capability.selection_callback_contract",
    "Capability.selection_contract",
    "Capability.selection_waterfall_contract",
    "Capability.selection_fallback_contract",
    "Capability.selection_upgrade_contract",
    "Capability.selection_outcomes",
    "Capability.election_empty_contract",
    "Capability.election_authority_contract",
    "Capability.election_membership_contract",
    "Capability.election_minimum_contract",
    "Capability.election_total_contract",
    "Capability.election_outcomes",
    "Capability.election_permutation_contract",
    "Capability.eligible_callback_contract",
    "Capability.eligible_election_contract",
    "Capability.eligible_empty_contract",
    "Capability.eligible_authority_contract",
    "Capability.eligible_membership_contract",
    "Capability.eligible_minimum_contract",
    "Capability.eligible_total_contract",
    "Capability.eligible_outcomes",
    "Capability.membership_contract",
    "Capability.membership_absent_contract",
    "Capability.membership_preserve_contract",
    "Capability.membership_repair_contract",
    "Capability.membership_remove_contract",
    "Capability.membership_replacement_eligible",
    "Capability.membership_outcomes",
    "Capability.membership_total_contract",
    "Capability.glare_contract",
    "Capability.peer_edge_contract",
    "Capability.peer_self_contract",
    "Capability.peer_capability_contract",
    "Capability.peer_relay_contract",
    "Capability.glare_antisymmetric",
    "Capability.peer_host_contract",
    "Capability.peer_mesh_contract",
    "Capability.peer_edge_outcomes",
    "Capability.peer_mesh_pair_contract",
    "Capability.peer_host_no_client_edges",
    "Capability.finalize_contract",
    "Capability.finalize_legal_pair",
    "Capability.finalize_member_support",
    "Capability.finalize_no_host",
    "Capability.finalize_host_eligible",
    "Capability.finalize_host_election",
    "Capability.finalize_authority_contract",
    "Capability.finalize_minimum_contract",
    "Capability.finalize_host_exists",
    "Capability.finalize_total_contract",
    "Capability.finalize_outcomes",
    "Capability.timestamp_key_order",
    "Capability.timestamp_key_equal",
    "Capability.uuid_key_equal",
    "Capability.uuid_key_order",
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
    "Projection.profile_fields_contract",
    "Projection.profile_support_contract",
    "Projection.connection_resolution_contract",
    "Projection.connection_profile_support_contract",
    "Projection.compact_default_contract",
    "Projection.negotiated_default_contract",
    "Projection.defaults_agree_contract",
    "Projection.default_rejects_contract",
    "Projection.profile_outcomes",
    "Projection.admission_contract",
    "Projection.admission_support_contract",
    "Projection.admission_host_independent",
    "Projection.admission_outcomes",
    "Projection.pregather_contract",
    "Projection.pregather_support_contract",
    "Projection.pregather_outcomes",
    "Projection.dedup_contract",
    "Projection.dedup_protocol_contract",
    "Projection.first_occurrences_loop",
    "Projection.first_occurrences_order",
    "Projection.first_occurrences_membership",
    "Projection.first_occurrences_nodup",
    "Projection.negotiated_axes_contract",
    "Projection.negotiated_axes_values",
    "Projection.negotiated_axes_outcomes",
    "Projection.negotiated_axes_legacy",
    "Projection.negotiated_axis_membership",
    "Projection.negotiated_axis_unique",
    "Projection.negotiated_axes_protocol_contract",
    "Projection.transport_status_contract",
    "Projection.transport_status_rejection_preserves",
    "Projection.transport_status_duplicate_contract",
    "Projection.transport_status_changed_contract",
    "Projection.transport_status_stale_generation",
    "Projection.transport_status_total_contract",
    "Projection.transport_status_outcomes",
)}
CAPABILITY_LIMIT = "Unknown constant `CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all`"
ALLOWED_AXIOMS = {"propext", "Quot.sound", "Classical.choice"}
SOURCES = ("src/protocol_core.rs", "src/session_capability.rs", "src/session_admission.rs", "src/session_member.rs")
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
    if scenario == "negotiation":
        targets += ("Negotiation/Verification/Handshake.lean:",)
    if scenario == "projection":
        targets += ("Projection/Verification/Defaults.lean:", "Projection/Verification/Profiles.lean:", "Projection/Verification/Admission.lean:", "Projection/Verification/Pregather.lean:", "Projection/Verification/TransportStatus.lean:", "Projection/Verification/CapabilityNegotiation.lean:", "Projection/Verification/NegotiatedAxes.lean:",)
    if scenario == "capability":
        targets += ("Capability/Verification/CallbackSemantics.lean:",
                    "Capability/Verification/ConcreteCallbacks.lean:",
                    "Capability/Verification/Members.lean:",
                    "Capability/Verification/HostValidity.lean:",
                    "Capability/Verification/Executability.lean:",
                    "Capability/Verification/Selection.lean:",
                    "Capability/Verification/Election.lean:",
                    "Capability/Verification/EligibleElection.lean:",
                    "Capability/Verification/Membership.lean:",
                    "Capability/Verification/PeerEdges.lean:",
                    "Capability/Verification/Finalize.lean:")
    errors = re.findall(r"^error: (.+)$", result.stdout, re.MULTILINE)
    locations = [error for error in errors if error not in ("Lean exited with code 1", "build failed")]
    if not locations or any(not error.startswith(targets) for error in locations):
        raise RuntimeError(f"Mutation failed outside the proof: {name}\n{result.stdout}")
    if name.startswith("handshake-") and not all(
            error.startswith("Negotiation/Verification/Handshake.lean:")
            and ("unsolved goals" in error or "Tactic `rfl` failed" in error)
            for error in locations):
        raise RuntimeError(f"Mutation did not fail the handshake contract: {name}\n{result.stdout}")
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
    if name in ("server-floor", "profile-version", "profile-transports") and not all(
            error.startswith("Projection/Verification/Profiles.lean:")
            and ("unsolved goals" in error or "Tactic `rfl` failed" in error)
            for error in locations):
        raise RuntimeError(f"Mutation did not fail the production profile proof: {name}\n{result.stdout}")
    if name.startswith("election-") and not all(
            error.startswith("Capability/Verification/Election.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the election contract: {name}\n{result.stdout}")
    if name.startswith("peer-") and not all(
            error.startswith("Capability/Verification/PeerEdges.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the peer-edge contract: {name}\n{result.stdout}")
    if name.startswith("finalize-") and not all(
            error.startswith("Capability/Verification/Finalize.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the finalize contract: {name}\n{result.stdout}")
    if name.startswith("membership-") and not all(
            error.startswith("Capability/Verification/Membership.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the membership contract: {name}\n{result.stdout}")
    if name.startswith("eligible-") and not all(
            error.startswith("Capability/Verification/EligibleElection.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the eligible-election contract: {name}\n{result.stdout}")
    if name.startswith("selection-") and not all(
            error.startswith("Capability/Verification/Selection.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the selection contract: {name}\n{result.stdout}")
    if name.startswith("executable-") and not all(
            error.startswith("Capability/Verification/Executability.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the executability contract: {name}\n{result.stdout}")
    if name.startswith("host-") and not all(
            error.startswith("Capability/Verification/HostValidity.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the host-validity contract: {name}\n{result.stdout}")
    if name.startswith("member-") and not all(
            error.startswith("Capability/Verification/Members.lean:")
            and ("unsolved goals" in error or "Type mismatch" in error
                 or "Tactic `rfl` failed" in error) for error in locations):
        raise RuntimeError(f"Mutation did not fail the actual member contract: {name}\n{result.stdout}")
    if name.startswith("admission-") and not all(
            error.startswith("Projection/Verification/Admission.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the admission contract: {name}\n{result.stdout}")
    if name.startswith("pregather-") and not all(
            error.startswith("Projection/Verification/Pregather.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the pregather contract: {name}\n{result.stdout}")
    if name.startswith("axes-") and not all(
            error.startswith("Projection/Verification/CapabilityNegotiation.lean:" if name in
                ("axes-dedup", "axes-order") else "Projection/Verification/NegotiatedAxes.lean:")
            and ("unsolved goals" in error or "Tactic `rfl` failed" in error) for error in locations):
        raise RuntimeError(f"Mutation did not fail the negotiated-axes contract: {name}\n{result.stdout}")
    if name.startswith("status-") and not all(
            error.startswith("Projection/Verification/TransportStatus.lean:")
            and "unsolved goals" in error for error in locations):
        raise RuntimeError(f"Mutation did not fail the transport-state contract: {name}\n{result.stdout}")
    if not any(marker in result.stdout for marker in (
            "unsolved goals", "Type mismatch", "type mismatch", "Tactic `")):
        raise RuntimeError(f"Mutation did not fail a proof obligation: {name}\n{result.stdout}")


def retain_proof_evidence(tree, crate, scenario, first_run, status):
    """Archive source, generated definitions, contracts, pins, and raw commands."""
    destination = LOGS / "proofs" / f"{scenario}-{time.time_ns()}"
    destination.mkdir(parents=True)
    project = crate / f"proofs/{scenario}/lean"
    inputs = [tree / name for name in SOURCES]
    inputs.extend(crate / name for name in ("Cargo.toml", "Cargo.lock", "hax.toml", "src/lib.rs", "verify.py"))
    inputs.extend(sorted((crate / "compatibility").glob("*")))
    inputs.extend(sorted((crate / "mutation-witnesses").glob("*.lean")))
    if project.exists():
        inputs.extend(project_evidence_inputs(project))
        if scenario in ("projection", "capability"):
            inputs.append(project / "llbc/signal_fish_proof_core.llbc")
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


def projection_mutations():
    """Define the production source faults used by projection checks."""
    return {
        "version-floor": ("src/session_capability.rs", "capabilities.version >= 3", "capabilities.version >= 2"),
        "transport-projection": ("src/session_capability.rs", "direct_transport: transports.contains(&Transport::Direct)",
                                 "direct_transport: transports.contains(&Transport::Relay)"),
        "default-version": ("src/session_capability.rs", "version: minimum,", "version: 3,"),
        "overwrite-profile-version": ("src/session_capability.rs", "Some(capabilities) => capabilities,",
                                      "Some(capabilities) => SessionCapabilities { version: minimum, ..capabilities },"),
        "server-floor": ("src/protocol_core.rs", "SERVER_MIN_PROTOCOL_VERSION: u16 = 2;",
                         "SERVER_MIN_PROTOCOL_VERSION: u16 = 3;"),
        "profile-version": ("src/session_capability.rs", "            protocol.version,",
                            "            crate::protocol_core::SERVER_MIN_PROTOCOL_VERSION,"),
        "profile-transports": ("src/session_capability.rs", "            &protocol.transports,",
                               "            &[],"),
        "admission-version": ("src/session_admission.rs", "protocol.version >= 3", "protocol.version >= 2"),
        "admission-topology": ("src/session_admission.rs", "protocol.topologies.contains(&stored.topology)", "true"),
        "admission-transport": ("src/session_admission.rs", "protocol.transports.contains(&stored.transport)", "true"),
        "admission-host": ("src/session_admission.rs", "protocol.version >= 3", "protocol.version >= 3 && stored.host.is_some()"),
        "pregather-switch": ("src/session_admission.rs", "    enable_pregather\n", "    true\n"),
        "pregather-webrtc": ("src/session_admission.rs", "        && enable_webrtc\n", "        && true\n"),
        "pregather-relay": ("src/session_admission.rs", "desired != Topology::Relay", "true"),
        "pregather-finalized": ("src/session_admission.rs", "!finalized", "true"),
        "pregather-version": ("src/session_admission.rs", "recipient.version >= 3", "recipient.version >= 2"),
        "pregather-transport": ("src/session_admission.rs", "recipient.transports.contains(&Transport::WebRtc)", "true"),
        "pregather-topology": ("src/session_admission.rs", "recipient.topologies.contains(&desired)", "true"),
        "axes-version": ("src/session_capability.rs", "if negotiated_version < 3 {", "if negotiated_version < 2 {"),
        "axes-transport-floor": ("src/session_capability.rs", "transports.push(Transport::Relay);", "let _ = &mut transports;"),
        "axes-topology-floor": ("src/session_capability.rs", "topologies.push(Topology::Relay);", "let _ = &mut topologies;"),
        "axes-transport-default": ("src/session_capability.rs", "supported_transports.unwrap_or_else(|| vec![Transport::Relay])", "supported_transports.unwrap_or_else(|| vec![Transport::WebRtc])"),
        "axes-topology-default": ("src/session_capability.rs", "supported_topologies.unwrap_or_else(|| vec![Topology::Relay])", "supported_topologies.unwrap_or_else(|| vec![Topology::Mesh])"),
        "axes-dedup": ("src/session_capability.rs", "if !unique.contains(&item) {", "if unique.contains(&item) || true {"),
        "axes-order": ("src/session_capability.rs", "unique.push(item);", "unique.insert(0, item);"),
        "status-version": ("src/session_capability.rs", "if protocol.version < 3 {", "if protocol.version < 2 {"),
        "status-axis": ("src/session_capability.rs", "if !protocol.transports.contains(&transport) {", "if false {"),
        "status-generation": ("src/session_capability.rs", "generation == membership_generation", "true"),
        "status-transport": ("src/session_capability.rs", "previous_transport == transport", "true"),
        "status-connected": ("src/session_capability.rs", "previous_connected == connected", "true"),
        "status-write": ("src/session_capability.rs", "*transport_status = Some((membership_generation, transport, connected));",
                         "let _ = (membership_generation, transport, connected);"),
    }


def projection_mutants(crate, *, names=None):
    """Check independent projection faults in one private incremental build tree.

    Reset every production input and remove the prior witness before extraction.
    Lake reuses only unchanged inputs; no build output reaches the checkout.
    """
    changes = projection_mutations()
    selected = changes if names is None else {name: changes[name] for name in names}
    with tempfile.TemporaryDirectory(prefix="signal-fish-lean-projection-mutants-") as tmp:
        tree = Path(tmp)
        copied = tree / "formal/source-linked/lean"
        shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
            ".lake*", "target", "llbc", "__pycache__", "*.log"))
        project = copied / "proofs/projection/lean"
        (project / ".lake").mkdir()
        (project / ".lake/packages").symlink_to(
            crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
        for name, (source, before, after) in selected.items():
            original = (ROOT / source).read_text()
            if original.count(before) != 1:
                raise RuntimeError(f"Mutation anchor changed: {name}")
            copy_sources(tree)
            (tree / source).write_text(original.replace(before, after))
            (project / "Counterexample.lean").unlink(missing_ok=True)
            first_run = len(RUNS)
            status = "negative control failed"
            try:
                extract(copied, "projection")
                run(["lake", "build", "Projection.Extraction"], project)
                if name.startswith("axes-"):
                    target = ["Projection.Verification.CapabilityNegotiation" if name in
                        ("axes-dedup", "axes-order") else "Projection.Verification.NegotiatedAxes"]
                elif name.startswith("status-"):
                    target = ["Projection.Verification.TransportStatus"]
                elif name.startswith("pregather-"):
                    target = ["Projection.Verification.Pregather"]
                elif name.startswith("admission-"):
                    target = ["Projection.Verification.Admission"]
                elif name in ("default-version", "overwrite-profile-version"):
                    target = ["Projection.Verification.Defaults"]
                elif name in ("server-floor", "profile-version", "profile-transports"):
                    target = ["Projection.Verification.Profiles"]
                else:
                    target = []
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
                try:
                    retain_proof_evidence(tree, copied, "projection", first_run, status)
                finally:
                    copy_sources(tree)
    verify(crate, "projection")


def negotiation_mutations():
    """Define the production source faults used by negotiation checks."""
    return {
        "lower-clamp": ("src/protocol_core.rs", "Some(version) => version,",
                        "Some(version) => if version < minimum { minimum } else { version },"),
        "ignore-server-ceiling": ("src/protocol_core.rs", "if requested < maximum {", "if true {"),
        "handshake-default": ("src/protocol_core.rs", "client_max.or(Some(endpoint_default))", "client_max.or(Some(minimum))"),
        "handshake-client": ("src/protocol_core.rs", "client_max.or(Some(endpoint_default))", "Some(endpoint_default).or(client_max)"),
        "handshake-floor": ("src/protocol_core.rs", "if version < minimum {", "if version > minimum {"),
        "handshake-lifecycle": ("src/protocol_core.rs", "if handshake_complete {", "if !handshake_complete {"),
        "handshake-ceiling": ("src/protocol_core.rs", "negotiate_protocol_version(requested, minimum, maximum)", "negotiate_protocol_version(requested, minimum, endpoint_default)"),
    }


def mutants(crate, *, names=None):
    """Reject isolated source faults, check their wrong results, then restore green."""
    changes = negotiation_mutations()
    selected = changes if names is None else {name: changes[name] for name in names}
    for name, (source, before, after) in selected.items():
        original = (ROOT / source).read_text()
        if original.count(before) != 1:
            raise RuntimeError(f"Mutation anchor changed: {name}")
        with tempfile.TemporaryDirectory(prefix=f"signal-fish-lean-{name}-") as tmp:
            tree = Path(tmp)
            copy_sources(tree)
            (tree / source).write_text(original.replace(before, after))
            copied = tree / "formal/source-linked/lean"
            shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
                ".lake*", "target", "llbc", "__pycache__", "*.log", "capability"))
            project = copied / "proofs/negotiation/lean"
            (project / ".lake").mkdir()
            (project / ".lake/packages").symlink_to(
                crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
            first_run = len(RUNS)
            status = "negative control failed"
            try:
                extract(copied)
                run(["lake", "build", "Negotiation.Extraction"], project)
                result = run(["lake", "build"], project, expect_success=False)
                check_proof_rejection(result, "negotiation", name)
                witness = "import Negotiation.Extraction\nopen CoreModels Aeneas Aeneas.Std\nopen signal_fish_proof_core\n"
                if name.startswith("handshake-"):
                    call, wrong = {
                        "handshake-default": (".None 3#u16 2#u16 3#u16 false", ".Accepted 2#u16"),
                        "handshake-client": ("(.Some 2#u16) 3#u16 2#u16 3#u16 false", ".Accepted 3#u16"),
                        "handshake-floor": ("(.Some 2#u16) 3#u16 3#u16 3#u16 true", ".Accepted 2#u16"),
                        "handshake-lifecycle": ("(.Some 2#u16) 3#u16 3#u16 3#u16 true", ".Retry 2#u16"),
                        "handshake-ceiling": ("(.Some 3#u16) 3#u16 2#u16 2#u16 false", ".Accepted 3#u16"),
                    }[name]
                    witness += f"theorem mutation_returns_wrong_value : protocol_core.decide_handshake_version {call} = RustM.ok ({wrong}) := by rfl\n"
                else:
                    client, minimum, maximum = (2, 3, 3) if name == "lower-clamp" else (3, 2, 2)
                    witness += f"theorem mutation_returns_wrong_value : protocol_core.negotiate_protocol_version (.Some (U16.ofNat {client})) (U16.ofNat {minimum}) (U16.ofNat {maximum}) = RustM.ok (U16.ofNat 3) := by rfl\n"
                witness += "\n#print axioms mutation_returns_wrong_value\n"
                (project / "Counterexample.lean").write_text(witness)
                report = run(["lake", "env", "lean", "Counterexample.lean"], project)
                check_axiom_reports(report.stdout, ("mutation_returns_wrong_value",))
                status = "proof rejected; concrete wrong result kernel-checked"
                print(f"REJECTED {name}: semantic contract failed; concrete wrong result kernel-checked", flush=True)
            finally:
                retain_proof_evidence(tree, copied, "negotiation", first_run, status)
    verify(crate)


def capability_mutations():
    """Define the production source faults used by capability checks."""
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
    changes.update({
        "member-getter": ("|member| member.capabilities", "|member| SessionCapabilities { version: 0, ..member.capabilities }"),
        "member-support": ("supports_session(self.capabilities, topology, transport)",
                           "supports_session(SessionCapabilities { version: 3, ..self.capabilities }, topology, transport)"),
        "member-direct-endpoint": ("transport != Transport::Direct || self.direct_endpoint.is_some()", "transport != Transport::Direct || true"),
        "member-webrtc-endpoint": ("transport != Transport::Direct || self.direct_endpoint.is_some()",
                                   "(transport != Transport::Direct || self.direct_endpoint.is_some()) && self.direct_endpoint.is_some()"),
    })
    changes.update({
        "host-identity": ("member.player_id == host && self.supported_by(member)",
                          "(member.player_id == host || true) && self.supported_by(member)"),
        "host-capability": ("member.player_id == host && self.supported_by(member)",
                            "member.player_id == host && (self.supported_by(member) || true)"),
        "host-topology": ("self.topology == Topology::Host", "self.topology != Topology::Host"),
        "host-missing": ("!self.host.is_some_and", "self.host.is_some_and"),
    })
    changes.update({
        "executable-transport": ("transport != Transport::Direct\n        || members", "transport == Transport::Direct\n        || members"),
        "executable-capability": (".any(|member| member.can_host(topology, transport))",
                                  ".any(|member| member.can_host(topology, transport) || member.direct_endpoint.is_some())"),
        "executable-missing": ("transport != Transport::Direct\n        || members", "transport != Transport::Direct\n        && members"),
    })
    changes.update({
        "selection-mesh-order": ("(Topology::Mesh, Transport::WebRtc),\n    (Topology::Host, Transport::WebRtc),",
                                 "(Topology::Host, Transport::WebRtc),\n    (Topology::Mesh, Transport::WebRtc),"),
        "selection-host-order": ("(Topology::Host, Transport::WebRtc),\n    (Topology::Host, Transport::Direct),",
                                 "(Topology::Host, Transport::Direct),\n    (Topology::Host, Transport::WebRtc),"),
        "selection-exact-ceiling": ("topology_rank(topology) <= topology_rank(desired)",
                                     "topology_rank(topology) == topology_rank(desired)"),
        "selection-no-ceiling": ("topology_rank(topology) <= topology_rank(desired)",
                                  "(topology_rank(topology) <= topology_rank(desired) || true)"),
        "selection-switch": ("Transport::WebRtc => enable_webrtc,", "Transport::WebRtc => enable_direct || enable_webrtc,"),
        "selection-members": ("&& all_support(members, topology, transport)",
                                "&& (all_support(members, topology, transport) || true)"),
        "selection-execution": ("&& rung_is_executable(members, topology, transport)",
                                  "&& (rung_is_executable(members, topology, transport) || true)"),
        "selection-floor": ("= (Topology::Relay, Transport::Relay);", "= (Topology::Host, Transport::Direct);"),
    })
    changes.update({
        "election-authority": ("if Some(member.player_id) == authority {", "if Some(member.player_id) == authority && false {"),
        "election-date": ("(member.joined_at, member.player_id) < candidate", "candidate < (member.joined_at, member.player_id)"),
        "election-uuid": ("(member.joined_at, member.player_id) < candidate", "(member.joined_at, member.player_id) < (candidate.0, member.player_id)"),
    })
    changes.update({
        "eligible-capability": (".filter(move |member| member.can_host(topology, transport))",
                                ".filter(move |member| member.can_host(topology, transport) || true)"),
        "eligible-endpoint": (".filter(move |member| member.can_host(topology, transport))",
                              ".filter(move |member| member.supports_session(topology, transport))"),
        "eligible-authority": (") -> Option<Uuid> {\n    elect_host(",
                               ") -> Option<Uuid> {\n    if let Some(host) = authority { return Some(host); }\n    elect_host("),
    })
    changes.update({
        "membership-validity": ("if !stored.host_invalid(members.clone()) {",
                                "if stored.host_invalid(members.clone()) {"),
        "membership-pair": ("let updated = ActiveSessionPlan {\n            host: Some(host),\n            ..stored\n        };",
                            "let updated = ActiveSessionPlan {\n            host: Some(host),\n            transport: Transport::Relay,\n            ..stored\n        };"),
        "membership-update": ("plan: relay,\n            active_plan_update: Some(None),",
                              "plan: relay,\n            active_plan_update: None,"),
        "membership-replan": ("active_plan_update: Some(Some(updated)),\n            is_replan: true,",
                              "active_plan_update: Some(Some(updated)),\n            is_replan: false,"),
    })
    changes.update({
        "peer-self": ("if recipient.player_id == peer.player_id\n", "if recipient.player_id == peer.player_id && false\n"),
        "peer-recipient": ("|| !recipient.supports_session(topology, transport)", "|| (!recipient.supports_session(topology, transport) && false)"),
        "peer-capability": ("|| !peer.supports_session(topology, transport)", "|| (!peer.supports_session(topology, transport) && false)"),
        "peer-mesh": ("local < remote", "remote < local"),
        "peer-star": ("Some(host) if peer.player_id == host => Some(true)", "Some(host) if peer.player_id != host => Some(true)"),
        "peer-host": ("Some(host) if recipient.player_id == host => Some(false)", "Some(host) if recipient.player_id == host => Some(true)"),
        "peer-relay": ("Topology::Relay => None,", "Topology::Relay => Some(false),"),
    })
    changes.update({
        "finalize-ceiling": ("members.as_ref(),\n        desired,", "members.as_ref(),\n        Topology::Relay,"),
        "finalize-switch": ("enable_direct,\n        enable_webrtc,", "enable_direct,\n        enable_direct,"),
        "finalize-host-gate": ("let host = if topology == Topology::Host {\n        crate::session_member::elect_eligible_host", "let host = if topology != Topology::Host {\n        crate::session_member::elect_eligible_host"),
        "finalize-authority": (
            "crate::session_member::elect_eligible_host(authority, members, topology, transport)",
            "if authority.is_some() { authority } else { crate::session_member::elect_eligible_host(authority, members, topology, transport) }"),
        "finalize-host-transport": (
            "crate::session_member::elect_eligible_host(authority, members, topology, transport)",
            "crate::session_member::elect_eligible_host(authority, members, topology, Transport::Direct)"),
    })
    return {
        name: (capability_mutation_source(name), before, after)
        for name, (before, after) in changes.items()
    }


def capability_mutation_source(name):
    """Resolve the production file for each capability fault family."""
    if name.startswith(("host-", "membership-", "finalize-")):
        return "src/session_admission.rs"
    if name.startswith(("member-", "executable-", "selection-", "election-", "eligible-", "peer-")):
        return "src/session_member.rs"
    if name == "wrong-callback-transport":
        return "src/session_capability.rs"
    return "src/protocol_core.rs"


def check_mutation_anchors(root):
    """Reject missing or ambiguous source anchors before proof tools start."""
    for changes in (negotiation_mutations(), capability_mutations(), projection_mutations()):
        for name, (source, before, _) in changes.items():
            count = (root / source).read_text().count(before)
            if count != 1:
                raise RuntimeError(f"Mutation anchor changed: {name}: {source}: expected 1 occurrence, found {count}")


def capability_mutants(crate, *, names=None):
    """Reject faults independently while reusing unchanged Lean build inputs.

    Lake tracks source and dependency hashes within this one isolated tree.
    Reset all production files between faults; never share mutable build output
    with the production checkout or another worker.
    """
    changes = capability_mutations()
    selected = changes if names is None else {name: changes[name] for name in names}
    with tempfile.TemporaryDirectory(prefix="signal-fish-lean-capability-mutants-") as tmp:
        tree = Path(tmp)
        copied = tree / "formal/source-linked/lean"
        shutil.copytree(crate, copied, ignore=shutil.ignore_patterns(
            ".lake*", "target", "llbc", "__pycache__", "*.log"))
        project = copied / "proofs/capability/lean"
        (project / ".lake").mkdir()
        (project / ".lake/packages").symlink_to(
            crate / "proofs/negotiation/lean/.lake/packages", target_is_directory=True)
        for name, (source, before, after) in selected.items():
            original = (ROOT / source).read_text()
            if original.count(before) != 1:
                raise RuntimeError(f"Mutation anchor changed: {name}")
            copy_sources(tree)
            (tree / source).write_text(original.replace(before, after))
            (project / "Counterexample.lean").unlink(missing_ok=True)
            first_run = len(RUNS)
            status = "negative control failed"
            try:
                extract(copied, "capability")
                check_iterator_alias(project, copied)
                run(["lake", "build", "Capability.Extraction"], project)
                target = {
                    "extra-callback": ["Capability.Verification.CallbackSemantics"],
                    "wrong-callback-transport": ["Capability.Verification.ConcreteCallbacks"],
                }.get(name, ["Capability.Verification.Finalize"] if name.startswith("finalize-") else
                      ["Capability.Verification.PeerEdges"] if name.startswith("peer-") else
                      ["Capability.Verification.Membership"] if name.startswith("membership-") else
                      ["Capability.Verification.EligibleElection"] if name.startswith("eligible-") else
                      ["Capability.Verification.Election"] if name.startswith("election-") else
                      ["Capability.Verification.Selection"] if name.startswith("selection-") else
                      ["Capability.Verification.Executability"] if name.startswith("executable-") else
                      ["Capability.Verification.HostValidity"] if name.startswith("host-") else
                      ["Capability.Verification.Members"] if name.startswith("member-") else [])
                result = run(["lake", "build", *target], project, expect_success=False)
                check_proof_rejection(result, "capability", name)
                run(["lake", "build", "Capability.Verification.FinalizeInputs", "Capability.Verification.ElectionFixtures"] if name.startswith("finalize-") else
                    ["lake", "build", "Capability.Verification.ElectionFixtures", "Capability.Verification.Members"] if name.startswith("peer-") else
                    ["lake", "build", "Capability.Verification.MembershipInputs"] if name.startswith("membership-") else
                    ["lake", "build", "Capability.Verification.ElectionFixtures", "Capability.Verification.ConcreteCallbacks"] if name.startswith("eligible-") else
                    ["lake", "build", "Capability.Verification.ElectionFixtures"] if name.startswith("election-") else
                    ["lake", "build", "Capability.Verification.SelectionEvaluation"] if name.startswith("selection-") else
                    ["lake", "build", "Capability.Verification.Callbacks"], project)
                witness = (crate / f"mutation-witnesses/{name}.lean").read_text()
                witness += "\n#print axioms Capability.mutation_returns_wrong_value\n"
                (project / "Counterexample.lean").write_text(witness)
                report = run(["lake", "env", "lean", "Counterexample.lean"], project)
                check_axiom_reports(report.stdout, ("Capability.mutation_returns_wrong_value",))
                status = "proof rejected; concrete wrong result kernel-checked"
                print(f"REJECTED {name}: {status}", flush=True)
            finally:
                try:
                    retain_proof_evidence(tree, copied, "capability", first_run, status)
                finally:
                    copy_sources(tree)
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


def isolate_capability_probe(crate):
    """Select only the original generic probe; preserve every other parsed setting."""
    path = crate / "hax.toml"
    original = path.read_text()
    expected = tomllib.loads(original)
    section = re.search(r"(?ms)^\[scenario\.capability\]\n.*?(?=^\[|\Z)", original)
    if section is None:
        raise RuntimeError("Missing capability scenario for isolated probe")
    expected["scenario"]["capability"]["include"] = [
        "signal_fish_proof_core::protocol_core::nonempty_all_support"]
    replacement, count = re.subn(
        r"(?ms)^include\s*=\s*\[[^]]*\]",
        'include = ["signal_fish_proof_core::protocol_core::nonempty_all_support"]',
        section[0], count=1)
    isolated = original[:section.start()] + replacement + original[section.end():]
    if count != 1 or tomllib.loads(isolated) != expected:
        raise RuntimeError("Capability probe changed settings outside its include list")
    path.write_text(isolated)


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
            isolate_capability_probe(copied)
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


def mutation_inventory():
    """Return the ordered, unique production fault inventory for all backends."""
    inventory = {"negotiation": list(negotiation_mutations()),
                 "capability": list(capability_mutations()),
                 "projection": list(projection_mutations())}
    names = [name for group in inventory.values() for name in group]
    if len(names) != len(set(names)):
        raise RuntimeError("Duplicate source fault name")
    return inventory


def select_mutations(scenarios, names=None, shard=None):
    """Partition every required fault once; explicit names support local loops."""
    inventory = mutation_inventory()
    available = [name for scenario in scenarios for name in inventory[scenario]]
    if names is not None:
        if len(names) != len(set(names)) or set(names) - set(available):
            raise ValueError("Unknown, repeated, or out-of-scenario source fault")
        available = [name for name in available if name in names]
    if shard is not None:
        index, count = shard
        if count < 1 or not 0 <= index < count or count > len(available):
            raise ValueError("Mutation shard needs 0 <= index < count <= fault count")
        available = available[index::count]
    return {scenario: [name for name in inventory[scenario] if name in available]
            for scenario in scenarios}


def parse_shard(value):
    """Parse a zero-based INDEX/COUNT without silently accepting bad bounds."""
    try:
        index, count = map(int, value.split("/"))
    except ValueError as error:
        raise argparse.ArgumentTypeError("Expected INDEX/COUNT") from error
    if count < 1 or not 0 <= index < count:
        raise argparse.ArgumentTypeError("Expected 0 <= INDEX < COUNT")
    return index, count


def audit_shards(directory, count):
    """Require complete current-input evidence before the aggregate CI passes."""
    paths = sorted(directory.glob("*/results.json"))
    if len(paths) != count:
        raise RuntimeError(f"Expected {count} shard reports, found {len(paths)}")
    source_hashes = {name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                     for name in SOURCES}
    seen = set()
    current_locks = current_lock_hashes()
    for path in paths:
        try:
            report = json.loads(path.read_text())
        except (OSError, ValueError) as error:
            raise RuntimeError(f"Unreadable shard report: {path}") from error
        if not isinstance(report, dict):
            raise RuntimeError(f"Invalid shard report: {path}")
        shard = report.get("mutation_shard")
        if (not isinstance(shard, list) or len(shard) != 2
                or any(type(value) is not int for value in shard)
                or shard[1] != count or not 0 <= shard[0] < count or shard[0] in seen):
            raise RuntimeError(f"Invalid or duplicate shard: {path}")
        expected = select_mutations(list(THEOREMS), shard=shard)
        if (report.get("completed") is not True or report.get("source_unchanged") is not True
                or report.get("locks_unchanged") is not True or report.get("hax_version") != VERSION
                or report.get("source_hashes") != source_hashes
                or report.get("proved_scenarios") != list(THEOREMS)
                or report.get("selected_mutations") != expected
                or report.get("completed_mutations") != expected
                or report.get("capability_probe_status") not in (
                    "extraction_typechecked; no aggregation theorem proved",
                    "extraction_typechecked with upstream alias backport; no aggregation theorem proved")):
            raise RuntimeError(f"Incomplete or stale shard: {path}")
        lock_hashes = report.get("lock_hashes", {})
        if not lock_hashes or lock_hashes != current_locks:
            raise RuntimeError(f"Stale proof pins or witnesses: {path}")
        seen.add(shard[0])
    if seen != set(range(count)):
        raise RuntimeError("Missing mutation shard")
    print(f"PASS: {count} current-input shards cover every required source fault", flush=True)


def lock_inputs():
    """List every pinned dependency, mapping and fault witness."""
    paths = [HERE / name for name in ("Cargo.toml", "Cargo.lock", "hax.toml", "src/lib.rs", "verify.py")]
    paths.extend(sorted((HERE / "compatibility").glob("*")))
    paths.extend(sorted((HERE / "mutation-witnesses").glob("*.lean")))
    for scenario in THEOREMS:
        project = HERE / f"proofs/{scenario}/lean"
        paths.extend(project_evidence_inputs(project))
    return paths


def current_lock_hashes():
    """Bind aggregate reports to the current pins and witnesses."""
    return {str(path.relative_to(HERE)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in lock_inputs()}


def main():
    """Run selected checks and record results while guarding source and tool pins."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mutations", action="store_true")
    parser.add_argument("--probe-capability", action="store_true",
                        help="classify harder-target extraction support; never claims an aggregation proof")
    parser.add_argument("--prepare", action="store_true",
                        help="fetch pinned dependencies and only required Mathlib cache modules")
    parser.add_argument("--scenario", action="append", choices=list(THEOREMS),
                        help="local loop: verify only the named scenario (repeatable)")
    parser.add_argument("--mutation", action="append",
                        help="local loop: check only this source fault (repeatable; needs --mutations)")
    parser.add_argument("--mutation-shard", type=parse_shard,
                        help="CI: zero-based INDEX/COUNT partition of source faults")
    parser.add_argument("--audit-shards", type=Path,
                        help="audit downloaded shard reports instead of running proof tools")
    parser.add_argument("--shard-count", type=int, default=4)
    args = parser.parse_args()
    if args.audit_shards is not None:
        if args.shard_count < 1:
            parser.error("--shard-count must be positive")
        audit_shards(args.audit_shards, args.shard_count)
        return
    if (args.mutation or args.mutation_shard) and not args.mutations:
        parser.error("fault selection requires --mutations")
    if args.mutation_shard and (args.scenario or args.mutation):
        parser.error("CI shards cannot restrict scenarios or faults")
    scenarios = args.scenario or list(THEOREMS)
    if len(scenarios) != len(set(scenarios)):
        parser.error("repeated scenario")
    try:
        selected = select_mutations(scenarios, args.mutation, args.mutation_shard) if args.mutations else {}
    except ValueError as error:
        parser.error(str(error))
    completed_mutations = {}
    if args.mutations:
        check_mutation_anchors(ROOT)
    version = run(["cargo", "hax", "--version"], HERE)
    check_version(version.stdout)
    source = ROOT / "src/protocol_core.rs"
    source_before = source.read_bytes()
    sources_before = {ROOT / name: (ROOT / name).read_bytes() for name in SOURCES}
    lock_paths = lock_inputs()
    locks_before = {path: path.read_bytes() for path in lock_paths}
    digest = hashlib.sha256(source_before).hexdigest()
    print(json.dumps({"production_source_sha256": digest}), flush=True)
    completed = False
    proved_scenarios = []
    capability_probe_status = "not run; no aggregation theorem proved"
    try:
        if args.prepare:
            prepare()
        for scenario in scenarios:
            verify(HERE, scenario)
            proved_scenarios.append(scenario)
        if args.mutations:
            runners = {"negotiation": mutants, "capability": capability_mutants,
                       "projection": projection_mutants}
            for scenario, names in selected.items():
                if names:
                    runners[scenario](HERE, names=names)
                completed_mutations[scenario] = names
        if args.probe_capability:
            capability_probe_status = "probe failed; no aggregation theorem proved"
            capability_probe_status = probe_capability(HERE)
        completed = True
    finally:
        changed_source_inputs = []
        changed_protected_inputs = []
        missing_inputs = []
        for before, changed in ((sources_before, changed_source_inputs),
                                (locks_before, changed_protected_inputs)):
            for path, value in before.items():
                relative = path.relative_to(ROOT).as_posix()
                try:
                    current = path.read_bytes()
                except FileNotFoundError:
                    changed.append(relative)
                    missing_inputs.append(relative)
                else:
                    if current != value:
                        changed.append(relative)
        unchanged = not changed_source_inputs
        locks_unchanged = not changed_protected_inputs
        LOGS.mkdir(parents=True, exist_ok=True)
        (LOGS / "results.json").write_text(json.dumps({
            "hax_version": VERSION, "source_sha256": digest,
            "completed": completed and unchanged and locks_unchanged,
            "source_unchanged": unchanged, "locks_unchanged": locks_unchanged,
            "changed_source_inputs": changed_source_inputs,
            "changed_protected_inputs": changed_protected_inputs,
            "missing_inputs": missing_inputs,
            "source_hashes": {str(path.relative_to(ROOT)): hashlib.sha256(value).hexdigest()
                              for path, value in sources_before.items()},
            "proved_scenarios": proved_scenarios, "capability_probe_status": capability_probe_status,
            "mutation_shard": args.mutation_shard,
            "selected_mutations": selected, "completed_mutations": completed_mutations,
            "lock_hashes": {str(path.relative_to(HERE)): hashlib.sha256(value).hexdigest()
                            for path, value in locks_before.items()},
            "runs": RUNS,
        }, indent=2) + "\n")
        if not unchanged:
            paths = ", ".join(f"{path} (missing)" if path in missing_inputs else path
                              for path in changed_source_inputs)
            raise RuntimeError(f"Production source changed during verification: {paths}")
        if not locks_unchanged:
            paths = ", ".join(f"{path} (missing)" if path in missing_inputs else path
                              for path in changed_protected_inputs)
            raise RuntimeError(f"Verification changed protected inputs: {paths}")
    print("PASS: current production source verified; source unchanged", flush=True)


if __name__ == "__main__":
    main()
