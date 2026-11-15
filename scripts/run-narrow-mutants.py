#!/usr/bin/env python3
"""Run narrow mutation experiments with the pinned upstream selector repair."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


REVISION = "b00aff0c6ca0670bccdf4eb2ece9e15c4c35c8b9"
REPOSITORY = "https://github.com/sourcefrog/cargo-mutants"
INSTALL_KEY = f"cargo-mutants 27.1.0 (git+{REPOSITORY}?rev={REVISION}#{REVISION})"
FIXTURE = """struct Config { enabled: bool, other: bool }
fn select(base: Config) -> Config {
    Config { enabled: true, other: true, ..base }
}
"""


def installed_tool(root):
    metadata = json.loads((root / ".crates2.json").read_text())
    if INSTALL_KEY not in metadata.get("installs", {}):
        raise ValueError(f"{root}: expected cargo-mutants source revision {REVISION}")
    return root / "bin" / ("cargo-mutants.exe" if os.name == "nt" else "cargo-mutants")


def selector_preflight(tool):
    with tempfile.TemporaryDirectory(prefix="signal-fish-mutant-selector-") as directory:
        fixture = Path(directory) / "selector.rs"
        fixture.write_text(FIXTURE)
        command = [str(tool), "mutants", "--no-config", "--Zmutate-file", str(fixture), "--json"]

        def names(arguments):
            output = subprocess.check_output(command + arguments, text=True)
            return [mutant["name"].split(": delete field ", 1)[-1] for mutant in json.loads(output)]

        enabled = "enabled from struct Config expression in select"
        other = "other from struct Config expression in select"
        baseline = names([])
        if baseline.count(enabled) != 1 or baseline.count(other) != 1:
            raise ValueError(f"selector preflight did not generate both struct fields: {baseline}")
        cases = [
            (["--re", "delete field enabled "], [enabled]),
            (["--exclude-re", "delete field enabled "], [name for name in baseline if name != enabled]),
            (["--re", "^THIS_NEVER_MATCHES$"], []),
            (["--exclude-re", "."], []),
        ]
        for arguments, expected in cases:
            actual = names(arguments)
            if actual != expected:
                raise ValueError(f"selector preflight {arguments}: expected {expected}, got {actual}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--install", action="store_true", help="Install the pinned source in the isolated tool root.")
    parser.add_argument("--tool-root", type=Path, default=Path(__file__).resolve().parents[1] / "progress/tools/cargo-mutants-selector")
    parser.add_argument("arguments", nargs=argparse.REMAINDER, help="Arguments after -- go to cargo-mutants after its selector preflight.")
    options = parser.parse_args()
    root = options.tool_root.resolve()
    arguments = options.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    try:
        if options.install and not (root / ".crates2.json").exists():
            subprocess.run([
                "cargo", "install", "--git", REPOSITORY, "--rev", REVISION, "--locked",
                "--root", str(root), "--target-dir", str(root / "target"), "cargo-mutants",
            ], check=True)
        tool = installed_tool(root)
        selector_preflight(tool)
        print(f"Selector preflight passed: {REVISION}", file=sys.stderr)
        if arguments:
            return subprocess.run([str(tool), "mutants", *arguments], check=False).returncode
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Narrow mutation experiment stopped: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
