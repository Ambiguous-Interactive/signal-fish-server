//! Property-based wire-format invariants for the Protocol v3 surface
//! (Deliverable C2, wire level).
//!
//! Complements the example-based suites (`tests/v3_protocol_samples.rs` pins
//! the canonical samples; `tests/v2_wire_golden.rs` freezes the v2 bytes) with
//! randomized round-trip properties over the v3 message types:
//!
//! - `Signal` (client->server and server->client) must round-trip ARBITRARY
//!   opaque JSON payloads through both wire encodings the server speaks:
//!   JSON text frames (`serde_json`) and MessagePack binary frames
//!   (`rmp_serde::to_vec_named`, equivalent to production's named MessagePack
//!   relay encoding). The server never inspects `signal`
//!   (ADR-0002), so payload fidelity is the entire contract.
//!
//!   FINDING (pinned, not papered over): with the default serde_json feature
//!   set (no `float_roundtrip`, which is what production compiles), JSON
//!   *parsing* of `f64` values is approximate — a value can drift by up to
//!   ~2 ULP per text-frame hop (measured max 2 ULP over 2M random bit
//!   patterns; roughly 30% of random f64s drift at all, and the drift does
//!   not always converge after one hop). MessagePack frames are bit-exact
//!   (`0xcb` + 8 raw bytes). The properties below therefore assert EXACT
//!   round-trips for all float-free JSON and for ALL MessagePack payloads,
//!   and pin the bounded-ULP behavior for JSON floats explicitly. If exact
//!   float fidelity on the JSON path is ever required, enable serde_json's
//!   `float_roundtrip` feature (a deliberate perf tradeoff left to the
//!   maintainers — matchbox-shaped signal payloads carry only strings, so no
//!   current client is affected).
//! - `SessionPlanPayload` / `SessionPeer` / `IceServer` / `Topology` /
//!   `Transport` round-trip with randomized contents (including the
//!   `skip_serializing_if` optional fields).
//! - `Authenticate` optional fields serialize present-iff-`Some` and
//!   round-trip in both encodings.
//! - TURN credential minting (`src/security/turn_credentials.rs`) is
//!   deterministic, per-player distinct, embeds `expiry = now + ttl` in the
//!   username, and the credential equals an INDEPENDENTLY recomputed
//!   `base64(HMAC-SHA1(secret, username))` (the coturn REST contract,
//!   Appendix F).
//!
//! Equality is asserted via `serde_json::to_value` canonicalization (the
//! payload types deliberately do not implement `PartialEq`).
//!
//! All proptest tests carry `#[cfg_attr(miri, ignore)]` per repository policy
//! (tests/ci_config_tests.rs::test_proptest_tests_ignored_under_miri); Miri
//! does not run integration tests, but the annotation keeps the policy
//! uniform.

use proptest::prelude::*;
use serde_json::{json, Value};
use signal_fish_server::config::TurnConfig;
use signal_fish_server::protocol::{
    ClientMessage, DirectEndpoint, GameDataEncoding, IceServer, ServerMessage, SessionPeer,
    SessionPlanPayload, Topology, Transport,
};
use signal_fish_server::security::{build_ice_servers, mint_turn_credentials, turn_expiry_unix};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Bounded-depth arbitrary JSON built over the given leaf strategy,
/// recursively nested into arrays and objects. Depth/size bounds keep cases
/// fast while still exercising deep mixed nesting.
fn arb_json_over(
    leaf: impl Strategy<Value = Value> + 'static,
    depth: u32,
) -> impl Strategy<Value = Value> {
    leaf.prop_recursive(depth, 64, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::from),
            proptest::collection::btree_map(".{0,8}", inner, 0..6)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

/// Every scalar JSON shape including finite floats — usable for the
/// bit-exact MessagePack path (see the module FINDING note for why the JSON
/// text path cannot promise exactness for floats).
fn arb_json(depth: u32) -> impl Strategy<Value = Value> {
    arb_json_over(
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            // Non-finite floats are not representable in JSON; serde_json's
            // Number::from_f64 returns None for them, so generate finite only.
            any::<f64>()
                .prop_filter("finite floats only", |f| f.is_finite())
                .prop_map(Value::from),
            ".{0,16}".prop_map(Value::from),
        ],
        depth,
    )
}

