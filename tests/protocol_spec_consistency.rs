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
//! spec's *declared wire tokens* — every `const:` value, every `enum:` member,
//! and the explicit physical-frame `x-rust-server-variant` marker — and asserts
//! each Rust variant/code appears among them by EXACT whole-token match.
//! Anchoring to the declaration sites is what
//! makes this a real drift guard: a token that only appears as a mapping KEY
//! (e.g. a `host:` field), inside prose, or in an example does NOT satisfy the
//! check — only a genuine declaration or physical-frame marker does. This is strictly more
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
/// `const:` value, every scalar inside an `enum:` sequence, and the explicit
/// `x-rust-server-variant` marker for a non-JSON physical frame.
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
                    Some("x-rust-server-variant") => out.extend(scalar_token(value)),
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
/// Every wire token this guard checks — message `type` discriminators, the
/// physical `GameDataBinary` variant marker, error
/// codes, the Transport / Topology / GameDataEncoding / DeliveryClass /
/// DeliveryGapReason / RelayTransport / SpectatorStateChangeReason /
/// LobbyState / ReplayStatus values, and the `ConnectionInfo` `type`
/// discriminators — is a STRING in the spec, so
/// `as_str()` is exact and complete; a non-string scalar there would be a spec
/// authoring error, not a token to match. (Returns `None` for non-string
/// scalars, which simply means they are not counted as tokens.)
fn scalar_token(node: &Yaml) -> Option<String> {
    node.as_str().map(str::to_string)
}

fn mapping_path<'doc, 'input>(
    mut node: &'doc Yaml<'input>,
    path: &[&str],
) -> Option<&'doc Yaml<'input>> {
    for key in path {
        node = node.as_mapping_get(key)?;
    }
    Some(node)
}

fn collect_local_references(node: &Yaml, references: &mut Vec<String>) {
    match node {
        Yaml::Mapping(mapping) => {
            for (key, value) in mapping.iter() {
                if key.as_str() == Some("$ref") {
                    let reference = value
                        .as_str()
                        .expect("protocol spec $ref values must be strings");
                    assert!(
                        reference.starts_with("#/"),
                        "protocol spec must use resolvable local references, got {reference}"
                    );
                    references.push(reference.to_string());
                }
                collect_local_references(value, references);
            }
        }
        Yaml::Sequence(items) => {
            for item in items.iter() {
                collect_local_references(item, references);
            }
        }
        _ => {}
    }
}

fn resolve_local_reference<'doc, 'input>(
    mut node: &'doc Yaml<'input>,
    reference: &str,
) -> Option<&'doc Yaml<'input>> {
    for raw_segment in reference.strip_prefix("#/")?.split('/') {
        let segment = raw_segment.replace("~1", "/").replace("~0", "~");
        node = node.as_mapping_get(segment.as_str())?;
    }
    Some(node)
}

#[test]
fn spec_has_no_dangling_local_references() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let mut references = Vec::new();
    collect_local_references(root, &mut references);
    assert!(
        !references.is_empty(),
        "protocol spec must contain references"
    );

    let dangling: Vec<_> = references
        .iter()
        .filter(|reference| resolve_local_reference(root, reference).is_none())
        .collect();
    assert!(
        dangling.is_empty(),
        "protocol spec contains dangling local references: {dangling:?}"
    );
}

