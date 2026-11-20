"""Decide and hand off the weekly release using the existing release workflows."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

APP_LOGIN = "bot-auto-commit[bot]"
VERSION = r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){2}"
SHA = r"[0-9a-f]{40}"
PR_WORKFLOWS = ("ci.yml", "doc-validation.yml")
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
)


class ReleaseError(Exception):
    """Release state that GitHub or the local checkout could not verify."""


def command(*args, env=None):
    return subprocess.check_output(args, env=env, text=True).strip()


def api(path, *, paginate=False, payload=None, method="POST", headers=()):
    args = ["gh", "api", path]
    for header in headers:
        args += ["--header", header]
    if paginate:
        args += ["--paginate", "--slurp"]
    if payload is not None:
        args += ["--method", method, "--input", "-"]
    output = subprocess.check_output(
        args, input=json.dumps(payload) if payload is not None else None,
        text=True,
    )
    return json.loads(output)


def output(**values):
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as handle:
        for name, value in values.items():
            print(f"{name}={value}", file=handle)


def summary(message):
    print(message, flush=True)
    if "GITHUB_STEP_SUMMARY" in os.environ:
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as handle:
            print(message, file=handle)


def action_for(complete, unchanged, has_notes):
    if not complete:
        return "retry"
    if unchanged or not has_notes:
        return "skip"
    return "prepare"


def asset_names(tag):
    names = {"sbom.cdx.json"}
    for target in TARGETS:
        extension = "zip" if "windows" in target else "tar.gz"
        archive = f"signal-fish-server-{tag}-{target}.{extension}"
        names.update((archive, f"{archive}.sha256"))
    return names


def release_complete(releases, tag, source, annotated, crate_exists):
    matches = [release for release in releases if release["tag_name"] == tag]
    if not matches:
        return False
    if len(matches) != 1:
        raise ReleaseError(f"Multiple releases match {tag}.")
    release = matches[0]
    lines = release["body"].splitlines()
    sources = [line for line in lines if line.startswith("Source revision:")]
    digests = [line for line in lines if line.startswith("Multi-architecture manifest digest:")]
    if (
        release["name"] != tag or release["draft"] is not False
        or release["prerelease"] is not False or not annotated
        or sources != [f"Source revision: `{source}`"]
        or len(digests) != 1
        or not re.fullmatch(r"Multi-architecture manifest digest: `sha256:[0-9a-f]{64}`", digests[0])
    ):
        raise ReleaseError(f"GitHub Release {tag} conflicts with the reviewed source.")
    uploaded = {asset["name"] for asset in release["assets"] if asset["state"] == "uploaded"}
    return crate_exists and asset_names(tag) <= uploaded


def decide(repo):
    head = command("git", "rev-parse", "HEAD")
    version = command("bash", "scripts/read-toml-string.sh", "Cargo.toml", "version", "package")
    if not re.fullmatch(VERSION, version):
        raise ReleaseError("Cargo.toml has no canonical release version.")
    tag = f"v{version}"
    source = command(
        "bash", "scripts/find-release-version-introduction.sh", ".", head, version,
        "scripts/read-toml-string.sh",
    )
    ref = f"refs/tags/{tag}"
    advertised = command("git", "ls-remote", "--refs", "origin", ref)
    if advertised:
        tag_object = command("git", "rev-parse", ref)
        if (
            advertised != f"{tag_object}\t{ref}"
            or command("git", "cat-file", "-t", ref) != "tag"
            or command("git", "rev-parse", f"{ref}^{{commit}}") != source
        ):
            raise ReleaseError(f"Tag {tag} conflicts with the reviewed source.")
    with tempfile.TemporaryDirectory(dir=os.environ["RUNNER_TEMP"]) as scratch:
        result = Path(scratch) / "crate"
        env = os.environ | {
            "RELEASE_VERSION": version, "RELEASE_SOURCE_REVISION": source,
            "RELEASE_OUTPUT_FILE": str(result),
        }
        command("bash", "scripts/check-crates-io-release.sh", env=env)
        crate_state = result.read_text(encoding="utf-8").strip()
        if crate_state not in {"exists=true", "exists=false"}:
            raise ReleaseError("The crates.io probe returned an unknown state.")
    pages = api(f"repos/{repo}/releases?per_page=100", paginate=True)
    releases = [release for page in pages for release in page]
    complete = release_complete(releases, tag, source, bool(advertised), crate_state == "exists=true")
    notes = Path("CHANGELOG.md").read_text(encoding="utf-8")
    unreleased = re.search(r"^## \[Unreleased\]\n(.*?)(?=^## |\Z)", notes, re.M | re.S)
    if unreleased is None:
        raise ReleaseError("CHANGELOG.md has no Unreleased section.")
    has_notes = bool(re.search(r"^\s*[-*][ \t]+\S", unreleased[1], re.M))
    action = action_for(complete, head == source, has_notes)
    if action == "skip":
        reason = "source already released" if head == source else "no unreleased notes"
        summary(f"Weekly release skipped ({reason}).")
    else:
        summary(f"Weekly release decision: {action} (current release {tag} at {source}).")
    output(action=action, source=source)


def previous_date():
    base = command("git", "rev-parse", "HEAD")
    refs = command("git", "ls-remote", "--heads", "origin", f"refs/heads/release/v*-weekly-{base}")
    if not refs:
        return
    match = re.fullmatch(rf"({SHA})\t(refs/heads/release/v({VERSION})-weekly-{base})", refs)
    if not match:
        raise ReleaseError("Multiple or malformed weekly branches match the preparation base.")
    old_head, ref, version = match.groups()
    command("git", "fetch", "--no-tags", "origin", ref)
    if command("git", "rev-parse", "FETCH_HEAD") != old_head:
        raise ReleaseError("The existing weekly branch moved while reading its date.")
    changelog = command("git", "show", f"{old_head}:CHANGELOG.md")
    dates = re.findall(rf"^## \[{re.escape(version)}\] - (\d{{4}}-\d{{2}}-\d{{2}})$", changelog, re.M)
    if len(dates) != 1:
        raise ReleaseError("The existing weekly branch has no unique release date.")
    output(date=dates[0])


def repo_name(ref):
    return (ref["repo"] or {}).get("full_name")


def verify_pr(pr, repo, default, branch, head):
    if not (
        pr["state"] == "open" and pr["draft"] is False and pr["user"]["login"] == APP_LOGIN
        and pr["head"]["ref"] == branch and pr["head"]["sha"] == head
        and repo_name(pr["head"]) == repo
        and pr["base"]["ref"] == default and repo_name(pr["base"]) == repo
    ):
        raise ReleaseError("The weekly PR is closed, untrusted, or changed identity.")


def prepare_pr(repo, default, version, branch):
    pages = api(f"repos/{repo}/pulls?state=open&base={default}&per_page=100", paginate=True)
    stale = []
    for pr in (pr for page in pages for pr in page):
        ref = pr["head"]["ref"]
        if ref.startswith(f"release/v{version}-refresh-") or ref == f"release/v{version}":
            if repo_name(pr["head"]) == repo:
                summary(f"Weekly preparation deferred to manual PR #{pr['number']}.")
                output(defer="true")
                return
        if ref != branch and re.fullmatch(rf"release/v{re.escape(version)}-weekly-{SHA}", ref):
            if pr["user"]["login"] == APP_LOGIN and repo_name(pr["head"]) == repo:
                stale.append(str(pr["number"]))
    output(stale=" ".join(stale))


def retire(repo, default, version, branch, numbers):
    for number in numbers.split():
        if not number.isdecimal():
            raise ReleaseError("Malformed superseded weekly PR number.")
        path = f"repos/{repo}/pulls/{number}"
        pr = api(path)
        if pr["state"] == "closed":
            continue
        old_branch = pr["head"]["ref"]
        if old_branch == branch or not re.fullmatch(rf"release/v{re.escape(version)}-weekly-{SHA}", old_branch):
            raise ReleaseError(f"PR #{number} is outside the superseded weekly namespace.")
        verify_pr(pr, repo, default, old_branch, pr["head"]["sha"])
        if api(path, method="PATCH", payload={"state": "closed"})["state"] != "closed":
            raise ReleaseError(f"GitHub did not close superseded weekly PR #{number}.")
        summary(f"Closed superseded weekly PR #{number}.")


def checks_passed(repo, branch, head, number):
    ready = True
    for workflow in PR_WORKFLOWS:
        path = (
            f"repos/{repo}/actions/workflows/{workflow}/runs"
            f"?event=pull_request&branch={branch}&head_sha={head}&per_page=1"
        )
        runs = api(path)["workflow_runs"]
        if not runs:
            ready = False
            continue
        run = runs[0]
        if (
            run["head_sha"] != head or run["head_branch"] != branch or run["event"] != "pull_request"
            or (run["pull_requests"] and not any(pr["number"] == number for pr in run["pull_requests"]))
        ):
            raise ReleaseError(f"{workflow} returned a run outside the expected PR.")
        if run["status"] != "completed":
            ready = False
        elif run["conclusion"] != "success":
            raise ReleaseError(f"{workflow} concluded {run['conclusion']} on PR #{number}.")
    return ready


def verify_base(repo, default, base):
    if api(f"repos/{repo}/git/ref/heads/{default}")["object"]["sha"] != base:
        raise ReleaseError("The default branch advanced. Prepare again from its new head.")


def wait_for_checks(repo, default, branch, base, head, number):
    deadline = time.monotonic() + 45 * 60
    while time.monotonic() < deadline:
        pr = api(f"repos/{repo}/pulls/{number}")
        verify_pr(pr, repo, default, branch, head)
        verify_base(repo, default, base)
        if checks_passed(repo, branch, head, number):
            return
        time.sleep(15)
    raise ReleaseError(f"Timed out waiting for the release checks on PR #{number}.")


def merge(repo, default, branch, base, head, number):
    path = f"repos/{repo}/pulls/{number}"
    verify_pr(api(path), repo, default, branch, head)
    verify_base(repo, default, base)
    if not checks_passed(repo, branch, head, number):
        raise ReleaseError("Release checks changed before merge.")
    expected_tree = api(f"repos/{repo}/git/commits/{head}")["tree"]["sha"]
    token = os.environ["RELEASE_APP_TOKEN"]
    reply = json.loads(command(
        "gh", "api", "--method", "PUT", f"{path}/merge",
        "-f", "merge_method=squash", "-f", f"sha={head}",
        env=os.environ | {"GH_TOKEN": token},
    ))
    source = reply["sha"]
    if reply["merged"] is not True or not re.fullmatch(SHA, source):
        raise ReleaseError("GitHub did not confirm the release PR merge.")
    merged_tree = api(f"repos/{repo}/git/commits/{source}")["tree"]["sha"]
    if merged_tree != expected_tree:
        raise ReleaseError("The merged release tree differs from its validated PR head.")
    output(source=source)
    summary(f"Squash-merged weekly release PR #{number} at {source}.")


def dispatch(repo, default, source):
    if not re.fullmatch(SHA, source):
        raise ReleaseError("A full reviewed source SHA is required for publication.")
    receipt = api(
        f"repos/{repo}/actions/workflows/release.yml/dispatches",
        payload={"ref": default, "inputs": {"expected_source_revision": source}},
        headers=("X-GitHub-Api-Version: 2026-03-10",),
    )
    run_id = receipt.get("workflow_run_id")
    if type(run_id) is not int or run_id <= 0 or not receipt.get("html_url"):
        raise ReleaseError("GitHub did not return the publication run receipt.")
    summary(f"Dispatched publication of {source}. [Release run]({receipt['html_url']})")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="action", required=True)
    for name, args in {
        "decide": (),
        "previous-date": (),
        "prepare-pr": ("version", "branch"),
        "retire": ("version", "branch", "numbers"),
        "wait": ("branch", "base", "head", "number"),
        "merge": ("branch", "base", "head", "number"),
        "dispatch": ("source",),
    }.items():
        sub = subcommands.add_parser(name)
        for arg in args:
            sub.add_argument(arg, type=int if arg == "number" else str)
    args = vars(parser.parse_args())
    action = args.pop("action")
    if action == "previous-date":
        previous_date()
    else:
        args["repo"] = os.environ["GITHUB_REPOSITORY"]
        if action != "decide":
            args["default"] = os.environ["DEFAULT_BRANCH"]
        {
            "decide": decide, "prepare-pr": prepare_pr, "retire": retire,
            "wait": wait_for_checks,
            "merge": merge, "dispatch": dispatch,
        }[action](**args)


if __name__ == "__main__":
    try:
        main()
    except (ReleaseError, subprocess.CalledProcessError, KeyError, TypeError, ValueError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        sys.exit(1)
