#!/usr/bin/env python3
"""Select affected verification and explicit Cargo targets without building tests."""
import argparse
import ast
import json
import os
import re
from pathlib import Path
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[1]
NESTED = {"package_contents_tests", "public_api_privacy_tests"}
PR_DEEP = NESTED | {"capacity_runner"}
def extracted_sources(text):
    for node in ast.parse(text).body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "SOURCES" for t in node.targets):
            return set(ast.literal_eval(node.value))
    raise ValueError("Lean verifier must declare its source inputs")


LEAN = extracted_sources((ROOT / "formal/source-linked/lean/verify.py").read_text())
CORE_LIBRARY = ROOT / "formal/source-linked/lean/src/lib.rs"
CORE_SOURCE_IMPORTS = {
    (CORE_LIBRARY.parent / path).resolve().relative_to(ROOT).as_posix()
    for path in re.findall(r'#\[path\s*=\s*"([^"\n]+)"\]', CORE_LIBRARY.read_text())
}


def classify(paths):
    """Unknown inputs keep conservative coverage; known unrelated inputs skip specialists."""
    known = ("src/", "tests/", "scripts/", "tools/", "formal/", "clients/", "third_party/", "benches/", "docs/", ".llm/", ".github/", ".devcontainer/", ".vscode/", "fuzz/", "test-fixtures/", "examples/", "spec/")
    known_files = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "build.rs", "README.md", "LICENSE", "CHANGELOG.md", "AGENTS.md", "CLAUDE.md", "GOAL.md", "PLAN.md"}
    if any("/" in p and not p.startswith(known) for p in paths):
        return dict.fromkeys(("lean", "proof_faults", "packaging", "fuzz", "capacity"), True)
    if any("/" not in p and p not in known_files and not p.startswith(".") for p in paths):
        return dict.fromkeys(("lean", "proof_faults", "packaging", "fuzz", "capacity"), True)
    proof_faults = any(p.startswith(("formal/", ".cargo/")) or p in {"rust-toolchain.toml", "Cargo.toml", "Cargo.lock", ".github/workflows/formal-verification.yml", "scripts/ci-cohort.py", "scripts/download-verified.sh", "scripts/install-hax-tools.sh", "scripts/read-toml-string.sh"} for p in paths)
    return {
        "capacity": any(
            p.startswith(("tests/capacity_runner/", ".llm/code-samples/capacity/"))
            or (p.startswith(("scripts/", "benches/")) and "capacity" in p)
            or p in {"Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain.toml",
                     "scripts/ci-cohort.py", "scripts/read-toml-string.sh",
                     "scripts/record-test-cpu-evidence.sh", ".github/workflows/ci.yml"}
            or p.startswith(".cargo/") or p.endswith(("/Cargo.toml", "/Cargo.lock"))
            for p in paths),
        "lean": proof_faults or bool(LEAN.intersection(paths)),
        "proof_faults": proof_faults,
        "packaging": any(p in {"Cargo.toml", "Cargo.lock", "build.rs", "README.md", "LICENSE", "src/lib.rs", "tests/package_contents_tests.rs", "tests/public_api_privacy_tests.rs", "scripts/ci-cohort.py"} or p.startswith(("tests/ui/", "src/websocket/", ".github/workflows/")) for p in paths),
        "fuzz": any(p.startswith(("src/", "fuzz/", ".github/workflows/")) or p in {"Cargo.toml", "Cargo.lock", "build.rs", "rust-toolchain.toml", "scripts/ci-cohort.py"} for p in paths),
    }


def owner(target):
    manifest = ROOT / "tools/repo-checks/Cargo.toml"
    if manifest.exists() and any(t["name"] == target for t in tomllib.loads(manifest.read_text()).get("test", [])):
        return "tools/repo-checks/Cargo.toml"
    return "Cargo.toml"


def source_owner(source):
    # A future feature-gated core module needs the server feature graph.
    # Do not silently drop its feature semantics in the small extraction crate.
    if source in LEAN & CORE_SOURCE_IMPORTS and not re.search(r"\bfeature\s*=", (ROOT / source).read_text()):
        return "formal/source-linked/lean/Cargo.toml"
    return "Cargo.toml"


def runtime_targets():
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    if manifest["package"].get("autotests", True):
        return sorted({p.stem for p in (ROOT / "tests").glob("*.rs")} - PR_DEEP)
    return sorted({t["name"] for t in manifest.get("test", [])} - PR_DEEP)


REQUIRED = {"quick-check", "ci-matrix", "lint", "nextest", "relay-allocations", "deny", "msrv", "docker", "repository-checks", "fuzz-check", "package-validation", "capacity-qualification"}


def failed_results(results, fuzz, packaging, capacity="true"):
    optional = {"fuzz-check": fuzz, "package-validation": packaging, "capacity-qualification": capacity}
    return sorted(name for name in REQUIRED
                  if name not in results or
                  (results[name]["result"] != "success" and not
                   (optional.get(name) == "false" and results[name]["result"] == "skipped")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--outputs", action="store_true")
    parser.add_argument("--required-result", action="store_true")
    parser.add_argument("--owner")
    parser.add_argument("--source-owner")
    parser.add_argument("--core-sources", action="store_true")
    parser.add_argument("--runtime", action="store_true")
    parser.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME", "pull_request"))
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.required_result:
        bad = failed_results(json.loads(os.environ["RESULTS"]), os.environ["FUZZ"], os.environ["PACKAGING"], os.environ["CAPACITY"])
        if bad:
            raise SystemExit("Required verification did not pass: " + ", ".join(bad))
    elif args.core_sources:
        print("\n".join(source for source in sorted(LEAN) if source_owner(source) != "Cargo.toml"))
    elif args.source_owner:
        print(source_owner(args.source_owner))
    elif args.owner:
        print(owner(args.owner))
    elif args.outputs:
        base = os.environ.get("BASE_SHA")
        # Non-PR events and missing bases must run all specialist checks.
        selected = dict.fromkeys(("lean", "proof_faults", "packaging", "fuzz", "capacity"), True)
        if args.event == "pull_request" and base:
            available = subprocess.run(["git", "cat-file", "-e", f"{base}^{{commit}}"], cwd=ROOT,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if available.returncode != 0:
                subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", base], cwd=ROOT, check=True)
            paths = subprocess.check_output(["git", "diff", "--no-renames", "--name-only", "-z", base, "HEAD"], cwd=ROOT).decode().split("\0")
            selected = classify([p for p in paths if p])
        for key, value in selected.items():
            print(f"{key}={str(value).lower()}")
    elif args.runtime:
        command = ["cargo", "nextest", "run", "--profile", "ci", "--locked", "--all-features", "--cargo-profile", "ci"]
        if args.event == "pull_request":
            command += ["--lib", "--bin", "signal-fish-server"]
            for target in runtime_targets():
                command += ["--test", target]
        if args.dry_run:
            print(json.dumps(command))
        else:
            raise SystemExit(subprocess.call(command, cwd=ROOT))
    else:
        parser.error("choose --outputs, --owner, --source-owner, --core-sources or --runtime")


if __name__ == "__main__":
    main()
