//! Meta-guard: keep config *presence* assertions in `tests/ci_config_tests.rs`
//! on the live (comment-stripped) view.
//!
//! The drift guards in `ci_config_tests.rs` assert that config files still
//! contain required tokens. Reading raw text with `read_file` lets a
//! *commented-out* line satisfy a `String::contains` presence check, so the
//! guard passes while the live config has silently lost the setting — the exact
//! regression those guards exist to catch. The fix is to assert against the
//! comment-stripped view (`read_live_file` / `strip_comment_lines`). See
//! `.llm/skills/ci-config-live-view-tests.md`.
//!
//! This scanner is a *conservative tripwire*, not a verifier. It flags the one
//! unambiguous, high-frequency footgun: a variable read via raw `read_file` from
//! a comment-bearing config path (`.yml`, `Dockerfile`, `.sh`, `.ps1`, `.toml`,
//! `.gitignore`, `.gitattributes`, JSONC) and then used in a *positive*
//! `var.contains(..)` presence check, inside a function that never derives a
//! comment-stripped view. A hit is always a real problem; a green run is a real
//! (if partial) signal. By design it stays silent rather than guess:
//!   - Absence checks (`!v.contains(..)`) are ignored: a commented occurrence is
//!     still a real occurrence those guards want to flag, so they stay raw.
//!   - Markdown (`.md`) is ignored: a leading `#` is a heading, not a comment.
//!   - A function that already calls `read_live_file` / `strip_comment_lines` is
//!     treated as live-aware and skipped (it has made a deliberate live/raw
//!     split).
//!   - A genuinely-raw presence check (e.g. structural parsing) opts out with an
//!     inline `// live-view-exempt:` justification in the function.
//!
//! Deliberate, documented limitations (the convention doc, the locked mechanism
//! tests `test_strip_comment_lines_*` / `test_drift_guards_reject_commented_out_config`,
//! and the agent self-review checklist are the primary backstops — this tripwire
//! is defense-in-depth on top of them):
//!   - It only inspects the direct `var.contains(` form. Indirect presence checks
//!     (`.lines().any(|l| l.contains(..))`, `.matches(..).count()`, paths reached
//!     through a function parameter or a `read_dir` loop) are not flagged.
//!   - Scope is `ci_config_tests.rs` only — the audited home of this guard class.
//!   - Raw-string fixtures embedded in the test source are neutralized before
//!     scanning (see `blank_raw_string_contents`) so fixture text cannot produce
//!     phantom functions or false hits.

mod common;

use common::{read_file, repo_root};

/// Replace the *contents* of Rust raw-string literals (`r"..."`, `r#"..."#`, with
/// any number of hashes) with spaces, preserving newlines, delimiters, and total
/// length. `ci_config_tests.rs` embeds shell/Rust fixtures inside raw strings; if
/// left intact, fixture text like a column-0 `fn name()` or a `read_file(..)` +
/// `contains(..)` snippet would masquerade as real code and corrupt the scan.
/// Neutralizing raw-string bodies makes the line-based heuristics below see only
/// real source. Ordinary `"..."` string literals (which carry the path literals
/// we need) are left untouched.
fn blank_raw_string_contents(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // A raw string opens with `r`, then >=0 `#`, then `"`. Require the `r` to
        // start a token (prev char not an identifier char) so `for`/`repo_root`
        // never trip it.
        let prev_is_ident = i
            .checked_sub(1)
            .map(|p| bytes[p].is_ascii_alphanumeric() || bytes[p] == b'_')
            .unwrap_or(false);
        if bytes[i] == b'r' && !prev_is_ident {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < bytes.len() && bytes[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                // Copy the opening delimiter `r##"` verbatim.
                out.extend_from_slice(&bytes[i..=j]);
                i = j + 1;
                // Blank the body until the matching `"##` closer.
                loop {
                    if i >= bytes.len() {
                        break;
                    }
                    if bytes[i] == b'"' {
                        let mut k = i + 1;
                        let mut h = 0;
                        while k < bytes.len() && h < hashes && bytes[k] == b'#' {
                            h += 1;
                            k += 1;
                        }
                        if h == hashes {
                            out.extend_from_slice(&bytes[i..k]);
                            i = k;
                            break;
                        }
                    }
                    out.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

/// Does a path string literal name a comment-bearing config file that
/// `strip_comment_lines` can safely reduce to its live view? Markdown is
/// excluded on purpose (`#` is a heading there).
fn is_config_path_literal(lit: &str) -> bool {
    // A real path literal has no spaces; this cheaply rejects diagnostic/message
    // strings that merely mention a config path (e.g. "No files in .github/...").
    if lit.is_empty() || lit.contains(' ') {
        return false;
    }
    lit.ends_with(".yml")
        || lit.ends_with(".yaml")
        || lit.ends_with("Dockerfile")
        || lit.ends_with(".sh")
        || lit.ends_with(".ps1")
        || lit.ends_with(".toml")
        || lit.ends_with(".gitignore")
        || lit.ends_with(".gitattributes")
        || lit.ends_with("devcontainer.json")
        // Directory roots whose iterated children are config (e.g.
        // `root.join(".github/workflows").join(file)`).
        || lit.contains(".github/workflows")
}

/// Extract every double-quoted string literal on a line. Good enough for this
/// test source: paths here are plain literals with no escaped quotes.
fn string_literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if let Some(rel) = line[i + 1..].find('"') {
                out.push(line[i + 1..i + 1 + rel].to_string());
                i = i + 1 + rel + 1;
                continue;
            }
            break;
        }
        i += 1;
    }
    out
}

/// Split top-level functions out of the source. Every `fn` in this test file is
/// at column 0 (optionally `pub`/`pub(crate)`/`async`). Returns `(name, body)`
/// where `body` is the text from the `fn` header to the next one.
fn functions(src: &str) -> Vec<(String, String)> {
    let is_header = |line: &str| -> Option<String> {
        let rest = line
            .strip_prefix("fn ")
            .or_else(|| line.strip_prefix("pub fn "))
            .or_else(|| line.strip_prefix("pub(crate) fn "))
            .or_else(|| line.strip_prefix("async fn "))
            .or_else(|| line.strip_prefix("pub async fn "))?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    };

    let lines: Vec<&str> = src.lines().collect();
    let mut headers: Vec<(usize, String)> = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        if let Some(name) = is_header(line) {
            headers.push((idx, name));
        }
    }

    let mut out = Vec::new();
    for (h, (start, name)) in headers.iter().enumerate() {
        let end = headers.get(h + 1).map(|(n, _)| *n).unwrap_or(lines.len());
        out.push((name.clone(), lines[*start..end].join("\n")));
    }
    out
}

