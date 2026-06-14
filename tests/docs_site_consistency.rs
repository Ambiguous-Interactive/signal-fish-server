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

use common::{read_file, repo_root};
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

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

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
