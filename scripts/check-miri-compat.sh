#!/usr/bin/env bash
# check-miri-compat.sh - Guard against Miri-incompatible wall-clock calls in tests.
#
# Miri interprets the library unit tests with OS isolation enabled, so any test
# that reaches a REALTIME wall-clock syscall aborts the whole `cargo miri test`
# binary:
#
#   - Utc::now()        (chrono  -> clock_gettime(CLOCK_REALTIME))
#   - SystemTime::now() (std     -> clock_gettime(CLOCK_REALTIME))
#
# WHY THIS EXISTS (and why it is discovery-based, not a hand-maintained list):
#   The original guard only inspected the body of each `#[test]` function and a
#   hand-curated allow-list of test names. It silently missed wall-clock calls
#   that a test reached *through a shared fixture helper* (e.g. a `member()`
#   builder that stamped `joined_at: Utc::now()`), which is exactly how a Miri
#   break slipped into CI. This check instead DISCOVERS the problem class:
#   it walks every test module, follows helper calls transitively, and flags any
#   non-ignored test that can reach a wall-clock call.
#
# THE RULE (complete for test code, no hand-maintained allow-list):
#   Inside test code (a `*_tests.rs` file or a `#[cfg(test)] mod` block), a
#   wall-clock call may only be reached by a test annotated
#   `#[cfg_attr(miri, ignore)]`. A non-ignored `#[test]` / `#[tokio::test]` that
#   reaches a wall-clock call — directly or via a same-module helper — is a
#   violation.
#
# HOW TO FIX A VIOLATION:
#   - PREFERRED: if the timestamp is incidental (a test fixture just needs *a*
#     time), replace the wall-clock call with a deterministic constant so the
#     test runs under Miri and stays reproducible. See `base_time()` in
#     src/server/session_policy_tests.rs for the pattern.
#   - Otherwise, if the test genuinely needs real time, add
#     `#[cfg_attr(miri, ignore)]` to that test.
#
# Wall-clock reached through *production* code (e.g. `Room::new()` calling
# `Utc::now()` internally) is intentionally out of scope here — that is Miri's
# own job to catch — so such tests still carry an explicit `#[cfg_attr(miri,
# ignore)]`. This script removes the fragile part: forgetting the annotation
# when the clock hides behind a test helper.
#
# Exit codes:
#   0 - no violations
#   1 - one or more violations (fast, before the slow Miri job)
#   2 - environment error (python3 unavailable)

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

# Optional first argument: a directory to scan instead of <repo>/src (the
# regression tests pass a fixture tree here). Defaults to src/.
SCAN_DIR="${1:-}"

if ! command -v python3 >/dev/null 2>&1; then
    echo "[miri-compat] ERROR: python3 is required but was not found on PATH." >&2
    exit 2
fi

python3 - "$REPO_ROOT" "$SCAN_DIR" <<'PY'
"""Discovery-based Miri wall-clock guard. See the shell header for the contract."""

import pathlib
import re
import sys

repo_root = pathlib.Path(sys.argv[1])
# Optional second arg selects the directory to scan (used by the regression
# tests to point at a fixture tree); defaults to the repository's src/.
scan_arg = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2] else ""
src_dir = pathlib.Path(scan_arg) if scan_arg else repo_root / "src"
if not src_dir.is_absolute():
    src_dir = (repo_root / src_dir).resolve()

# Wall-clock APIs that hit clock_gettime(CLOCK_REALTIME) under Miri isolation.
CLOCK_RE = re.compile(r"\b(?:Utc::now|SystemTime::now)\s*\(\s*\)")
FN_RE = re.compile(r"^(?P<indent>\s*)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(?P<name>\w+)")
ATTR_RE = re.compile(r"^\s*(#\[|//|///)")
TEST_ATTR_RE = re.compile(r"#\[(?:tokio::)?test")
IGNORE_RE = re.compile(r"cfg_attr\(miri,\s*ignore\)")
# A call to another in-module function: `name(` not preceded by `::` or `.`
# (so `Foo::name(` method/assoc calls and `x.name(` do not create spurious edges).
CALL_RE = re.compile(r"(?<![\w:.])(\w+)\s*\(")