#[test]
fn direct_session_plan_serialization_matches_the_executable_schema_branch() {
    use signal_fish_server::protocol::{
        DirectEndpoint, ServerMessage, SessionPlanPayload, Topology, Transport,
    };
    use uuid::Uuid;

    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let schema = mapping_path(
        root,
        &["components", "schemas", "SessionPlan", "properties", "data"],
    )
    .expect("spec must define SessionPlan.data");
    let endpoint_schema = mapping_path(root, &["components", "schemas", "DirectEndpoint"])
        .expect("spec must define DirectEndpoint");

    let endpoint_required: BTreeSet<_> = endpoint_schema
        .as_mapping_get("required")
        .and_then(Yaml::as_sequence)
        .expect("DirectEndpoint must list required fields")
        .iter()
        .map(|field| field.as_str().expect("required field must be a string"))
        .collect();
    assert_eq!(endpoint_required, BTreeSet::from(["host", "port"]));
    assert_eq!(
        mapping_path(endpoint_schema, &["properties", "host", "minLength"])
            .and_then(Yaml::as_integer),
        Some(1)
    );
    assert_eq!(
        mapping_path(endpoint_schema, &["properties", "port", "minimum"])
            .and_then(Yaml::as_integer),
        Some(1)
    );

    let branches = schema
        .as_mapping_get("oneOf")
        .and_then(Yaml::as_sequence)
        .expect("SessionPlan.data must declare exact legal branches");
    let legal_pairs: BTreeSet<_> = branches
        .iter()
        .map(|branch| {
            let topology = mapping_path(branch, &["properties", "topology", "const"])
                .and_then(Yaml::as_str)
                .expect("each SessionPlan branch must fix topology");
            let transport = mapping_path(branch, &["properties", "transport", "const"])
                .and_then(Yaml::as_str)
                .expect("each SessionPlan branch must fix transport");
            (topology, transport)
        })
        .collect();
    assert_eq!(
        legal_pairs,
        BTreeSet::from([
            ("relay", "relay"),
            ("host", "direct"),
            ("host", "webrtc"),
            ("mesh", "webrtc"),
        ]),
        "SessionPlan schema must reject every illegal topology/transport cross-product"
    );
    for branch in branches {
        let required: BTreeSet<_> = branch
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .expect("every SessionPlan branch must list required fields")
            .iter()
            .map(|field| field.as_str().expect("required field must be a string"))
            .collect();
        assert!(
            required.contains("generation"),
            "every SessionPlan branch must require its generation fence"
        );
        assert_eq!(
            mapping_path(branch, &["properties", "generation", "$ref"]).and_then(Yaml::as_str),
            Some("#/components/schemas/SessionGeneration")
        );
    }

    let direct_branch = branches
        .iter()
        .find(|branch| {
            mapping_path(branch, &["properties", "transport", "const"]).and_then(Yaml::as_str)
                == Some("direct")
        })
        .expect("SessionPlan.data must define host+direct");
    let direct_required: BTreeSet<_> = direct_branch
        .as_mapping_get("required")
        .and_then(Yaml::as_sequence)
        .expect("Direct SessionPlan branch must list required fields")
        .iter()
        .map(|field| field.as_str().expect("required field must be a string"))
        .collect();
    assert_eq!(
        direct_required,
        BTreeSet::from([
            "generation",
            "topology",
            "transport",
            "host",
            "direct_endpoint",
            "peers",
            "fallback",
        ])
    );
    assert_eq!(
        mapping_path(direct_branch, &["properties", "direct_endpoint", "$ref"])
            .and_then(Yaml::as_str),
        Some("#/components/schemas/DirectEndpoint")
    );

    let host = Uuid::from_u128(10);
    let message = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        generation: Uuid::from_u128(9),
        topology: Topology::Host,
        transport: Transport::Direct,
        host: Some(host),
        direct_endpoint: Some(DirectEndpoint {
            host: "192.0.2.10".to_string(),
            port: 7777,
        }),
        peers: Vec::new(),
        ice_servers: Vec::new(),
        fallback: Transport::Relay,
    }));
    let serialized = serde_json::to_value(message).expect("serialize Direct SessionPlan");
    assert_eq!(serialized["type"], "SessionPlan");
    assert_eq!(serialized["data"]["topology"], "host");
    assert_eq!(serialized["data"]["transport"], "direct");
    assert_eq!(serialized["data"]["host"], host.to_string());
    assert_eq!(serialized["data"]["direct_endpoint"]["host"], "192.0.2.10");
    assert_eq!(serialized["data"]["direct_endpoint"]["port"], 7777);
    assert!(serialized["data"].get("ice_servers").is_none());
}