/// Arbitrary JSON without float leaves: null/bool/u64/i64/strings nest
/// arbitrarily. Integers parse exactly, so this subset round-trips the JSON
/// text path byte-faithfully.
fn arb_json_no_floats(depth: u32) -> impl Strategy<Value = Value> {
    arb_json_over(
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<u64>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            ".{0,16}".prop_map(Value::from),
        ],
        depth,
    )
}

fn arb_uuid() -> impl Strategy<Value = Uuid> {
    any::<u128>().prop_map(Uuid::from_u128)
}

fn arb_transport() -> impl Strategy<Value = Transport> {
    proptest::sample::select(vec![Transport::Relay, Transport::Direct, Transport::WebRtc])
}

fn arb_topology() -> impl Strategy<Value = Topology> {
    proptest::sample::select(vec![Topology::Relay, Topology::Host, Topology::Mesh])
}

fn arb_ice_server() -> impl Strategy<Value = IceServer> {
    (
        proptest::collection::vec("[a-z0-9:./?=-]{1,32}", 0..4),
        proptest::option::of("[ -~]{1,48}"),
        proptest::option::of("[ -~]{1,48}"),
    )
        .prop_map(|(urls, username, credential)| IceServer {
            urls,
            username,
            credential,
        })
}

fn arb_session_peer() -> impl Strategy<Value = SessionPeer> {
    (arb_uuid(), ".{0,24}", any::<bool>(), any::<bool>()).prop_map(
        |(player_id, player_name, is_authority, initiate)| SessionPeer {
            player_id,
            player_name,
            is_authority,
            initiate,
        },
    )
}

fn arb_session_plan() -> impl Strategy<Value = SessionPlanPayload> {
    (
        arb_topology(),
        arb_transport(),
        proptest::option::of(arb_uuid()),
        proptest::option::of(
            ("[a-z]{1,12}", 1u16..=u16::MAX).prop_map(|(host, port)| DirectEndpoint { host, port }),
        ),
        proptest::collection::vec(arb_session_peer(), 0..5),
        proptest::collection::vec(arb_ice_server(), 0..3),
        arb_transport(),
    )
        .prop_map(
            |(topology, transport, host, direct_endpoint, peers, ice_servers, fallback)| {
                SessionPlanPayload {
                    topology,
                    transport,
                    host,
                    direct_endpoint,
                    peers,
                    ice_servers,
                    fallback,
                }
            },
        )
}

// ---------------------------------------------------------------------------
// Round-trip helpers
// ---------------------------------------------------------------------------

/// Canonical JSON view used for equality (the wire types have no PartialEq).
fn canonical<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("wire type serializes to a JSON value")
}

/// JSON text-frame round trip, exactly as the WebSocket text path encodes.
fn json_roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    let encoded = serde_json::to_string(value).expect("JSON-encodes");
    serde_json::from_str(&encoded).expect("JSON round-trip decodes")
}

/// MessagePack round trip via named encoding, equivalent to the production
/// relay encoder; `from_slice` is the production decoder.
fn msgpack_roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    let encoded = rmp_serde::to_vec_named(value).expect("MessagePack-encodes");
    rmp_serde::from_slice(&encoded).expect("MessagePack round-trip decodes")
}

// ---------------------------------------------------------------------------
// Signal: opaque payload fidelity (both directions, both encodings)
// ---------------------------------------------------------------------------

/// Pull the opaque payload back out of a decoded client `Signal`.
fn client_signal_parts(message: ClientMessage) -> (Uuid, Value) {
    match message {
        ClientMessage::Signal { to, signal } => (to, signal),
        other => panic!("decoded to a different variant: {other:?}"),
    }
}

/// Pull the opaque payload back out of a decoded server `Signal`.
fn server_signal_parts(message: ServerMessage) -> (Uuid, Value) {
    match message {
        ServerMessage::Signal { from, signal } => (from, signal),
        other => panic!("decoded to a different variant: {other:?}"),
    }
}

