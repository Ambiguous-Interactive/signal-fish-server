#!/usr/bin/env python3
"""Run one disjoint partition of the compiled Miri library test list."""

import argparse
import json
from pathlib import Path
import re
import subprocess


def listed_tests(output):
    """Read libtest records without treating its summary as a test name."""
    names = [line.removesuffix(": test") for line in output.splitlines()
             if line.endswith(": test")]
    if not names or len(names) != len(set(names)):
        raise ValueError("Miri test listing is empty or contains duplicate names")
    if any("::" not in name or any(character.isspace() for character in name)
           for name in names):
        raise ValueError("Miri test listing contains an invalid test path")
    return sorted(names)


def select_tests(names, modules):
    """Match only the root module, including when nested modules share names."""
    if not modules or len(modules) != len(set(modules)):
        raise ValueError("Miri modules must be nonempty and unique")
    if any(re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_]*", module) is None for module in modules):
        raise ValueError("Miri modules must be Rust identifiers")
    selected = [name for name in names if name.split("::", 1)[0] in modules]
    if not selected:
        raise ValueError("Miri lane selects no listed tests")
    return selected


def run_lane(toolchain, modules, manifest):
    """Preserve listing and analyzer failures; use libtest's exact-name mode."""
    command = ["cargo", f"+{toolchain}", "miri", "test", "--locked", "--lib", "--no-fail-fast", "--"]
    listing = subprocess.run([*command, "--list"], text=True, stdout=subprocess.PIPE, check=False)
    if listing.returncode:
        print(listing.stdout, end="", flush=True)
        return listing.returncode
    names = listed_tests(listing.stdout)
    selected = select_tests(names, modules)
    manifest.write_text(json.dumps({
        "toolchain": toolchain, "modules": modules,
        "listed_count": len(names), "selected_count": len(selected), "tests": selected,
        "module_counts": {module: sum(name.split("::", 1)[0] == module for name in selected)
                          for module in modules},
    }, indent=2) + "\n")
    print(f"Miri selects {len(selected)} of {len(names)} listed tests for {', '.join(modules)}", flush=True)
    return subprocess.run([*command, "--exact", *selected], check=False).returncode


def main():
    """Select modules from the workflow matrix and retain their exact test paths."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", required=True)
    parser.add_argument("--modules", nargs="+", required=True)
    parser.add_argument("--manifest", type=Path, default=Path("miri-selected-tests.json"))
    args = parser.parse_args()
    return run_lane(args.toolchain, args.modules, args.manifest)


if __name__ == "__main__":
    raise SystemExit(main())
