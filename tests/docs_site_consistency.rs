//! Enforceable accuracy + rendering guards for the public documentation site.
//!
//! The `.llm/code-samples/protocol/*.jsonl` files are already type-checked
//! against the real Rust message enums (`tests/v3_protocol_samples.rs`,
//! `tests/v2_wire_golden.rs`). The user-facing MkDocs pages under `docs/`,
//! however, had **no** automated guard tying them to the source of truth — so
//! they could silently drift as the v2/v3 protocol evolved. This module closes
//! that gap with checks that fail (red) when a doc page omits a real protocol
//! surface or links to an anchor that does not render on the published site.
//!
//! What is guarded:
//! 1. `docs/reference/error-codes.md` documents **every** `ErrorCode` variant
//!    (parsed from source and converted to the exact serde wire token), so a
//!    newly added error code cannot ship undocumented.
//! 2. `docs/protocol.md` has a section for **every** `ClientMessage` /
//!    `ServerMessage` variant (parsed from source), so a new message cannot ship
//!    undocumented.
//! 3. `docs/protocol.md` documents the user-facing wire enum tokens.
//! 4. The public docs carry **no** stale/removed protocol tokens or
//!    managed-TURN language (the server no longer provisions TURN).
//! 5. Every intra-`docs/` Markdown `#anchor` link resolves under MkDocs'
//!    (Python-Markdown `toc`) slugify rules. This catches the GitHub-vs-MkDocs
//!    slug divergence: a heading like `## A / B` is `#a--b` on GitHub but
//!    `#a-b` on the MkDocs Pages site, so a hand-written GitHub-style anchor
//!    silently 404s on the published site.

#![cfg(test)]

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use common::{read_file, read_live_file, repo_root};
use serde_json::Value;
use signal_fish_server::config::ProtocolConfig;
use signal_fish_server::protocol::GameDataEncoding;

fn docs_dir() -> PathBuf {
    repo_root().join("docs")
}

// ---------------------------------------------------------------------------
// Source parsing helpers (keep the guards self-maintaining — no hand-kept lists)
// ---------------------------------------------------------------------------

/// Extract the top-level variant identifiers of `enum <enum_name>` from Rust
/// source. Variants are the brace-depth-0 lines inside the enum body whose first
/// token is an UpperCamelCase identifier (doc comments, attributes, and struct
/// fields are skipped because they do not start with an ASCII uppercase letter
/// at depth 0).
fn enum_variants(src: &str, enum_name: &str) -> Vec<String> {
    let needle = format!("enum {enum_name} {{");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("could not find `{needle}` in source"));
    let body_start = src[start..].find('{').expect("enum opening brace") + start + 1;

    // Walk to the matching closing brace of the enum body.
    let mut depth = 1usize;
    let mut body_end = body_start;
    for (i, c) in src[body_start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = body_start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    let body = &src[body_start..body_end];
    let mut variants = Vec::new();
    let mut nest: i32 = 0; // nesting from variant `{ ... }` / `( ... )` payloads
    for line in body.lines() {
        let trimmed = line.trim_start();
        if nest == 0 {
            if let Some(first) = trimmed.chars().next() {
                if first.is_ascii_uppercase() {
                    let ident: String = trimmed
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let after = trimmed[ident.len()..].chars().next();
                    let boundary = matches!(after, None | Some('{') | Some('(') | Some(','))
                        || after.is_some_and(char::is_whitespace);
                    if !ident.is_empty() && boundary {
                        variants.push(ident);
                    }
                }
            }
        }
        for c in line.chars() {
            match c {
                '{' | '(' => nest += 1,
                '}' | ')' => nest -= 1,
                _ => {}
            }
        }
    }
    variants
}

fn enum_variants_from_all_blocks(src: &str, enum_name: &str) -> Vec<Vec<String>> {
    let needle = format!("enum {enum_name} {{");
    let mut blocks = Vec::new();
    let mut search_offset = 0usize;

    while let Some(relative_start) = src[search_offset..].find(&needle) {
        let start = search_offset + relative_start;
        let body_start = src[start..].find('{').expect("enum opening brace") + start + 1;

        let mut depth = 1usize;
        let mut end = body_start;
        for (i, c) in src[body_start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }

        blocks.push(enum_variants(&src[start..end], enum_name));
        search_offset = end;
    }

    blocks
}

/// Convert a PascalCase identifier to serde's `SCREAMING_SNAKE_CASE` wire token
/// (the exact rule `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]` applies: a
/// `_` before every non-initial uppercase letter, then uppercase everything).
fn to_screaming_snake(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 4);
    for (i, c) in ident.char_indices() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

// ---------------------------------------------------------------------------
// MkDocs (Python-Markdown `toc`) slugify + heading/link extraction
// ---------------------------------------------------------------------------