# Opening fence of a raw / byte-raw string: (b?)r#*"  -> close is "#* with the
# same number of hashes.
RAW_OPEN_RE = re.compile(r'b?r(#*)"')


def strip_noncode(text):
    """Return `text` with comments and string/char literals removed, so only real
    code feeds the clock/call matchers.

    A single left-to-right scanner (rather than a regex) is used because Rust
    block comments NEST (`/* a /* b */ c */`), which is not a regular language,
    and because comments, strings, and char literals can each contain the
    others' delimiters (`// "` , `"/*"`). Scanning resolves all of that by only
    ever entering one construct at a time. Newlines inside removed spans are
    dropped, which is harmless: function framing and attribute parsing run on the
    original lines; this output only feeds substring searches for wall-clock
    calls and helper-call edges. The char-literal arm consumes exactly one
    char/escape, so Rust lifetimes (`<'a>`, `&'a str`) are preserved as code.
    """
    out = []
    i, n = 0, len(text)
    while i < n:
        two = text[i : i + 2]
        if two == "//":  # line comment (covers //, ///, //!)
            nl = text.find("\n", i)
            i = n if nl == -1 else nl
            continue
        if two == "/*":  # nested-aware block comment
            depth, i = 1, i + 2
            while i < n and depth:
                pair = text[i : i + 2]
                if pair == "/*":
                    depth, i = depth + 1, i + 2
                elif pair == "*/":
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            continue
        raw = RAW_OPEN_RE.match(text, i)
        if raw:  # raw / byte-raw string: r"..", r#".."#, br#".."#
            close = '"' + "#" * len(raw.group(1))
            end = text.find(close, raw.end())
            i = n if end == -1 else end + len(close)
            continue
        if text[i] == '"' or two == 'b"':  # normal / byte string (with escapes)
            i += 2 if two == 'b"' else 1
            while i < n and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            i += 1
            continue
        if text[i] == "'":  # char literal vs lifetime/label
            if i + 1 < n and text[i + 1] == "\\":  # '\n', '\'', '\\' ...
                j = i + 2
                while j < n and text[j] != "'":
                    j += 1
                i = j + 1
                continue
            if i + 2 < n and text[i + 2] == "'":  # 'x'
                i += 3
                continue
            # Otherwise a lifetime/label (`'a`): keep it as code.
        out.append(text[i])
        i += 1
    return "".join(out)


class Fn:
    __slots__ = ("name", "line", "is_test", "is_ignored", "direct_clock", "calls", "tainted")

    def __init__(self, name, line):
        self.name = name
        self.line = line
        self.is_test = False
        self.is_ignored = False
        self.direct_clock = False
        self.calls = set()
        self.tainted = False


def test_regions(path: pathlib.Path, lines):
    """Yield (start, end) line-index ranges that hold test code.

    A `*_tests.rs` file is entirely test code (it is only compiled under
    `#[cfg(test)] mod ..;`). Otherwise, each `#[cfg(test)]` that precedes a
    `mod` opens a region that runs to end of file (inline test modules sit at
    the bottom of their file in this codebase).
    """
    if path.name.endswith("_tests.rs"):
        yield 0, len(lines)
        return
    for i, line in enumerate(lines):
        if line.strip() == "#[cfg(test)]":
            # The module declaration may be on the next non-blank line.
            j = i + 1
            while j < len(lines) and not lines[j].strip():
                j += 1
            if j < len(lines) and re.match(r"\s*(pub\s+)?mod\s+\w+", lines[j]):
                yield j, len(lines)
                return


