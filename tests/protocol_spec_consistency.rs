//! Drift guard tying the machine-readable protocol spec to the Rust source.
//!
//! `spec/signal-fish-protocol.asyncapi.yaml` is the codegen-facing, source-of-
//! truth description of the Signal Fish WebSocket protocol (see the header
//! comment in that file). Client-library authors generate models from it, so it
//! MUST stay in lockstep with the real Rust message enums. This test mirrors the
//! parsing technique in `tests/docs_site_consistency.rs`: it extracts every
//! variant of `ClientMessage` / `ServerMessage` (from `src/protocol/messages.rs`)
//! and every `ErrorCode` variant (from `src/protocol/error_codes.rs`) directly
//! from source — no hand-kept emitted-code list — and asserts the spec contains
//! exactly the emitted variants after subtracting an explicit compatibility-only
//! reserve. A companion source sweep prevents those reserved variants from
//! being referenced by production emitter paths.
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
use std::fs;
use std::path::Path;

use common::{read_file, repo_root, strip_comment_lines};
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

/// Validate the JSON value-shape keywords used by the protocol's versioned
/// envelope schemas. This deliberately covers the object-union contract under
/// test (`$ref`, `oneOf`/`anyOf`/`allOf`, `not`, `const`, `type`, `required`,
/// `properties`, `additionalProperties`, and array `items`) rather than
/// pretending to be a general-purpose JSON Schema implementation.
fn schema_shape_matches(root: &Yaml, schema: &Yaml, value: &serde_json::Value) -> bool {
    if let Some(reference) = schema.as_mapping_get("$ref").and_then(Yaml::as_str) {
        return resolve_local_reference(root, reference)
            .is_some_and(|resolved| schema_shape_matches(root, resolved, value));
    }

    if value.is_null() && schema.as_mapping_get("nullable").and_then(Yaml::as_bool) == Some(true) {
        return true;
    }

    if let Some(branches) = schema.as_mapping_get("oneOf").and_then(Yaml::as_sequence) {
        if branches
            .iter()
            .filter(|branch| schema_shape_matches(root, branch, value))
            .count()
            != 1
        {
            return false;
        }
    }
    if let Some(branches) = schema.as_mapping_get("anyOf").and_then(Yaml::as_sequence) {
        if !branches
            .iter()
            .any(|branch| schema_shape_matches(root, branch, value))
        {
            return false;
        }
    }
    if let Some(branches) = schema.as_mapping_get("allOf").and_then(Yaml::as_sequence) {
        if !branches
            .iter()
            .all(|branch| schema_shape_matches(root, branch, value))
        {
            return false;
        }
    }
    if let Some(forbidden) = schema.as_mapping_get("not") {
        if schema_shape_matches(root, forbidden, value) {
            return false;
        }
    }
    if let Some(condition) = schema.as_mapping_get("if") {
        let consequence = if schema_shape_matches(root, condition, value) {
            schema.as_mapping_get("then")
        } else {
            schema.as_mapping_get("else")
        };
        if consequence.is_some_and(|branch| !schema_shape_matches(root, branch, value)) {
            return false;
        }
    }

    if let Some(expected) = schema.as_mapping_get("const") {
        let matches = expected
            .as_str()
            .is_some_and(|expected| value.as_str() == Some(expected))
            || expected
                .as_integer()
                .is_some_and(|expected| value.as_i64() == Some(expected))
            || expected
                .as_bool()
                .is_some_and(|expected| value.as_bool() == Some(expected));
        if !matches {
            return false;
        }
    }
    if let Some(allowed) = schema.as_mapping_get("enum").and_then(Yaml::as_sequence) {
        let matches = allowed.iter().any(|expected| {
            expected
                .as_str()
                .is_some_and(|expected| value.as_str() == Some(expected))
                || expected
                    .as_integer()
                    .is_some_and(|expected| value.as_i64() == Some(expected))
                || expected
                    .as_bool()
                    .is_some_and(|expected| value.as_bool() == Some(expected))
        });
        if !matches {
            return false;
        }
    }

    let schema_type = schema.as_mapping_get("type").and_then(Yaml::as_str);
    let has_object_keywords = schema.as_mapping_get("required").is_some()
        || schema.as_mapping_get("properties").is_some()
        || schema.as_mapping_get("additionalProperties").is_some();
    if schema_type == Some("object") || has_object_keywords {
        let Some(object) = value.as_object() else {
            return false;
        };
        let required: BTreeSet<_> = schema
            .as_mapping_get("required")
            .and_then(Yaml::as_sequence)
            .into_iter()
            .flatten()
            .filter_map(Yaml::as_str)
            .collect();
        if required.iter().any(|field| !object.contains_key(*field)) {
            return false;
        }

        let properties = schema
            .as_mapping_get("properties")
            .and_then(Yaml::as_mapping);
        if schema
            .as_mapping_get("additionalProperties")
            .and_then(Yaml::as_bool)
            == Some(false)
            && object.keys().any(|field| {
                properties.is_none_or(|properties| {
                    !properties
                        .keys()
                        .filter_map(Yaml::as_str)
                        .any(|property| property == field)
                })
            })
        {
            return false;
        }

        if let Some(properties) = properties {
            for (key, property_schema) in properties.iter() {
                if let Some((_, field_value)) =
                    key.as_str().and_then(|key| object.get_key_value(key))
                {
                    if !schema_shape_matches(root, property_schema, field_value) {
                        return false;
                    }
                }
            }
        }
    } else if schema_type == Some("array") || schema.as_mapping_get("items").is_some() {
        let Some(items) = value.as_array() else {
            return false;
        };
        if schema
            .as_mapping_get("minItems")
            .and_then(Yaml::as_integer)
            .is_some_and(|minimum| items.len() < minimum as usize)
            || schema
                .as_mapping_get("maxItems")
                .and_then(Yaml::as_integer)
                .is_some_and(|maximum| items.len() > maximum as usize)
        {
            return false;
        }
        if let Some(item_schema) = schema.as_mapping_get("items") {
            if !items
                .iter()
                .all(|item| schema_shape_matches(root, item_schema, item))
            {
                return false;
            }
        }
    } else {
        match schema_type {
            Some("string") if !value.is_string() => return false,
            Some("integer") if !value.is_i64() && !value.is_u64() => return false,
            Some("boolean") if !value.is_boolean() => return false,
            Some("number") if !value.is_number() => return false,
            _ => {}
        }
    }

    true
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
fn accountability_component_objects_reject_unknown_fields() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    for path in [
        &["components", "schemas", "ReliableDeliveryCounters"][..],
        &["components", "schemas", "LatestDeliveryCounters"],
        &["components", "schemas", "VolatileDeliveryCounters"],
        &["components", "schemas", "DeliveryCountersByClass"],
        &["components", "schemas", "DeliveryGap"],
        &["components", "schemas", "SenderWatermark"],
        &["components", "schemas", "RelayStats"],
        &["components", "schemas", "RelayStats", "properties", "data"],
        &["components", "schemas", "DeliveryReport"],
        &[
            "components",
            "schemas",
            "DeliveryReport",
            "properties",
            "data",
        ],
    ] {
        let schema = mapping_path(root, path)
            .unwrap_or_else(|| panic!("missing accountability schema at {}", path.join(".")));
        assert_eq!(
            schema
                .as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{} must reject unknown fields",
            path.join(".")
        );
    }
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
fn accountability_message_envelopes_use_closed_versioned_branches() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    let envelopes = [
        ("RoomJoined", &["V2RoomJoined", "V3RoomJoined"][..]),
        ("PlayerJoined", &["V2PlayerJoined", "V3PlayerJoined"][..]),
        ("PlayerLeft", &["V2PlayerLeft", "V3PlayerLeft"][..]),
        (
            "ServerGameData",
            &["V2ServerGameData", "V3ServerGameData"][..],
        ),
        ("Reconnected", &["V2Reconnected", "V3Reconnected"][..]),
        (
            "PlayerReconnected",
            &["V2PlayerReconnected", "V3PlayerReconnected"][..],
        ),
        (
            "SpectatorJoined",
            &[
                "V2SpectatorJoined",
                "V3SpectatorJoined",
                "EmptySpectatorJoined",
            ][..],
        ),
    ];

    for (public_name, branch_names) in envelopes {
        let public_schema = mapping_path(root, &["components", "schemas", public_name])
            .unwrap_or_else(|| panic!("spec must define {public_name}"));
        let references: Vec<_> = public_schema
            .as_mapping_get("oneOf")
            .and_then(Yaml::as_sequence)
            .unwrap_or_else(|| panic!("{public_name} must be an exact versioned oneOf"))
            .iter()
            .map(|branch| {
                branch
                    .as_mapping_get("$ref")
                    .and_then(Yaml::as_str)
                    .unwrap_or_else(|| panic!("{public_name} branches must be local references"))
            })
            .collect();
        assert_eq!(
            references,
            branch_names
                .iter()
                .map(|name| format!("#/components/schemas/{name}"))
                .collect::<Vec<_>>(),
            "{public_name} branch order or version coverage drifted"
        );

        for branch_name in branch_names {
            let branch = mapping_path(root, &["components", "schemas", branch_name])
                .unwrap_or_else(|| panic!("spec must define {branch_name}"));
            assert_eq!(
                branch
                    .as_mapping_get("additionalProperties")
                    .and_then(Yaml::as_bool),
                Some(false),
                "{branch_name} must reject unknown outer-envelope fields"
            );
            let required: BTreeSet<_> = branch
                .as_mapping_get("required")
                .and_then(Yaml::as_sequence)
                .unwrap_or_else(|| panic!("{branch_name} must list required envelope fields"))
                .iter()
                .map(|field| field.as_str().expect("required field must be a string"))
                .collect();
            assert_eq!(required, BTreeSet::from(["type", "data"]));

            let data = mapping_path(branch, &["properties", "data"])
                .unwrap_or_else(|| panic!("{branch_name} must define data"));
            assert_eq!(
                data.as_mapping_get("additionalProperties")
                    .and_then(Yaml::as_bool),
                Some(false),
                "{branch_name}.data must reject unknown and cross-version fields"
            );
        }
    }
}

