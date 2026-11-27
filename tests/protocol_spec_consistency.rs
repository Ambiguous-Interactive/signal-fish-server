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
mod test_helpers;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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
    schema_shape_matches_version(root, schema, value, None)
}

/// Apply version metadata at every visited schema node. Shared branches stay
/// eligible. This is a schema-shape check, not a runtime capability gate.
fn schema_shape_matches_version(
    root: &Yaml,
    schema: &Yaml,
    value: &serde_json::Value,
    version: Option<i64>,
) -> bool {
    if version.is_some_and(|version| {
        schema
            .as_mapping_get("x-protocol-version")
            .and_then(Yaml::as_integer)
            .is_some_and(|advertised| advertised != version)
    }) {
        return false;
    }
    if let Some(reference) = schema.as_mapping_get("$ref").and_then(Yaml::as_str) {
        return resolve_local_reference(root, reference)
            .is_some_and(|resolved| schema_shape_matches_version(root, resolved, value, version));
    }

    if let Some(branches) = schema.as_mapping_get("oneOf").and_then(Yaml::as_sequence) {
        if branches
            .iter()
            .filter(|branch| schema_shape_matches_version(root, branch, value, version))
            .count()
            != 1
        {
            return false;
        }
    }
    if let Some(branches) = schema.as_mapping_get("anyOf").and_then(Yaml::as_sequence) {
        if !branches
            .iter()
            .any(|branch| schema_shape_matches_version(root, branch, value, version))
        {
            return false;
        }
    }
    if let Some(branches) = schema.as_mapping_get("allOf").and_then(Yaml::as_sequence) {
        if !branches
            .iter()
            .all(|branch| schema_shape_matches_version(root, branch, value, version))
        {
            return false;
        }
    }
    if let Some(forbidden) = schema.as_mapping_get("not") {
        if schema_shape_matches_version(root, forbidden, value, version) {
            return false;
        }
    }
    if let Some(condition) = schema.as_mapping_get("if") {
        let consequence = if schema_shape_matches_version(root, condition, value, version) {
            schema.as_mapping_get("then")
        } else {
            schema.as_mapping_get("else")
        };
        if consequence
            .is_some_and(|branch| !schema_shape_matches_version(root, branch, value, version))
        {
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
    if let Some(types) = schema.as_mapping_get("type").and_then(Yaml::as_sequence) {
        let matches = types.iter().any(|kind| match kind.as_str() {
            Some("null") => value.is_null(),
            Some("string") => value.is_string(),
            Some("integer") => value.is_i64() || value.is_u64(),
            Some("boolean") => value.is_boolean(),
            Some("number") => value.is_number(),
            Some("object") => value.is_object(),
            Some("array") => value.is_array(),
            other => panic!("unsupported schema type in union: {other:?}"),
        });
        if !matches {
            return false;
        }
    }
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
                    if !schema_shape_matches_version(root, property_schema, field_value, version) {
                        return false;
                    }
                }
            }
        }
    } else if schema_type == Some("array") || schema.as_mapping_get("items").is_some() {
        let Some(items) = value.as_array() else {
            return false;
        };
        if let Some(minimum) = schema.as_mapping_get("minItems").and_then(Yaml::as_integer) {
            let Ok(minimum) = usize::try_from(minimum) else {
                return false;
            };
            if items.len() < minimum {
                return false;
            }
        }
        if let Some(maximum) = schema.as_mapping_get("maxItems").and_then(Yaml::as_integer) {
            let Ok(maximum) = usize::try_from(maximum) else {
                return false;
            };
            if items.len() > maximum {
                return false;
            }
        }
        if let Some(item_schema) = schema.as_mapping_get("items") {
            if !items
                .iter()
                .all(|item| schema_shape_matches_version(root, item_schema, item, version))
            {
                return false;
            }
        }
    } else {
        match schema_type {
            Some("null") if !value.is_null() => return false,
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
fn spec_models_both_negotiated_token_binding_client_envelopes() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    let signed_json = mapping_path(root, &["components", "schemas", "ClientTokenBoundJson"])
        .expect("spec must define the signed JSON client envelope");
    let mut json_references = Vec::new();
    collect_local_references(signed_json, &mut json_references);
    assert!(
        json_references.contains(&"#/components/schemas/ClientMessageEnvelope".to_string())
            && json_references.contains(&"#/components/schemas/TokenBindingProof".to_string()),
        "signed JSON must compose every ordinary client message with TokenBindingProof"
    );
    assert_eq!(
        signed_json
            .as_mapping_get("x-json-number-contract")
            .and_then(Yaml::as_str),
        Some("safe-integers-only"),
        "signed JSON must expose its recursive portable-number restriction to code generators"
    );

    let signed_binary = mapping_path(root, &["components", "schemas", "ClientTokenBoundBinary"])
        .expect("spec must define the signed binary client envelope");
    let mut binary_references = Vec::new();
    collect_local_references(signed_binary, &mut binary_references);
    assert!(
        binary_references.contains(&"#/components/schemas/TokenBindingProof".to_string()),
        "signed binary must carry TokenBindingProof"
    );

    for message in ["ClientTokenBoundJson", "ClientTokenBoundBinary"] {
        let protocol = mapping_path(
            root,
            &["components", "messages", message, "x-websocket-subprotocol"],
        )
        .and_then(Yaml::as_str);
        assert_eq!(protocol, Some("signalfish.tokenbinding.v2"));
    }
}

#[test]
fn optional_wire_fields_use_draft7_null_types_instead_of_legacy_nullable() {
    use serde_json::{json, Value};
    use signal_fish_server::protocol::ServerMessage;
    use signal_fish_server::security::TokenBindingProof;

    let docs = Yaml::load_from_str(&spec_text()).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");

    let proof_schema = mapping_path(root, &["components", "schemas", "TokenBindingProof"])
        .expect("token binding proof schema");
    let fingerprint = mapping_path(proof_schema, &["properties", "fingerprint"])
        .expect("token binding fingerprint property");
    assert_eq!(
        fingerprint
            .as_mapping_get("type")
            .and_then(Yaml::as_sequence)
            .map(|types| types.iter().map(Yaml::as_str).collect::<BTreeSet<_>>()),
        Some(BTreeSet::from([Some("string"), Some("null")])),
        "TokenBindingProof.fingerprint must declare string and null as Draft 7 types"
    );
    assert!(
        fingerprint.as_mapping_get("nullable").is_none(),
        "TokenBindingProof.fingerprint must not use legacy nullable"
    );

    let base = json!({
        "version": 2,
        "scheme": "server_nonce_hkdf_sha256",
        "sequence": 1,
        "signature": "AA=="
    });
    let omitted = base.clone();
    let mut explicit_null = base.clone();
    explicit_null["fingerprint"] = Value::Null;
    let mut wrong_type = base.clone();
    wrong_type["fingerprint"] = json!(17);

    assert!(serde_json::from_value::<TokenBindingProof>(omitted.clone())
        .expect("omitted fingerprint")
        .fingerprint
        .is_none());
    assert!(
        serde_json::from_value::<TokenBindingProof>(explicit_null.clone())
            .expect("explicit null fingerprint")
            .fingerprint
            .is_none()
    );
    assert!(
        serde_json::from_value::<TokenBindingProof>(wrong_type.clone()).is_err(),
        "Rust accepted a non-string fingerprint"
    );
    assert!(schema_shape_matches(root, proof_schema, &omitted));
    assert!(
        schema_shape_matches(root, proof_schema, &explicit_null),
        "Draft 7 schema rejected the null fingerprint the server accepts"
    );
    assert!(!schema_shape_matches(root, proof_schema, &wrong_type));

    let going_away_schema =
        mapping_path(root, &["components", "schemas", "GoingAway"]).expect("going away schema");
    let retry = mapping_path(
        going_away_schema,
        &["properties", "data", "properties", "retry_after_secs"],
    )
    .expect("going away retry hint");
    assert_eq!(
        retry.as_mapping_get("type").and_then(Yaml::as_str),
        Some("integer")
    );
    assert!(
        retry.as_mapping_get("nullable").is_none(),
        "GoingAway.retry_after_secs must not use legacy nullable"
    );

    let without_retry = serde_json::to_value(ServerMessage::GoingAway {
        deadline_ms: 1,
        retry_after_secs: None,
    })
    .expect("serialize going away without a retry hint");
    let with_retry = serde_json::to_value(ServerMessage::GoingAway {
        deadline_ms: 1,
        retry_after_secs: Some(30),
    })
    .expect("serialize going away with a retry hint");
    assert_eq!(without_retry["data"].get("retry_after_secs"), None);
    assert_eq!(with_retry["data"]["retry_after_secs"], json!(30));
    assert!(schema_shape_matches(
        root,
        going_away_schema,
        &without_retry
    ));
    assert!(schema_shape_matches(root, going_away_schema, &with_retry));
    let mut null_retry = with_retry.clone();
    null_retry["data"]["retry_after_secs"] = Value::Null;
    assert!(
        !schema_shape_matches(root, going_away_schema, &null_retry),
        "GoingAway.retry_after_secs must not claim a null value the server never emits"
    );

    fn collect_nullable_keys<'a>(
        node: &'a Yaml<'a>,
        path: &mut Vec<String>,
        found: &mut Vec<String>,
    ) {
        match node {
            Yaml::Mapping(mapping) => {
                for (key, value) in mapping.iter() {
                    let name = key.as_str().unwrap_or("<non-string>");
                    path.push(name.to_string());
                    if name == "nullable" {
                        found.push(path.join("."));
                    }
                    collect_nullable_keys(value, path, found);
                    path.pop();
                }
            }
            Yaml::Sequence(items) => {
                for (index, item) in items.iter().enumerate() {
                    path.push(format!("[{index}]"));
                    collect_nullable_keys(item, path, found);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    collect_nullable_keys(root, &mut Vec::new(), &mut found);
    assert!(
        found.is_empty(),
        "legacy nullable keywords remain in the protocol spec: {found:?}"
    );
}

#[test]
fn direct_session_plan_serialization_matches_the_executable_schema_branch() {
    use signal_fish_server::protocol::{
        AddressedBinaryPeer, DirectEndpoint, ServerMessage, SessionPlanPayload, Topology, Transport,
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
        assert!(!required.contains("addressed_binary_peers"));
        assert_eq!(
            mapping_path(branch, &["properties", "addressed_binary_peers", "$ref"])
                .and_then(Yaml::as_str),
            Some("#/components/schemas/AddressedBinaryPeers")
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
        addressed_binary_peers: None,
    }));
    let mut serialized = serde_json::to_value(message).expect("serialize Direct SessionPlan");
    assert_eq!(serialized["type"], "SessionPlan");
    assert_eq!(serialized["data"]["topology"], "host");
    assert_eq!(serialized["data"]["transport"], "direct");
    assert_eq!(serialized["data"]["host"], host.to_string());
    assert_eq!(serialized["data"]["direct_endpoint"]["host"], "192.0.2.10");
    assert_eq!(serialized["data"]["direct_endpoint"]["port"], 7777);
    assert!(serialized["data"].get("ice_servers").is_none());
    assert!(serialized["data"].get("addressed_binary_peers").is_none());
    assert!(schema_shape_matches(root, schema, &serialized["data"]));

    serialized["data"]["addressed_binary_peers"] = serde_json::to_value([AddressedBinaryPeer {
        player_id: Uuid::from_u128(11),
        epoch: 2,
    }])
    .expect("serialize addressed seat");
    assert!(schema_shape_matches(root, schema, &serialized["data"]));
    serialized["data"]["addressed_binary_peers"][0]
        .as_object_mut()
        .expect("addressed seat object")
        .remove("epoch");
    assert!(!schema_shape_matches(root, schema, &serialized["data"]));
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
            &["properties", "sdp", "type"],
        )
        .and_then(Yaml::as_sequence)
        .map(|types| types.iter().map(Yaml::as_str).collect::<BTreeSet<_>>()),
        Some(BTreeSet::from([Some("string"), Some("null")])),
        "WebRTC SDP must declare string and null as Draft 7 types"
    );
}

#[test]
fn legacy_relay_metadata_has_one_informational_untrusted_contract() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    let relay_label_description = mapping_path(
        root,
        &["components", "schemas", "RelayTypeLabel", "description"],
    )
    .and_then(Yaml::as_str)
    .expect("RelayTypeLabel must document its semantics");
    for phrase in ["Informational only", "does not select", "SessionPlan"] {
        assert!(
            relay_label_description.contains(phrase),
            "RelayTypeLabel must preserve the `{phrase}` trust-boundary wording"
        );
    }

    assert_eq!(
        mapping_path(
            root,
            &[
                "components",
                "schemas",
                "PeerConnectionInfo",
                "properties",
                "relay_type",
                "$ref",
            ],
        )
        .and_then(Yaml::as_str),
        Some("#/components/schemas/RelayTypeLabel")
    );
    for schema in [
        "V2RoomJoined",
        "V3RoomJoined",
        "V2Reconnected",
        "V3Reconnected",
    ] {
        assert_eq!(
            mapping_path(
                root,
                &[
                    "components",
                    "schemas",
                    schema,
                    "properties",
                    "data",
                    "properties",
                    "relay_type",
                    "$ref",
                ],
            )
            .and_then(Yaml::as_str),
            Some("#/components/schemas/RelayTypeLabel"),
            "{schema}.relay_type must reuse the shared informational label contract"
        );
    }

    let branches = mapping_path(root, &["components", "schemas", "ConnectionInfo", "oneOf"])
        .and_then(Yaml::as_sequence)
        .expect("ConnectionInfo must be an exact oneOf");
    let relay = branches
        .iter()
        .find(|branch| {
            mapping_path(branch, &["properties", "type", "const"]).and_then(Yaml::as_str)
                == Some("relay")
        })
        .expect("ConnectionInfo.relay branch");
    let relay_description = relay
        .as_mapping_get("description")
        .and_then(Yaml::as_str)
        .expect("ConnectionInfo.relay must document its trust boundary");
    for phrase in [
        "client-supplied",
        "does not resolve",
        "authenticate",
        "source-identity",
    ] {
        assert!(
            relay_description.contains(phrase),
            "ConnectionInfo.relay must preserve the `{phrase}` trust-boundary wording"
        );
    }
}