def gather_attrs(lines, fn_idx):
    """Collect the contiguous attribute/comment block directly above a fn."""
    attrs = []
    k = fn_idx - 1
    while k >= 0 and ATTR_RE.match(lines[k]):
        attrs.append(lines[k])
        k -= 1
    return attrs


def parse_region(lines, start, end):
    """Build the function table for one test region using indentation framing."""
    headers = []
    for i in range(start, end):
        m = FN_RE.match(lines[i])
        if m:
            headers.append((i, len(m.group("indent")), m.group("name")))

    fns = {}
    for h, (idx, indent, name) in enumerate(headers):
        # Body runs until the next fn header at the same-or-shallower indent,
        # excluding that next fn's own attribute/doc block (which sits just above
        # its header) so a neighbour's `#[cfg_attr(...)]` never leaks into this
        # body.
        body_end = end
        for nidx, nindent, _ in headers[h + 1:]:
            if nindent <= indent:
                body_end = nidx - len(gather_attrs(lines, nidx))
                break
        body = strip_noncode("\n".join(lines[idx:body_end]))

        fn = Fn(name, idx + 1)
        attrs = gather_attrs(lines, idx)
        fn.is_test = any(TEST_ATTR_RE.search(a) for a in attrs)
        fn.is_ignored = any(IGNORE_RE.search(a) for a in attrs)
        fn.direct_clock = CLOCK_RE.search(body) is not None
        fn.calls = set(CALL_RE.findall(body)) - {name}
        # Last definition wins if a name repeats; fine for this guard.
        fns[name] = fn
    return fns


def taint(fns):
    """Mark every fn that can reach a wall-clock call (transitive closure)."""
    for fn in fns.values():
        fn.tainted = fn.direct_clock
    changed = True
    while changed:
        changed = False
        for fn in fns.values():
            if fn.tainted:
                continue
            if any(callee in fns and fns[callee].tainted for callee in fn.calls):
                fn.tainted = True
                changed = True


def chain(fns, fn):
    """Describe one shortest path from a test to a wall-clock call, for diagnostics."""
    path = [fn.name]
    cur = fn
    seen = {fn.name}
    while not cur.direct_clock:
        nxt = next(
            (fns[c] for c in sorted(cur.calls) if c in fns and fns[c].tainted and c not in seen),
            None,
        )
        if nxt is None:
            break
        path.append(nxt.name)
        seen.add(nxt.name)
        cur = nxt
    return " -> ".join(path) + " -> Utc::now()/SystemTime::now()"


violations = []
scanned = 0
for path in sorted(src_dir.rglob("*.rs")):
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    for start, end in test_regions(path, lines):
        fns = parse_region(lines, start, end)
        if not fns:
            continue
        taint(fns)
        for fn in fns.values():
            if fn.is_test:
                scanned += 1
                if fn.tainted and not fn.is_ignored:
                    # Display repo-relative when possible; a fixture tree passed
                    # to the regression tests may live outside the repo root.
                    try:
                        loc = path.relative_to(repo_root)
                    except ValueError:
                        loc = path
                    violations.append((f"{loc}:{fn.line}", fn.name, chain(fns, fn)))

print(f"[miri-compat] Scanned {scanned} test function(s) across {src_dir}.")
if not violations:
    print("[miri-compat] OK: no non-ignored test reaches a wall-clock call.")
    sys.exit(0)

print("")
print(f"[miri-compat] {len(violations)} test(s) reach a wall-clock call without "
      "#[cfg_attr(miri, ignore)]:")
print("")
for loc, name, why in violations:
    print(f"  {loc}: `{name}`")
    print(f"      reaches: {why}")
print("")
print("[miri-compat] Fix each by EITHER:")
print("[miri-compat]   - replacing the wall-clock call with a deterministic")
print("[miri-compat]     constant (preferred — keeps the test running under Miri), or")
print("[miri-compat]   - adding #[cfg_attr(miri, ignore)] to the test.")
print("[miri-compat] See base_time() in src/server/session_policy_tests.rs.")
sys.exit(1)
PY