#[test]
fn reconnect_replay_uses_exact_versioned_control_message_unions() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    for (version, versioned) in [
        (
            "V2",
            ["V2PlayerJoined", "V2PlayerLeft", "V2PlayerReconnected"],
        ),
        (
            "V3",
            ["V3PlayerJoined", "V3PlayerLeft", "V3PlayerReconnected"],
        ),
    ] {
        let union_name = format!("{version}ReplayableServerMessageEnvelope");
        let union = mapping_path(root, &["components", "schemas", &union_name])
            .unwrap_or_else(|| panic!("spec must define {union_name}"));
        let references: Vec<_> = union
            .as_mapping_get("oneOf")
            .and_then(Yaml::as_sequence)
            .unwrap_or_else(|| panic!("{union_name} must be an exact oneOf"))
            .iter()
            .map(|branch| {
                branch
                    .as_mapping_get("$ref")
                    .and_then(Yaml::as_str)
                    .unwrap_or_else(|| panic!("{union_name} branches must be references"))
            })
            .collect();
        let expected: Vec<_> = versioned
            .iter()
            .chain(
                [
                    "NewSpectatorJoined",
                    "SpectatorDisconnected",
                    "LobbyStateChanged",
                    "AuthorityChanged",
                ]
                .iter(),
            )
            .map(|name| format!("#/components/schemas/{name}"))
            .collect();
        assert_eq!(references, expected, "{union_name} replay coverage drifted");
    }

    for shared in [
        "NewSpectatorJoined",
        "SpectatorDisconnected",
        "LobbyStateChanged",
        "AuthorityChanged",
    ] {
        let envelope = mapping_path(root, &["components", "schemas", shared])
            .unwrap_or_else(|| panic!("spec must define {shared}"));
        assert_eq!(
            envelope
                .as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{shared} must be closed before it enters the replay union"
        );
        assert_eq!(
            mapping_path(envelope, &["properties", "data", "additionalProperties"])
                .and_then(Yaml::as_bool),
            Some(false),
            "{shared}.data must be closed before it enters the replay union"
        );
    }
}