/// Is this `.contains(` call negated — `!v.contains(..)` or `!(v.contains(..))`?
/// Distinguishes a unary `!` (negation) from a macro bang like `assert!(` by
/// requiring the character before the `!` to be a non-identifier char.
fn is_negated(body: &str, receiver_start: usize) -> bool {
    let before = body[..receiver_start].trim_end();
    // Peel one optional grouping paren so `!(v.contains` is seen as negated.
    let before = before
        .strip_suffix('(')
        .map(str::trim_end)
        .unwrap_or(before);
    match before.strip_suffix('!') {
        // A bang whose preceding char is an identifier char is a macro
        // (`assert!`, `matches!`), not negation — the call is positive.
        Some(head) => head
            .chars()
            .next_back()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true),
        None => false,
    }
}

/// Scan test source for raw-`read_file` config-presence footguns, returning a
/// human-readable line per offending `(function, variable)`. Pulled out of the
/// `#[test]` so the self-tests below can exercise it on synthetic snippets.
fn scan_violations(raw_src: &str) -> Vec<String> {
    let src = blank_raw_string_contents(raw_src);
    let mut violations = Vec::new();

    for (fn_name, body) in functions(&src) {
        // Live-aware functions have made a deliberate live/raw split — skip.
        if body.contains("read_live_file(") || body.contains("strip_comment_lines(") {
            continue;
        }
        // Explicit, justified opt-out for a genuinely-raw presence check.
        if body.contains("live-view-exempt") {
            continue;
        }

        // Map each `let <var> = read_file(<arg>)` to whether it reads config.
        for raw_line in body.lines() {
            let line = raw_line.trim_start();
            let Some(after_let) = line.strip_prefix("let ") else {
                continue;
            };
            let Some(eq) = after_let.find('=') else {
                continue;
            };
            // Extract the bound identifier: drop an optional `mut`, then take the
            // leading identifier (stops at a `:` type annotation or whitespace).
            let binding = after_let[..eq].trim();
            let binding = binding.strip_prefix("mut ").unwrap_or(binding);
            let var: String = binding
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if var.is_empty() || !after_let[eq..].contains("read_file(") {
                continue;
            }

            // Resolve the read path: literals on the read_file line itself, or
            // (when the arg is a `&path`/`path` variable) on that variable's
            // `let` line elsewhere in the function.
            let mut path_lits = string_literals(raw_line);
            if path_lits.is_empty() {
                // arg is a variable — find its binding line.
                let arg = after_let[eq + 1..]
                    .split("read_file(")
                    .nth(1)
                    .unwrap_or("")
                    .trim_start_matches('&')
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect::<String>();
                if !arg.is_empty() {
                    let needle = format!("let {arg} ");
                    let needle_eq = format!("let {arg}=");
                    for cand in body.lines() {
                        let t = cand.trim_start();
                        if t.starts_with(&needle) || t.starts_with(&needle_eq) {
                            path_lits.extend(string_literals(cand));
                        }
                    }
                }
            }

            let reads_config = path_lits.iter().any(|l| is_config_path_literal(l));
            if !reads_config {
                continue;
            }

            // Flag a positive (non-negated) `<var>.contains(` presence check.
            let needle = format!("{var}.contains(");
            let mut search_from = 0;
            while let Some(rel) = body[search_from..].find(&needle) {
                let at = search_from + rel;
                // Require a word boundary before the receiver so `xcontent`
                // does not match `content`.
                let boundary_ok = at == 0
                    || !body.as_bytes()[at - 1].is_ascii_alphanumeric()
                        && body.as_bytes()[at - 1] != b'_';
                if boundary_ok && !is_negated(&body, at) {
                    violations.push(format!(
                        "  - fn `{fn_name}`: `{var}` is read with raw `read_file` from a \
                         comment-bearing config path ({}) and used in a positive \
                         `{var}.contains(..)` presence check.",
                        path_lits.join(", ")
                    ));
                    break;
                }
                search_from = at + needle.len();
            }
        }
    }

    violations
}

