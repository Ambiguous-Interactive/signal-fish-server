"""Trace 64-byte Rust allocations while the real refused-start test runs."""

import argparse
import json
import platform
import re
import subprocess
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    args = parser.parse_args()
    subprocess.run(["git", "rev-parse", "HEAD"], cwd=args.source, check=True)
    subprocess.run(["rustc", "-vV"], cwd=args.source, check=True)
    build = subprocess.run(
        [
            "cargo", "rustc", "--locked", "--profile", "ci", "--all-features",
            "--test", "session_plan_allocations", "--message-format=json", "--",
            "-C", "strip=none",
        ],
        cwd=args.source,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    artifacts = [json.loads(line) for line in build.stdout.splitlines()]
    (executable,) = (
        artifact["executable"]
        for artifact in artifacts
        if artifact.get("executable")
        and artifact["target"]["name"] == "session_plan_allocations"
    )
    register = {"arm64": "x0", "aarch64": "x0", "x86_64": "rdi"}[platform.machine()]
    trace = subprocess.run(
        [
            "lldb", "-x", "-b", "-o", "settings set target.disable-aslr false",
            "-o", f'breakpoint set -r "__rust_alloc(_zeroed)?$" -c "(unsigned long)${register} == 64" -G true',
            "-o", 'breakpoint command add -o "bt 24" 1',
            "-o", "run", "-o", "breakpoint list 1",
            "--", executable, "--exact", "unready_start_skips_player_metadata", "--nocapture",
        ],
        cwd=args.source,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    print(trace.stdout, end="", flush=True)
    if trace.returncode or not re.search(r"hit count = [1-9]", trace.stdout):
        raise SystemExit("LLDB did not capture any 64-byte allocation")
    if not re.search(r"UNREADY_START bytes=\d+", trace.stdout):
        raise SystemExit("The refused-start test did not reach its measurements")
    if "test result:" not in trace.stdout:
        raise SystemExit("Missing refused-start test verdict")


if __name__ == "__main__":
    main()
