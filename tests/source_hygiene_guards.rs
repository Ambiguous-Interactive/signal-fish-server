//! Static hygiene guards for repo *source* (integration tests + shell scripts).
//!
//! Siblings of the protocol/doc drift guards (`tests/docs_site_consistency.rs`,
//! `tests/protocol_spec_consistency.rs`). Those keep docs honest about the wire
//! contract; these keep our own tooling honest. Each guard here encodes a
//! footgun that already bit this repo and that nothing else in CI would catch:
//!
//! 1. An integration test that dials a hardcoded `ws://host:port` can only pass
//!    against a server a human started by hand, so it is invariably `#[ignore]`d
//!    — and an ignored test silently drifts from real behavior. A stale
//!    `lobby_e2e_tests.rs` kept expecting a `LobbyStateChanged` broadcast the
//!    server had stopped sending; nobody noticed because it never ran. Real
//!    integration tests use the in-process harness (a `127.0.0.1:0` listener,
//!    `ws://{addr}`), so a literal endpoint is always the wrong shape.
//!
//! 2. The dev-container bootstrap (`.devcontainer/post-create.sh`) is
//!    best-effort: optional steps are wrapped `if ! step; then warn; continue`.
//!    A function called that way must `return` non-zero, never `exit` — an `exit`
//!    inside it tears down the whole container setup and defeats the recovery
//!    (the Codex install did exactly this). This guard is deliberately scoped to
//!    bootstrap scripts: fail-fast checkers (`scripts/check-*.sh`) and
//!    fail-closed steps (`run-tla-model-check.sh` refusing an unverified jar)
//!    correctly `exit` from helpers, and must not be flagged.
//!
//! 3. A bare `pip install` may target a different interpreter than the `python3`
//!    that later imports the package; `python3 -m pip` installs into the
//!    interpreter that runs it. (`run-z3-proofs.sh` checked `python3 -c import`
//!    but installed with bare `pip`.)
//!
//! 4. A detached in-process `axum::serve` task can outlive its integration
//!    test, leaving upgraded WebSocket task graphs for LeakSanitizer to report.
//!    Real-socket tests must use the shared scoped server fixture.

#![cfg(test)]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::{read_file, repo_root};

// ---------------------------------------------------------------------------
// Guard 1 — integration tests must not hardcode a ws:// endpoint
// ---------------------------------------------------------------------------

/// True if `line` contains a hardcoded `ws://host:port` literal. The harness
/// builds endpoints from an ephemeral port (`ws://{addr}…`, starting with `{`),
/// which is fine; only a literal `host:port` (a fixed numeric port) is flagged.
fn has_hardcoded_ws_endpoint(line: &str) -> bool {
    let mut rest = line;
    while let Some(pos) = rest.find("ws://") {
        rest = &rest[pos + "ws://".len()..];
        let authority: String = rest
            .chars()
            .take_while(|c| !matches!(c, '/' | '"' | '\'' | ' ' | '\t' | '`' | ')'))
            .collect();
        let port_is_numeric = authority
            .rsplit_once(':')
            .and_then(|(_, port)| port.chars().next())
            .is_some_and(|c| c.is_ascii_digit());
        if !authority.starts_with('{') && port_is_numeric {
            return true;
        }
    }
    false
}

