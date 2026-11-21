#!/usr/bin/env python3
"""Run isolated, paired capacity spool replay trials on Linux."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import time

PROBE = "room_spool::tests::room_spool_replay_memory_profile"


def git(*args):
    return subprocess.check_output(["git", *args], text=True).strip()


def run_attempt(binary, root, rooms, sends, mode, name):
    environment = os.environ.copy()
    environment.update(
        CAPACITY_SPOOL_PROFILE_ROOMS=str(rooms),
        CAPACITY_SPOOL_PROFILE_SENDS=str(sends),
        CAPACITY_SPOOL_PROFILE_MODE=mode,
        CAPACITY_SPOOL_PROFILE_DIR=str(root),
    )
    args = [str(binary), "--exact", PROBE, "--ignored", "--nocapture"]
    started = time.monotonic()
    log = root / f"{name}.log"
    with log.open("xb") as output:
        child = subprocess.Popen(args, env=environment, stdout=output, stderr=subprocess.STDOUT)
        _, status, usage = os.wait4(child.pid, 0)
        child.returncode = os.waitstatus_to_exitcode(status)
    lines = log.read_text().splitlines()
    evidence = {
        "name": name,
        "mode": mode,
        "exit": child.returncode,
        "wall_seconds": time.monotonic() - started,
        "peak_rss_kib": usage.ru_maxrss,
        "stdout": str(log),
    }
    for key, prefix in [("digest", "ROOM_SPOOL_DIGEST "), ("spool_bytes", "ROOM_SPOOL_BYTES ")]:
        for line in lines:
            if line.startswith(prefix):
                evidence[key] = line.removeprefix(prefix)
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    if platform.system() != "Linux":
        parser.error("Linux wait4 is required for KiB peak RSS")
    binary = args.binary.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    with binary.open("rb") as source:
        binary_digest = hashlib.file_digest(source, "sha256").hexdigest()
    repository = Path(git("rev-parse", "--show-toplevel"))
    measured_sources = sorted((repository / "tests/capacity_runner").rglob("*.rs"))
    measured_sources += [repository / name for name in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]]
    metadata = {
        "revision": git("rev-parse", "HEAD"),
        "current_source_sha256": {
            str(path.relative_to(repository)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in measured_sources
        },
        "binary": str(binary),
        "binary_sha256": binary_digest,
        "host": platform.platform(),
        "affinity": sorted(os.sched_getaffinity(0)),
    }
    (output / "profile.json").write_text(json.dumps(metadata, indent=2) + "\n")
    failures = []
    summaries = []
    with (output / "attempts.jsonl").open("x", buffering=1) as ledger:
        for rooms, sends in [(2, 100), (2, 400), (4, 400), (8, 400)]:
            cell = output / f"{rooms}r-{sends}s"
            cell.mkdir()

            def attempt(mode, name):
                evidence = run_attempt(binary, cell, rooms, sends, mode, name)
                evidence.update(rooms=rooms, sends_per_player=sends)
                ledger.write(json.dumps(evidence) + "\n")
                return evidence

            producer = attempt("write", "fixture")
            if producer["exit"] or "spool_bytes" not in producer:
                failures.append(f"{cell.name} fixture")
                continue
            summary = {
                "rooms": rooms,
                "sends_per_player": sends,
                "spool_bytes": int(producer["spool_bytes"]),
                "jsonl_bytes": (cell / "deliveries.jsonl").stat().st_size,
            }
            for comparison in ["decode", "evaluate"]:
                suffix = "-eval" if comparison == "evaluate" else ""
                observations = []
                for repeat in range(3):
                    modes = ["legacy", "partitioned"]
                    if repeat % 2:
                        modes.reverse()
                    for mode in modes:
                        selected = f"{mode}{suffix}"
                        observations.append(attempt(selected, f"{repeat}-{selected}"))
                digests = {record.get("digest") for record in observations}
                if any(record["exit"] for record in observations) or None in digests or len(digests) != 1:
                    failures.append(f"{cell.name} {comparison}")
                    continue
                summary[comparison] = {"digest": digests.pop()}
                for mode in ["legacy", "partitioned"]:
                    selected = f"{mode}{suffix}"
                    samples = [
                        record["peak_rss_kib"] for record in observations if record["mode"] == selected
                    ]
                    summary[comparison][mode] = {
                        "median_peak_rss_kib": statistics.median(samples),
                        "min_peak_rss_kib": min(samples),
                        "max_peak_rss_kib": max(samples),
                    }
            summaries.append(summary)
    (output / "summary.json").write_text(json.dumps(summaries, indent=2) + "\n")
    if failures:
        raise SystemExit("Failed trials: " + ", ".join(failures))


if __name__ == "__main__":
    main()