#[test]
fn test_ci_config_presence_checks_use_live_view() {
    let src = read_file(&repo_root().join("tests/ci_config_tests.rs"));
    let violations = scan_violations(&src);
    assert!(
        violations.is_empty(),
        "Config presence assertions must run against the live (comment-stripped) view so a \
         commented-out config line cannot satisfy them.\n\n{}\n\n\
         Fix each by reading the file with `read_live_file(..)` instead of `read_file(..)` \
         (or, when the same variable also feeds an absence/structural check, add \
         `let v_live = strip_comment_lines(&v);` and move the presence assertion to `v_live`).\n\
         If the raw read is genuinely required for a presence check, justify it with an inline \
         `// live-view-exempt: <reason>` comment in the function.\n\
         See .llm/skills/ci-config-live-view-tests.md.",
        violations.join("\n")
    );
}

#[cfg(test)]
mod self_tests {
    use super::*;

    #[test]
    fn blank_raw_string_neutralizes_fixtures_but_keeps_real_code() {
        let src = r##"let p = "x.yml";
let body = r#"
fn fake_fixture() { let c = read_file("y.yml"); assert!(c.contains("z")); }
"#;
let real = read_file("x.yml");"##;
        let blanked = blank_raw_string_contents(src);
        // Real code and the real (regular-string) path literals survive.
        assert!(blanked.contains("\"x.yml\""));
        assert!(blanked.contains("let real = read_file("));
        // Fixture text inside the raw string is neutralized.
        assert!(!blanked.contains("fake_fixture"), "blanked:\n{blanked}");
        assert!(!blanked.contains("y.yml"));
        // Length and line count are preserved so any offsets stay valid.
        assert_eq!(blanked.len(), src.len());
        assert_eq!(blanked.lines().count(), src.lines().count());
    }

    #[test]
    fn negation_is_classified_correctly() {
        let idx = |s: &str| s.find("c.contains(").unwrap();
        // Positive: macro bang must not read as negation.
        let pos = r#"assert!(c.contains("x"))"#;
        assert!(!is_negated(pos, idx(pos)));
        let bare = r#"if c.contains("x") {"#;
        assert!(!is_negated(bare, idx(bare)));
        // Negated: unary `!`, parenthesized `!(`, and inside a closure.
        let neg = r#"assert!(!c.contains("x"))"#;
        assert!(is_negated(neg, idx(neg)));
        let neg_paren = r#"assert!(!(c.contains("x")))"#;
        assert!(is_negated(neg_paren, idx(neg_paren)));
        let neg_closure = r#"|m| !c.contains(m)"#;
        assert!(is_negated(neg_closure, idx(neg_closure)));
    }

    #[test]
    fn scan_flags_raw_config_presence_but_allows_safe_forms() {
        // RED: raw read_file of a workflow + positive contains, no strip.
        let bad = r#"fn t() {
    let content = read_file(&root.join(".github/workflows/ci.yml"));
    assert!(content.contains("concurrency:"));
}"#;
        assert_eq!(
            scan_violations(bad).len(),
            1,
            "raw config presence must flag"
        );

        // GREEN: live view, absence check, explicit exemption, and markdown all
        // pass (markdown is never stripped — `#` is a heading there).
        let live = r#"fn t() {
    let content = read_live_file(&root.join(".github/workflows/ci.yml"));
    assert!(content.contains("concurrency:"));
}"#;
        let absence = r#"fn t() {
    let content = read_file(&root.join(".github/workflows/ci.yml"));
    assert!(!content.contains("permissions: write-all"));
}"#;
        let exempt = r#"fn t() {
    // live-view-exempt: structural parse needs raw text
    let content = read_file(&root.join(".github/workflows/ci.yml"));
    assert!(content.contains("awk '"));
}"#;
        let markdown = r##"fn t() {
    let content = read_file(&root.join("README.md"));
    assert!(content.contains("# Heading"));
}"##;
        for (name, safe) in [
            ("live", live),
            ("absence", absence),
            ("exempt", exempt),
            ("markdown", markdown),
        ] {
            assert!(
                scan_violations(safe).is_empty(),
                "{name} form must not be flagged"
            );
        }
    }
}
