#!/usr/bin/env python3
"""Check production Rust with Kani; reject vacuity and unrelated mutant failures."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]
CRATE = Path("formal/source-linked/kani")
SOURCE = Path("src/protocol_core.rs")
VERSION = "0.68.0"
CONTRACTS = {
    "negotiation_respects_ceilings": {
        "server ceiling", "client ceiling", "preserve client below ceiling",
        "downgrade to server ceiling",
    },
    "negotiation_defaults_and_refusal": {
        "missing version uses floor", "below-floor client remains rejectable",
    },
    "capability_aggregation_through_eight_members": {
        "empty room never upgrades", "every member must support upgrade",
        "nonempty all-supported room upgrades",
    },
}
COVERS = {
    "negotiation_respects_ceilings": set(),
    "negotiation_defaults_and_refusal": {"v2 refusal reachable", "full range reachable"},
    "capability_aggregation_through_eight_members": {
        "empty room reachable", "full supported room reachable", "full rejected room reachable",
    },
}
HARNESSES = set(CONTRACTS)
MUTATIONS = [
    (
        "lower-clamp",
        "if requested < maximum {",
        "if requested < minimum { minimum } else if requested < maximum {",
        "negotiation_respects_ceilings",
        "client ceiling",
    ),
    (
        "ceiling",
        "if requested < maximum {",
        "if requested > maximum {",
        "negotiation_respects_ceilings",
        "server ceiling",
    ),
    (
        "empty-room",
        "!members.is_empty() && members.iter().all(supports)",
        "members.iter().all(supports)",
        "capability_aggregation_through_eight_members",
        "empty room never upgrades",
    ),
    (
        "skip-member",
        "members.iter().all(supports)",
        "members.iter().skip(1).all(supports)",
        "capability_aggregation_through_eight_members",
        "every member must support upgrade",
    ),
]


def check_report(report, expected_harnesses, expected_failure=None):
    """The JSON schema and result names are pinned with the Kani release."""
    if report["metadata"]["kani_version"] != VERSION:
        raise RuntimeError("unexpected Kani version in proof report")
    results = report["verification_results"]["results"]
    if {item["harness_id"] for item in results} != expected_harnesses:
        raise RuntimeError("missing or unexpected proof harness")
    if len(results) != len(expected_harnesses):
        raise RuntimeError("duplicate proof harness")
    checks = [check for item in results for check in item["checks"]]
    allowed = {"Success", "Failure", "Unreachable", "Satisfied", "Unsatisfiable"}
    if any(check["status"] not in allowed for check in checks):
        raise RuntimeError("verification contains unknown or incomplete check status")
    failures = [check for check in checks if check["status"] == "Failure"]
    if expected_failure is not None:
        if any(item["status"] != "Failure" for item in results):
            raise RuntimeError("mutant harness did not report failure")
        if not failures or any(check["category"] != "assertion" for check in failures):
            raise RuntimeError("mutant did not fail solely on contract assertions")
        if not any(check["description"].strip('"') == expected_failure for check in failures):
            raise RuntimeError("mutant did not fail the intended contract")
    else:
        if failures or any(item["status"] != "Success" for item in results):
            raise RuntimeError("a production proof failed")
        for item in results:
            harness = item["harness_id"]
            covers = [check for check in item["checks"] if check["category"] == "cover"]
            if ({check["description"] for check in covers} != COVERS[harness]
                    or len(covers) != len(COVERS[harness])):
                raise RuntimeError(f"{harness}: missing or unexpected reachability witness")
            if any(check["status"] != "Satisfied" for check in covers):
                raise RuntimeError(f"{harness}: a required witness was not satisfied")
            contracts = [
                check for check in item["checks"] if check["category"] == "assertion"
                and check["description"].strip('"') in CONTRACTS[harness]
            ]
            if (len(contracts) != len(CONTRACTS[harness]) or
                    {check["description"].strip('"'): check["status"] for check in contracts}
                    != dict.fromkeys(CONTRACTS[harness], "Success")):
                raise RuntimeError(f"{harness}: a named contract is absent, unreachable, or unproved")


def run(root, output, name, harness=None, expected_failure=None):
    report_path = output / f"{name}.json"
    report_path.unlink(missing_ok=True)
    command = [
        "cargo", "kani", "--manifest-path", str(root / CRATE / "Cargo.toml"),
        "-Z", "unstable-options", "--export-json", str(report_path),
    ]
    if harness:
        command += ["--harness", harness, "--exact"]
    start = time.monotonic()
    with (output / f"{name}.log").open("w") as log:
        result = subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, timeout=300)
    elapsed = time.monotonic() - start
    if not report_path.is_file():
        raise RuntimeError(f"{name}: no verification report; inspect {name}.log")
    report = json.loads(report_path.read_text())
    check_report(report, {harness} if harness else HARNESSES, expected_failure)
    if (result.returncode == 0) != (expected_failure is None):
        raise RuntimeError(f"{name}: unexpected verifier exit {result.returncode}")
    print(f"PASS {name}: {elapsed:.3f}s", flush=True)
    return {"name": name, "seconds": round(elapsed, 3), "exit": result.returncode}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mutations", action="store_true")
    args = parser.parse_args()
    output = ROOT / "target/source-linked/kani"
    output.mkdir(parents=True, exist_ok=True)
    original = (ROOT / SOURCE).read_bytes()
    original_lock = (ROOT / CRATE / "Cargo.lock").read_bytes()
    records = []
    completed = False
    try:
        records.append(run(ROOT, output, "baseline"))
        records.append(run(ROOT, output, "warm"))
        if args.mutations:
            for name, before, after, harness, failure in MUTATIONS:
                text = original.decode()
                if text.count(before) != 1:
                    raise RuntimeError(f"{name}: mutation anchor drifted")
                with tempfile.TemporaryDirectory(prefix="signal-fish-kani-") as temporary:
                    root = Path(temporary)
                    shutil.copytree(ROOT / CRATE, root / CRATE,
                                    ignore=shutil.ignore_patterns("target", "__pycache__"))
                    (root / SOURCE).parent.mkdir(parents=True)
                    (root / SOURCE).write_text(text.replace(before, after, 1))
                    records.append(run(root, output, name, harness, failure))
            records.append(run(ROOT, output, "restored"))
        completed = True
    finally:
        unchanged = (ROOT / SOURCE).read_bytes() == original
        lock_unchanged = (ROOT / CRATE / "Cargo.lock").read_bytes() == original_lock
        (output / "results.json").write_text(json.dumps({
            "kani_version": VERSION,
            "completed": completed and unchanged and lock_unchanged,
            "source_sha256": hashlib.sha256(original).hexdigest(),
            "source_unchanged": unchanged,
            "lock_unchanged": lock_unchanged,
            "runs": records,
        }, indent=2) + "\n")
        if not unchanged:
            raise RuntimeError("production source changed during verification")
        if not lock_unchanged:
            raise RuntimeError("verification changed the dependency lockfile")


if __name__ == "__main__":
    main()