#[test]
fn accountability_message_schemas_accept_rust_wire_shapes_and_reject_hybrids() {
    use signal_fish_server::protocol::{
        LobbyState, PlayerInfo, ReconnectedPayload, ReplayStatus, RoomJoinedPayload,
        SenderWatermark, ServerMessage, SpectatorJoinedPayload,
    };
    use uuid::Uuid;

    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");
    let player_id = Uuid::from_u128(1);
    let room_id = Uuid::from_u128(2);
    let connected_at = chrono::DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
        .expect("valid timestamp fixture")
        .with_timezone(&chrono::Utc);
    let player = |accountable: bool| PlayerInfo {
        id: player_id,
        name: "Alice".to_string(),
        is_authority: true,
        is_ready: false,
        connected_at,
        connection_info: None,
        epoch: accountable.then_some(3),
        seq: accountable.then_some(7),
        region_id: String::new(),
    };
    let room_joined = |accountable: bool| {
        ServerMessage::RoomJoined(Box::new(RoomJoinedPayload {
            room_id,
            room_code: "ABC123".to_string(),
            player_id,
            game_name: "game".to_string(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![player(accountable)],
            is_authority: true,
            lobby_state: LobbyState::Waiting,
            ready_players: Vec::new(),
            relay_type: "WebSocket".to_string(),
            current_spectators: Vec::new(),
            ice_servers: Vec::new(),
            reconnection_token: accountable.then(|| "join-token".to_string()),
        }))
    };
    let reconnected = |accountable: bool| {
        ServerMessage::Reconnected(Box::new(ReconnectedPayload {
            room_id,
            room_code: "ABC123".to_string(),
            player_id,
            game_name: "game".to_string(),
            max_players: 4,
            supports_authority: true,
            current_players: vec![player(accountable)],
            is_authority: true,
            lobby_state: LobbyState::Waiting,
            ready_players: Vec::new(),
            relay_type: "WebSocket".to_string(),
            current_spectators: Vec::new(),
            ice_servers: Vec::new(),
            missed_events: vec![ServerMessage::PlayerLeft {
                player_id: Uuid::from_u128(4),
                epoch: accountable.then_some(2),
                final_seq: accountable.then_some(5),
            }],
            replay: accountable.then_some(ReplayStatus::Complete),
            sender_watermarks: accountable
                .then_some(vec![SenderWatermark {
                    player_id,
                    epoch: 3,
                    seq: 7,
                }])
                .unwrap_or_default(),
            reconnection_token: accountable.then(|| "next-token".to_string()),
        }))
    };
    let spectator_joined = |accountable: bool| {
        ServerMessage::SpectatorJoined(Box::new(SpectatorJoinedPayload {
            room_id,
            room_code: "ABC123".to_string(),
            spectator_id: Uuid::from_u128(3),
            game_name: "game".to_string(),
            current_players: vec![player(accountable)],
            current_spectators: Vec::new(),
            lobby_state: LobbyState::Waiting,
            reason: None,
        }))
    };

    let mut empty_spectator =
        serde_json::to_value(spectator_joined(false)).expect("serialize SpectatorJoined");
    empty_spectator["data"]["current_players"] = serde_json::json!([]);

    let valid_cases = [
        ("RoomJoined", serde_json::to_value(room_joined(false))),
        ("RoomJoined", serde_json::to_value(room_joined(true))),
        (
            "PlayerJoined",
            serde_json::to_value(ServerMessage::PlayerJoined {
                player: player(false),
            }),
        ),
        (
            "PlayerJoined",
            serde_json::to_value(ServerMessage::PlayerJoined {
                player: player(true),
            }),
        ),
        (
            "PlayerLeft",
            serde_json::to_value(ServerMessage::PlayerLeft {
                player_id,
                epoch: None,
                final_seq: None,
            }),
        ),
        (
            "PlayerLeft",
            serde_json::to_value(ServerMessage::PlayerLeft {
                player_id,
                epoch: Some(3),
                final_seq: Some(7),
            }),
        ),
        (
            "ServerGameData",
            serde_json::to_value(ServerMessage::GameData {
                from_player: player_id,
                data: serde_json::json!({"move": "up"}),
                seq: None,
                epoch: None,
                class: None,
                key: None,
            }),
        ),
        (
            "ServerGameData",
            serde_json::to_value(ServerMessage::GameData {
                from_player: player_id,
                data: serde_json::json!({"state": "fresh"}),
                seq: Some(8),
                epoch: Some(3),
                class: Some(signal_fish_server::protocol::DeliveryClass::Latest),
                key: Some(9),
            }),
        ),
        ("Reconnected", serde_json::to_value(reconnected(false))),
        ("Reconnected", serde_json::to_value(reconnected(true))),
        (
            "PlayerReconnected",
            serde_json::to_value(ServerMessage::PlayerReconnected {
                player_id,
                epoch: None,
            }),
        ),
        (
            "PlayerReconnected",
            serde_json::to_value(ServerMessage::PlayerReconnected {
                player_id,
                epoch: Some(3),
            }),
        ),
        (
            "SpectatorJoined",
            serde_json::to_value(spectator_joined(false)),
        ),
        (
            "SpectatorJoined",
            serde_json::to_value(spectator_joined(true)),
        ),
        ("SpectatorJoined", Ok(empty_spectator)),
    ];

    for (schema_name, serialized) in valid_cases {
        let value = serialized.unwrap_or_else(|error| {
            panic!("failed to serialize representative {schema_name}: {error}")
        });
        let schema = mapping_path(root, &["components", "schemas", schema_name])
            .unwrap_or_else(|| panic!("spec must define {schema_name}"));
        assert!(
            schema_shape_matches(root, schema, &value),
            "{schema_name} rejected an actual Rust wire shape: {value}"
        );
    }

    let mut invalid_cases = Vec::new();

    let mut extra_outer = serde_json::to_value(room_joined(false)).expect("serialize RoomJoined");
    extra_outer["unexpected"] = serde_json::json!(true);
    invalid_cases.push(("RoomJoined", "unknown outer field", extra_outer));

    let mut mixed_room = serde_json::to_value(room_joined(false)).expect("serialize RoomJoined");
    mixed_room["data"]["current_players"] = serde_json::json!([player(false), player(true)]);
    invalid_cases.push(("RoomJoined", "mixed snapshot versions", mixed_room));

    let mut mixed_player = serde_json::to_value(ServerMessage::PlayerJoined {
        player: player(false),
    })
    .expect("serialize PlayerJoined");
    mixed_player["data"]["player"]["epoch"] = serde_json::json!(3);
    invalid_cases.push(("PlayerJoined", "unpaired player epoch", mixed_player));

    let mut partial_left = serde_json::to_value(ServerMessage::PlayerLeft {
        player_id,
        epoch: None,
        final_seq: None,
    })
    .expect("serialize PlayerLeft");
    partial_left["data"]["epoch"] = serde_json::json!(3);
    invalid_cases.push(("PlayerLeft", "unpaired terminal epoch", partial_left));

    let mut partial_data = serde_json::to_value(ServerMessage::GameData {
        from_player: player_id,
        data: serde_json::json!(null),
        seq: None,
        epoch: None,
        class: None,
        key: None,
    })
    .expect("serialize GameData");
    partial_data["data"]["seq"] = serde_json::json!(1);
    invalid_cases.push(("ServerGameData", "unpaired sequence", partial_data));

    let mut class_without_stamp = serde_json::to_value(ServerMessage::GameData {
        from_player: player_id,
        data: serde_json::json!(null),
        seq: None,
        epoch: None,
        class: None,
        key: None,
    })
    .expect("serialize GameData");
    class_without_stamp["data"]["class"] = serde_json::json!("volatile");
    invalid_cases.push((
        "ServerGameData",
        "v3 class on a v2 envelope",
        class_without_stamp,
    ));

    let mut reconnect_hybrid =
        serde_json::to_value(reconnected(false)).expect("serialize Reconnected");
    reconnect_hybrid["data"]["replay"] = serde_json::json!("complete");
    invalid_cases.push((
        "Reconnected",
        "partial v3 reconnect state",
        reconnect_hybrid,
    ));

    let mut mixed_spectator =
        serde_json::to_value(spectator_joined(false)).expect("serialize SpectatorJoined");
    mixed_spectator["data"]["current_players"] = serde_json::json!([player(false), player(true)]);
    invalid_cases.push((
        "SpectatorJoined",
        "mixed snapshot versions",
        mixed_spectator,
    ));

    for (schema_name, case, value) in invalid_cases {
        let schema = mapping_path(root, &["components", "schemas", schema_name])
            .unwrap_or_else(|| panic!("spec must define {schema_name}"));
        assert!(
            !schema_shape_matches(root, schema, &value),
            "{schema_name} accepted {case}: {value}"
        );
    }
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
fn spec_documents_exactly_the_emitted_error_codes() {
    use signal_fish_server::protocol::ErrorCode;

    let source = read_file(&repo_root().join("src/protocol/error_codes.rs"));
    let variants = enum_variants(&source, "ErrorCode");
    assert!(
        variants.len() >= 40,
        "expected to parse the full ErrorCode enum, only found {} variants: {variants:?}",
        variants.len()
    );

    let rust_codes: BTreeSet<String> = variants.iter().map(|v| to_screaming_snake(v)).collect();
    let reserved: BTreeSet<String> = ErrorCode::NON_EMITTED
        .iter()
        .map(|code| {
            serde_json::to_value(code)
                .expect("non-emitted ErrorCode must serialize")
                .as_str()
                .expect("ErrorCode wire value must be a string")
                .to_string()
        })
        .collect();
    assert!(
        reserved.is_subset(&rust_codes),
        "reserved compatibility codes must remain decodable Rust variants"
    );

    let expected: BTreeSet<_> = rust_codes.difference(&reserved).cloned().collect();
    let docs = Yaml::load_from_str(&spec_text()).expect("protocol spec must be valid YAML");
    let root = docs
        .first()
        .expect("protocol spec must contain one YAML document");
    let declared: BTreeSet<String> =
        mapping_path(root, &["components", "schemas", "ErrorCode", "enum"])
            .and_then(Yaml::as_sequence)
            .expect("components.schemas.ErrorCode.enum must be a sequence")
            .iter()
            .map(|item| {
                item.as_str()
                    .expect("ErrorCode enum members must be strings")
                    .to_string()
            })
            .collect();

    let missing: Vec<_> = expected.difference(&declared).cloned().collect();
    let unexpected: Vec<_> = declared.difference(&expected).cloned().collect();

    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "spec ErrorCode must equal the server-emitted Rust set; missing={missing:?}, \
         unexpected={unexpected:?} (reserved non-emitted={reserved:?})"
    );

    // The game-start codes must remain part of the emitted contract.
    for token in ["GAME_START_NOT_READY", "GAME_START_FORBIDDEN"] {
        assert!(
            declared.contains(token),
            "spec must list error code {token}"
        );
    }
}

#[test]
fn non_emitted_error_codes_have_only_pinned_production_references() {
    use signal_fish_server::protocol::ErrorCode;

    fn visit(dir: &Path, variants: &[String], references: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("production source directory must be readable") {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                visit(&path, variants, references);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                && !path.ends_with("protocol/error_codes.rs")
            {
                let source = strip_comment_lines(&read_file(&path));
                for variant in variants {
                    for _ in source.match_indices(variant) {
                        references.push(format!("{} references {variant}", path.display()));
                    }
                }
            }
        }
    }

    let mut variants: Vec<String> = ErrorCode::NON_EMITTED
        .iter()
        .map(|code| format!("{code:?}"))
        .collect();
    variants.push("NON_EMITTED".to_string());
    let mut references = Vec::new();
    visit(&repo_root().join("src"), &variants, &mut references);

    let connection_path = repo_root().join("src/websocket/connection.rs");
    let auth_error_path = repo_root().join("src/auth/error.rs");
    let mut allowed = Vec::new();
    for code in ErrorCode::NON_EMITTED {
        let variant = format!("{code:?}");
        if variant.starts_with("AppId") {
            allowed.extend(
                std::iter::repeat_n(
                    format!("{} references {variant}", auth_error_path.display()),
                    1,
                )
                .chain(std::iter::repeat_n(
                    format!("{} references {variant}", connection_path.display()),
                    2,
                )),
            );
        }
    }
    references.sort();
    allowed.sort();
    assert_eq!(
        references, allowed,
        "non-emitted variants may appear only in the pinned future app-status definitions/mapping; \
         any other production reference requires reclassifying and documenting the emitted contract"
    );
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