#[test]
fn authority_option_fields_are_required_but_nullable_on_the_wire() {
    use serde_json::json;
    use signal_fish_server::protocol::ServerMessage;

    let docs = Yaml::load_from_str(&spec_text()).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schema = |name| {
        mapping_path(root, &["components", "schemas", name]).expect("authority message schema")
    };
    let player_id = uuid::Uuid::from_u128(1);

    for (name, field, wire, populated) in [
        (
            "AuthorityChanged",
            "authority_player",
            serde_json::to_value(ServerMessage::AuthorityChanged {
                authority_player: None,
                you_are_authority: false,
            })
            .expect("serialize authority release"),
            serde_json::to_value(ServerMessage::AuthorityChanged {
                authority_player: Some(player_id),
                you_are_authority: false,
            })
            .expect("serialize authority assignment"),
        ),
        (
            "AuthorityResponse",
            "reason",
            serde_json::to_value(ServerMessage::AuthorityResponse {
                granted: true,
                reason: None,
                error_code: None,
            })
            .expect("serialize granted authority response"),
            serde_json::to_value(ServerMessage::AuthorityResponse {
                granted: false,
                reason: Some("Authority is already assigned.".to_string()),
                error_code: None,
            })
            .expect("serialize refused authority response"),
        ),
    ] {
        let mut envelopes = vec![name];
        if name == "AuthorityChanged" {
            envelopes.extend([
                "V2ReplayableServerMessageEnvelope",
                "V3ReplayableServerMessageEnvelope",
            ]);
        }
        let mut missing = wire.clone();
        missing["data"]
            .as_object_mut()
            .expect("object payload")
            .remove(field);
        let mut wrong_type = wire.clone();
        wrong_type["data"][field] = json!(17);

        assert_eq!(wire["data"].get(field), Some(&serde_json::Value::Null));
        for envelope in envelopes {
            let schema = schema(envelope);
            assert!(
                schema_shape_matches(root, schema, &wire),
                "{envelope}: {wire}"
            );
            assert!(
                schema_shape_matches(root, schema, &populated),
                "{envelope}: {populated}"
            );
            assert!(
                !schema_shape_matches(root, schema, &missing),
                "{envelope}: {missing}"
            );
            assert!(
                !schema_shape_matches(root, schema, &wrong_type),
                "{envelope}: {wrong_type}"
            );
        }

        // The local shape matcher honors legacy nullable annotations, but Draft 7 ignores them.
        assert!(
            mapping_path(
                schema(name),
                &["properties", "data", "properties", field, "nullable"]
            )
            .is_none(),
            "{name}.{field} must express null using Draft 7 keywords"
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
        Some(i64::try_from(DELIVERY_REPORT_MAX_GAPS).expect("gap limit fits i64")),
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
fn spec_models_the_addressed_binary_prefix_and_envelope() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let message = mapping_path(root, &["components", "messages", "AddressedGameDataBinary"])
        .expect("addressed binary physical message");
    assert_eq!(
        message
            .as_mapping_get("x-addressed-prefix")
            .and_then(Yaml::as_integer),
        Some(1)
    );
    assert_eq!(
        mapping_path(message, &["x-addressed-envelope", "$ref"]).and_then(Yaml::as_str),
        Some("#/components/schemas/V3BinaryGameDataEnvelope")
    );
    let payload = outbound_schemas(root)
        .into_iter()
        .find_map(|(name, payload)| (name == "AddressedGameDataBinary").then_some(payload))
        .expect("reachable addressed binary payload");
    assert_eq!(
        payload
            .as_mapping_get("x-protocol-version")
            .and_then(Yaml::as_integer),
        Some(3)
    );
    assert_eq!(
        payload.as_mapping_get("format").and_then(Yaml::as_str),
        Some("binary")
    );
    assert!(!schema_shape_matches(
        root,
        payload,
        &serde_json::json!({"type": "AddressedGameDataBinary", "data": {}})
    ));
}

#[test]
fn spec_models_v2_raw_binary_game_data() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let raw = schemas
        .iter()
        .find(|(tag, payload)| {
            tag == "GameDataBinary"
                && payload.as_mapping_get("type").and_then(Yaml::as_str) == Some("string")
        })
        .map(|(_, payload)| *payload)
        .expect("receiveFromServer must expose a raw octet-stream GameDataBinary payload");
    assert_eq!(
        raw.as_mapping_get("format").and_then(Yaml::as_str),
        Some("binary")
    );

    for (encoding, bytes) in [
        ("protobuf", &[0x08, 0x2a][..]),
        ("rkyv", &[0x2a, 0, 0, 0][..]),
    ] {
        let octets = serde_json::Value::String(bytes.iter().copied().map(char::from).collect());
        assert!(
            schema_shape_matches_version(root, raw, &octets, Some(2)),
            "raw v2 {encoding} gameplay bytes must match the octet-stream payload"
        );
        assert!(
            !schema_shape_matches_version(root, raw, &octets, Some(3)),
            "raw {encoding} payload must be v2 only"
        );
    }
    assert!(
        !schema_shape_matches(
            root,
            raw,
            &serde_json::json!({
                "from_player": "0123456789abcdef",
                "encoding": "message_pack",
                "payload": "*"
            })
        ),
        "a decoded MessagePack map must not match the raw payload schema"
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
    let connected_at =
        str::parse::<jiff::Timestamp>("2024-01-02T03:04:05Z").expect("valid timestamp fixture");
    let player = |accountable: bool| PlayerInfo {
        id: player_id,
        name: "Alice".to_string(),
        is_authority: true,
        is_ready: false,
        // Cohort-shaped fixture (issue #539): a v2 snapshot carries the
        // frozen legacy `connected_at`; the server strips it from every v3
        // snapshot before the frame is written, so the v3 wire shape omits
        // it.
        connected_at: (!accountable).then_some(connected_at),
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
                player: None,
            }),
        ),
        (
            "PlayerReconnected",
            serde_json::to_value(ServerMessage::PlayerReconnected {
                player_id,
                epoch: Some(3),
                player: None,
            }),
        ),
        (
            "PlayerReconnected",
            serde_json::to_value(ServerMessage::PlayerReconnected {
                player_id,
                epoch: None,
                player: Some(Box::new(player(false))),
            }),
        ),
        (
            "PlayerReconnected",
            serde_json::to_value(ServerMessage::PlayerReconnected {
                player_id,
                epoch: Some(3),
                player: Some(Box::new(player(true))),
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

    // Issue #529: a v3 snapshot must never echo the legacy self-declared
    // connection metadata the write layer strips.
    let mut v3_player_with_connection_info = serde_json::to_value(ServerMessage::PlayerJoined {
        player: player(true),
    })
    .expect("serialize PlayerJoined");
    v3_player_with_connection_info["data"]["player"]["connection_info"] = serde_json::json!({
        "type": "relay",
        "host": "relay.example.test",
        "port": 3478,
        "transport": "auto",
        "allocation_id": "alloc-1",
        "token": "cred-looking-secret"
    });
    invalid_cases.push((
        "PlayerJoined",
        "v3 snapshot echoing legacy self-declared connection metadata",
        v3_player_with_connection_info,
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

/// Follow only envelope unions, never nested payload objects, to identify the
/// wire discriminators reachable from an AsyncAPI operation.
fn envelope_tags(root: &Yaml, schema: &Yaml) -> BTreeSet<String> {
    if let Some(reference) = schema.as_mapping_get("$ref").and_then(Yaml::as_str) {
        let resolved =
            resolve_local_reference(root, reference).expect("resolved envelope reference");
        return envelope_tags(root, resolved);
    }
    if let Some(tag) = mapping_path(schema, &["properties", "type", "const"]).and_then(Yaml::as_str)
    {
        return BTreeSet::from([tag.to_string()]);
    }
    schema
        .as_mapping_get("oneOf")
        .and_then(Yaml::as_sequence)
        .expect("envelope must declare a discriminator or versioned union")
        .iter()
        .flat_map(|branch| envelope_tags(root, branch))
        .collect()
}

fn outbound_schemas<'doc, 'input>(root: &'doc Yaml<'input>) -> Vec<(String, &'doc Yaml<'input>)> {
    let operation =
        mapping_path(root, &["operations", "receiveFromServer"]).expect("server receive operation");
    assert_eq!(
        operation.as_mapping_get("action").and_then(Yaml::as_str),
        Some("receive")
    );
    assert_eq!(
        mapping_path(operation, &["channel", "$ref"]).and_then(Yaml::as_str),
        Some("#/channels/signaling")
    );
    let mut seen_messages = BTreeSet::new();
    operation
        .as_mapping_get("messages")
        .and_then(Yaml::as_sequence)
        .expect("outbound operation messages")
        .iter()
        .map(|entry| {
            let channel_ref = entry
                .as_mapping_get("$ref")
                .and_then(Yaml::as_str)
                .expect("channel message reference");
            assert!(channel_ref.starts_with("#/channels/signaling/messages/"));
            let channel_message =
                resolve_local_reference(root, channel_ref).expect("reachable channel message");
            let message_ref = channel_message
                .as_mapping_get("$ref")
                .and_then(Yaml::as_str)
                .expect("component message reference");
            assert!(
                seen_messages.insert(message_ref),
                "duplicate outbound operation message {message_ref}"
            );
            let message =
                resolve_local_reference(root, message_ref).expect("reachable component message");
            let payload = message.as_mapping_get("payload").expect("message payload");
            let tags = if let Some(physical) = message
                .as_mapping_get("x-rust-server-variant")
                .and_then(Yaml::as_str)
            {
                let content_type = match (message_ref, physical) {
                    ("#/components/messages/GameDataBinary", "GameDataBinary") => {
                        "application/msgpack"
                    }
                    ("#/components/messages/V2RawGameDataBinary", "GameDataBinary")
                    | (
                        "#/components/messages/AddressedGameDataBinary",
                        "AddressedGameDataBinary",
                    ) => "application/octet-stream",
                    _ => panic!("unexpected physical message {message_ref} ({physical})"),
                };
                assert_eq!(
                    message.as_mapping_get("contentType").and_then(Yaml::as_str),
                    Some(content_type),
                    "{message_ref} content type"
                );
                BTreeSet::from([physical.to_string()])
            } else {
                envelope_tags(root, payload)
            };
            assert_eq!(
                tags.len(),
                1,
                "{message_ref}: expected one message discriminator, got {tags:?}"
            );
            (tags.into_iter().next().expect("one tag"), payload)
        })
        .collect()
}

#[test]
fn outbound_schema_operation_reaches_exact_rust_message_set() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let actual: BTreeSet<_> = schemas.iter().map(|(tag, _)| tag.clone()).collect();
    assert_eq!(
        actual.len() + 1,
        schemas.len(),
        "only GameDataBinary has two outbound messages"
    );
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let mut expected: BTreeSet<_> = enum_variants(&source, "ServerMessage")
        .into_iter()
        .collect();
    assert!(
        !expected.is_empty(),
        "Rust server message inventory must not be empty"
    );
    // This negotiated extension uses its own frame type outside ServerMessage.
    expected.insert("TokenBindingChallenge".to_string());
    assert_eq!(
        actual, expected,
        "receiveFromServer differs from Rust variants plus the token-binding extension"
    );
}

#[test]
fn outbound_schema_accepts_canonical_server_samples() {
    use signal_fish_server::protocol::ServerMessage;

    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let mut covered = BTreeSet::new();
    for (version, samples) in [
        (
            2,
            include_str!("../.llm/code-samples/protocol/v2-server-messages.jsonl"),
        ),
        (
            3,
            include_str!("../.llm/code-samples/protocol/v3-server-messages.jsonl"),
        ),
    ] {
        for (index, line) in samples
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
        {
            let wire: serde_json::Value = serde_json::from_str(line).expect("server sample JSON");
            let tag = wire["type"].as_str().expect("server sample discriminator");
            let schema = schemas
                .iter()
                .find(|(name, _)| name == tag)
                .unwrap_or_else(|| panic!("v{version} line {}: unreachable {tag}", index + 1))
                .1;
            let resolved = schema
                .as_mapping_get("$ref")
                .and_then(Yaml::as_str)
                .and_then(|reference| resolve_local_reference(root, reference))
                .unwrap_or(schema);
            // Versioned unions may also have a shared branch (for example,
            // an empty spectator-only room). Keep that branch eligible.
            if let Some(branches) = resolved.as_mapping_get("oneOf").and_then(Yaml::as_sequence) {
                let branches: Vec<_> = branches
                    .iter()
                    .filter_map(|branch| {
                        branch
                            .as_mapping_get("$ref")
                            .and_then(Yaml::as_str)
                            .and_then(|reference| resolve_local_reference(root, reference))
                    })
                    .collect();
                if branches
                    .iter()
                    .any(|branch| branch.as_mapping_get("x-protocol-version").is_some())
                {
                    assert!(branches.iter().any(|branch| {
                        branch.as_mapping_get("x-protocol-version").and_then(Yaml::as_integer)
                            .is_none_or(|advertised| advertised == version)
                            && schema_shape_matches(root, branch, &wire)
                    }), "v{version} line {}: sample does not match its version or a shared branch: {wire}", index + 1);
                }
            }
            let parsed: ServerMessage =
                serde_json::from_value(wire.clone()).expect("Rust server sample");
            let encoded = serde_json::to_value(parsed).expect("serialize server sample");
            assert_eq!(
                wire,
                encoded,
                "v{version} line {}: Rust round trip",
                index + 1
            );
            assert!(
                schema_shape_matches(root, schema, &wire),
                "v{version} line {}: schema rejected {wire}",
                index + 1
            );
            let mut malformed = wire.clone();
            malformed
                .as_object_mut()
                .expect("message object")
                .remove("type");
            assert!(
                !schema_shape_matches(root, schema, &malformed),
                "{tag}: schema accepted a missing discriminator"
            );
            if wire.get("data").is_some() {
                malformed = wire.clone();
                malformed["data"] = serde_json::Value::Bool(false);
                assert!(
                    !schema_shape_matches(root, schema, &malformed),
                    "{tag}: schema accepted boolean payload"
                );
            }
            covered.insert(tag.to_string());
        }
    }
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let mut expected: BTreeSet<_> = enum_variants(&source, "ServerMessage")
        .into_iter()
        .collect();
    // These two variants use separate physical frame controls instead of JSON samples.
    assert!(expected.remove("GameDataBinary"));
    assert!(expected.remove("AddressedGameDataBinary"));
    assert_eq!(
        covered, expected,
        "canonical outbound sample inventory drifted"
    );
}

#[test]
fn nested_result_schema_matches_exact_rust_variant_set() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let envelope = schemas
        .iter()
        .find(|(tag, _)| tag == "RoomOperationResult")
        .expect("reachable result envelope")
        .1;
    let reference = envelope
        .as_mapping_get("$ref")
        .and_then(Yaml::as_str)
        .expect("result envelope reference");
    let envelope = resolve_local_reference(root, reference).expect("result envelope schema");
    let branches = mapping_path(
        envelope,
        &["properties", "data", "properties", "result", "oneOf"],
    )
    .and_then(Yaml::as_sequence)
    .expect("reachable nested result union");
    let mut actual = BTreeSet::new();
    for branch in branches {
        let tags = envelope_tags(root, branch);
        assert_eq!(tags.len(), 1, "each nested branch must name one result");
        let tag = tags.into_iter().next().expect("one result tag");
        assert!(actual.insert(tag.clone()), "duplicate nested result {tag}");
    }
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let expected: BTreeSet<_> = enum_variants(&source, "RoomOperationResult")
        .into_iter()
        .collect();
    assert!(
        !expected.is_empty(),
        "Rust result inventory must not be empty"
    );
    assert_eq!(actual, expected, "reachable result union differs from Rust");
}

#[test]
fn nested_result_schema_accepts_all_wire_variants_and_rejects_invalid_results() {
    use serde_json::{json, Value};
    use signal_fish_server::protocol::{RoomOperationResult, ServerMessage};

    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let schema = schemas
        .iter()
        .find(|(tag, _)| tag == "RoomOperationResult")
        .expect("reachable result envelope")
        .1;
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let expected: BTreeSet<_> = enum_variants(&source, "RoomOperationResult")
        .into_iter()
        .collect();
    let mut results = Vec::new();
    for samples in [
        include_str!("../.llm/code-samples/protocol/v2-server-messages.jsonl"),
        include_str!("../.llm/code-samples/protocol/v3-server-messages.jsonl"),
    ] {
        for line in samples.lines().filter(|line| !line.trim().is_empty()) {
            let wire: Value = serde_json::from_str(line).expect("canonical server sample");
            let tag = wire["type"].as_str().expect("sample tag");
            if expected.contains(tag) {
                results.push(wire);
            } else if tag == "RoomOperationResult" {
                results.push(wire["data"]["result"].clone());
            }
        }
    }
    // Result-only variants have no top-level ServerMessage equivalent.
    results.push(json!({"type":"OperationFailed", "data":{
        "reason":"not in room", "error_code":"NOT_IN_ROOM"
    }}));
    for tag in [
        "PlayerKicked",
        "PlayerBanned",
        "PlayerUnbanned",
        "AuthorityTransferred",
    ] {
        results.push(json!({"type":tag, "data":{
            "player_id":"bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        }}));
    }
    results.push(json!({"type":"RoomCodeRegenerated", "data":{"room_code":"ABC123"}}));
    for protected in [false, true] {
        results.push(json!({"type":"RoomAccessUpdated", "data":{"requires_password":protected}}));
    }
    let mut covered = BTreeSet::new();
    for result in results {
        let tag = result["type"].as_str().expect("result tag");
        let wire = json!({"type":"RoomOperationResult", "data":{
            "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "result":result
        }});
        let parsed: ServerMessage = serde_json::from_value(wire.clone())
            .unwrap_or_else(|error| panic!("Rust rejected nested {tag}: {error}: {wire}"));
        assert_eq!(
            serde_json::to_value(parsed).expect("result serialization"),
            wire,
            "nested {tag}: Rust round trip"
        );
        assert!(
            schema_shape_matches(root, schema, &wire),
            "schema rejected nested {tag}: {wire}"
        );

        for pointer in ["/data/result/type", "/data/result/data"] {
            if wire.pointer(pointer).is_some() {
                let mut invalid = wire.clone();
                *invalid.pointer_mut(pointer).expect("existing result field") = json!(false);
                assert!(
                    serde_json::from_value::<ServerMessage>(invalid.clone()).is_err(),
                    "Rust accepted invalid nested {tag} at {pointer}"
                );
                assert!(
                    !schema_shape_matches(root, schema, &invalid),
                    "schema accepted invalid nested {tag} at {pointer}: {invalid}"
                );
            }
        }
        covered.insert(tag.to_string());
    }
    assert_eq!(
        covered, expected,
        "nested wire vectors differ from Rust variants"
    );

    // A valid top-level event is not necessarily a valid correlated result.
    for result in [
        json!({"type":"Pong"}),
        json!({"type":"RoomOperationResult", "data":{}}),
        json!({"type":"OperationFailed", "data":{}}),
        Value::Null,
    ] {
        assert!(
            serde_json::from_value::<RoomOperationResult>(result.clone()).is_err(),
            "Rust accepted unsupported or malformed result: {result}"
        );
        let wire = json!({"type":"RoomOperationResult", "data":{
            "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "result":result
        }});
        assert!(
            !schema_shape_matches(root, schema, &wire),
            "schema accepted invalid result: {wire}"
        );
    }
}

#[test]
fn nested_result_schema_preserves_version_eligibility() {
    use serde_json::{json, Value};

    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let schema = schemas
        .iter()
        .find(|(tag, _)| tag == "RoomOperationResult")
        .expect("reachable result envelope")
        .1;
    let wrap = |result: Value| {
        json!({"type":"RoomOperationResult", "data":{
            "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "result":result
        }})
    };
    let samples: Vec<Vec<Value>> = [
        include_str!("../.llm/code-samples/protocol/v2-server-messages.jsonl"),
        include_str!("../.llm/code-samples/protocol/v3-server-messages.jsonl"),
    ]
    .iter()
    .map(|samples| {
        samples
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("server sample JSON"))
            .collect()
    })
    .collect();
    let sample = |index: usize, tag: &str| {
        samples[index]
            .iter()
            .find(|wire| wire["type"] == tag)
            .unwrap_or_else(|| panic!("v{} lacks {tag} sample", index + 2))
            .clone()
    };
    let player_joined = |index| {
        json!({"type":"PlayerJoined", "data":{
            "player":sample(index, "RoomJoined")["data"]["current_players"][0].clone()
        }})
    };
    let empty_spectator = sample(0, "SpectatorJoined");
    for (index, version) in [(0, 2), (1, 3)] {
        let other_version = 5 - version;
        let joined = sample(index, "RoomJoined");
        let mut reconnected = sample(index, "Reconnected");
        reconnected["data"]["missed_events"] = json!([player_joined(index)]);
        let mut spectator = empty_spectator.clone();
        spectator["data"]["current_players"] = joined["data"]["current_players"].clone();
        for result in [joined, reconnected, spectator] {
            let tag = result["type"].as_str().expect("result tag");
            let wire = wrap(result.clone());
            assert!(
                schema_shape_matches(root, schema, &wire),
                "v{version} nested {tag}: unversioned shape rejected {wire}"
            );
            assert!(
                schema_shape_matches_version(root, schema, &wire, Some(version)),
                "v{version} nested {tag}: advertised branch rejected {wire}"
            );
            assert!(
                !schema_shape_matches_version(root, schema, &wire, Some(other_version)),
                "v{version} nested {tag}: wrong version {other_version} accepted {wire}"
            );

            let mut hybrid = result.clone();
            let player = &mut hybrid["data"]["current_players"][0];
            if version == 2 {
                player["epoch"] = json!(1);
                player["seq"] = json!(0);
            } else {
                player["connected_at"] = json!("2026-09-20T12:00:00Z");
            }
            assert!(
                !schema_shape_matches_version(root, schema, &wrap(hybrid), Some(version)),
                "v{version} nested {tag}: hybrid player snapshot accepted"
            );
            if tag == "Reconnected" {
                let mut mixed_replay = result;
                mixed_replay["data"]["missed_events"] = json!([player_joined(1 - index)]);
                assert!(
                    !schema_shape_matches_version(root, schema, &wrap(mixed_replay), Some(version)),
                    "v{version}: cross-version nested replay item accepted"
                );
            }
        }
        // Shared shapes cannot identify the recipient version. Do not infer
        // runtime envelope eligibility from either version's shape acceptance.
        for result in [empty_spectator.clone(), json!({"type":"RoomLeft"})] {
            let wire = wrap(result);
            assert!(
                schema_shape_matches_version(root, schema, &wire, Some(version)),
                "v{version}: shared nested result rejected {wire}"
            );
        }
    }
}

#[test]
fn populated_spectator_rosters_match_schema_boundaries() {
    use serde_json::{json, Value};
    use signal_fish_server::protocol::{ServerMessage, SpectatorInfo};

    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schemas = outbound_schemas(root);
    let schema_for = |wire: &Value| {
        schemas
            .iter()
            .find(|(tag, _)| wire["type"] == tag.as_str())
            .expect("reachable server envelope")
            .1
    };
    let wrap = |result: Value| {
        json!({"type":"RoomOperationResult", "data":{
            "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "result":result
        }})
    };
    let timestamp =
        str::parse::<jiff::Timestamp>("2024-01-02T03:04:05Z").expect("spectator timestamp");
    let spectator = |version| {
        serde_json::to_value(SpectatorInfo {
            id: uuid::Uuid::from_u128(3),
            name: "観客".to_string(),
            connected_at: (version == 2).then_some(timestamp),
        })
        .expect("serialize spectator")
    };
    let check = |version, wire: &Value| {
        let decoded: ServerMessage =
            serde_json::from_value(wire.clone()).expect("Rust accepts populated envelope");
        assert_eq!(
            serde_json::to_value(decoded).expect("serialize populated envelope"),
            *wire,
            "v{version}: Rust wire identity changed"
        );
        assert!(
            schema_shape_matches_version(root, schema_for(wire), wire, Some(version)),
            "v{version}: schema rejected populated spectator envelope {wire}"
        );
    };
    for (version, samples) in [
        (
            2,
            include_str!("../.llm/code-samples/protocol/v2-server-messages.jsonl"),
        ),
        (
            3,
            include_str!("../.llm/code-samples/protocol/v3-server-messages.jsonl"),
        ),
    ] {
        let samples: Vec<Value> = samples
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("server sample JSON"))
            .collect();
        let sample = |tag: &str| {
            samples
                .iter()
                .find(|wire| wire["type"] == tag)
                .unwrap_or_else(|| panic!("v{version} lacks {tag}"))
                .clone()
        };
        let member = sample("RoomJoined")["data"]["current_players"].clone();
        let roster = json!([spectator(version)]);
        for tag in ["RoomJoined", "Reconnected", "SpectatorJoined"] {
            let mut wire = if tag == "SpectatorJoined" {
                include_str!("../.llm/code-samples/protocol/v2-server-messages.jsonl")
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| serde_json::from_str::<Value>(line).expect("server sample JSON"))
                    .find(|wire| wire["type"] == tag)
                    .expect("shared spectator join sample")
            } else {
                sample(tag)
            };
            wire["data"]["current_players"] = member.clone();
            wire["data"]["current_spectators"] = roster.clone();
            check(version, &wire);
            check(version, &wrap(wire.clone()));

            // Keep the player snapshot fixed so a wrong spectator item cannot
            // escape through the other version's envelope branch.
            let mut wrong_version = wire.clone();
            wrong_version["data"]["current_spectators"] = json!([spectator(5 - version)]);
            for invalid in [
                wrong_version,
                {
                    let mut invalid = wire.clone();
                    invalid["data"]["current_spectators"][0]["name"] = json!(false);
                    invalid
                },
                {
                    let mut invalid = wire.clone();
                    invalid["data"]["current_spectators"][0]["unexpected"] = json!(true);
                    invalid
                },
            ] {
                for candidate in [invalid.clone(), wrap(invalid)] {
                    assert!(
                        !schema_shape_matches_version(
                            root,
                            schema_for(&candidate),
                            &candidate,
                            Some(version)
                        ),
                        "v{version} {tag}: invalid spectator accepted {candidate}"
                    );
                }
            }
            if tag == "SpectatorJoined" {
                wire["data"]["current_players"] = json!([]);
                check(version, &wire);
                check(version, &wrap(wire));
            }
        }

        // V3 roster events are deltas; v2 events carry the full roster.
        let events = [
            json!({"type":"NewSpectatorJoined", "data":{
                "spectator":spectator(version),
                "current_spectators":if version == 2 {roster.clone()} else {json!([])}
            }}),
            json!({"type":"SpectatorDisconnected", "data":{
                "spectator_id":uuid::Uuid::from_u128(4),
                "current_spectators":if version == 2 {roster.clone()} else {json!([])}
            }}),
        ]
        .map(|mut event| {
            if version == 3 {
                event["data"]["spectator_count"] = json!(1);
            }
            event
        });
        for event in &events {
            check(version, event);
        }
        let left = json!({"type":"SpectatorLeft", "data":{"current_spectators":roster}});
        check(version, &left);
        check(version, &wrap(left));
        if version == 3 {
            let mut reconnect = sample("Reconnected");
            reconnect["data"]["current_spectators"] = json!([spectator(version)]);
            reconnect["data"]["missed_events"] = json!(events);
            check(version, &reconnect);
            check(version, &wrap(reconnect.clone()));
            reconnect["data"]["missed_events"][0]["data"]["spectator"]["name"] = json!(false);
            for invalid in [reconnect.clone(), wrap(reconnect)] {
                assert!(
                    !schema_shape_matches_version(
                        root,
                        schema_for(&invalid),
                        &invalid,
                        Some(version)
                    ),
                    "invalid replay spectator accepted {invalid}"
                );
            }
        }
    }
}

#[test]
fn inbound_schema_unions_match_exact_rust_operation_sets() {
    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    for (enum_name, path) in [
        (
            "ClientMessage",
            vec!["components", "schemas", "ClientMessageEnvelope", "oneOf"],
        ),
        (
            "RoomOperationRequest",
            vec![
                "components",
                "schemas",
                "RoomOperation",
                "properties",
                "data",
                "properties",
                "operation",
                "oneOf",
            ],
        ),
    ] {
        let branches = mapping_path(root, &path)
            .and_then(Yaml::as_sequence)
            .unwrap_or_else(|| panic!("{enum_name}: missing inbound union at {path:?}"));
        let declared: Vec<_> = branches
            .iter()
            .map(|branch| {
                let reference = branch
                    .as_mapping_get("$ref")
                    .and_then(Yaml::as_str)
                    .unwrap_or_else(|| panic!("{enum_name}: expected a schema reference"));
                let schema = resolve_local_reference(root, reference)
                    .unwrap_or_else(|| panic!("{enum_name}: unresolved {reference}"));
                mapping_path(schema, &["properties", "type", "const"])
                    .and_then(Yaml::as_str)
                    .unwrap_or_else(|| panic!("{enum_name}: {reference} needs an exact type tag"))
                    .to_string()
            })
            .collect();
        let unique: BTreeSet<_> = declared.iter().cloned().collect();
        assert_eq!(
            unique.len(),
            declared.len(),
            "{enum_name}: duplicate inbound branch"
        );
        let expected: BTreeSet<_> = enum_variants(&source, enum_name).into_iter().collect();
        assert!(
            !expected.is_empty(),
            "{enum_name}: source inventory must not be empty"
        );
        assert_eq!(
            unique, expected,
            "{enum_name}: inbound union differs from Rust variants"
        );
    }
}

#[test]
fn inbound_schema_accepts_wire_samples_and_only_supported_nested_commands() {
    use serde_json::{json, Value};
    use signal_fish_server::protocol::ClientMessage;

    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let envelope = mapping_path(root, &["components", "schemas", "ClientMessageEnvelope"])
        .expect("client envelope");
    let source = read_file(&repo_root().join("src/protocol/messages.rs"));
    let nested_variants: BTreeSet<_> = enum_variants(&source, "RoomOperationRequest")
        .into_iter()
        .collect();
    let mut covered = BTreeSet::new();
    let mut nested_covered = BTreeSet::new();
    let wrap = |operation: Value| {
        json!({
            "type": "RoomOperation",
            "data": {
                "operation_id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                "operation": operation
            }
        })
    };
    let check = |wire: &Value| {
        let parsed: ClientMessage = serde_json::from_value(wire.clone())
            .unwrap_or_else(|error| panic!("Rust rejected {wire}: {error}"));
        assert!(
            schema_shape_matches(root, envelope, wire),
            "schema rejected input {wire}"
        );
        let encoded = serde_json::to_value(parsed).expect("serialize client command");
        assert!(
            schema_shape_matches(root, envelope, &encoded),
            "schema rejected output {encoded}"
        );
    };

    for (version, samples) in [
        (
            2,
            include_str!("../.llm/code-samples/protocol/v2-client-messages.jsonl"),
        ),
        (
            3,
            include_str!("../.llm/code-samples/protocol/v3-client-messages.jsonl"),
        ),
    ] {
        for (line, sample) in samples.lines().enumerate() {
            let wire: Value = serde_json::from_str(sample)
                .unwrap_or_else(|error| panic!("v{version} sample {}: {error}", line + 1));
            check(&wire);
            let kind = wire["type"].as_str().expect("sample type");
            covered.insert(kind.to_string());
            // A command's presence in the top-level union does not authorize it
            // inside RoomOperation. Runtime version/capability gates are separate.
            if nested_variants.contains(kind) {
                check(&wrap(wire.clone()));
                nested_covered.insert(kind.to_string());
            } else {
                let invalid = wrap(wire.clone());
                assert!(
                    serde_json::from_value::<ClientMessage>(invalid.clone()).is_err(),
                    "Rust accepted unsupported nested {kind}"
                );
                assert!(
                    !schema_shape_matches(root, envelope, &invalid),
                    "schema accepted unsupported nested {kind}"
                );
            }
        }
    }
    let player = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
    for operation in [
        json!({"type":"KickPlayer", "data":{"player_id":player}}),
        json!({"type":"RegenerateRoomCode"}),
        json!({"type":"SetRoomAccess", "data":{"password":"room-password"}}),
        json!({"type":"SetRoomAccess", "data":{"password":null}}),
        json!({"type":"BanPlayer", "data":{"player_id":player}}),
        json!({"type":"UnbanPlayer", "data":{"player_id":player}}),
        json!({"type":"TransferAuthority", "data":{"player_id":player}}),
    ] {
        let kind = operation["type"].as_str().expect("operation type");
        check(&wrap(operation.clone()));
        nested_covered.insert(kind.to_string());
        assert!(
            serde_json::from_value::<ClientMessage>(operation.clone()).is_err(),
            "Rust accepted nested-only {kind} at top level"
        );
        assert!(
            !schema_shape_matches(root, envelope, &operation),
            "schema accepted nested-only {kind} at top level"
        );
    }
    assert_eq!(
        covered,
        enum_variants(&source, "ClientMessage")
            .into_iter()
            .collect(),
        "canonical samples must exercise every top-level command"
    );
    assert_eq!(
        nested_covered, nested_variants,
        "wire cases must exercise every nested command"
    );

    for (name, invalid) in [
        ("unknown command", wrap(json!({"type":"UnknownOperation"}))),
        (
            "missing operation",
            json!({"type":"RoomOperation", "data":{"operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"}}),
        ),
        (
            "missing operation id",
            json!({"type":"RoomOperation", "data":{"operation":{"type":"LeaveRoom"}}}),
        ),
        (
            "missing target",
            wrap(json!({"type":"KickPlayer", "data":{}})),
        ),
        (
            "wrong target type",
            wrap(json!({"type":"TransferAuthority", "data":{"player_id":42}})),
        ),
    ] {
        assert!(
            serde_json::from_value::<ClientMessage>(invalid.clone()).is_err(),
            "Rust accepted {name}: {invalid}"
        );
        assert!(
            !schema_shape_matches(root, envelope, &invalid),
            "schema accepted {name}: {invalid}"
        );
    }
}

#[test]
fn connection_metadata_variants_match_input_and_legacy_output_schemas() {
    use serde_json::{json, Value};
    use signal_fish_server::protocol::{ClientMessage, ConnectionInfo, ServerMessage};

    let docs = Yaml::load_from_str(&spec_text()).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let schema = |name| {
        mapping_path(root, &["components", "schemas", name]).expect("metadata boundary schema")
    };
    let source = read_file(&repo_root().join("src/protocol/types.rs"));
    let expected: BTreeSet<_> = enum_variants(&source, "ConnectionInfo")
        .into_iter()
        .collect();
    let mut covered = BTreeSet::new();
    let mut cases = vec![
        (
            "Direct",
            json!({"type":"direct", "host":"例え.example", "port":0}),
        ),
        (
            "Direct",
            json!({"type":"direct", "host":"192.0.2.1", "port":65535}),
        ),
        (
            "UnityRelay",
            json!({"type":"unity_relay", "allocation_id":"allocation", "connection_data":"opaque", "key":"fixture"}),
        ),
    ];
    for transport in [
        None,
        Some("auto"),
        Some("tcp"),
        Some("udp"),
        Some("websocket"),
    ] {
        for client_id in [None, Some(Value::Null), Some(json!(0)), Some(json!(65535))] {
            let mut info = json!({"type":"relay", "host":"relay.example", "port":7777,
                "allocation_id":"allocation", "token":"fixture"});
            if let Some(transport) = transport {
                info["transport"] = json!(transport);
            }
            if let Some(client_id) = client_id {
                info["client_id"] = client_id;
            }
            cases.push(("Relay", info));
        }
    }
    for sdp in [None, Some(Value::Null), Some(json!("SDP-観客"))] {
        for candidates in [json!([]), json!(["candidate:fixture", "候補"])] {
            let mut info = json!({"type":"webrtc", "ice_candidates":candidates});
            if let Some(sdp) = &sdp {
                info["sdp"] = sdp.clone();
            }
            cases.push(("WebRTC", info));
        }
    }
    for data in [
        Value::Null,
        json!(true),
        json!(42),
        json!(1.5),
        json!("観客"),
        json!([null, false, {"nested":[1, "two"]}]),
        json!({"type":"direct", "data":{"arbitrary":true}}),
    ] {
        cases.push(("Custom", json!({"type":"custom", "data":data})));
    }
    for (tag, field) in [
        ("direct", "port"),
        ("relay", "port"),
        ("relay", "client_id"),
    ] {
        let branch = schema("ConnectionInfo")
            .as_mapping_get("oneOf")
            .and_then(Yaml::as_sequence)
            .unwrap()
            .iter()
            .find(|branch| {
                mapping_path(branch, &["properties", "type", "const"]).and_then(Yaml::as_str)
                    == Some(tag)
            })
            .expect("numeric metadata branch");
        let property = mapping_path(branch, &["properties", field]).expect("u16 metadata field");
        let declared_type = property
            .as_mapping_get("type")
            .expect("metadata field type");
        if field == "client_id" {
            assert_eq!(
                declared_type
                    .as_sequence()
                    .map(|types| types.iter().map(Yaml::as_str).collect::<BTreeSet<_>>()),
                Some(BTreeSet::from([Some("integer"), Some("null")])),
                "relay.client_id must declare integer and null as Draft 7 types"
            );
        } else {
            assert_eq!(declared_type.as_str(), Some("integer"));
        }
        assert_eq!(
            property
                .as_mapping_get("minimum")
                .and_then(Yaml::as_integer),
            Some(0)
        );
        assert_eq!(
            property
                .as_mapping_get("maximum")
                .and_then(Yaml::as_integer),
            Some(i64::from(u16::MAX))
        );
        for invalid in [json!(-1), json!(u32::from(u16::MAX) + 1)] {
            let mut info = cases
                .iter()
                .find(|(_, info)| info["type"] == tag)
                .unwrap()
                .1
                .clone();
            info[field] = invalid;
            assert!(
                serde_json::from_value::<ConnectionInfo>(info.clone()).is_err(),
                "Rust accepted out-of-range {tag}.{field}: {info}"
            );
        }
    }
    let v2_join: Value = include_str!("../.llm/code-samples/protocol/v2-server-messages.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("canonical sample"))
        .find(|wire| wire["type"] == "RoomJoined")
        .expect("v2 room join");
    let v3_join: Value = include_str!("../.llm/code-samples/protocol/v3-server-messages.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("canonical sample"))
        .find(|wire| wire["type"] == "RoomJoined")
        .expect("v3 room join");
    for (variant, info) in &cases {
        covered.insert((*variant).to_string());
        let parsed: ConnectionInfo = serde_json::from_value(info.clone()).expect("Rust metadata");
        let normalized = serde_json::to_value(parsed).expect("serialize metadata");
        assert!(
            schema_shape_matches(root, schema("ConnectionInfo"), info),
            "schema rejected accepted {variant} input: {info}"
        );
        assert!(
            schema_shape_matches(root, schema("ConnectionInfo"), &normalized),
            "schema rejected emitted {variant}: {normalized}"
        );
        let mut expected_wire = info.clone();
        if *variant == "Relay" {
            if expected_wire.get("transport").is_none() {
                expected_wire["transport"] = json!("auto");
            }
            if expected_wire["client_id"].is_null() {
                expected_wire.as_object_mut().unwrap().remove("client_id");
            }
        } else if *variant == "WebRTC" && expected_wire.get("sdp").is_none() {
            expected_wire["sdp"] = Value::Null;
        }
        assert_eq!(normalized, expected_wire, "metadata normalization: {info}");
        let reparsed: ConnectionInfo =
            serde_json::from_value(normalized.clone()).expect("metadata round trip");
        assert_eq!(serde_json::to_value(reparsed).unwrap(), normalized);
        let input = json!({"type":"ProvideConnectionInfo", "data":{"connection_info":info}});
        let parsed: ClientMessage =
            serde_json::from_value(input.clone()).expect("metadata command");
        assert!(
            schema_shape_matches(root, schema("ClientMessageEnvelope"), &input),
            "schema rejected metadata command: {input}"
        );
        let emitted = serde_json::to_value(parsed).unwrap();
        assert_eq!(emitted["data"]["connection_info"], normalized);
        assert!(schema_shape_matches(
            root,
            schema("ClientMessageEnvelope"),
            &emitted
        ));
        let wrapped = json!({"type":"RoomOperation", "data":{
            "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "operation":input}});
        assert!(serde_json::from_value::<ClientMessage>(wrapped.clone()).is_err());
        assert!(!schema_shape_matches(
            root,
            schema("ClientMessageEnvelope"),
            &wrapped
        ));
        let mut join = v2_join.clone();
        join["data"]["current_players"][0]["connection_info"] = normalized.clone();
        let mut v3_with_metadata = v3_join.clone();
        v3_with_metadata["data"]["current_players"][0]["connection_info"] = normalized.clone();
        assert!(
            !schema_shape_matches_version(root, schema("RoomJoined"), &v3_with_metadata, Some(3)),
            "v3 schema accepted legacy metadata: {v3_with_metadata}"
        );
        let starting = json!({"type":"GameStarting", "data":{"peer_connections":[{
            "player_id":"00000000-0000-0000-0000-000000000001", "player_name":"観客",
            "is_authority":true, "relay_type":"custom-label", "connection_info":normalized
        }]}});
        for (name, wire) in [("RoomJoined", join), ("GameStarting", starting)] {
            let parsed: ServerMessage =
                serde_json::from_value(wire.clone()).expect("legacy metadata envelope");
            assert_eq!(
                serde_json::to_value(parsed).unwrap(),
                wire,
                "legacy round trip: {wire}"
            );
            assert!(
                schema_shape_matches(root, schema(name), &wire),
                "legacy schema: {wire}"
            );
        }
    }
    assert_eq!(
        covered, expected,
        "metadata cases must cover every Rust variant"
    );
    for invalid in [
        json!({"type":"unknown"}),
        json!({"type":"direct", "host":false, "port":7777}),
        json!({"type":"direct", "host":"host"}),
        json!({"type":"unity_relay", "allocation_id":"a", "connection_data":"b"}),
        json!({"type":"relay", "host":"host", "port":7777, "allocation_id":"a", "token":false}),
        json!({"type":"relay", "host":"host", "port":7777, "allocation_id":"a", "token":"t", "transport":"unknown"}),
        json!({"type":"relay", "host":"host", "port":7777, "allocation_id":"a", "token":"t", "client_id":"one"}),
        json!({"type":"webrtc", "ice_candidates":[false]}),
        json!({"type":"webrtc", "sdp":null}),
        json!({"type":"custom"}),
        json!({"type":"relay", "host":"host", "port":7777, "allocation_id":"a", "token":"t", "transport":null}),
        json!({"type":"webrtc", "ice_candidates":[], "sdp":false}),
    ] {
        assert!(
            serde_json::from_value::<ConnectionInfo>(invalid.clone()).is_err(),
            "Rust accepted {invalid}"
        );
        assert!(
            !schema_shape_matches(root, schema("ConnectionInfo"), &invalid),
            "schema accepted {invalid}"
        );
        for wrapped in [false, true] {
            let input = json!({"type":"ProvideConnectionInfo", "data":{"connection_info":invalid}});
            let wire = if wrapped {
                json!({"type":"RoomOperation", "data":{
                "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "operation":input}})
            } else {
                input
            };
            assert!(serde_json::from_value::<ClientMessage>(wire.clone()).is_err());
            assert!(!schema_shape_matches(
                root,
                schema("ClientMessageEnvelope"),
                &wire
            ));
        }
    }
}

#[test]
fn join_room_optional_fields_match_schema_in_both_envelopes() {
    use serde_json::{json, Value};
    use signal_fish_server::protocol::ClientMessage;

    let text = spec_text();
    let docs = Yaml::load_from_str(&text).expect("valid protocol YAML");
    let root = docs.first().expect("protocol document");
    let envelope = mapping_path(root, &["components", "schemas", "ClientMessageEnvelope"])
        .expect("client envelope");
    for (field, valid, invalid) in [
        ("room_code", json!("ABC123"), json!(42)),
        ("max_players", json!(4), json!("four")),
        ("supports_authority", json!(false), json!("false")),
        ("relay_transport", json!("auto"), json!("unknown")),
        ("password", json!("room-password"), json!(false)),
        ("join_only", json!(false), json!("false")),
    ] {
        for (case, value, accepted) in [
            ("omitted", None, true),
            ("null", Some(Value::Null), true),
            ("valid", Some(valid), true),
            ("wrong type or token", Some(invalid), false),
        ] {
            let mut join =
                json!({"type":"JoinRoom", "data":{"game_name":"game", "player_name":"player"}});
            if let Some(value) = value {
                join["data"][field] = value;
            }
            for wrapped in [false, true] {
                let wire = if wrapped {
                    json!({"type":"RoomOperation", "data":{
                        "operation_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                        "operation":join
                    }})
                } else {
                    join.clone()
                };
                let parsed = serde_json::from_value::<ClientMessage>(wire.clone());
                assert_eq!(
                    parsed.is_ok(),
                    accepted,
                    "Rust {field}/{case}, wrapped={wrapped}: {wire}"
                );
                assert_eq!(
                    schema_shape_matches(root, envelope, &wire),
                    accepted,
                    "schema {field}/{case}, wrapped={wrapped}: {wire}"
                );
                if let Ok(parsed) = parsed {
                    let encoded = serde_json::to_value(parsed).expect("serialize join");
                    assert!(
                        schema_shape_matches(root, envelope, &encoded),
                        "schema rejected emitted {field}/{case}, wrapped={wrapped}: {encoded}"
                    );
                }
            }
        }
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

    fn visit(
        root: &Path,
        dir: &Path,
        variants: &[String],
        references: &mut Vec<(PathBuf, String)>,
    ) {
        for entry in fs::read_dir(dir).expect("production source directory must be readable") {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                visit(root, &path, variants, references);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
                && !path.ends_with("protocol/error_codes.rs")
            {
                let source = strip_comment_lines(&read_file(&path));
                let relative = path
                    .strip_prefix(root)
                    .expect("visited production source must be below repository root")
                    .to_path_buf();
                for variant in variants {
                    for _ in source.match_indices(variant) {
                        references.push((relative.clone(), variant.clone()));
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
    let root = repo_root();
    visit(&root, &root.join("src"), &variants, &mut references);

    let connection_path = PathBuf::from("src").join("websocket").join("connection.rs");
    let auth_error_path = PathBuf::from("src").join("auth").join("error.rs");
    let mut allowed = Vec::new();
    for code in ErrorCode::NON_EMITTED {
        let variant = format!("{code:?}");
        if variant.starts_with("AppId") {
            allowed.extend(
                std::iter::repeat_n((auth_error_path.clone(), variant.clone()), 1).chain(
                    std::iter::repeat_n((connection_path.clone(), variant.clone()), 2),
                ),
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
        // rkyv and protobuf are negotiable behind their #627 opt-in knobs and
        // are declared wire values, so the spec lists them and we check them.
        for value in [Json, MessagePack, Rkyv, Protobuf] {
            assert_wire_token(&declared, value, "GameDataEncoding");
        }
        let _exhaustive = |value: GameDataEncoding| match value {
            Json | MessagePack | Rkyv | Protobuf => {}
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

#[test]
fn room_operation_schema_matches_canonical_uuid_and_additive_serde_boundaries() {
    const CANONICAL_UUID_PATTERN: &str =
        "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";

    let text = spec_text();
    let docs = Yaml::load_from_str(&text)
        .unwrap_or_else(|error| panic!("protocol spec is not valid YAML: {error}"));
    let root = docs
        .first()
        .expect("protocol spec must contain one document");

    let operation_id = mapping_path(root, &["components", "schemas", "RoomOperationId"])
        .expect("spec must define RoomOperationId");
    assert_eq!(
        operation_id.as_mapping_get("format").and_then(Yaml::as_str),
        Some("uuid")
    );
    assert_eq!(
        operation_id
            .as_mapping_get("pattern")
            .and_then(Yaml::as_str),
        Some(CANONICAL_UUID_PATTERN),
        "RoomOperationId must require lowercase canonical hyphenated UUID text"
    );

    for envelope_name in ["RoomOperation", "RoomOperationResult"] {
        let envelope = mapping_path(root, &["components", "schemas", envelope_name])
            .unwrap_or_else(|| panic!("spec must define {envelope_name}"));
        assert_ne!(
            envelope
                .as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{envelope_name} must match serde's additive outer-envelope boundary"
        );
        let data = mapping_path(envelope, &["properties", "data"])
            .unwrap_or_else(|| panic!("{envelope_name} must define data"));
        assert_ne!(
            data.as_mapping_get("additionalProperties")
                .and_then(Yaml::as_bool),
            Some(false),
            "{envelope_name}.data must match serde's additive field boundary"
        );
        assert_eq!(
            mapping_path(data, &["properties", "operation_id", "$ref"]).and_then(Yaml::as_str),
            Some("#/components/schemas/RoomOperationId"),
            "{envelope_name}.operation_id must share the canonical UUID schema"
        );
    }

    let operation_failed = mapping_path(root, &["components", "schemas", "OperationFailed"])
        .expect("spec must define OperationFailed");
    assert_ne!(
        operation_failed
            .as_mapping_get("additionalProperties")
            .and_then(Yaml::as_bool),
        Some(false),
        "OperationFailed must match serde's additive outer-result boundary"
    );
    assert_ne!(
        mapping_path(
            operation_failed,
            &["properties", "data", "additionalProperties"]
        )
        .and_then(Yaml::as_bool),
        Some(false),
        "OperationFailed.data must match serde's additive field boundary"
    );
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

mod projection_runtime {
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{json, Value};
    use signal_fish_server::websocket::create_router;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    use super::{mapping_path, schema_shape_matches_version, spec_text, test_helpers};
    use saphyr::{LoadableYamlNode, Yaml};

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn send(socket: &mut Socket, value: Value) {
        socket
            .send(Message::Text(value.to_string().into()))
            .await
            .expect("send metadata scenario command");
    }

    async fn receive(socket: &mut Socket, tag: &str) -> Value {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, socket.next())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {tag}"))
                .expect("socket remains open")
                .expect("read scenario frame");
            let Message::Text(text) = frame else {
                continue;
            };
            let value: Value = serde_json::from_str(&text).expect("raw JSON frame");
            assert!(
                !matches!(value["type"].as_str(), Some("Error" | "RoomJoinFailed")),
                "unexpected refusal while waiting for {tag}: {value}"
            );
            if value["type"] == tag {
                return value;
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn metadata_reaches_mixed_version_snapshots_and_game_start_on_real_sockets() {
        let docs = Yaml::load_from_str(&spec_text()).expect("protocol YAML");
        let root = docs.first().expect("protocol document");
        let cases = [
            json!({"type":"direct", "host":"192.0.2.1", "port":7777}),
            json!({"type":"unity_relay", "allocation_id":"allocation", "connection_data":"opaque", "key":"fixture"}),
            json!({"type":"relay", "host":"relay.example", "port":65535, "transport":"udp",
                "allocation_id":"allocation", "token":"fixture", "client_id":0}),
            json!({"type":"webrtc", "sdp":"SDP-観客", "ice_candidates":["candidate:fixture", "候補"]}),
            json!({"type":"webrtc", "sdp":null, "ice_candidates":["candidate:fixture"]}),
            json!({"type":"custom", "data":{"nested":[null, false, "観客", {"port":0}]}}),
        ];
        for info in cases {
            let mut protocol = test_helpers::test_protocol_config();
            protocol.sdk_compatibility.enforce = false;
            let server = test_helpers::create_test_server_with_config(
                test_helpers::test_server_config(),
                protocol,
            )
            .await;
            let router = create_router("http://localhost:3000").with_state(server.clone());
            let running = test_helpers::RunningTestServer::spawn(server, router).await;
            let url = format!("ws://{}/ws", running.addr());
            let mut sockets = Vec::new();
            let mut ids = Vec::new();
            let mut room_code = Value::Null;
            let mut room_id = Value::Null;
            for (index, version) in [2_u16, 2, 3].into_iter().enumerate() {
                let (mut socket, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(10), connect_async(&url))
                        .await
                        .expect("connect deadline")
                        .expect("connect WebSocket");
                send(
                    &mut socket,
                    json!({"type":"Authenticate", "data":{
                        "app_id":"metadata-runtime", "protocol_version":version
                    }}),
                )
                .await;
                receive(&mut socket, "Authenticated").await;
                let negotiation = receive(&mut socket, "ProtocolInfo").await;
                assert_eq!(
                    negotiation["data"]["protocol_version"],
                    if version == 3 { json!(3) } else { Value::Null }
                );
                send(
                    &mut socket,
                    json!({"type":"JoinRoom", "data":{
                        "game_name":"metadata-runtime", "room_code":room_code,
                        "player_name":format!("Member{index}"), "max_players":3,
                        "supports_authority":false
                    }}),
                )
                .await;
                let joined = receive(&mut socket, "RoomJoined").await;
                let schema = mapping_path(root, &["components", "schemas", "RoomJoined"])
                    .expect("reachable RoomJoined schema");
                assert!(
                    schema_shape_matches_version(root, schema, &joined, Some(i64::from(version))),
                    "v{version} runtime snapshot schema: {info}: {joined}"
                );
                ids.push(joined["data"]["player_id"].clone());
                assert_eq!(
                    joined["data"]["current_players"]
                        .as_array()
                        .expect("roster")
                        .len(),
                    index + 1
                );
                if index == 0 {
                    room_code = joined["data"]["room_code"].clone();
                    room_id = joined["data"]["room_id"].clone();
                    send(
                        &mut socket,
                        json!({"type":"ProvideConnectionInfo", "data":{
                            "connection_info":info
                        }}),
                    )
                    .await;
                    send(&mut socket, json!({"type":"Ping"})).await;
                    receive(&mut socket, "Pong").await;
                } else {
                    assert_eq!(joined["data"]["room_id"], room_id);
                    let members = joined["data"]["current_players"]
                        .as_array()
                        .expect("roster");
                    let actual_ids: std::collections::BTreeSet<_> = members
                        .iter()
                        .map(|member| member["id"].as_str().expect("player UUID"))
                        .collect();
                    let expected_ids: std::collections::BTreeSet<_> = ids
                        .iter()
                        .map(|id| id.as_str().expect("joined UUID"))
                        .collect();
                    assert_eq!(actual_ids, expected_ids);
                    let provider = members
                        .iter()
                        .find(|member| member["id"] == ids[0])
                        .expect("metadata provider is in snapshot");
                    if version == 2 {
                        assert_eq!(
                            provider.get("connection_info"),
                            Some(&info),
                            "v2 runtime snapshot preserves exact metadata: {joined}"
                        );
                        assert!(provider.get("connected_at").is_some());
                        assert!(provider.get("epoch").is_none());
                        assert!(provider.get("seq").is_none());
                    } else {
                        assert!(provider.get("connection_info").is_none());
                        assert!(provider.get("connected_at").is_none());
                        assert!(provider["epoch"].as_u64().is_some_and(|epoch| epoch > 0));
                        assert_eq!(provider["seq"], 0);
                    }
                }
                sockets.push(socket);
            }
            for socket in &mut sockets {
                send(socket, json!({"type":"PlayerReady"})).await;
                send(socket, json!({"type":"Ping"})).await;
                receive(socket, "Pong").await;
            }
            send(&mut sockets[0], json!({"type":"StartGame"})).await;
            for (socket, version) in sockets.iter_mut().zip([2_u16, 2, 3]) {
                let starting = receive(socket, "GameStarting").await;
                let schema = mapping_path(root, &["components", "schemas", "GameStarting"])
                    .expect("reachable GameStarting schema");
                assert!(
                    schema_shape_matches_version(root, schema, &starting, Some(i64::from(version))),
                    "v{version} runtime handoff schema: {info}: {starting}"
                );
                let peers = starting["data"]["peer_connections"]
                    .as_array()
                    .expect("handoff peers");
                let actual_ids: std::collections::BTreeSet<_> = peers
                    .iter()
                    .map(|peer| peer["player_id"].as_str().expect("peer UUID"))
                    .collect();
                let expected_ids: std::collections::BTreeSet<_> = ids
                    .iter()
                    .map(|id| id.as_str().expect("joined UUID"))
                    .collect();
                assert_eq!(actual_ids, expected_ids);
                assert_eq!(peers.len(), ids.len());
                for peer in peers {
                    let expected = (peer["player_id"] == ids[0]).then_some(&info);
                    assert_eq!(
                        peer.get("connection_info"),
                        expected,
                        "v{version} runtime handoff metadata: {info}: {starting}"
                    );
                }
            }
            for socket in &mut sockets {
                socket.close(None).await.expect("close scenario socket");
            }
            running.shutdown().await;
        }
    }

    /// #826: shared room events must keep identities and values when the same
    /// publication is projected to mixed v2/v3 players and spectators.
    #[tokio::test(flavor = "multi_thread")]
    async fn spectator_values_reach_mixed_version_snapshots_and_events_on_real_sockets() {
        let docs = Yaml::load_from_str(&spec_text()).expect("protocol YAML");
        let root = docs.first().expect("protocol document");
        let mut protocol = test_helpers::test_protocol_config();
        protocol.sdk_compatibility.enforce = false;
        let server = test_helpers::create_test_server_with_config(
            test_helpers::test_server_config(),
            protocol,
        )
        .await;
        let router = create_router("http://localhost:3000").with_state(server.clone());
        let running = test_helpers::RunningTestServer::spawn(server, router).await;
        let url = format!("ws://{}/ws", running.addr());
        let mut clients = Vec::new();
        let mut spectators = Vec::new();
        let mut players = Vec::new();
        let mut room_code = Value::Null;
        let mut room_id = Value::Null;
        // The later players see populated rosters in both versions. Each
        // spectator sees its own baseline and publications for later members.
        for (version, is_spectator, name) in [
            (2, false, "Host"),
            (2, true, "Watcher-観客"),
            (3, true, "Keeper-é"),
            (3, false, "Member-v3"),
            (2, false, "Member-v2"),
            (2, true, "Last-Ω"),
        ] {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(10), connect_async(&url))
                    .await
                    .expect("connect deadline")
                    .expect("connect WebSocket");
            send(
                &mut socket,
                json!({"type":"Authenticate", "data":{
                    "app_id":"spectator-runtime", "protocol_version":version
                }}),
            )
            .await;
            receive_exact(&mut socket, "Authenticated").await;
            let negotiation = receive_exact(&mut socket, "ProtocolInfo").await;
            assert_eq!(
                negotiation["data"]["protocol_version"],
                if version == 3 { json!(3) } else { Value::Null }
            );
            let (command, response, id_field) = if is_spectator {
                (
                    json!({"type":"JoinAsSpectator", "data":{
                        "game_name":"spectator-runtime", "room_code":room_code,
                        "spectator_name":name
                    }}),
                    "SpectatorJoined",
                    "spectator_id",
                )
            } else {
                (
                    json!({"type":"JoinRoom", "data":{
                        "game_name":"spectator-runtime", "room_code":room_code,
                        "player_name":name, "max_players":8, "supports_authority":false
                    }}),
                    "RoomJoined",
                    "player_id",
                )
            };
            send(&mut socket, command).await;
            let joined = receive_exact(&mut socket, response).await;
            assert_runtime_shape(root, &joined, version);
            let id = joined["data"][id_field].clone();
            assert!(id.as_str().is_some(), "assigned identity: {joined}");
            if clients.is_empty() {
                room_code = joined["data"]["room_code"].clone();
                room_id = joined["data"]["room_id"].clone();
            }
            assert_eq!(joined["data"]["room_code"], room_code);
            assert_eq!(joined["data"]["room_id"], room_id);
            if is_spectator {
                spectators.push((id.clone(), name));
                assert_eq!(joined["data"]["reason"], "joined");
            } else {
                players.push((id.clone(), name));
            }
            assert_spectator_roster(&joined["data"]["current_spectators"], &spectators, version);
            assert_roster_values(&joined["data"]["current_players"], &players);
            for (existing, recipient_version, _) in &mut clients {
                if is_spectator {
                    let event = receive_exact(existing, "NewSpectatorJoined").await;
                    assert_runtime_shape(root, &event, *recipient_version);
                    assert_spectator_entry(
                        &event["data"]["spectator"],
                        &id,
                        name,
                        *recipient_version,
                    );
                    assert_eq!(event["data"]["reason"], "joined");
                    assert_fanout_roster(&event, &spectators, *recipient_version);
                } else {
                    let event = receive_exact(existing, "PlayerJoined").await;
                    assert_runtime_shape(root, &event, *recipient_version);
                    assert_eq!(event["data"]["player"]["id"], id);
                    assert_eq!(event["data"]["player"]["name"], name);
                }
            }
            if clients.is_empty() {
                let lobby = receive_exact(&mut socket, "LobbyStateChanged").await;
                assert_runtime_shape(root, &lobby, version);
                assert_eq!(lobby["data"]["lobby_state"], "lobby");
                assert_eq!(lobby["data"]["ready_players"], json!([]));
                assert_eq!(lobby["data"]["all_ready"], false);
            }
            clients.push((socket, version, true));
        }
        // Both leave acknowledgments retain a populated roster before the
        // final spectator leaves. All live members see exact remaining values.
        for index in [1, 2, 5] {
            let (departed, _) = spectators.remove(0);
            send(&mut clients[index].0, json!({"type":"LeaveSpectator"})).await;
            let left = receive_exact(&mut clients[index].0, "SpectatorLeft").await;
            let version = clients[index].1;
            assert_runtime_shape(root, &left, version);
            assert_eq!(left["data"]["room_id"], room_id);
            assert_eq!(left["data"]["room_code"], room_code);
            assert_eq!(left["data"]["reason"], "voluntary_leave");
            assert_spectator_roster(&left["data"]["current_spectators"], &spectators, version);
            clients[index].2 = false;
            for (socket, version, in_room) in &mut clients {
                // Departed spectators are roomless and get no later fan-out.
                if !*in_room {
                    continue;
                }
                let event = receive_exact(socket, "SpectatorDisconnected").await;
                assert_runtime_shape(root, &event, *version);
                assert_eq!(event["data"]["spectator_id"], departed);
                assert_eq!(event["data"]["reason"], "voluntary_leave");
                assert_fanout_roster(&event, &spectators, *version);
            }
            // Ping is an ordered barrier, including on roomless sockets. The
            // exact next JSON frame catches duplicate or unexpected events.
            for (socket, _, _) in &mut clients {
                send(socket, json!({"type":"Ping"})).await;
                receive_exact(socket, "Pong").await;
            }
        }
        for (socket, _, _) in &mut clients {
            socket
                .close(None)
                .await
                .expect("close spectator scenario socket");
        }
        running.shutdown().await;
    }

    /// #826: exercise nested result projection at the production socket writer,
    /// with independent legacy recipients and canonical operation identities.
    #[tokio::test(flavor = "multi_thread")]
    async fn correlated_lifecycle_results_preserve_values_on_real_sockets() {
        let docs = Yaml::load_from_str(&spec_text()).expect("protocol YAML");
        let root = docs.first().expect("protocol document");
        let mut protocol = test_helpers::test_protocol_config();
        protocol.sdk_compatibility.enforce = false;
        let server = test_helpers::create_test_server_with_config(
            test_helpers::test_server_config(),
            protocol,
        )
        .await;
        let router = create_router("http://localhost:3000").with_state(server.clone());
        let running = test_helpers::RunningTestServer::spawn(server, router).await;
        let url = format!("ws://{}/ws", running.addr());
        let mut sockets = Vec::new();
        for version in [2, 2, 3] {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(10), connect_async(&url))
                    .await
                    .expect("connect deadline")
                    .expect("connect WebSocket");
            send(
                &mut socket,
                json!({"type":"Authenticate", "data":{
                    "app_id":"correlated-runtime", "protocol_version":version,
                    "requested_capabilities":["room_operation_ids"]
                }}),
            )
            .await;
            receive_exact(&mut socket, "Authenticated").await;
            let info = receive_exact(&mut socket, "ProtocolInfo").await;
            assert_eq!(
                info["data"]["protocol_version"],
                if version == 3 { json!(3) } else { Value::Null }
            );
            assert_eq!(
                info["data"]["capabilities"]
                    .as_array()
                    .expect("capabilities")
                    .contains(&json!("room_operation_ids")),
                version == 3
            );
            sockets.push(socket);
        }
        let mut actor = sockets.pop().unwrap();
        let mut watcher = sockets.pop().unwrap();
        let mut host = sockets.pop().unwrap();
        send(
            &mut host,
            json!({"type":"JoinRoom", "data":{
                "game_name":"correlated-runtime", "player_name":"Host-観客",
                "max_players":4, "supports_authority":false
            }}),
        )
        .await;
        let joined = receive_exact(&mut host, "RoomJoined").await;
        assert_runtime_shape(root, &joined, 2);
        let room_id = joined["data"]["room_id"].clone();
        let room_code = joined["data"]["room_code"].clone();
        let host_id = joined["data"]["player_id"].clone();
        receive_exact(&mut host, "LobbyStateChanged").await;
        let metadata = json!({"type":"custom", "data":{"nested":[null, false, "観客"]}});
        send(
            &mut host,
            json!({"type":"ProvideConnectionInfo", "data":{
                "connection_info":metadata
            }}),
        )
        .await;
        send(&mut host, json!({"type":"Ping"})).await;
        receive_exact(&mut host, "Pong").await;
        send(
            &mut watcher,
            json!({"type":"JoinAsSpectator", "data":{
                "game_name":"correlated-runtime", "room_code":room_code,
                "spectator_name":"Watcher-é"
            }}),
        )
        .await;
        let watched = receive_exact(&mut watcher, "SpectatorJoined").await;
        assert_runtime_shape(root, &watched, 2);
        let watcher_id = watched["data"]["spectator_id"].clone();
        assert_eq!(
            watched["data"]["current_players"][0]["connection_info"],
            metadata
        );
        receive_exact(&mut host, "NewSpectatorJoined").await;

        // Join-only makes the missing-room refusal independent of random codes.
        let failure = operation_result(
            root,
            &mut actor,
            1,
            json!({"type":"JoinRoom", "data":{
                "game_name":"missing-correlated-game", "room_code":room_code,
                "player_name":"Member-Ω", "join_only":true
            }}),
            "RoomJoinFailed",
        )
        .await;
        assert_eq!(failure["data"]["error_code"], "ROOM_NOT_FOUND");
        assert!(failure["data"]["reason"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
        let member = operation_result(
            root,
            &mut actor,
            2,
            json!({"type":"JoinRoom", "data":{
                "game_name":"correlated-runtime", "room_code":room_code,
                "player_name":"Member-Ω", "supports_authority":false, "join_only":true
            }}),
            "RoomJoined",
        )
        .await;
        let actor_id = member["data"]["player_id"].clone();
        assert_eq!(member["data"]["room_id"], room_id);
        assert_eq!(member["data"]["room_code"], room_code);
        assert_roster_values(
            &member["data"]["current_players"],
            &[
                (host_id.clone(), "Host-観客"),
                (actor_id.clone(), "Member-Ω"),
            ],
        );
        assert_v3_member_fields(&member["data"]["current_players"]);
        assert_spectator_roster(
            &member["data"]["current_spectators"],
            &[(watcher_id.clone(), "Watcher-é")],
            3,
        );
        for socket in [&mut host, &mut watcher] {
            let event = receive_exact(socket, "PlayerJoined").await;
            assert_runtime_shape(root, &event, 2);
            assert_eq!(event["data"]["player"]["id"], actor_id);
            assert_eq!(event["data"]["player"]["name"], "Member-Ω");
        }
        let left =
            operation_result(root, &mut actor, 3, json!({"type":"LeaveRoom"}), "RoomLeft").await;
        assert_eq!(left, json!({"type":"RoomLeft"}));
        for socket in [&mut host, &mut watcher] {
            let event = receive_exact(socket, "PlayerLeft").await;
            assert_runtime_shape(root, &event, 2);
            assert_eq!(event["data"]["player_id"], actor_id);
        }

        for (join_id, leave_id, populated) in [(4, 5, true), (6, 7, false)] {
            let joined = operation_result(
                root,
                &mut actor,
                join_id,
                json!({"type":"JoinAsSpectator", "data":{
                    "game_name":"correlated-runtime", "room_code":room_code,
                    "spectator_name":"Actor-Ω"
                }}),
                "SpectatorJoined",
            )
            .await;
            assert_eq!(joined["data"]["room_id"], room_id);
            assert_eq!(joined["data"]["room_code"], room_code);
            assert_eq!(joined["data"]["spectator_id"], actor_id);
            assert_eq!(joined["data"]["reason"], "joined");
            assert_roster_values(
                &joined["data"]["current_players"],
                &[(host_id.clone(), "Host-観客")],
            );
            assert_v3_member_fields(&joined["data"]["current_players"]);
            assert_spectator_roster(
                &joined["data"]["current_spectators"],
                &[
                    (watcher_id.clone(), "Watcher-é"),
                    (actor_id.clone(), "Actor-Ω"),
                ],
                3,
            );
            for socket in [&mut host, &mut watcher] {
                let event = receive_exact(socket, "NewSpectatorJoined").await;
                assert_runtime_shape(root, &event, 2);
                assert_spectator_entry(&event["data"]["spectator"], &actor_id, "Actor-Ω", 2);
                assert_fanout_roster(
                    &event,
                    &[
                        (watcher_id.clone(), "Watcher-é"),
                        (actor_id.clone(), "Actor-Ω"),
                    ],
                    2,
                );
            }
            if !populated {
                send(&mut watcher, json!({"type":"LeaveSpectator"})).await;
                let left = receive_exact(&mut watcher, "SpectatorLeft").await;
                assert_runtime_shape(root, &left, 2);
                assert_spectator_roster(
                    &left["data"]["current_spectators"],
                    &[(actor_id.clone(), "Actor-Ω")],
                    2,
                );
                receive_exact(&mut host, "SpectatorDisconnected").await;
                let event = receive_exact(&mut actor, "SpectatorDisconnected").await;
                assert_runtime_shape(root, &event, 3);
                assert_eq!(event["data"]["spectator_id"], watcher_id);
                assert_fanout_roster(&event, &[(actor_id.clone(), "Actor-Ω")], 3);
            }
            let left = operation_result(
                root,
                &mut actor,
                leave_id,
                json!({"type":"LeaveSpectator"}),
                "SpectatorLeft",
            )
            .await;
            assert_eq!(left["data"]["room_id"], room_id);
            assert_eq!(left["data"]["room_code"], room_code);
            assert_eq!(left["data"]["reason"], "voluntary_leave");
            let expected = if populated {
                vec![(watcher_id.clone(), "Watcher-é")]
            } else {
                vec![]
            };
            assert_spectator_roster(&left["data"]["current_spectators"], &expected, 3);
            let event = receive_exact(&mut host, "SpectatorDisconnected").await;
            assert_runtime_shape(root, &event, 2);
            assert_eq!(event["data"]["spectator_id"], actor_id);
            assert_fanout_roster(&event, &expected, 2);
            if populated {
                receive_exact(&mut watcher, "SpectatorDisconnected").await;
            }
            for socket in [&mut host, &mut watcher, &mut actor] {
                send(socket, json!({"type":"Ping"})).await;
                receive_exact(socket, "Pong").await;
            }
        }
        for socket in [&mut host, &mut watcher, &mut actor] {
            socket
                .close(None)
                .await
                .expect("close correlated scenario socket");
        }
        running.shutdown().await;
    }

    /// #826: reconnect and replay must cross the actual correlated writer seam.
    #[tokio::test(flavor = "multi_thread")]
    async fn correlated_reconnect_preserves_snapshot_and_replay_on_real_sockets() {
        let docs = Yaml::load_from_str(&spec_text()).expect("protocol YAML");
        let root = docs.first().expect("protocol document");
        let mut protocol = test_helpers::test_protocol_config();
        protocol.sdk_compatibility.enforce = false;
        let server = test_helpers::create_test_server_with_config(
            test_helpers::test_server_config(),
            protocol,
        )
        .await;
        let router = create_router("http://localhost:3000").with_state(server.clone());
        let running = test_helpers::RunningTestServer::spawn(server.clone(), router).await;
        let url = format!("ws://{}/ws", running.addr());
        let mut sockets = Vec::new();
        for version in [2, 3, 2, 3] {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(10), connect_async(&url))
                    .await
                    .expect("connect deadline")
                    .expect("connect WebSocket");
            send(
                &mut socket,
                json!({"type":"Authenticate", "data":{
                    "app_id":"reconnect-runtime", "protocol_version":version,
                    "requested_capabilities":["room_operation_ids"]
                }}),
            )
            .await;
            receive_exact(&mut socket, "Authenticated").await;
            let info = receive_exact(&mut socket, "ProtocolInfo").await;
            assert_eq!(
                info["data"]["capabilities"]
                    .as_array()
                    .expect("capabilities")
                    .contains(&json!("room_operation_ids")),
                version == 3
            );
            sockets.push(socket);
        }
        let mut retry = sockets.pop().unwrap();
        let mut watcher = sockets.pop().unwrap();
        let mut original = sockets.pop().unwrap();
        let mut host = sockets.pop().unwrap();
        send(
            &mut host,
            json!({"type":"JoinRoom", "data":{
                "game_name":"reconnect-runtime", "player_name":"Host-観客",
                "max_players":4, "supports_authority":false
            }}),
        )
        .await;
        let room = receive_exact(&mut host, "RoomJoined").await;
        let room_id = room["data"]["room_id"].clone();
        let room_code = room["data"]["room_code"].clone();
        let host_id = room["data"]["player_id"].clone();
        receive_exact(&mut host, "LobbyStateChanged").await;
        let metadata = json!({"type":"custom", "data":{"nested":[null, false, "観客"]}});
        send(
            &mut host,
            json!({"type":"ProvideConnectionInfo", "data":{
                "connection_info":metadata
            }}),
        )
        .await;
        send(&mut host, json!({"type":"Ping"})).await;
        receive_exact(&mut host, "Pong").await;
        let joined = operation_result(
            root,
            &mut original,
            101,
            json!({
                "type":"JoinRoom", "data":{
                    "game_name":"reconnect-runtime", "room_code":room_code,
                    "player_name":"Recover-é", "supports_authority":false, "join_only":true
                }
            }),
            "RoomJoined",
        )
        .await;
        let player_id = joined["data"]["player_id"].clone();
        let token = joined["data"]["reconnection_token"]
            .as_str()
            .expect("join token")
            .to_owned();
        let old_epoch = joined["data"]["current_players"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == player_id)
            .unwrap()["epoch"]
            .as_u64()
            .unwrap();
        receive_exact(&mut host, "PlayerJoined").await;
        // A TCP loss creates the pending record through the production lifecycle.
        drop(original);
        let identity = uuid::Uuid::parse_str(player_id.as_str().unwrap()).unwrap();
        let manager = server.reconnection_manager().expect("reconnect manager");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !manager.has_pending_reconnection(&identity).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("physical loss arms reconnect");
        let left = receive_exact(&mut host, "PlayerLeft").await;
        assert_eq!(left["data"]["player_id"], player_id);

        // A spectator joins during absence. Its legacy event is an independent
        // witness for both replay identity and v3 delta projection.
        send(
            &mut watcher,
            json!({"type":"JoinAsSpectator", "data":{
                "game_name":"reconnect-runtime", "room_code":room_code,
                "spectator_name":"Missed-Ω"
            }}),
        )
        .await;
        let watched = receive_exact(&mut watcher, "SpectatorJoined").await;
        assert_runtime_shape(root, &watched, 2);
        let watcher_id = watched["data"]["spectator_id"].clone();
        assert_eq!(
            watched["data"]["current_players"][0]["connection_info"],
            metadata
        );
        let live = receive_exact(&mut host, "NewSpectatorJoined").await;
        assert_runtime_shape(root, &live, 2);
        assert_fanout_roster(&live, &[(watcher_id.clone(), "Missed-Ω")], 2);
        let failed = operation_result(
            root,
            &mut retry,
            102,
            json!({
                "type":"Reconnect", "data":{
                    "player_id":player_id, "room_id":room_id, "auth_token":"wrong-token"
                }
            }),
            "ReconnectionFailed",
        )
        .await;
        assert_eq!(failed["data"]["error_code"], "RECONNECTION_TOKEN_INVALID");
        assert!(
            manager.has_pending_reconnection(&identity).await,
            "a wrong token must not consume pending recovery"
        );
        let restored = operation_result(
            root,
            &mut retry,
            103,
            json!({
                "type":"Reconnect", "data":{
                    "player_id":player_id, "room_id":room_id, "auth_token":token
                }
            }),
            "Reconnected",
        )
        .await;
        let data = &restored["data"];
        assert_eq!(data["room_id"], room_id);
        assert_eq!(data["room_code"], room_code);
        assert_eq!(data["player_id"], player_id);
        assert_eq!(data["game_name"], "reconnect-runtime");
        assert_eq!(data["max_players"], 4);
        assert_eq!(data["supports_authority"], false);
        assert_eq!(data["is_authority"], false);
        assert_eq!(data["lobby_state"], "lobby");
        assert_eq!(data["ready_players"], json!([]));
        assert_eq!(data["replay"], "complete");
        let rotated = data["reconnection_token"].as_str().expect("rotated token");
        assert!(!rotated.is_empty());
        assert_ne!(rotated, token);
        assert_roster_values(
            &data["current_players"],
            &[
                (host_id.clone(), "Host-観客"),
                (player_id.clone(), "Recover-é"),
            ],
        );
        assert_v3_member_fields(&data["current_players"]);
        assert_spectator_roster(
            &data["current_spectators"],
            &[(watcher_id.clone(), "Missed-Ω")],
            3,
        );
        let players = data["current_players"].as_array().unwrap();
        let recovered = players.iter().find(|p| p["id"] == player_id).unwrap();
        assert!(recovered["epoch"].as_u64().unwrap() > old_epoch);
        let watermarks = data["sender_watermarks"]
            .as_array()
            .expect("sender watermarks");
        assert_eq!(watermarks.len(), players.len());
        for player in players {
            let watermark = watermarks
                .iter()
                .find(|w| w["player_id"] == player["id"])
                .expect("each current sender has a watermark");
            assert_eq!(watermark["epoch"], player["epoch"]);
            assert_eq!(watermark["seq"], player["seq"]);
        }
        let replay = data["missed_events"].as_array().expect("replay array");
        assert_eq!(
            replay.len(),
            1,
            "only the missed spectator delta is replayed: {replay:?}"
        );
        assert_eq!(replay[0]["type"], "NewSpectatorJoined");
        assert_eq!(replay[0]["data"]["reason"], live["data"]["reason"]);
        assert_spectator_entry(&replay[0]["data"]["spectator"], &watcher_id, "Missed-Ω", 3);
        assert_fanout_roster(&replay[0], &[(watcher_id.clone(), "Missed-Ω")], 3);
        for socket in [&mut host, &mut watcher] {
            let event = receive_exact(socket, "PlayerReconnected").await;
            assert_runtime_shape(root, &event, 2);
            assert_eq!(event["data"]["player_id"], player_id);
            assert_eq!(event["data"]["player"]["id"], player_id);
            assert_eq!(event["data"]["player"]["name"], "Recover-é");
            assert!(event["data"]["player"]["connected_at"].is_string());
            assert!(event["data"].get("epoch").is_none());
        }
        for socket in [&mut host, &mut watcher, &mut retry] {
            send(socket, json!({"type":"Ping"})).await;
            receive_exact(socket, "Pong").await;
        }
        for socket in [&mut host, &mut watcher, &mut retry] {
            socket
                .close(None)
                .await
                .expect("close reconnect scenario socket");
        }
        running.shutdown().await;
    }

    async fn operation_result(
        root: &Yaml<'_>,
        socket: &mut Socket,
        id: u128,
        operation: Value,
        tag: &str,
    ) -> Value {
        let id = uuid::Uuid::from_u128(id).to_string();
        send(
            socket,
            json!({"type":"RoomOperation", "data":{
                "operation_id":id, "operation":operation
            }}),
        )
        .await;
        let frame = receive_exact(socket, "RoomOperationResult").await;
        assert_eq!(
            frame["data"]["operation_id"], id,
            "exact correlation: {frame}"
        );
        let result = &frame["data"]["result"];
        assert_eq!(result["type"], tag, "exact nested result: {frame}");
        assert_runtime_shape(root, &frame, 3);
        result.clone()
    }

    fn assert_v3_member_fields(roster: &Value) {
        for member in roster.as_array().expect("member roster") {
            assert!(
                member.get("connection_info").is_none(),
                "v3 metadata omission: {member}"
            );
            assert!(
                member.get("connected_at").is_none(),
                "v3 timestamp omission: {member}"
            );
            assert!(member["epoch"].as_u64().is_some_and(|epoch| epoch > 0));
            assert_eq!(member["seq"], 0);
        }
    }

    async fn receive_exact(socket: &mut Socket, tag: &str) -> Value {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, socket.next())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for next {tag}"))
                .expect("socket remains open")
                .expect("read spectator scenario frame");
            match frame {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text).expect("raw JSON frame");
                    assert_eq!(value["type"], tag, "exact next message: {value}");
                    return value;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("expected next JSON {tag}, got {other:?}"),
            }
        }
    }

    fn assert_runtime_shape(root: &Yaml<'_>, frame: &Value, version: i64) {
        let tag = frame["type"].as_str().expect("message tag");
        let schema =
            mapping_path(root, &["components", "schemas", tag]).expect("reachable server schema");
        assert!(
            schema_shape_matches_version(root, schema, frame, Some(version)),
            "v{version} runtime {tag} schema shape: {frame}"
        );
    }

    fn assert_roster_values(roster: &Value, expected: &[(Value, &str)]) {
        let roster = roster.as_array().expect("roster array");
        assert_eq!(
            roster.len(),
            expected.len(),
            "exact roster size: {roster:?}"
        );
        let actual: std::collections::BTreeSet<_> = roster
            .iter()
            .map(|entry| {
                (
                    entry["id"].as_str().expect("member UUID"),
                    entry["name"].as_str().expect("member name"),
                )
            })
            .collect();
        let expected: std::collections::BTreeSet<_> = expected
            .iter()
            .map(|(id, name)| (id.as_str().expect("assigned UUID"), *name))
            .collect();
        assert_eq!(actual, expected, "exact roster identities and names");
    }

    fn assert_spectator_entry(entry: &Value, id: &Value, name: &str, version: i64) {
        assert_eq!(&entry["id"], id, "exact spectator identity: {entry}");
        assert_eq!(entry["name"], name, "exact spectator name: {entry}");
        let keys: std::collections::BTreeSet<_> = entry
            .as_object()
            .expect("spectator object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected = if version == 2 {
            std::collections::BTreeSet::from(["id", "name", "connected_at"])
        } else {
            std::collections::BTreeSet::from(["id", "name"])
        };
        assert_eq!(keys, expected, "v{version} spectator projection: {entry}");
        if version == 2 {
            str::parse::<jiff::Timestamp>(entry["connected_at"].as_str().expect("v2 timestamp"))
                .expect("valid v2 timestamp");
        }
    }

    fn assert_spectator_roster(roster: &Value, expected: &[(Value, &str)], version: i64) {
        assert_roster_values(roster, expected);
        for entry in roster.as_array().expect("spectator roster") {
            let (id, name) = expected
                .iter()
                .find(|(id, _)| entry["id"] == *id)
                .expect("known spectator");
            assert_spectator_entry(entry, id, name, version);
        }
    }

    fn assert_fanout_roster(frame: &Value, expected: &[(Value, &str)], version: i64) {
        let data = &frame["data"];
        if version == 2 {
            assert!(
                data.get("spectator_count").is_none(),
                "frozen v2 count omission: {frame}"
            );
            assert_spectator_roster(&data["current_spectators"], expected, version);
        } else {
            assert_eq!(
                data["spectator_count"],
                expected.len(),
                "v3 post-change count: {frame}"
            );
            assert_eq!(
                data["current_spectators"],
                json!([]),
                "v3 delta roster: {frame}"
            );
        }
    }
}