#[test]
fn integration_tests_do_not_hardcode_ws_endpoints() {
    let tests_dir = repo_root().join("tests");
    let mut files = Vec::new();
    collect_files(&tests_dir, "rs", &mut files);

    // Skip this guard's own source so its detection literals never self-match.
    let own = Path::new(file!()).file_name();

    let mut violations = Vec::new();
    for file in &files {
        if file.file_name() == own {
            continue;
        }
        for (i, line) in read_file(file).lines().enumerate() {
            if has_hardcoded_ws_endpoint(line) {
                violations.push(format!("  {}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Integration tests must use the in-process harness — dial `ws://{{addr}}` from a \
         127.0.0.1:0 listener (`tests/e2e_tests.rs::start_test_server`), or drive the \
         server directly (`tests/test_helpers.rs::create_test_server`) — never a hardcoded \
         ws://host:port. A literal endpoint needs a hand-started server, forcing \
         `#[ignore]` and silent drift:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Guard 2 — in-process Axum servers must use the scoped fixture
// ---------------------------------------------------------------------------

#[test]
fn integration_tests_do_not_detach_axum_servers() {
    let tests_dir = repo_root().join("tests");
    let fixture = tests_dir.join("test_helpers.rs");
    let own = Path::new(file!()).file_name();
    let mut files = Vec::new();
    collect_files(&tests_dir, "rs", &mut files);

    let mut violations = Vec::new();
    for file in files {
        if file == fixture || file.file_name() == own {
            continue;
        }
        for (index, line) in read_file(&file).lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("axum::serve(") {
                violations.push(format!("  {}:{}", file.display(), index + 1));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Integration tests must not call axum::serve directly; detached serve tasks retain \
         WebSocket ownership graphs at process exit. Use test_helpers::RunningTestServer, whose \
         Drop contract requires awaited shutdown:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Guard 3 — bootstrap functions must `return`, never `exit`
// ---------------------------------------------------------------------------

/// Scripts whose contract is "warn and continue on optional-step failure", so a
/// function reachable from a recovery site (`if ! f` / `f ||`) must not `exit`.
/// Intentionally narrow: fail-fast/fail-closed scripts exit from helpers on
/// purpose and would be false-positived by this rule.
const BOOTSTRAP_SCRIPTS: &[&str] = &[".devcontainer/post-create.sh"];

struct ShellFn {
    name: String,
    body: String,
}

/// Reduce a shell script to *code only*, preserving line count: comments,
/// quoted-string bodies, and heredoc bodies are blanked out. Every downstream
/// step (brace matching, statement/token scanning) then operates on text where a
/// `{`, `}`, `exit`, or `name()` can only be real code — never something that
/// merely appears inside a string, comment, or a heredoc that writes another
/// script (which `post-create.sh` does). This is what makes the analysis robust
/// rather than a fragile regex over raw text.
fn strip_shell_noise(src: &str) -> String {
    let mut cleaned = String::with_capacity(src.len());
    let mut heredoc_terminator: Option<String> = None;
    for line in src.lines() {
        if let Some(term) = &heredoc_terminator {
            // Drop the heredoc body; its terminator (accepting `<<-` indentation)
            // ends the block. The blank line keeps line numbers aligned.
            if line.trim() == term.as_str() {
                heredoc_terminator = None;
            }
        } else {
            let (code, opened) = scan_shell_line(line);
            heredoc_terminator = opened;
            cleaned.push_str(&code);
        }
        cleaned.push('\n');
    }
    cleaned
}

/// Single quote-aware pass over one line: returns the line with comments and
/// quoted-string bodies removed, plus any heredoc terminator the line opens.
/// Doing both in one pass (tracking quote state) is what makes it correct — a
/// `<<` inside a string is ignored, yet a quoted delimiter (`<<'EOF'`) is still
/// captured. Char-based throughout, so multibyte input never panics.
fn scan_shell_line(line: &str) -> (String, Option<String>) {
    let mut out = String::with_capacity(line.len());
    let mut heredoc = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // Single quotes are literal in shell — no escapes.
            '\'' => {
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                }
            }
            // Double quotes honor backslash escapes.
            '"' => {
                while let Some(d) = chars.next() {
                    match d {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            // A `#` begins a comment only at a word boundary (line start or after
            // whitespace); elsewhere it is an ordinary character (e.g. `a#b`).
            '#' if out.chars().last().is_none_or(char::is_whitespace) => break,
            '<' if chars.peek() == Some(&'<') => {
                chars.next(); // consume the second '<'
                out.push_str("<<");
                if chars.peek() == Some(&'<') {
                    chars.next(); // `<<<` is a here-string, not a heredoc
                    out.push('<');
                } else {
                    heredoc = parse_heredoc_delimiter(&mut chars);
                }
            }
            _ => out.push(c),
        }
    }
    (out, heredoc)
}

/// Parse a heredoc delimiter immediately after `<<`: optional `-`, whitespace,
/// optional quote, then the terminator word. Returns the word only if it starts
/// like an identifier, so a left-shift `$((x << 2))` (digit) is not mistaken for
/// a heredoc. Consumes exactly the delimiter from `chars`.
fn parse_heredoc_delimiter(chars: &mut std::iter::Peekable<std::str::Chars>) -> Option<String> {
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
        chars.next();
    }
    if matches!(chars.peek(), Some('\'') | Some('"')) {
        chars.next();
    }
    let mut word = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
            chars.next();
        } else {
            break;
        }
    }
    word.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        .then_some(word)
}

/// Split already-noise-stripped script text into statement segments on `;`, `&`,
/// and `|` (so `&&`/`||`/pipes all break statements), trimmed and non-empty.
fn statements(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|line| line.split([';', '&', '|']))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The first command token of a statement, after peeling leading control
/// keywords/operators (`if`, `then`, `!`, …), a `sudo` prefix, and any leading
/// `VAR=value` environment assignments — so `sudo pip` / `FOO=1 pip` still
/// resolve to `pip`.
fn first_token(stmt: &str) -> Option<&str> {
    let mut s = stmt.trim();
    loop {
        if let Some(rest) = [
            "if ", "then ", "else ", "elif ", "do ", "while ", "until ", "! ", "{ ", "( ", "sudo ",
        ]
        .iter()
        .find_map(|kw| s.strip_prefix(kw))
        {
            s = rest.trim_start();
            continue;
        }
        match s.split_once(char::is_whitespace) {
            Some((head, rest)) if is_env_assignment(head) => s = rest.trim_start(),
            _ => break,
        }
    }
    s.split_whitespace().next()
}

/// True if `token` is a `NAME=value` environment assignment (a valid identifier
/// left of `=`), as opposed to a flag like `--set=x`.
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        }
        None => false,
    }
}

/// Parse `name() { … }` and `function name { … }` definitions from already-
/// noise-stripped script text, capturing each body by brace matching.
fn parse_shell_functions(cleaned: &str) -> Vec<ShellFn> {
    let lines: Vec<&str> = cleaned.lines().collect();
    let mut fns = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let (header, had_function_keyword) = match trimmed.strip_prefix("function ") {
            Some(rest) => (rest.trim(), true),
            None => (trimmed, false),
        };
        if let Some(name) = function_header_name(header, had_function_keyword) {
            let mut depth = 0i32;
            let mut started = false;
            let mut body = String::new();
            let mut j = i;
            while j < lines.len() {
                // On the header line the body begins right after the opening `{`,
                // so a one-liner `f() { exit 1; }` is captured too. Brace depth is
                // still counted over the whole line.
                let segment = match (j == i, lines[j].find('{')) {
                    (true, Some(brace)) => &lines[j][brace + 1..],
                    (true, None) => "",
                    (false, _) => lines[j],
                };
                for c in lines[j].chars() {
                    match c {
                        '{' => {
                            depth += 1;
                            started = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if started {
                    body.push_str(segment);
                    body.push('\n');
                }
                if started && depth <= 0 {
                    break;
                }
                j += 1;
            }
            fns.push(ShellFn { name, body });
            i = j + 1;
        } else {
            i += 1;
        }
    }
    fns
}

/// Extract the function name from a definition header: `name()` (optionally
/// followed by `{`), or — when the line began with the `function` keyword — a
/// bare `name` / `name {`. Returns `None` if the line is not a definition.
fn function_header_name(header: &str, had_function_keyword: bool) -> Option<String> {
    if let Some(paren) = header.find("()") {
        let name = header[..paren].trim();
        let after = header[paren + 2..].trim();
        return (is_function_name(name) && (after.is_empty() || after.starts_with('{')))
            .then(|| name.to_string());
    }
    if had_function_keyword {
        let name = header.split_whitespace().next()?.trim_end_matches('{');
        return is_function_name(name).then(|| name.to_string());
    }
    None
}

fn is_function_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.'))
}

/// Function names invoked at a *recovery* site — `if ! NAME` / `! NAME` (the
/// caller branches on the failure) or `NAME ||` (the caller recovers from it),
/// spaced or not (`NAME||warn`). Token-based on noise-stripped text, so name
/// boundaries are exact (no `foo`/`foobar` confusion) and a name inside a
/// string/comment cannot match.
fn recovery_call_targets(cleaned: &str, names: &BTreeSet<String>) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for line in cleaned.lines() {
        let tokens: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || c == ';')
            .filter(|t| !t.is_empty())
            .collect();
        for (k, &tok) in tokens.iter().enumerate() {
            let next = tokens.get(k + 1);
            // `! NAME` — caller branches on the failure.
            if tok == "!" && next.is_some_and(|n| names.contains(*n)) {
                targets.insert((*next.unwrap()).to_string());
            }
            // `NAME||…` (unspaced) or `NAME ||` (next token starts with `||`).
            match tok.split_once("||") {
                Some((head, _)) if names.contains(head) => {
                    targets.insert(head.to_string());
                }
                None if names.contains(tok) && next.is_some_and(|n| n.starts_with("||")) => {
                    targets.insert(tok.to_string());
                }
                _ => {}
            }
        }
    }
    targets
}

/// The functions in `script_src` that are invoked at a recovery site (`if ! f` /
/// `f ||`) yet can still `exit` — directly or transitively. Factored out of the
/// guard so it can be unit-tested on synthetic scripts.
fn recoverable_functions_that_can_exit(script_src: &str) -> BTreeSet<String> {
    let cleaned = strip_shell_noise(script_src);
    let fns = parse_shell_functions(&cleaned);
    let names: BTreeSet<String> = fns.iter().map(|f| f.name.clone()).collect();

    // Functions that `exit` directly, grown to everything that can reach an
    // `exit` through the call graph (fixed-point closure — terminates because the
    // set only grows and is bounded by `names`).
    let mut can_exit: BTreeSet<String> = fns
        .iter()
        .filter(|f| {
            statements(&f.body)
                .iter()
                .any(|s| first_token(s) == Some("exit"))
        })
        .map(|f| f.name.clone())
        .collect();
    loop {
        let mut changed = false;
        for f in &fns {
            if !can_exit.contains(&f.name)
                && statements(&f.body)
                    .iter()
                    .filter_map(|s| first_token(s))
                    .any(|t| can_exit.contains(t))
            {
                can_exit.insert(f.name.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    recovery_call_targets(&cleaned, &names)
        .into_iter()
        .filter(|name| can_exit.contains(name))
        .collect()
}

#[test]
fn bootstrap_recoverable_functions_do_not_exit() {
    let root = repo_root();
    let mut violations = Vec::new();
    for rel in BOOTSTRAP_SCRIPTS {
        for name in recoverable_functions_that_can_exit(&read_file(&root.join(rel))) {
            violations.push(format!(
                "  {rel}: `{name}` is invoked at a recovery site (`if ! …` / `… ||`) but can still `exit`"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "A bootstrap function whose caller recovers from failure must `return` a \
         non-zero status, not `exit` — an `exit` aborts the whole setup and defeats \
         the `if ! step; then warn; continue` handling:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Guard 4 — install Python packages via the interpreter's own pip module
// ---------------------------------------------------------------------------

#[test]
fn scripts_install_python_packages_via_python_m_pip() {
    let mut violations = Vec::new();
    for script in operational_shell_scripts() {
        let raw = read_file(&script);
        let cleaned = strip_shell_noise(&raw);
        // `strip_shell_noise` preserves line count, so cleaned/raw lines align;
        // detect on cleaned text (no `pip` in a comment/string), report the raw.
        for (i, (raw_line, cleaned_line)) in raw.lines().zip(cleaned.lines()).enumerate() {
            let invokes_bare_pip = statements(cleaned_line)
                .iter()
                .any(|s| matches!(first_token(s), Some("pip" | "pip3")));
            if invokes_bare_pip {
                violations.push(format!(
                    "  {}:{}: {}",
                    script.display(),
                    i + 1,
                    raw_line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Install Python packages with `python3 -m pip` so they land in the \
         interpreter that runs them; a bare `pip` may target a different Python:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Shared file collection
// ---------------------------------------------------------------------------

/// Operational shell scripts (the ones CI/devcontainer actually execute).
/// Excludes vendored (`node_modules`), build (`target`), and illustrative
/// `.agents/skills/*/references` scripts.
fn operational_shell_scripts() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["scripts", ".devcontainer", ".github"] {
        let path = root.join(dir);
        if path.is_dir() {
            collect_files(&path, "sh", &mut files);
        }
    }
    files
}

/// Recursively collect files with the given extension, skipping vendored and
/// build directories.
fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !matches!(name, "node_modules" | "target" | ".git") {
                collect_files(&path, ext, out);
            }
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the analysis helpers (lock in the tricky shell/ws edge cases
// so the GUARDS themselves cannot silently regress into false pos/negatives)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn flags_exit_in_recoverable_function_directly_and_transitively() {
        let direct = "f() { exit 1; }\nif ! f; then warn; fi\n";
        assert!(recoverable_functions_that_can_exit(direct).contains("f"));

        let transitive = "inner() { exit 1; }\nouter() { inner; }\nouter || warn\n";
        assert!(recoverable_functions_that_can_exit(transitive).contains("outer"));
    }

    #[test]
    fn does_not_flag_exit_only_at_a_fatal_call_site() {
        // Called bare (not `if ! f` / `f ||`): a fatal `exit` is intended.
        let fatal = "f() { exit 1; }\nf\n";
        assert!(recoverable_functions_that_can_exit(fatal).is_empty());
    }

    #[test]
    fn exit_inside_a_heredoc_body_is_not_a_function_exit() {
        // Reviewer #1: a function that WRITES a script containing `exit` must not
        // be flagged — the `exit` is data, not control flow.
        let src = "\
gen() {
  cat > out.sh <<'SH'
if [ -z \"$1\" ]; then exit 1; fi
SH
}
if ! gen; then warn; fi
";
        assert!(recoverable_functions_that_can_exit(src).is_empty());
    }

    #[test]
    fn brace_inside_a_string_does_not_corrupt_function_boundaries() {
        // Reviewer #2: a `}` in a string must not prematurely close a function and
        // hide a later real `exit`.
        let src = "danger() {\n  echo \"literal } brace\"\n  exit 1\n}\ndanger || warn\n";
        assert!(recoverable_functions_that_can_exit(src).contains("danger"));
    }

    #[test]
    fn parses_the_function_keyword_form() {
        // Reviewer #3: `function name { … }` (no parens) must be parsed.
        let src = "function f {\n  exit 1\n}\nf || warn\n";
        assert!(recoverable_functions_that_can_exit(src).contains("f"));
    }

    #[test]
    fn first_token_peels_sudo_and_env_assignments() {
        // Reviewer #5: `sudo pip` / `FOO=1 pip` still resolve to `pip`.
        assert_eq!(first_token("sudo pip install x"), Some("pip"));
        assert_eq!(first_token("FOO=1 pip install x"), Some("pip"));
        assert_eq!(first_token("python3 -m pip install x"), Some("python3"));
        assert_eq!(
            first_token("if ! install_codex_cli"),
            Some("install_codex_cli")
        );
        assert!(!is_env_assignment("--set=x")); // a flag is not an assignment
    }

    #[test]
    fn ws_endpoint_detection_flags_only_literal_numeric_ports() {
        assert!(has_hardcoded_ws_endpoint(
            r#"connect("ws://127.0.0.1:3536/ws")"#
        ));
        assert!(!has_hardcoded_ws_endpoint(r#"format!("ws://{addr}/ws")"#));
        assert!(!has_hardcoded_ws_endpoint(
            r#"format!("ws://127.0.0.1:{port}/ws")"#
        ));
        assert!(!has_hardcoded_ws_endpoint(r#""ws://localhost/health""#)); // no port
    }

    #[test]
    fn detects_unspaced_recovery_operator() {
        // Reviewer A #3: `NAME||warn` (no spaces) is the same recovery site as
        // `NAME || warn` in bash and must be detected too.
        let src = "f() {\n  exit 1\n}\nf||warn\n";
        assert!(recoverable_functions_that_can_exit(src).contains("f"));
    }
}