proptest! {
    /// JSON text frames: `Signal` carries any FLOAT-FREE opaque payload
    /// through a parse/serialize cycle without any alteration in either
    /// direction (the server relays it verbatim and never parses it —
    /// ADR-0002). Floats are excluded here because serde_json's default
    /// parser is approximate for f64 (see the module FINDING note); their
    /// bounded drift is pinned separately below.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn signal_json_roundtrips_float_free_payloads_exactly(
        id in arb_uuid(),
        signal in arb_json_no_floats(4),
    ) {
        let client = ClientMessage::Signal { to: id, signal: signal.clone() };
        let (to, decoded) = client_signal_parts(json_roundtrip(&client));
        prop_assert_eq!(to, id);
        prop_assert_eq!(&decoded, &signal, "opaque payload must be preserved exactly");

        let server = ServerMessage::Signal { from: id, signal: signal.clone() };
        let (from, decoded) = server_signal_parts(json_roundtrip(&server));
        prop_assert_eq!(from, id);
        prop_assert_eq!(&decoded, &signal, "opaque payload must be preserved exactly");
    }

    /// MessagePack binary frames: `Signal` round-trips ARBITRARY payloads —
    /// floats included — bit-exactly in both directions (f64 is encoded as
    /// 0xcb + 8 raw bytes; no decimal text is involved).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn signal_msgpack_roundtrips_arbitrary_payloads_exactly(
        id in arb_uuid(),
        signal in arb_json(4),
    ) {
        let client = ClientMessage::Signal { to: id, signal: signal.clone() };
        let (to, decoded) = client_signal_parts(msgpack_roundtrip(&client));
        prop_assert_eq!(to, id);
        prop_assert_eq!(&decoded, &signal, "MessagePack must preserve the payload bit-exactly");

        let server = ServerMessage::Signal { from: id, signal: signal.clone() };
        let (from, decoded) = server_signal_parts(msgpack_roundtrip(&server));
        prop_assert_eq!(from, id);
        prop_assert_eq!(&decoded, &signal, "MessagePack must preserve the payload bit-exactly");
    }

    /// JSON float drift is BOUNDED: a float relayed through one JSON
    /// parse/serialize hop is within a few ULP of the original (observed max
    /// 2 ULP over 2M random bit patterns; 4 is asserted as a safety margin).
    /// This pins today's behavior so an unbounded regression — or a silent
    /// fix — is caught either way. Exactness would require serde_json's
    /// `float_roundtrip` feature.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn signal_json_float_drift_is_bounded(id in arb_uuid(), bits in any::<u64>()) {
        let float = f64::from_bits(bits);
        prop_assume!(float.is_finite());

        let message = ClientMessage::Signal { to: id, signal: json!({ "f": float }) };
        let (_, decoded) = client_signal_parts(json_roundtrip(&message));
        let roundtripped = decoded
            .get("f")
            .and_then(Value::as_f64)
            .expect("float survives as a number");

        let ulp_distance = (roundtripped.to_bits() as i64).abs_diff(float.to_bits() as i64);
        prop_assert!(
            ulp_distance <= 4,
            "JSON float drift exceeded the pinned bound: {float} -> {roundtripped} ({ulp_distance} ULP)"
        );
    }
}

// ---------------------------------------------------------------------------
// v3 payload types: randomized round trips
// ---------------------------------------------------------------------------

