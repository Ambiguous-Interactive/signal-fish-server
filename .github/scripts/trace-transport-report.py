"""Compare 64-byte allocation stacks in pinned transport-report tests."""

import argparse
import hashlib
import json
import platform
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

TEST = "transport_report_delivery_cost"
PROFILES = re.compile(r"\bTRANSPORT_REPORT_PROFILE ")
TEST_EXIT = re.compile(r"Process \d+ exited with status = (?:0|101)\b")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--samples", type=int, default=8)
    parser.add_argument("--traces", type=int, default=4)
    parser.add_argument("--matched", action="store_true")
    args = parser.parse_args()

    subprocess.run(["git", "rev-parse", "HEAD"], cwd=args.source, check=True)
    subprocess.run(["git", "status", "--porcelain"], cwd=args.source, check=True)
    subprocess.run(["rustc", "-vV"], cwd=args.source, check=True)
    subprocess.run(["lldb", "--version"], cwd=args.source, check=True)
    print(f"platform={platform.platform()}", flush=True)
    print(f"lock_sha256={hashlib.sha256((args.source / 'Cargo.lock').read_bytes()).hexdigest()}", flush=True)

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
    with Path(executable).open("rb") as binary:
        print(f"binary={executable} sha256={hashlib.file_digest(binary, 'sha256').hexdigest()}", flush=True)

    test = [executable, "--exact", TEST, "--nocapture"]
    profile_count = 10 if args.matched else 5
    regions = 82 if args.matched else 11

    def sample(_: int) -> subprocess.CompletedProcess[str]:
        return subprocess.run(test, cwd=args.source, capture_output=True, text=True, timeout=60)

    with ThreadPoolExecutor(max_workers=4) as pool:
        for index, control in enumerate(pool.map(sample, range(args.samples)), start=1):
            print(f"UNTRACED attempt={index} test_exit={control.returncode}", flush=True)
            print(control.stdout, control.stderr, sep="", end="", flush=True)
            if control.returncode not in (0, 101) or len(PROFILES.findall(control.stdout)) != profile_count:
                raise SystemExit("Untraced test did not complete the expected cohorts")

    register = {"arm64": "x0", "aarch64": "x0", "x86_64": "rdi"}[platform.machine()]
    commands = [
        "settings set target.disable-aslr false",
        f'breakpoint set -r "__rust_alloc(_zeroed)?$" -c "(unsigned long)${register} == 64" -G true',
        'breakpoint command add -o "bt 24" 1',
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

    for index in range(1, args.traces + 1):
        trace = subprocess.run(
            [*invocation, "--", *test],
            cwd=args.source,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=120,
        )
        print(f"TRACED attempt={index}", flush=True)
        print(trace.stdout, end="", flush=True)
        if trace.returncode or not TEST_EXIT.search(trace.stdout):
            raise SystemExit("LLDB did not retain the test exit status")
        if len(PROFILES.findall(trace.stdout)) != profile_count:
            raise SystemExit("Traced test did not complete the expected cohorts")
        for marker in (2, 3):
            if not re.search(rf"(?m)^{marker}: .*hit count = {regions}\b", trace.stdout):
                raise SystemExit("LLDB missed an allocation measurement region")


if __name__ == "__main__":
    main()