#[test]
fn signal_schemas_require_the_session_generation_fence() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    for (schema, peer_field) in [("ClientSignal", "to"), ("ServerSignal", "from")] {
        let data = mapping_path(
            root,
            &["components", "schemas", schema, "properties", "data"],
        )
        .unwrap_or_else(|| panic!("spec must define {schema}.data"));
        let required: BTreeSet<_> = data
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .unwrap_or_else(|| panic!("{schema}.data must list required fields"))
            .iter()
            .map(|field| field.as_str().expect("required field must be a string"))
            .collect();
        assert_eq!(
            required,
            BTreeSet::from([peer_field, "generation", "signal"])
        );
        assert_eq!(
            mapping_path(data, &["properties", "generation", "$ref"]).and_then(Yaml::as_str),
            Some("#/components/schemas/SessionGeneration")
        );
    }
}

#[test]
fn connection_info_schema_is_an_exact_union_of_rust_wire_shapes() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let branches = mapping_path(root, &["components", "schemas", "ConnectionInfo", "oneOf"])
        .and_then(Yaml::as_sequence)
        .expect("ConnectionInfo must be an exact oneOf");

    let expected = [
        ("direct", BTreeSet::from(["type", "host", "port"])),
        (
            "unity_relay",
            BTreeSet::from(["type", "allocation_id", "connection_data", "key"]),
        ),
        (
            "relay",
            BTreeSet::from(["type", "host", "port", "allocation_id", "token"]),
        ),
        ("webrtc", BTreeSet::from(["type", "ice_candidates"])),
        ("custom", BTreeSet::from(["type", "data"])),
    ];
    assert_eq!(branches.len(), expected.len());
    for (token, required) in expected {
        let branch = branches
            .iter()
            .find(|branch| {
                mapping_path(branch, &["properties", "type", "const"]).and_then(Yaml::as_str)
                    == Some(token)
            })
            .unwrap_or_else(|| panic!("missing ConnectionInfo branch {token}"));
        assert_eq!(
            branch
                .as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{token} must reject fields from other variants"
        );
        let actual: BTreeSet<_> = branch
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .expect("branch must declare required fields")
            .iter()
            .map(|field| field.as_str().expect("required field must be a string"))
            .collect();
        assert_eq!(actual, required, "required fields drifted for {token}");
    }

    assert_eq!(
        mapping_path(
            branches
                .iter()
                .find(|branch| {
                    mapping_path(branch, &["properties", "type", "const"]).and_then(Yaml::as_str)
                        == Some("webrtc")
                })
                .expect("webrtc branch"),
            &["properties", "sdp", "nullable"],
        )
        .and_then(Yaml::as_bool),
        Some(true),
        "Rust accepts omitted WebRTC.sdp and serializes None as an explicit null"
    );
}

#[test]
fn authority_option_fields_are_required_but_nullable_on_the_wire() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    for (schema_name, field) in [
        ("AuthorityChanged", "authority_player"),
        ("AuthorityResponse", "reason"),
    ] {
        let data = mapping_path(
            root,
            &["components", "schemas", schema_name, "properties", "data"],
        )
        .unwrap_or_else(|| panic!("missing {schema_name}.data"));
        let required: BTreeSet<_> = data
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .expect("authority payload must list required fields")
            .iter()
            .map(|value| value.as_str().expect("required field must be a string"))
            .collect();
        assert!(
            required.contains(field),
            "{schema_name}.{field} is always serialized"
        );
        assert_eq!(
            mapping_path(data, &["properties", field, "nullable"]).and_then(Yaml::as_bool),
            Some(true),
            "{schema_name}.{field}=None serializes as null"
        );
    }
}

#[test]
fn spec_delivery_report_gap_bound_matches_protocol_constant() {
    use signal_fish_server::protocol::DELIVERY_REPORT_MAX_GAPS;

    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let gaps = mapping_path(
        root,
        &[
            "components",
            "schemas",
            "DeliveryReport",
            "properties",
            "data",
            "properties",
            "gaps",
        ],
    )
    .expect("protocol spec must define DeliveryReport.data.gaps");

    assert_eq!(
        gaps.as_mapping_get("minItems").and_then(Yaml::as_integer),
        Some(1),
        "a present DeliveryReport.gaps array must be non-empty"
    );
    assert_eq!(
        gaps.as_mapping_get("maxItems").and_then(Yaml::as_integer),
        Some(DELIVERY_REPORT_MAX_GAPS as i64),
        "AsyncAPI DeliveryReport.gaps maxItems must match DELIVERY_REPORT_MAX_GAPS"
    );
}