/// Reproduce the default Python-Markdown `toc` slugify used by MkDocs:
/// drop every character that is not word/space/hyphen, lowercase, then collapse
/// each run of hyphen/whitespace to a single `-`. NOTE this intentionally
/// differs from GitHub (which does **not** collapse repeated hyphens), which is
/// exactly the divergence this guard exists to catch.
fn mkdocs_slug(text: &str) -> String {
    let filtered: String = text
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || c.is_whitespace())
        .collect();
    let lowered = filtered.trim().to_lowercase();

    let mut slug = String::with_capacity(lowered.len());
    let mut prev_sep = false;
    for c in lowered.chars() {
        if c == '-' || c.is_whitespace() {
            if !prev_sep {
                slug.push('-');
                prev_sep = true;
            }
        } else {
            slug.push(c);
            prev_sep = false;
        }
    }
    slug.trim_matches('-').to_string()
}

/// Reproduce GitHub's heading-anchor slugify (the `github-slugger` rules used to
/// render `docs/*.md` directly on github.com): lowercase, drop characters that
/// are not alphanumeric / `_` / `-`, map each space to a `-`, and — crucially —
/// do **not** collapse repeated hyphens. A heading like `A / B` therefore yields
/// `a--b` here but `a-b` under [`mkdocs_slug`]; an anchor that is hand-written to
/// match one renderer silently 404s on the other.
fn github_slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        if c.is_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else if c == ' ' || c == '\t' {
            out.push('-');
        }
    }
    out
}

/// Heading anchors a Markdown file produces, computed under BOTH renderers.
#[derive(Default)]
struct HeadingAnchors {
    mkdocs: BTreeSet<String>,
    github: BTreeSet<String>,
}

/// Collect the heading anchors of a Markdown file under both the MkDocs and
/// GitHub slug rules, skipping fenced code blocks so `#` inside code is not
/// mistaken for a heading.
fn heading_anchors(markdown: &str) -> HeadingAnchors {
    let mut anchors = HeadingAnchors::default();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) {
            let rest = &trimmed[hashes..];
            if rest.starts_with(' ') || rest.is_empty() {
                let title = rest.trim().trim_end_matches('#').trim();
                anchors.mkdocs.insert(mkdocs_slug(title));
                anchors.github.insert(github_slug(title));
            }
        }
    }
    anchors
}

/// Extract inline-link targets (`](target)`) from Markdown source.
fn link_targets(markdown: &str) -> Vec<String> {
    let bytes = markdown.as_bytes();
    let mut targets = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j] != b')' && bytes[j] != b'\n' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b')' {
                let target = markdown[i + 2..j].trim().to_string();
                if !target.is_empty() {
                    targets.push(target);
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    targets
}

fn markdown_link_destinations(markdown: &str) -> Vec<String> {
    let mut targets = link_targets(markdown);
    targets.extend(markdown.lines().filter_map(|line| {
        let definition = line.trim().strip_prefix('[')?.split_once("]:")?.1.trim();
        definition.split_whitespace().next().map(str::to_string)
    }));
    targets
}

fn level_two_headings(markdown: &str) -> BTreeSet<String> {
    let mut headings = BTreeSet::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence {
            if let Some(heading) = trimmed.strip_prefix("## ") {
                if !heading.starts_with('#') {
                    headings.insert(heading.trim_end_matches('#').trim().to_string());
                }
            }
        }
    }
    headings
}

fn mkdocs_nav_page_targets(mkdocs: &str) -> BTreeSet<&str> {
    let mut lines = mkdocs.lines().skip_while(|line| *line != "nav:");
    assert_eq!(lines.next(), Some("nav:"), "mkdocs.yml must define nav");

    lines
        .take_while(|line| line.starts_with(' ') || line.trim().is_empty())
        .filter_map(|line| line.trim().strip_prefix("- "))
        .filter_map(|item| item.rsplit_once(':').map(|(_, target)| target.trim()))
        .map(|target| target.trim_matches(['\'', '"']))
        .filter(|target| target.ends_with(".md"))
        .collect()
}

/// Normalize a relative link (handling `.` / `..`) against the linking file.
fn resolve_relative(base_file: &Path, rel: &str) -> PathBuf {
    let mut path = base_file
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .to_path_buf();
    for comp in rel.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            other => path.push(other),
        }
    }
    path
}

fn collect_markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_markdown_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

fn fenced_blocks_with_info<'a>(markdown: &'a str, expected_info: &str) -> Vec<&'a str> {
    let mut blocks = Vec::new();
    let mut offset = 0;
    let lines: Vec<&str> = markdown.split_inclusive('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line_start = offset;
        if let Some((marker, info)) = code_fence_open(lines[i]) {
            offset += lines[i].len();
            let body_start = offset;
            i += 1;
            while i < lines.len() && !is_code_fence_close(lines[i], &marker) {
                offset += lines[i].len();
                i += 1;
            }
            if info == expected_info {
                blocks.push(&markdown[body_start..offset]);
            }
            if i < lines.len() {
                offset += lines[i].len();
            }
        } else {
            offset = line_start + lines[i].len();
        }
        i += 1;
    }
    blocks
}

