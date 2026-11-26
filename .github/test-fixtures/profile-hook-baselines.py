# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///

"""Profile the pre-commit hook against real Git indexes."""

import argparse
import os
import re
import subprocess
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from pathlib import Path
from statistics import median_low
from tempfile import TemporaryDirectory
from time import perf_counter_ns

REFACTOR = "0141f270bb5d19b2c2b09e6e7b45679cc4f641a2"
BASELINE = "753a665975a748a3a77d41be78c0c278d8d24c14"
HOOK = Path("scripts/hooks/pre-commit.ps1")


class ProfileOptions(argparse.Namespace):
    mode: str
    repetitions: int
    fetch: bool
    summary: Path | None


def git(directory: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments],
        cwd=directory,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    ).stdout.strip()


@contextmanager
def worktree(repository: Path, path: Path, commit: str) -> Iterator[Path]:
    git(repository, "worktree", "add", "--quiet", "--detach", str(path), commit)
    try:
        yield path
    finally:
        git(repository, "worktree", "remove", "--force", str(path))


def hook_sample(hook: Path, replay: Path, mode: str, *, exact: bool = False) -> int:
    command = [
        "pwsh",
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        str(hook),
    ]
    if mode == "worktree":
        command.append("-Worktree")
    start = perf_counter_ns()
    result = subprocess.run(
        command,
        cwd=replay,
        env={**os.environ, "SIGNAL_FISH_HOOK_PROFILE": "1"},
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        check=False,
    )
    wall = (perf_counter_ns() - start) // 1_000_000
    print(result.stdout, end="", flush=True)
    print(f"Process wall {wall}ms", flush=True)
    result.check_returncode()
    match = re.search(r"^\[pre-commit\] Completed in (\d+)ms$", result.stdout, re.M)
    if match is None or (
        exact and "PASS: Changelog gate" not in result.stdout.splitlines()
    ):
        raise RuntimeError(f"Missing {mode} hook result")
    return int(match[1])


def fetch_pins(repository: Path) -> None:
    if git(repository, "rev-parse", "--is-shallow-repository") == "true":
        git(repository, "fetch", "--no-tags", "--depth=2", "origin", REFACTOR)
        git(repository, "fetch", "--no-tags", "--depth=1", "origin", BASELINE)
    else:
        git(repository, "fetch", "--no-tags", "origin", REFACTOR, BASELINE)


def profile_refactor(
    repository: Path, temporary: Path, candidate: str, repetitions: int
) -> None:
    print(f"refactor={REFACTOR} baseline={BASELINE} candidate={candidate}", flush=True)
    with (
        worktree(repository, temporary / "baseline", BASELINE) as baseline,
        worktree(repository, temporary / "candidate", candidate) as topic,
        worktree(repository, temporary / "replay", f"{REFACTOR}^") as replay,
    ):
        git(replay, "read-tree", "--reset", "-u", REFACTOR)
        if git(replay, "write-tree") != git(
            replay, "rev-parse", f"{REFACTOR}^{{tree}}"
        ):
            raise RuntimeError("Refactor index differs from the original commit")
        versions = (("baseline", baseline / HOOK), ("candidate", topic / HOOK))
        for mode in ("staged", "worktree"):
            for run in range(1, repetitions + 1):
                for version, hook in versions if run % 2 else reversed(versions):
                    print(
                        f"=== exact refactor {mode} run {run} {version} ===", flush=True
                    )
                    hook_sample(hook, replay, mode, exact=True)


def profile_standard(
    repository: Path, temporary: Path, candidate: str, repetitions: int
) -> dict[str, list[int]]:
    totals: dict[str, list[int]] = {}
    with worktree(repository, temporary / "standard", candidate) as replay:
        for mode in ("worktree", "staged"):
            if mode == "staged":
                for name, marker in (
                    ("src/lib.rs", b"\n// Hook baseline measurement marker.\n"),
                    ("CHANGELOG.md", b"\n- Hooks: Hook baseline measurement marker.\n"),
                ):
                    with (replay / name).open("ab") as output:
                        output.write(marker)
                git(replay, "add", "--", "src/lib.rs", "CHANGELOG.md")
            totals[mode] = []
            for run in range(1, repetitions + 1):
                print(
                    f"=== pre-commit {mode} run {run} of {repetitions} ===", flush=True
                )
                totals[mode].append(hook_sample(replay / HOOK, replay, mode))
    return totals


def save_summary(totals: dict[str, list[int]], report: Path) -> None:
    rows = [
        f"### Pre-commit hook baselines — {os.environ.get('MATRIX_OS', 'local')}"
        f" (runner image {os.environ.get('ImageVersion', 'unknown')})",
        "",
        "| Mode | First run | Later runs | Later median |",
        "| --- | ---: | --- | ---: |",
    ]
    for mode, times in totals.items():
        warm = ", ".join(f"{duration} ms" for duration in times[1:]) or "—"
        median = f"{median_low(times[1:])} ms" if len(times) > 1 else "—"
        rows.append(f"| {mode} | {times[0]} ms | {warm} | {median} |")
    report.write_text("\n".join(rows) + "\n", encoding="utf-8")
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with Path(summary).open("a", encoding="utf-8") as output:
            output.write(report.read_text(encoding="utf-8"))


def main(arguments: Sequence[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("refactor", "standard"))
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--fetch", action="store_true")
    parser.add_argument("--summary", type=Path)
    options = ProfileOptions()
    parser.parse_args(arguments, namespace=options)
    if not 1 <= options.repetitions <= 20:
        parser.error("--repetitions must be between 1 and 20")
    repository = Path(__file__).resolve().parents[2]
    if options.fetch:
        fetch_pins(repository)
    candidate = git(repository, "rev-parse", "HEAD")
    with TemporaryDirectory(prefix="hook-baseline-") as temporary_directory:
        temporary = Path(temporary_directory)
        if options.mode == "refactor":
            profile_refactor(repository, temporary, candidate, options.repetitions)
        else:
            totals = profile_standard(
                repository, temporary, candidate, options.repetitions
            )
            if options.summary:
                save_summary(totals, options.summary)


if __name__ == "__main__":
    main()