#[test]
fn spec_models_disjoint_v2_and_v3_physical_binary_envelopes() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    assert!(
        mapping_path(root, &["components", "schemas", "GameDataBinary"]).is_none(),
        "physical GameDataBinary must not retain an unreferenced JSON-envelope schema"
    );
    let message = mapping_path(root, &["components", "messages", "GameDataBinary"])
        .expect("spec must define the physical GameDataBinary message");
    let alternatives = mapping_path(message, &["payload", "oneOf"])
        .and_then(Yaml::as_sequence)
        .expect("physical GameDataBinary payload must use oneOf");
    let references: Vec<_> = alternatives
        .iter()
        .map(|alternative| {
            alternative
                .as_mapping_get("$ref")
                .and_then(Yaml::as_str)
                .expect("physical binary alternative must be a schema reference")
        })
        .collect();
    assert_eq!(
        references,
        [
            "#/components/schemas/V2BinaryGameDataEnvelope",
            "#/components/schemas/V3BinaryGameDataEnvelope",
        ]
    );
    assert_eq!(
        message
            .as_mapping_get("x-rust-server-variant")
            .and_then(Yaml::as_str),
        Some("GameDataBinary")
    );

    let binary_id = mapping_path(root, &["components", "schemas", "BinaryPlayerId"])
        .expect("spec must define the binary UUID representation");
    for constraint in ["minLength", "maxLength"] {
        assert_eq!(
            binary_id
                .as_mapping_get(constraint)
                .and_then(Yaml::as_integer),
            Some(16),
            "BinaryPlayerId {constraint} must be 16 bytes"
        );
    }
    assert_eq!(
        binary_id
            .as_mapping_get("x-messagepack-type")
            .and_then(Yaml::as_str),
        Some("bin")
    );

    for (name, version, required) in [
        (
            "V2BinaryGameDataEnvelope",
            2,
            &["from_player", "encoding", "payload"][..],
        ),
        (
            "V3BinaryGameDataEnvelope",
            3,
            &["from_player", "encoding", "payload", "seq", "epoch"][..],
        ),
    ] {
        let envelope = mapping_path(root, &["components", "schemas", name])
            .unwrap_or_else(|| panic!("spec must define {name}"));
        assert_eq!(
            envelope
                .as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{name} must reject fields from the other physical version"
        );
        assert_eq!(
            envelope
                .as_mapping_get("x-protocol-version")
                .and_then(Yaml::as_integer),
            Some(version)
        );
        let actual_required: Vec<_> = envelope
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .expect("binary envelope must list required fields")
            .iter()
            .map(|field| field.as_str().expect("required field must be a string"))
            .collect();
        assert_eq!(actual_required, required);
        assert_eq!(
            mapping_path(envelope, &["properties", "from_player", "$ref"]).and_then(Yaml::as_str),
            Some("#/components/schemas/BinaryPlayerId")
        );
    }

    let v2 = mapping_path(root, &["components", "schemas", "V2BinaryGameDataEnvelope"])
        .expect("spec must define V2BinaryGameDataEnvelope");
    assert_eq!(
        mapping_path(v2, &["properties", "encoding", "const"]).and_then(Yaml::as_str),
        Some("message_pack")
    );
    let v3 = mapping_path(root, &["components", "schemas", "V3BinaryGameDataEnvelope"])
        .expect("spec must define V3BinaryGameDataEnvelope");
    assert_eq!(
        mapping_path(v3, &["properties", "epoch", "maximum"]).and_then(Yaml::as_integer),
        Some(u32::MAX.into())
    );
}

