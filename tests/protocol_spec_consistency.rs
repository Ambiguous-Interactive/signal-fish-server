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
//! The spec is parsed as YAML 1.2 with `saphyr` (already a dev-dependency, used
//! by `tests/ci_config_tests.rs`). Rather than scan the raw text, we collect the
//! spec's *declared wire tokens* — every `const:` value and every `enum:` member,
//! at any depth — and assert each Rust variant/code appears among them by EXACT
//! whole-token match. Anchoring to the JSON-Schema declaration sites is what
//! makes this a real drift guard: a token that only appears as a mapping KEY
//! (e.g. a `host:` field), inside prose, or in an example does NOT satisfy the
//! check — only a genuine `const`/`enum` declaration does. This is strictly more
//! precise than the substring `doc.contains` scan `docs_site_consistency` uses,
//! and parsing makes a malformed spec fail loudly rather than silently passing.

#![cfg(test)]

mod common;

use std::collections::BTreeSet;

use common::{read_file, repo_root};
use saphyr::{LoadableYamlNode, Yaml};

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

/// Parse the spec as YAML 1.2 and collect its *declared wire tokens*: every
/// `const:` value and every scalar inside an `enum:` sequence, at any depth.
///
/// Anchoring to the JSON-Schema declaration sites — rather than every scalar in
/// the document — is what makes membership a real drift guard: a token that
/// merely appears as a mapping KEY (e.g. a `host:` field) or inside prose/an
/// example does NOT count; only an actual `const`/`enum` declaration does. This
/// is strictly more precise than both the old substring scan AND a naive
/// all-scalars collection. A parse failure panics — the spec must always be
/// valid YAML.
fn spec_declared_tokens() -> BTreeSet<String> {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text).unwrap_or_else(|error| {
        panic!("spec/signal-fish-protocol.asyncapi.yaml is not valid YAML: {error}")
    });
    let mut tokens = BTreeSet::new();
    for doc in &docs {
        collect_declared_tokens(doc, &mut tokens);
    }
    assert!(
        !tokens.is_empty(),
        "parsed spec declared no const/enum tokens — the spec is empty or failed to load"
    );
    tokens
}

fn collect_declared_tokens(node: &Yaml, out: &mut BTreeSet<String>) {
    match node {
        Yaml::Mapping(mapping) => {
            for (key, value) in mapping.iter() {
                match key.as_str() {
                    Some("const") => out.extend(scalar_token(value)),
                    Some("enum") => {
                        if let Yaml::Sequence(items) = value {
                            for item in items.iter() {
                                out.extend(scalar_token(item));
                            }
                        }
                    }
                    _ => {}
                }
                // Recurse into the value to reach nested schemas (oneOf, arrays
                // of message objects, component schemas, …).
                collect_declared_tokens(value, out);
            }
        }
        Yaml::Sequence(items) => {
            for item in items.iter() {
                collect_declared_tokens(item, out);
            }
        }
        _ => {}
    }
}

/// Render a scalar `const`/`enum` value as its token string.
///
/// Every wire token this guard checks — message `type` discriminators, error
/// codes, and Transport/Topology/GameDataEncoding values — is a STRING in the
/// spec, so `as_str()` is exact and complete; a non-string scalar there would be
/// a spec authoring error, not a token to match. (Returns `None` for non-string
/// scalars, which simply means they are not counted as tokens.)
fn scalar_token(node: &Yaml) -> Option<String> {
    node.as_str().map(str::to_string)
}

#[test]
fn spec_documents_every_client_and_server_message_variant() {
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let declared = spec_declared_tokens();

    let mut variants = enum_variants(&source, "ClientMessage");
    variants.extend(enum_variants(&source, "ServerMessage"));
    let unique: BTreeSet<String> = variants.into_iter().collect();
    assert!(
        unique.len() >= 35,
        "expected to parse both message enums, only found {} variants: {unique:?}",
        unique.len()
    );

    // Each variant's wire `type` token is modeled in the spec as a JSON-Schema
    // `const: <Variant>` value, so the variant name must appear as an exact
    // scalar in the parsed spec (a name appearing only inside prose would not
    // count).
    let missing: Vec<&String> = unique
        .iter()
        .filter(|variant| !declared.contains(*variant))
        .collect();

    assert!(
        missing.is_empty(),
        "spec/signal-fish-protocol.asyncapi.yaml is missing a `const: <type>` for these \
         message variant(s) from src/protocol/messages.rs: {missing:?}",
    );

    // StartGame is the freshly added client message; assert it explicitly so a
    // regression naming it differently is unambiguous.
    assert!(
        declared.contains("StartGame"),
        "spec must model the StartGame client message"
    );
}

#[test]
fn spec_documents_every_error_code_variant() {
    let source = read_file(&repo_root().join("src/protocol/error_codes.rs"));
    let declared = spec_declared_tokens();

    let variants = enum_variants(&source, "ErrorCode");
    assert!(
        variants.len() >= 40,
        "expected to parse the full ErrorCode enum, only found {} variants: {variants:?}",
        variants.len()
    );

    let missing: Vec<String> = variants
        .iter()
        .map(|v| to_screaming_snake(v))
        .filter(|token| !declared.contains(token.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "spec/signal-fish-protocol.asyncapi.yaml's ErrorCode enum is missing {} code(s) \
         that exist in src/protocol/error_codes.rs: {missing:?}",
        missing.len()
    );

    // The new game-start codes must be present.
    for token in ["GAME_START_NOT_READY", "GAME_START_FORBIDDEN"] {
        assert!(
            declared.contains(token),
            "spec must list error code {token}"
        );
    }
}

#[test]
fn spec_lists_the_wire_token_enums() {
    let declared = spec_declared_tokens();
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
            declared.contains(token),
            "spec must document wire token '{token}'"
        );
    }
}
