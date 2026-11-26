"""Trace 64-byte Rust allocations while the real refused-start test runs."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import platform
import re
import subprocess
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--samples", type=int, default=12)
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
    test = [executable, "--exact", "unready_start_skips_player_metadata", "--nocapture"]
    verdict = re.compile(r"UNREADY_START bytes=\d+")
    with ThreadPoolExecutor(max_workers=6) as pool:
        def sample(_: int) -> subprocess.CompletedProcess[str]:
            return subprocess.run(test, cwd=args.source, capture_output=True, text=True)

        for index, control in enumerate(pool.map(sample, range(args.samples)), start=1):
            print(f"UNTRACED attempt={index} test_exit={control.returncode}", flush=True)
            print(control.stdout, control.stderr, sep="", end="", flush=True)
            if len(verdict.findall(control.stdout)) != 6:
                raise SystemExit("Untraced legacy run did not reach all six cells")

    commands = [
        "settings set target.disable-aslr false",
        f'breakpoint set -r "__rust_alloc(_zeroed)?$" -c "(unsigned long)${register} == 64" -G true',
        'breakpoint command add -o "bt 18" 1',
        "breakpoint disable 1",
        'breakpoint set -r "stats_alloc::Region.*::new(::h[0-9a-f]+)?$" -G true',
        'breakpoint command add -o "breakpoint enable 1" 2',
        'breakpoint set -r "stats_alloc::Region.*::change(::h[0-9a-f]+)?$" -G true',
        'breakpoint command add -o "breakpoint disable 1" 3',
        "run",
        "breakpoint list",
    ]
    invocation = ["lldb", "-x", "-b"]
    for command in commands:
        invocation.extend(["-o", command])
    for index in range(1, args.samples + 1):
        trace = subprocess.run(
            [*invocation, "--", *test],
            cwd=args.source,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        print(f"TRACED attempt={index}", flush=True)
        print(trace.stdout, end="", flush=True)
        # A test failure is evidence. The debugger must capture the entire test,
        # and the log must report the test's own exit status.
        if trace.returncode or not re.search(r"Process \d+ exited with status = [01]\b", trace.stdout):
            raise SystemExit("LLDB did not retain the test exit status")
        if len(verdict.findall(trace.stdout)) != 6:
            raise SystemExit("Traced legacy run did not reach all six cells")
        for marker in (2, 3):
            if not re.search(rf"(?m)^{marker}: .*hit count = 6\b", trace.stdout):
                raise SystemExit("LLDB missed a legacy measurement region")


if __name__ == "__main__":
    main()