#[test]
fn spec_relay_stats_bounds_match_client_validation() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let fields = mapping_path(
        root,
        &[
            "components",
            "schemas",
            "RelayStats",
            "properties",
            "data",
            "properties",
        ],
    )
    .expect("spec must define RelayStats.data properties");
    for (field, minimum) in [
        ("interval_ms", 1),
        ("sent_to_you", 0),
        ("dropped_for_you", 0),
        ("backpressure_events", 0),
    ] {
        assert_eq!(
            mapping_path(fields, &[field, "minimum"]).and_then(Yaml::as_integer),
            Some(minimum),
            "RelayStats.{field} minimum drifted from client validation"
        );
    }
}

#[test]
fn spec_player_snapshots_have_disjoint_versioned_relay_baselines() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let player_info = mapping_path(root, &["components", "schemas", "PlayerInfo"])
        .expect("spec must define PlayerInfo");
    let refs: Vec<_> = player_info
        .as_mapping_get("oneOf")
        .and_then(Yaml::as_sequence)
        .expect("PlayerInfo must distinguish protocol versions")
        .iter()
        .map(|branch| {
            branch
                .as_mapping_get("$ref")
                .and_then(Yaml::as_str)
                .expect("PlayerInfo branches must be local refs")
        })
        .collect();
    assert_eq!(
        refs,
        [
            "#/components/schemas/V2PlayerInfo",
            "#/components/schemas/V3PlayerInfo"
        ]
    );

    for (name, version_fields) in [
        ("V2PlayerInfo", &[][..]),
        ("V3PlayerInfo", &["epoch", "seq"][..]),
    ] {
        let schema = mapping_path(root, &["components", "schemas", name])
            .unwrap_or_else(|| panic!("spec must define {name}"));
        assert_eq!(
            schema
                .as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{name} must not accept the other version's fields"
        );
        let required: BTreeSet<_> = schema
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .expect("PlayerInfo variant must list required fields")
            .iter()
            .map(|field| field.as_str().expect("required field must be a string"))
            .collect();
        for field in ["epoch", "seq"] {
            assert_eq!(
                required.contains(field),
                version_fields.contains(&field),
                "{name}.{field} requiredness drifted"
            );
            assert_eq!(
                mapping_path(schema, &["properties", field]).is_some(),
                version_fields.contains(&field),
                "{name}.{field} property shape drifted"
            );
        }
    }

    let v3 = mapping_path(root, &["components", "schemas", "V3PlayerInfo"])
        .expect("spec must define V3PlayerInfo");
    assert_eq!(
        mapping_path(v3, &["properties", "epoch", "minimum"]).and_then(Yaml::as_integer),
        Some(1)
    );
    assert_eq!(
        mapping_path(v3, &["properties", "seq", "minimum"]).and_then(Yaml::as_integer),
        Some(0)
    );
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
/// `GameDataEncoding` / `DeliveryClass` / `DeliveryGapReason` /
/// `RelayTransport` / `SpectatorStateChangeReason` / `LobbyState` /
/// `ReplayStatus` value, or a new internally-tagged
/// `ConnectionInfo` `type` discriminator, can no longer ship undocumented.
#[test]
fn spec_documents_every_wire_token_enum_variant() {
    use signal_fish_server::protocol::{
        ConnectionInfo, DeliveryClass, DeliveryGapReason, GameDataEncoding, LobbyState,
        RelayTransport, ReplayStatus, SpectatorStateChangeReason, Topology, Transport,
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
        use DeliveryClass::*;
        for value in [Reliable, Latest, Volatile] {
            assert_wire_token(&declared, value, "DeliveryClass");
        }
        let _exhaustive = |value: DeliveryClass| match value {
            Reliable | Latest | Volatile => {}
        };
    }
    {
        use DeliveryGapReason::*;
        for value in [
            LatestSuperseded,
            LatestDroppedFull,
            VolatileDropped,
            UnsupportedFormat,
        ] {
            assert_wire_token(&declared, value, "DeliveryGapReason");
        }
        let _exhaustive = |value: DeliveryGapReason| match value {
            LatestSuperseded | LatestDroppedFull | VolatileDropped | UnsupportedFormat => {}
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
