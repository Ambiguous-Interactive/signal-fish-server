//! Drift guard tying the machine-readable protocol spec to the Rust source.
//!
//! `spec/signal-fish-protocol.asyncapi.yaml` is the codegen-facing, source-of-
//! truth description of the Signal Fish WebSocket protocol (see the header
//! comment in that file). Client-library authors generate models from it, so it
//! MUST stay in lockstep with the real Rust message enums. This test mirrors the
//! parsing technique in `tests/docs_site_consistency.rs`: it extracts every
//! variant of `ClientMessage` / `ServerMessage` (from `src/protocol/messages.rs`)
//! and every `ErrorCode` variant (from `src/protocol/error_codes.rs`) directly
//! from source — no hand-kept lists — and asserts each appears in the spec.
//!
//! No YAML parser is available as a dev-dependency (checked Cargo.toml), and the
//! task says to prefer whatever needs NO new dependency. The spec models each
//! message variant's wire `type` token as a JSON-Schema `const: <Variant>` and
//! lists every error code in the `ErrorCode` enum, so a plain substring check
//! (the same `doc.contains` strategy `docs_site_consistency` uses) is a robust,
//! dependency-free way to assert presence. The `const:` anchoring makes the
//! match intentional rather than incidental.

#![cfg(test)]

mod common;

use std::collections::BTreeSet;

use common::{read_file, repo_root};

/// Extract the top-level variant identifiers of `enum <enum_name>` from Rust
/// source. Variants are the brace-depth-0 lines inside the enum body whose first
/// token is an UpperCamelCase identifier (doc comments, attributes, and struct
/// fields are skipped because they do not start with an ASCII uppercase letter
/// at depth 0). Copied from `tests/docs_site_consistency.rs` to keep this guard
/// self-contained.
fn enum_variants(src: &str, enum_name: &str) -> Vec<String> {
    let needle = format!("enum {enum_name} {{");
    let start = src
        .find(&needle)
        .unwrap_or_else(|| panic!("could not find `{needle}` in source"));
    let body_start = src[start..].find('{').expect("enum opening brace") + start + 1;

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
    let mut nest: i32 = 0;
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

/// Convert a PascalCase identifier to serde's `SCREAMING_SNAKE_CASE` wire token.
/// Copied from `tests/docs_site_consistency.rs`.
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

fn spec_text() -> String {
    read_file(&repo_root().join("spec/signal-fish-protocol.asyncapi.yaml"))
}

#[test]
fn spec_documents_every_client_and_server_message_variant() {
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let spec = spec_text();

    let mut variants = enum_variants(&source, "ClientMessage");
    variants.extend(enum_variants(&source, "ServerMessage"));
    let unique: BTreeSet<String> = variants.into_iter().collect();
    assert!(
        unique.len() >= 35,
        "expected to parse both message enums, only found {} variants: {unique:?}",
        unique.len()
    );

    // Each variant's wire `type` token is modeled in the spec as a JSON-Schema
    // `const: <Variant>`. Anchoring on `const: ` keeps the match intentional
    // (a variant name appearing only inside prose would not count).
    let missing: Vec<&String> = unique
        .iter()
        .filter(|variant| !spec.contains(&format!("const: {variant}")))
        .collect();

    assert!(
        missing.is_empty(),
        "spec/signal-fish-protocol.asyncapi.yaml is missing a `const: <type>` for these \
         message variant(s) from src/protocol/messages.rs: {missing:?}",
    );

    // StartGame is the freshly added client message; assert it explicitly so a
    // regression naming it differently is unambiguous.
    assert!(
        spec.contains("const: StartGame"),
        "spec must model the StartGame client message"
    );
}

#[test]
fn spec_documents_every_error_code_variant() {
    let source = read_file(&repo_root().join("src/protocol/error_codes.rs"));
    let spec = spec_text();

    let variants = enum_variants(&source, "ErrorCode");
    assert!(
        variants.len() >= 40,
        "expected to parse the full ErrorCode enum, only found {} variants: {variants:?}",
        variants.len()
    );

    let missing: Vec<String> = variants
        .iter()
        .map(|v| to_screaming_snake(v))
        .filter(|token| !spec.contains(token.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "spec/signal-fish-protocol.asyncapi.yaml's ErrorCode enum is missing {} code(s) \
         that exist in src/protocol/error_codes.rs: {missing:?}",
        missing.len()
    );

    // The new game-start codes must be present.
    for token in ["GAME_START_NOT_READY", "GAME_START_FORBIDDEN"] {
        assert!(spec.contains(token), "spec must list error code {token}");
    }
}

#[test]
fn spec_lists_the_wire_token_enums() {
    let spec = spec_text();
    // Transport / Topology / GameDataEncoding wire tokens a codegen consumer
    // needs. Guards against the spec drifting from src/protocol/types.rs.
    for token in [
        "relay",
        "direct",
        "webrtc", // Transport
        "host",
        "mesh", // Topology (relay shared)
        "json",
        "message_pack", // GameDataEncoding
    ] {
        assert!(
            spec.contains(token),
            "spec must document wire token '{token}'"
        );
    }
}