// ---------------------------------------------------------------------------
// Server-emitted wire-string extraction (keep doc examples honest)
// ---------------------------------------------------------------------------

/// Extract the first double-quoted string literal from `s`. The reason/message
/// literals this guard compares are plain ASCII with no embedded quotes or
/// escapes, so a first-quote/next-quote scan is exact (and far simpler than a
/// real lexer).
fn first_string_literal(s: &str) -> Option<String> {
    let rest = &s[s.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

/// The `reason` strings produced by `ReconnectionError::Display` — the right-hand
/// string literals of its `match` in `src/reconnection.rs`. The typed-rejection
/// path sends these via `error.to_string()` (`reconnection_service.rs`). This is
/// only PART of the allowed set; other rejections send a literal reason that lives
/// in `reconnection_service.rs` (collected by [`string_literals`]). Parsed from
/// source — never a hand-kept list — so a new variant's reason joins the set
/// automatically.
fn reconnection_reason_strings(src: &str) -> BTreeSet<String> {
    let anchor = "impl std::fmt::Display for ReconnectionError";
    let start = src
        .find(anchor)
        .expect("src/reconnection.rs must `impl Display for ReconnectionError`");

    // Bound the scan to this impl's body via brace matching so an unrelated
    // `=> \"...\"` elsewhere in the file can never widen the allowed set.
    let body_start = src[start..].find('{').expect("impl opening brace") + start + 1;
    let mut depth = 1usize;
    let mut body_end = body_start;
    for (i, c) in src[body_start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    body_end = body_start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    src[body_start..body_end]
        .lines()
        .filter_map(|line| {
            line.split_once("=>")
                .and_then(|(_, rhs)| first_string_literal(rhs))
        })
        .collect()
}

/// Every double-quoted string literal in `src`. The reconnection guard feeds it
/// `reconnection_service.rs` to get the reasons that module can put on the wire.
///
/// A reason reaches a `ReconnectionFailed` through more than one path — an inline
/// `reason: "..."` field AND a `reason: &str` helper parameter
/// (`reject_claimed_reconnect`) — so enumerating paths is brittle (it already
/// missed one). Taking EVERY literal in the module is a deliberate *superset*: it
/// also holds a few log strings, which is the safe direction — it can never reject
/// a real reason, while an invented paraphrase still matches nothing. `\"` escapes
/// are honored and `//` comments skipped; the module has no raw strings or
/// quote char-literals (a lexer-pitfall check in the guard's tests asserts this).
fn string_literals(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // Skip `//` line comments so a `"` in a comment can't open a literal.
            '/' if chars.peek() == Some(&'/') => {
                for d in chars.by_ref() {
                    if d == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut literal = String::new();
                while let Some(d) = chars.next() {
                    match d {
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                literal.push(escaped);
                            }
                        }
                        '"' => break,
                        _ => literal.push(d),
                    }
                }
                out.insert(literal);
            }
            _ => {}
        }
    }
    out
}

/// Every `reason` documented inside a `ReconnectionFailed` example, paired with
/// the 1-based line of the fenced JSON block it appears in. Each ```json block is
/// parsed and walked for any `ReconnectionFailed` object (compact or pretty,
/// nested or top-level), so a paraphrased reason cannot hide behind formatting.
/// Unparsable blocks are skipped — safe because every real wire example is valid
/// JSON, and a bogus reason can only be asserted via a valid example anyway.
fn documented_reconnection_reasons(markdown: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        // Skip whole fenced blocks at once. A non-JSON block (including a
        // ```` wrapper that merely *shows* ```json as literal text) is consumed
        // without parsing, so a nested fence is never mistaken for a real example.
        if let Some((marker, info)) = code_fence_open(lines[i]) {
            let fence_line = i + 1;
            i += 1;
            let mut block = String::new();
            while i < lines.len() && !is_code_fence_close(lines[i], &marker) {
                block.push_str(lines[i]);
                block.push('\n');
                i += 1;
            }
            if info == "json" {
                if let Ok(value) = serde_json::from_str::<Value>(&block) {
                    collect_reconnection_reasons(&value, fence_line, &mut found);
                }
            }
        }
        i += 1;
    }
    found
}

/// If `line` opens a fenced code block (3+ backticks or tildes), return the fence
/// marker and the lowercased first token of its info string (`json`, `rust`, …).
/// Handles 4+-fence wrappers and attributes/case (` ```JSON title="x" ` → `json`).
fn code_fence_open(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let ch = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = trimmed[len..]
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    Some((ch.to_string().repeat(len), info))
}

/// A closing fence: only the opener's fence character, at least as long.
fn is_code_fence_close(line: &str, marker: &str) -> bool {
    let trimmed = line.trim();
    let ch = marker.chars().next().expect("non-empty fence marker");
    trimmed.len() >= marker.len() && !trimmed.is_empty() && trimmed.chars().all(|c| c == ch)
}

