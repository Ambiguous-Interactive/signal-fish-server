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
/// codes, the Transport / Topology / GameDataEncoding / RelayTransport /
/// SpectatorStateChangeReason / LobbyState / ReplayStatus values, and the
/// `ConnectionInfo` `type` discriminators — is a STRING in the spec, so
/// `as_str()` is exact and complete; a non-string scalar there would be a spec
/// authoring error, not a token to match. (Returns `None` for non-string
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

/// Every wire-token enum's serde representation is documented in the spec.
///
/// Where the message / error-code guards parse Rust source, this asserts the
/// EXACT serde wire token of every variant by SERIALIZING it — authoritative
/// against the three different `rename_all` styles and the per-variant
/// `#[serde(rename)]` overrides these enums use, so the guard can never
/// miscompute a token. Each enum's variants are pinned by a compile-time
/// exhaustiveness `match`: adding a variant fails to compile until it is listed
/// here, and is then checked against the spec. This closes the gap a
/// hand-maintained token list left — a new `Transport` / `Topology` /
/// `GameDataEncoding` / `RelayTransport` / `SpectatorStateChangeReason` /
/// `LobbyState` / `ReplayStatus` value, or a new internally-tagged
/// `ConnectionInfo` `type` discriminator, can no longer ship undocumented.
#[test]
fn spec_documents_every_wire_token_enum_variant() {
    use signal_fish_server::protocol::{
        ConnectionInfo, GameDataEncoding, LobbyState, RelayTransport, ReplayStatus,
        SpectatorStateChangeReason, Topology, Transport,
    };

    let declared = spec_declared_tokens();

    {
        use Transport::*;
        for value in [Relay, Direct, WebRtc] {
            assert_wire_token(&declared, value, "Transport");
        }
        // Compile-time exhaustiveness guard (never called): a new variant fails
        // to compile here until it is added to the checked list above.
        let _exhaustive = |value: Transport| match value {
            Relay | Direct | WebRtc => {}
        };
    }
    {
        use Topology::*;
        for value in [Relay, Host, Mesh] {
            assert_wire_token(&declared, value, "Topology");
        }
        let _exhaustive = |value: Topology| match value {
            Relay | Host | Mesh => {}
        };
    }
    {
        use GameDataEncoding::*;
        // `rkyv` is reserved/internal (not advertised in `ProtocolInfo`) but is
        // still a declared wire value, so the spec lists it and we check it.
        for value in [Json, MessagePack, Rkyv] {
            assert_wire_token(&declared, value, "GameDataEncoding");
        }
        let _exhaustive = |value: GameDataEncoding| match value {
            Json | MessagePack | Rkyv => {}
        };
    }
    {
        use RelayTransport::*;
        for value in [Tcp, Udp, Websocket, Auto] {
            assert_wire_token(&declared, value, "RelayTransport");
        }
        let _exhaustive = |value: RelayTransport| match value {
            Tcp | Udp | Websocket | Auto => {}
        };
    }
    {
        use SpectatorStateChangeReason::*;
        for value in [Joined, VoluntaryLeave, Disconnected, Removed, RoomClosed] {
            assert_wire_token(&declared, value, "SpectatorStateChangeReason");
        }
        let _exhaustive = |value: SpectatorStateChangeReason| match value {
            Joined | VoluntaryLeave | Disconnected | Removed | RoomClosed => {}
        };
    }
    {
        use LobbyState::*;
        for value in [Waiting, Lobby, Finalized] {
            assert_wire_token(&declared, value, "LobbyState");
        }
        let _exhaustive = |value: LobbyState| match value {
            Waiting | Lobby | Finalized => {}
        };
    }
    {
        use ReplayStatus::*;
        for value in [Complete, Truncated, Unavailable] {
            assert_wire_token(&declared, value, "ReplayStatus");
        }
        let _exhaustive = |value: ReplayStatus| match value {
            Complete | Truncated | Unavailable => {}
        };
    }
    {
        // `ConnectionInfo` is internally tagged (`#[serde(tag = "type")]`): each
        // variant serializes to an object whose `type` field is the wire token a
        // codegen consumer switches on. Construct one of each (the field values
        // are irrelevant — only the discriminator is asserted) and check `type`.
        use ConnectionInfo::*;
        let variants = [
            Direct {
                host: "h".to_string(),
                port: 1,
            },
            UnityRelay {
                allocation_id: "a".to_string(),
                connection_data: "c".to_string(),
                key: "k".to_string(),
            },
            Relay {
                host: "h".to_string(),
                port: 1,
                transport: RelayTransport::Auto,
                allocation_id: "a".to_string(),
                token: "t".to_string(),
                client_id: None,
            },
            WebRTC {
                sdp: None,
                ice_candidates: Vec::new(),
            },
            Custom {
                data: serde_json::Value::Null,
            },
        ];
        for value in variants {
            assert_tagged_wire_token(&declared, value, "ConnectionInfo");
        }
        let _exhaustive = |value: ConnectionInfo| match value {
            Direct { .. } | UnityRelay { .. } | Relay { .. } | WebRTC { .. } | Custom { .. } => {}
        };
    }
}

/// Serialize a wire-enum variant and assert its serde token is declared as a
/// `const`/`enum` value in the spec. Serializing — rather than recomputing the
/// rename rule — makes the asserted token authoritative.
fn assert_wire_token<T>(declared: &BTreeSet<String>, value: T, enum_name: &str)
where
    T: serde::Serialize + std::fmt::Debug,
{
    let json = serde_json::to_string(&value).unwrap_or_else(|error| {
        panic!("failed to serialize {enum_name} variant {value:?}: {error}")
    });
    let token = json.trim_matches('"');
    assert!(
        declared.contains(token),
        "spec/signal-fish-protocol.asyncapi.yaml must declare wire token '{token}' \
         (serde form of {enum_name}::{value:?}) as a const/enum value",
    );
}

/// Serialize an internally-tagged (`#[serde(tag = "type")]`) wire-enum variant
/// and assert its `type` discriminator token is declared in the spec. Reading
/// the tag off the serialized object — rather than hard-coding it — keeps the
/// asserted token authoritative against the per-variant `#[serde(rename)]`.
fn assert_tagged_wire_token<T>(declared: &BTreeSet<String>, value: T, enum_name: &str)
where
    T: serde::Serialize + std::fmt::Debug,
{
    let json = serde_json::to_value(&value).unwrap_or_else(|error| {
        panic!("failed to serialize {enum_name} variant {value:?}: {error}")
    });
    let token = json
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| {
            panic!("{enum_name} variant {value:?} serialized without a string `type` tag: {json}")
        });
    assert!(
        declared.contains(token),
        "spec/signal-fish-protocol.asyncapi.yaml must declare {enum_name} `type` token '{token}' \
         (serde form of {enum_name}::{value:?}) as a const/enum value",
    );
}