proptest! {
    /// `SessionPlanPayload` (with nested peers + ICE incl. optional TURN
    /// credentials) round-trips both encodings, including the
    /// `skip_serializing_if` optionals (`host`, empty `ice_servers`).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn session_plan_payload_roundtrips(plan in arb_session_plan()) {
        let original = canonical(&plan);
        prop_assert_eq!(canonical(&json_roundtrip(&plan)), original.clone());
        prop_assert_eq!(canonical(&msgpack_roundtrip(&plan)), original.clone());

        // The optional fields are omitted from the JSON wire exactly when
        // absent/empty (frozen-wire additivity: absent means "not present").
        let object = original.as_object().expect("payload serializes to an object");
        prop_assert_eq!(object.contains_key("host"), plan.host.is_some());
        prop_assert_eq!(object.contains_key("ice_servers"), !plan.ice_servers.is_empty());
    }

    /// `SessionPeer` and `IceServer` round-trip with randomized contents;
    /// credential-less ICE entries omit `username`/`credential` on the wire.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn session_peer_and_ice_server_roundtrip(peer in arb_session_peer(), ice in arb_ice_server()) {
        prop_assert_eq!(canonical(&json_roundtrip(&peer)), canonical(&peer));
        prop_assert_eq!(canonical(&msgpack_roundtrip(&peer)), canonical(&peer));
        prop_assert_eq!(canonical(&json_roundtrip(&ice)), canonical(&ice));
        prop_assert_eq!(canonical(&msgpack_roundtrip(&ice)), canonical(&ice));

        let ice_value = canonical(&ice);
        let ice_object = ice_value.as_object().expect("ice serializes to an object");
        prop_assert_eq!(ice_object.contains_key("username"), ice.username.is_some());
        prop_assert_eq!(ice_object.contains_key("credential"), ice.credential.is_some());
    }

    /// The two capability enums use the Appendix A lowercase wire tokens
    /// (`webrtc`, not `web_rtc`) and round-trip both encodings.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn topology_and_transport_tokens_roundtrip(topology in arb_topology(), transport in arb_transport()) {
        let topology_token = canonical(&topology);
        let transport_token = canonical(&transport);

        let valid_topology_tokens = [json!("relay"), json!("host"), json!("mesh")];
        let valid_transport_tokens = [json!("relay"), json!("direct"), json!("webrtc")];
        prop_assert!(valid_topology_tokens.contains(&topology_token));
        prop_assert!(valid_transport_tokens.contains(&transport_token));

        prop_assert_eq!(json_roundtrip(&topology), topology);
        prop_assert_eq!(msgpack_roundtrip(&topology), topology);
        prop_assert_eq!(json_roundtrip(&transport), transport);
        prop_assert_eq!(msgpack_roundtrip(&transport), transport);
    }

    /// `PeerTransportStatus` (the server-side fan-out of an accepted
    /// `TransportStatus` state change) round-trips both encodings for every
    /// `(peer_id, transport, connected)` combination, with the exact
    /// externally-tagged wire shape (`type` tag + `peer_id` / `transport` /
    /// `connected` data fields, lowercase transport tokens).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn peer_transport_status_roundtrips(
        peer_id in arb_uuid(),
        transport in arb_transport(),
        connected in any::<bool>(),
    ) {
        let message = ServerMessage::PeerTransportStatus {
            peer_id,
            transport,
            connected,
        };

        let original = canonical(&message);
        prop_assert_eq!(canonical(&json_roundtrip(&message)), original.clone());
        prop_assert_eq!(canonical(&msgpack_roundtrip(&message)), original.clone());

        prop_assert_eq!(
            original.get("type").cloned(),
            Some(json!("PeerTransportStatus"))
        );
        let data = original
            .get("data")
            .and_then(Value::as_object)
            .expect("PeerTransportStatus serializes data as an object");
        prop_assert_eq!(data.get("peer_id").cloned(), Some(json!(peer_id.to_string())));
        prop_assert_eq!(data.get("transport").cloned(), Some(canonical(&transport)));
        prop_assert_eq!(data.get("connected").cloned(), Some(json!(connected)));
        prop_assert_eq!(data.len(), 3, "exactly the three documented fields");
    }
}

// ---------------------------------------------------------------------------
// Authenticate: optional-field presence/absence
// ---------------------------------------------------------------------------

fn arb_authenticate() -> impl Strategy<Value = ClientMessage> {
    (
        "[a-z0-9_]{1,24}",
        proptest::option::of("[0-9.]{1,12}"),
        proptest::option::of("[a-z]{1,12}"),
        proptest::option::of(proptest::sample::select(vec![
            GameDataEncoding::Json,
            GameDataEncoding::MessagePack,
        ])),
        proptest::option::of(2u16..=4u16),
        proptest::option::of(proptest::collection::vec(arb_transport(), 0..4)),
        proptest::option::of(proptest::collection::vec(arb_topology(), 0..4)),
    )
        .prop_map(
            |(
                app_id,
                sdk_version,
                platform,
                game_data_format,
                protocol_version,
                supported_transports,
                supported_topologies,
            )| ClientMessage::Authenticate {
                app_id,
                sdk_version,
                platform,
                game_data_format,
                protocol_version,
                supported_transports,
                supported_topologies,
            },
        )
}