/// Recursively collect `data.reason` from every `ReconnectionFailed` object in a
/// parsed JSON value.
fn collect_reconnection_reasons(value: &Value, line: usize, out: &mut Vec<(usize, String)>) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("ReconnectionFailed") {
                if let Some(reason) = map
                    .get("data")
                    .and_then(|data| data.get("reason"))
                    .and_then(Value::as_str)
                {
                    out.push((line, reason.to_string()));
                }
            }
            for nested in map.values() {
                collect_reconnection_reasons(nested, line, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_reconnection_reasons(item, line, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

#[test]
fn readme_is_an_on_ramp_to_authoritative_docs() {
    let readme = read_file(&repo_root().join("README.md"));
    let headings = level_two_headings(&readme);
    let links = markdown_link_destinations(&readme);
    let word_count = readme.split_whitespace().count();
    let reference_manual_sections = [
        "Configuration",
        "Protocol Reference",
        "Building from Source",
        "Development",
        "Contributing",
        "Project Structure",
    ];
    let exposed_sections: Vec<&str> = reference_manual_sections
        .into_iter()
        .filter(|heading| headings.contains(*heading))
        .collect();

    let authoritative_docs = [
        "docs/configuration.md",
        "docs/protocol.md",
        "docs/development.md",
    ];
    let missing_links: Vec<&str> = authoritative_docs
        .into_iter()
        .filter(|target| {
            !links.iter().any(|link| {
                link.split(['#', '?'])
                    .next()
                    .is_some_and(|destination| destination.ends_with(*target))
            })
        })
        .collect();

    assert!(
        exposed_sections.is_empty() && missing_links.is_empty() && word_count <= 800,
        "README.md must be a concise user on-ramp, not a duplicate reference manual. \
         Move exhaustive configuration, protocol, build/contributor, and project-structure \
         material to the authoritative docs.\nTop-level reference sections still exposed: \
         {exposed_sections:?}\nMissing authoritative doc links: {missing_links:?}\nREADME word count: \
         {word_count} (maximum 800)"
    );
}

#[test]
fn onboarding_join_room_examples_keep_the_wire_shape() {
    let root = repo_root();
    for (path, minimum_examples) in [("README.md", 1), ("docs/quickstart.md", 2)] {
        let markdown = read_file(&root.join(path));
        let examples: Vec<&str> = fenced_blocks_with_info(&markdown, "javascript")
            .into_iter()
            .filter(|block| block.contains("type: \"JoinRoom\""))
            .collect();
        assert!(
            examples.len() >= minimum_examples,
            "{path} must keep at least {minimum_examples} JavaScript JoinRoom example(s)"
        );

        if path == "README.md" {
            assert!(
                examples.iter().any(|example| {
                    example.contains("new WebSocket(")
                        && (example.contains(".onopen")
                            || example.contains("addEventListener(\"open\""))
                }),
                "README.md JoinRoom onboarding must include a self-contained WebSocket example that sends after the socket opens"
            );
        }

        for example in examples {
            for required in ["data:", "game_name:", "player_name:"] {
                assert!(
                    example.contains(required),
                    "{path} JoinRoom examples must preserve the wire field `{required}`"
                );
            }
            for drifted in ["gameName:", "playerName:"] {
                assert!(
                    !example.contains(drifted),
                    "{path} JoinRoom examples must use snake_case wire fields, not `{drifted}`"
                );
            }
        }
    }
}

#[test]
fn mkdocs_public_nav_excludes_contributor_only_pages() {
    let mkdocs = read_live_file(&repo_root().join("mkdocs.yml"));
    let nav_targets = mkdocs_nav_page_targets(&mkdocs);
    let contributor_only_pages = [
        "development.md",
        "releasing.md",
        "architecture/formal-verification.md",
    ];
    let exposed: Vec<&str> = contributor_only_pages
        .into_iter()
        .filter(|page| nav_targets.contains(page))
        .collect();

    assert!(
        exposed.is_empty(),
        "mkdocs.yml public nav must focus on user documentation; contributor-only \
         development, release, and deep formal-verification pages remain available \
         in the repository but must not be primary navigation entries. Exposed pages: {exposed:?}"
    );
}

#[test]
fn error_code_reference_documents_every_error_code() {
    let root = repo_root();
    let source = read_file(&root.join("src/protocol/error_codes.rs"));
    let doc = read_file(&root.join("docs/reference/error-codes.md"));

    let variants = enum_variants(&source, "ErrorCode");
    assert!(
        variants.len() >= 40,
        "expected to parse the full ErrorCode enum, only found {} variants: {variants:?}",
        variants.len()
    );

    let missing: Vec<String> = variants
        .iter()
        .map(|v| to_screaming_snake(v))
        .filter(|token| !doc.contains(token.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "docs/reference/error-codes.md is missing {} error code(s) that exist in \
         src/protocol/error_codes.rs: {missing:?}",
        missing.len()
    );
}

#[test]
fn protocol_reference_documents_every_message_variant() {
    let root = repo_root();
    let source = read_file(&root.join("src/protocol/messages.rs"));
    let doc = read_file(&root.join("docs/protocol.md"));

    let mut variants = enum_variants(&source, "ClientMessage");
    variants.extend(enum_variants(&source, "ServerMessage"));
    let unique: BTreeSet<String> = variants.into_iter().collect();
    assert!(
        unique.len() >= 35,
        "expected to parse both message enums, only found {} variants: {unique:?}",
        unique.len()
    );

    let missing: Vec<&String> = unique
        .iter()
        .filter(|variant| !doc.contains(variant.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "docs/protocol.md does not document these message variant(s) from \
         src/protocol/messages.rs: {missing:?}",
    );
}

#[test]
fn protocol_reference_documents_user_facing_wire_tokens() {
    let root = repo_root();
    let doc = read_file(&root.join("docs/protocol.md"));

    // Tokens a client author must know to drive v2/v3 traffic. `rkyv` is
    // intentionally excluded: `ProtocolConfig::supported_game_data_formats`
    // only ever advertises `json` + `message_pack`, so `rkyv` is not a
    // negotiable, user-facing encoding.
    let required = [
        "webrtc",       // Transport::WebRtc
        "direct",       // Transport::Direct
        "mesh",         // Topology::Mesh
        "host",         // Topology::Host
        "message_pack", // GameDataEncoding::MessagePack
    ];
    let missing: Vec<&str> = required
        .into_iter()
        .filter(|token| !doc.contains(token))
        .collect();

    assert!(
        missing.is_empty(),
        "docs/protocol.md is missing user-facing wire token(s): {missing:?}",
    );
}

#[test]
fn rust_client_guide_game_data_encoding_matches_advertised_protocol_formats() {
    let guide = read_file(&docs_dir().join("guides/rust-client.md"));
    let enum_blocks = enum_variants_from_all_blocks(&guide, "GameDataEncoding");
    assert!(
        !enum_blocks.is_empty(),
        "docs/guides/rust-client.md must define GameDataEncoding in its Rust samples"
    );

    let expected: Vec<String> = ProtocolConfig::default()
        .supported_game_data_formats()
        .into_iter()
        .map(|format| format!("{format:?}"))
        .collect();
    assert_eq!(
        expected,
        vec!["Json".to_string(), "MessagePack".to_string()],
        "this guard assumes the default ProtocolInfo formats remain json + message_pack"
    );

    let reserved = format!("{:?}", GameDataEncoding::Rkyv);
    for variants in enum_blocks {
        assert_eq!(
            variants, expected,
            "Rust guide GameDataEncoding samples must mirror ProtocolConfig::supported_game_data_formats(); reserved/internal variants such as {reserved} must not be presented as ProtocolInfo-advertised formats"
        );
    }
}

#[test]
fn public_docs_have_no_stale_protocol_tokens() {
    let root = repo_root();
    let mut files = Vec::new();
    collect_markdown_files(&docs_dir(), &mut files);
    files.push(root.join("README.md"));

    // Removed message shapes from earlier protocol revisions. These are exact
    // wire tokens that no longer exist in `src/protocol/messages.rs`, mirroring
    // the anti-drift list in `scripts/check-doc-consistency.sh`. (Managed-TURN
    // language is deliberately NOT token-matched here: the docs legitimately
    // explain that there is *no* managed-TURN mode, so a blunt token check
    // would false-positive on correct prose.)
    //
    // The `relay_type` entry guards a real value-drift bug: `relay_type` is a
    // free-form String whose canonical/default value is "matchbox"
    // (src/config/defaults.rs default_relay_type); several examples had drifted
    // to the bogus value "WebRTC" (v3 transport lives in `SessionPlan.transport`,
    // not `relay_type`).
    let forbidden = [
        "CreateRoom",
        "RoomCreated",
        "SetReady",
        "AuthorityGranted",
        "server_version",
        "\"relay_type\": \"WebRTC\"",
    ];

    let mut hits = Vec::new();
    for file in &files {
        let content = read_file(file);
        for token in forbidden {
            if content.contains(token) {
                hits.push(format!("{} contains stale token '{token}'", file.display()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "Public docs reference stale/removed protocol tokens:\n{}",
        hits.join("\n")
    );
}

#[test]
fn reconnection_failure_docs_use_canonical_reason_strings() {
    // A `ReconnectionFailed.reason` on the wire is one the server can actually
    // emit: a `ReconnectionError::Display` string (the typed path sends
    // `error.to_string()`) OR a literal reason inlined in `reconnection_service.rs`
    // (e.g. "Reconnection is not enabled"). Docs that invent a paraphrase — e.g.
    // "The reconnection token is invalid or malformed." — lie about the wire
    // contract a client matches against. This guard ties every documented
    // ReconnectionFailed example back to that source-derived set.
    let root = repo_root();
    let mut allowed = reconnection_reason_strings(&read_file(&root.join("src/reconnection.rs")));
    allowed.extend(string_literals(&read_file(
        &root.join("src/server/reconnection_service.rs"),
    )));
    assert!(
        allowed.len() >= 5,
        "expected to parse every ReconnectionError::Display reason, got {allowed:?}"
    );

    let mut files = Vec::new();
    collect_markdown_files(&docs_dir(), &mut files);
    files.push(root.join("README.md"));

    let mut violations = Vec::new();
    for file in &files {
        for (line_no, reason) in documented_reconnection_reasons(&read_file(file)) {
            if !allowed.contains(&reason) {
                violations.push(format!(
                    "  {}:{line_no}: documents reason {reason:?}",
                    file.display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "These ReconnectionFailed examples show a `reason` the server never sends \
         (the wire reason comes from `ReconnectionError::Display` in src/reconnection.rs \
         or a literal in src/server/reconnection_service.rs). Use one of {allowed:?}:\n{}",
        violations.join("\n")
    );
}

#[test]
fn docs_internal_anchor_links_resolve_on_both_github_and_mkdocs() {
    let docs = docs_dir();
    let mut files = Vec::new();
    collect_markdown_files(&docs, &mut files);

    // Cache each doc's heading anchor sets (both renderers).
    let anchors: Vec<(PathBuf, HeadingAnchors)> = files
        .iter()
        .map(|f| (f.clone(), heading_anchors(&read_file(f))))
        .collect();
    let anchors_for = |path: &Path| anchors.iter().find(|(p, _)| p == path).map(|(_, a)| a);

    // A linked anchor must resolve on BOTH the published MkDocs Pages site and
    // when the same `.md` is read on github.com. Because the two renderers
    // slugify headings differently (MkDocs collapses repeated hyphens, GitHub
    // does not), the only durable fix for a divergent heading like `A / B` is to
    // reword the heading so both slugs agree — not to hand-pick one renderer's
    // anchor (which breaks the other).
    let check = |fragment: &str, target: &HeadingAnchors| -> Option<&'static str> {
        match (
            target.mkdocs.contains(fragment),
            target.github.contains(fragment),
        ) {
            (true, true) => None,
            (false, true) => Some("resolves on GitHub but NOT on the MkDocs site"),
            (true, false) => Some("resolves on the MkDocs site but NOT on GitHub"),
            (false, false) => Some("no matching heading on either renderer"),
        }
    };

    let mut failures = Vec::new();
    for file in &files {
        let content = read_file(file);
        let own_anchors = anchors_for(file).expect("own anchors");
        for target in link_targets(&content) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let Some((path_part, fragment)) = target.split_once('#') else {
                continue;
            };
            if fragment.is_empty() {
                continue;
            }

            if path_part.is_empty() {
                if let Some(why) = check(fragment, own_anchors) {
                    failures.push(format!("{} -> '#{fragment}' ({why})", file.display()));
                }
                continue;
            }

            // Cross-file anchor: only validate links that stay inside docs/ and
            // point at a Markdown page we render. Links to repo files outside
            // docs/ (../tests, ../.llm, ../clients, ...) are intentionally
            // unresolved by MkDocs and out of scope here.
            if !path_part.ends_with(".md") {
                continue;
            }
            let resolved = resolve_relative(file, path_part);
            if !resolved.starts_with(&docs) {
                continue;
            }
            if let Some(target_anchors) = anchors_for(&resolved) {
                if let Some(why) = check(fragment, target_anchors) {
                    failures.push(format!(
                        "{} -> '{path_part}#{fragment}' ({why})",
                        file.display()
                    ));
                }
            }
            // Target .md not among docs pages -> file-existence is covered by
            // other tooling; don't double-report here.
        }
    }

    assert!(
        failures.is_empty(),
        "Found {} intra-docs anchor link(s) that do not resolve identically on \
         GitHub and the MkDocs Pages site. Reword the target heading so both \
         slugs agree (avoid ` / `, `&`, and double spaces):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn css_scheme_value<'a>(css: &'a str, scheme: &str, property: &str) -> &'a str {
    let selector = format!(r#"[data-md-color-scheme="{scheme}"]"#);
    let scheme_start = css
        .find(&selector)
        .unwrap_or_else(|| panic!("missing CSS scheme selector {selector}"));
    let block_start = css[scheme_start..]
        .find('{')
        .map(|offset| scheme_start + offset + 1)
        .expect("scheme block must open");
    let block_end = css[block_start..]
        .find('}')
        .map(|offset| block_start + offset)
        .expect("scheme block must close");
    let declaration = format!("{property}:");

    css[block_start..block_end]
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix(&declaration)
                .map(|value| value.trim().trim_end_matches(';').trim())
        })
        .unwrap_or_else(|| panic!("{selector} must define {property}"))
}

fn srgb_luminance(hex: &str) -> f64 {
    assert!(
        hex.len() == 7 && hex.starts_with('#'),
        "expected a six-digit hexadecimal CSS color, got {hex}"
    );
    let channel = |start| {
        let value = u8::from_str_radix(&hex[start..start + 2], 16)
            .unwrap_or_else(|_| panic!("invalid hexadecimal CSS color {hex}"));
        let normalized = f64::from(value) / 255.0;
        if normalized <= 0.04045 {
            normalized / 12.92
        } else {
            ((normalized + 0.055) / 1.055).powf(2.4)
        }
    };

    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
}

fn contrast_ratio(first: &str, second: &str) -> f64 {
    let first = srgb_luminance(first);
    let second = srgb_luminance(second);
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}

#[test]
fn docs_brand_assets_and_self_hosted_fonts_are_wired() {
    let root = repo_root();
    let mkdocs = read_live_file(&root.join("mkdocs.yml"));
    let css = read_file(&root.join("docs/stylesheets/extra.css"));

    for required in [
        "logo: assets/logo.svg",
        "favicon: assets/favicon.svg",
        "font: false",
        "scheme: slate",
        "scheme: default",
        "primary: custom",
        "accent: custom",
        "copyright: © 2026 Ambiguous Interactive",
    ] {
        assert!(
            mkdocs.contains(required),
            "mkdocs.yml must preserve the approved Signal Fish Server brand contract: {required}"
        );
    }

    for asset in [
        "docs/assets/logo.svg",
        "docs/assets/favicon.svg",
        "docs/assets/logo-banner.svg",
        "docs/assets/fonts/hanken-grotesk-latin.woff2",
        "docs/assets/fonts/jetbrains-mono-latin.woff2",
        "docs/assets/fonts/space-grotesk-latin.woff2",
        "docs/assets/fonts/Hanken-Grotesk-OFL.txt",
        "docs/assets/fonts/JetBrains-Mono-OFL.txt",
        "docs/assets/fonts/Space-Grotesk-OFL.txt",
    ] {
        assert!(root.join(asset).is_file(), "missing brand asset {asset}");
    }

    for family in ["Hanken Grotesk", "JetBrains Mono", "Space Grotesk"] {
        assert!(
            css.contains(&format!("font-family: \"{family}\"")),
            "extra.css must self-host the approved {family} family"
        );
    }
    assert!(
        !css.contains("fonts.googleapis.com") && !css.contains("fonts.gstatic.com"),
        "the documentation theme must not fetch fonts from third-party origins at runtime"
    );

    let banner = read_file(&root.join("docs/assets/logo-banner.svg"));
    assert!(
        !banner.contains("<text"),
        "logo-banner.svg typography must be converted to paths so GitHub and MkDocs render the same approved lockup"
    );
}

#[test]
fn docs_brand_text_tokens_meet_wcag_aa_contrast() {
    let css = read_file(&repo_root().join("docs/stylesheets/extra.css"));
    for scheme in ["slate", "default"] {
        let background = css_scheme_value(&css, scheme, "--md-default-bg-color");
        for property in [
            "--md-default-fg-color",
            "--md-default-fg-color--light",
            "--md-default-fg-color--lighter",
            "--md-typeset-color",
            "--md-typeset-a-color",
            "--sf-accent",
        ] {
            let foreground = css_scheme_value(&css, scheme, property);
            let ratio = contrast_ratio(foreground, background);
            assert!(
                ratio >= 4.5,
                "{scheme} {property} ({foreground}) has only {ratio:.2}:1 contrast against {background}; text tokens require at least 4.5:1"
            );
        }

        for (foreground_property, background_property, surface) in [
            ("--sf-on-accent", "--sf-accent", "primary button"),
            ("--sf-code-comment", "--md-code-bg-color", "code comment"),
            (
                "--md-primary-bg-color",
                "--md-primary-fg-color",
                "site header",
            ),
        ] {
            let foreground = css_scheme_value(&css, scheme, foreground_property);
            let surface_color = css_scheme_value(&css, scheme, background_property);
            let ratio = contrast_ratio(foreground, surface_color);
            assert!(
                ratio >= 4.5,
                "{scheme} {surface} colors {foreground}/{surface_color} have only {ratio:.2}:1 contrast; text requires at least 4.5:1"
            );
        }
    }

    let logo = read_file(&repo_root().join("docs/assets/logo.svg"));
    assert!(
        logo.contains("#2EE6C6") && logo.contains("#0B1A22"),
        "the approved Vector logo must retain its aqua-on-dark-tile colorway"
    );
    assert!(
        contrast_ratio("#2EE6C6", "#0B1A22") >= 3.0,
        "the compact Vector mark must retain at least 3:1 non-text contrast"
    );
}

// ---------------------------------------------------------------------------
// Unit tests for the reconnection reason extraction (lock in JSON-shape
// robustness so the guard cannot silently miss a drift behind formatting)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod reconnection_reason_tests {
    use super::*;

    fn reasons(md: &str) -> Vec<String> {
        documented_reconnection_reasons(md)
            .into_iter()
            .map(|(_, reason)| reason)
            .collect()
    }

    #[test]
    fn finds_reason_in_pretty_compact_and_nested_json() {
        let pretty = "```json\n{\n  \"type\": \"ReconnectionFailed\",\n  \"data\": { \"reason\": \"A\" }\n}\n```\n";
        assert_eq!(reasons(pretty), ["A"]);

        // Compact / single-line — the old line-scanner missed this.
        let compact =
            "```json\n{ \"type\": \"ReconnectionFailed\", \"data\": { \"reason\": \"B\" } }\n```\n";
        assert_eq!(reasons(compact), ["B"]);

        // Nested inside an envelope, after an unrelated `}` and a sibling object.
        let nested = "```json\n{ \"note\": \"has a } brace\", \"frames\": [ {}, { \"type\": \"ReconnectionFailed\", \"data\": { \"reason\": \"C\" } } ] }\n```\n";
        assert_eq!(reasons(nested), ["C"]);
    }

    #[test]
    fn ignores_non_json_blocks_and_unrelated_reasons() {
        let other = "```json\n{ \"type\": \"RoomJoinFailed\", \"data\": { \"reason\": \"Room is full\" } }\n```\n";
        assert!(reasons(other).is_empty());

        // Not a fenced JSON block → not scanned.
        let prose = "The reason field is `ReconnectionFailed`.\n";
        assert!(reasons(prose).is_empty());
    }

    #[test]
    fn finds_reason_in_uppercase_and_thick_fences() {
        // Reviewer A #1/#2: 4+-backtick fences and an upper/attributed info string
        // must still be scanned.
        let thick = "````json\n{ \"type\": \"ReconnectionFailed\", \"data\": { \"reason\": \"D\" } }\n````\n";
        assert_eq!(reasons(thick), ["D"]);

        let upper = "```JSON title=\"x\"\n{ \"type\": \"ReconnectionFailed\", \"data\": { \"reason\": \"E\" } }\n```\n";
        assert_eq!(reasons(upper), ["E"]);

        // A ```json shown literally *inside* a ```` wrapper is documentation of
        // markup, not a wire example, so it must NOT be scanned.
        let wrapped = "````text\n```json\n{ \"type\": \"ReconnectionFailed\", \"data\": { \"reason\": \"WRAP\" } }\n```\n````\n";
        assert!(reasons(wrapped).is_empty());
    }

    #[test]
    fn allowed_set_is_parsed_from_display_impl() {
        let src = "\
impl std::fmt::Display for ReconnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            Self::TokenMismatch => \"Invalid reconnection token\",
            Self::WindowExpired => \"Reconnection window has expired\",
        };
        f.write_str(reason)
    }
}";
        let set = reconnection_reason_strings(src);
        assert!(set.contains("Invalid reconnection token"));
        assert!(set.contains("Reconnection window has expired"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn string_literals_collects_inline_and_helper_reasons() {
        // Reviewer B #3: reasons reach `ReconnectionFailed` via BOTH an inline
        // field and a `reject_claimed_reconnect(reason: &str)` helper. Collecting
        // every literal covers both paths; `\"`-escapes don't terminate early and
        // `//` comments are skipped.
        let src = "\
        send(ReconnectionFailed { reason: \"Reconnection is not enabled\".to_string(), code });
        self.reject_claimed_reconnect(guard, \"Room is full\", ErrorCode::ReconnectionFailed);
        // \"this is a comment, not a reason\"
        let q = \"a \\\" quote\";";
        let set = string_literals(src);
        assert!(set.contains("Reconnection is not enabled")); // inline field path
        assert!(set.contains("Room is full")); // helper-parameter path
        assert!(set.contains("a \" quote")); // escaped quote handled
        assert!(!set.contains("this is a comment, not a reason")); // comment skipped
    }

    #[test]
    fn reconnection_service_has_no_lexer_pitfalls() {
        // `string_literals` is a simple double-quote scanner; it stays exact only
        // while reconnection_service.rs has no raw strings or quote char-literals.
        // Assert that precondition so a future one fails loudly here rather than
        // silently skewing the allowed set.
        let src = read_file(&repo_root().join("src/server/reconnection_service.rs"));
        let bytes = src.as_bytes();
        // A raw-string prefix is `r`/`r#`…`"` where `r` STARTS a token — not the
        // `r` that merely ends a word like `error"` (which is a normal literal).
        let has_raw_string = (0..bytes.len()).any(|i| {
            bytes[i] == b'r'
                && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_'))
                && {
                    let mut j = i + 1;
                    while bytes.get(j) == Some(&b'#') {
                        j += 1;
                    }
                    bytes.get(j) == Some(&b'"')
                }
        });
        assert!(
            !has_raw_string,
            "raw string in reconnection_service.rs breaks string_literals()"
        );
        assert!(
            !src.contains("'\"'"),
            "a quote char-literal in reconnection_service.rs breaks string_literals()"
        );
    }
}