proptest! {
    /// Every optional `Authenticate` field appears on the JSON wire iff it is
    /// `Some` (`skip_serializing_if` + `default`: absent fields deserialize
    /// to `None`, so v2 senders that omit the v3 fields stay byte-identical),
    /// and the whole message round-trips both encodings.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn authenticate_optional_fields_roundtrip(message in arb_authenticate()) {
        let original = canonical(&message);
        prop_assert_eq!(canonical(&json_roundtrip(&message)), original.clone());
        prop_assert_eq!(canonical(&msgpack_roundtrip(&message)), original.clone());

        let data = original
            .get("data")
            .and_then(Value::as_object)
            .expect("Authenticate serializes data as an object");
        let ClientMessage::Authenticate {
            sdk_version,
            platform,
            game_data_format,
            protocol_version,
            supported_transports,
            supported_topologies,
            ..
        } = &message
        else {
            unreachable!("strategy only builds Authenticate");
        };

        prop_assert!(data.contains_key("app_id"), "app_id is mandatory");
        prop_assert_eq!(data.contains_key("sdk_version"), sdk_version.is_some());
        prop_assert_eq!(data.contains_key("platform"), platform.is_some());
        prop_assert_eq!(data.contains_key("game_data_format"), game_data_format.is_some());
        prop_assert_eq!(data.contains_key("protocol_version"), protocol_version.is_some());
        prop_assert_eq!(
            data.contains_key("supported_transports"),
            supported_transports.is_some()
        );
        prop_assert_eq!(
            data.contains_key("supported_topologies"),
            supported_topologies.is_some()
        );
    }
}

// ---------------------------------------------------------------------------
// TURN credentials (coturn REST scheme, Appendix F)
// ---------------------------------------------------------------------------

/// Independent recomputation of `base64(HMAC-SHA1(secret, message))` so the
/// production credential is checked against the documented construction, not
/// against itself.
fn expected_hmac_credential(secret: &str, message: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use hmac::{Hmac, KeyInit, Mac};

    let mut mac = <Hmac<sha1::Sha1> as KeyInit>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}

proptest! {
    /// Minting is a pure function: identical (secret, player, expiry) yields
    /// identical pairs; the username embeds exactly `{expiry}:{player_id}`;
    /// the credential is the independently recomputed HMAC.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn turn_minting_is_deterministic_and_matches_hmac(
        secret in "[ -~]{1,48}",
        player in arb_uuid(),
        expiry in 0i64..=4_102_444_800i64,
    ) {
        let first = mint_turn_credentials(&secret, player, expiry);
        let second = mint_turn_credentials(&secret, player, expiry);
        prop_assert_eq!(&first, &second, "same inputs must mint the same pair");

        prop_assert_eq!(&first.username, &format!("{expiry}:{player}"));
        prop_assert_eq!(
            &first.credential,
            &expected_hmac_credential(&secret, &first.username),
            "credential must equal base64(HMAC-SHA1(secret, username))"
        );
    }

    /// Distinct players minted under the same secret + expiry receive
    /// distinct usernames AND distinct credentials (per-recipient pairs).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn turn_minting_distinguishes_players(
        secret in "[ -~]{1,48}",
        (a, b) in (any::<u128>(), any::<u128>()).prop_filter("distinct players", |(a, b)| a != b),
        expiry in 0i64..=4_102_444_800i64,
    ) {
        let creds_a = mint_turn_credentials(&secret, Uuid::from_u128(a), expiry);
        let creds_b = mint_turn_credentials(&secret, Uuid::from_u128(b), expiry);
        prop_assert_ne!(&creds_a.username, &creds_b.username);
        prop_assert_ne!(&creds_a.credential, &creds_b.credential);
    }

    /// The full ICE builder embeds `expiry = now + ttl` (saturating) in the
    /// TURN entry's username for the exact recipient it was minted for, and
    /// is deterministic for the same (config, player, now).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn built_ice_turn_entry_embeds_now_plus_ttl(
        player in arb_uuid(),
        now in 0i64..=4_102_444_800i64,
        ttl in 1u64..=86_400u64,
        secret in "[ -~]{1,48}",
    ) {
        let turn = TurnConfig {
            enabled: true,
            static_auth_secret: secret.clone(),
            urls: vec!["turn:turn.example.com:3478".to_string()],
            stun_urls: vec!["stun:stun.example.com:19302".to_string()],
            credential_ttl_secs: ttl,
        };

        let servers = build_ice_servers(&turn, player, now);
        prop_assert_eq!(servers.len(), 2, "STUN entry then TURN entry");
        prop_assert!(servers[0].username.is_none(), "public STUN is credential-less");

        let username = servers[1].username.as_ref().expect("TURN entry has a username");
        let expected_expiry = turn_expiry_unix(now, ttl);
        prop_assert_eq!(username, &format!("{expected_expiry}:{player}"));
        let credential = servers[1].credential.as_ref().expect("TURN entry has a credential");
        prop_assert_eq!(credential, &expected_hmac_credential(&secret, username));

        // Determinism across invocations with the same captured `now`.
        let again = build_ice_servers(&turn, player, now);
        prop_assert_eq!(canonical(&servers), canonical(&again));
    }
}
