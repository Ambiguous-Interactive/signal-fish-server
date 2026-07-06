//! Protocol v3 (P1) capability-negotiation wire + helper tests.
//!
//! Covers Transport/Topology serde tokens (Appendix A), `Authenticate`
//! round-tripping with and without the new optional fields, and the
//! `ProtocolInfoPayload` version fields. The byte-for-byte v2 freeze lives in
//! `v2_wire_golden.rs`; this file asserts the *additive* v3 surface.

use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, GameDataEncoding, IceServer, PlayerId, ProtocolInfoPayload, ServerMessage,
    SessionPeer, SessionPlanPayload, Topology, Transport,
};

// ---------------------------------------------------------------------------
// Transport / Topology serde tokens (Appendix A): `webrtc`, not `web_rtc`.
// ---------------------------------------------------------------------------

#[test]
fn transport_serializes_to_appendix_a_tokens() {
    assert_eq!(
        serde_json::to_string(&Transport::Relay).unwrap(),
        r#""relay""#
    );
    assert_eq!(
        serde_json::to_string(&Transport::Direct).unwrap(),
        r#""direct""#
    );
    assert_eq!(
        serde_json::to_string(&Transport::WebRtc).unwrap(),
        r#""webrtc""#,
        "Appendix A requires `webrtc`, not the snake_case `web_rtc`"
    );
}

#[test]
fn topology_serializes_to_appendix_a_tokens() {
    assert_eq!(
        serde_json::to_string(&Topology::Relay).unwrap(),
        r#""relay""#
    );
    assert_eq!(serde_json::to_string(&Topology::Host).unwrap(), r#""host""#);
    assert_eq!(serde_json::to_string(&Topology::Mesh).unwrap(), r#""mesh""#);
}

#[test]
fn transport_round_trips_json_and_msgpack() {
    for t in [Transport::Relay, Transport::Direct, Transport::WebRtc] {
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Transport>(&json).unwrap(), t);

        let mp = rmp_serde::to_vec_named(&t).unwrap();
        assert_eq!(rmp_serde::from_slice::<Transport>(&mp).unwrap(), t);
    }
}

#[test]
fn topology_round_trips_json_and_msgpack() {
    for t in [Topology::Relay, Topology::Host, Topology::Mesh] {
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Topology>(&json).unwrap(), t);

        let mp = rmp_serde::to_vec_named(&t).unwrap();
        assert_eq!(rmp_serde::from_slice::<Topology>(&mp).unwrap(), t);
    }
}

#[test]
fn webrtc_token_deserializes_back_to_variant() {
    let parsed: Transport = serde_json::from_str(r#""webrtc""#).unwrap();
    assert_eq!(parsed, Transport::WebRtc);
}

// ---------------------------------------------------------------------------
// Authenticate round-trip: new fields are optional + skipped when absent.
// ---------------------------------------------------------------------------

#[test]
fn authenticate_without_new_fields_is_pure_v2() {
    let msg = ClientMessage::Authenticate {
        app_id: "app".to_string(),
        sdk_version: None,
        platform: None,
        game_data_format: None,
        protocol_version: None,
        supported_transports: None,
        supported_topologies: None,
    };
    // Absent fields must not appear on the wire (skip_serializing_if).
    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        value,
        json!({ "type": "Authenticate", "data": { "app_id": "app" } })
    );

    // A bare v2 payload deserializes to all-None for the new fields.
    let parsed: ClientMessage =
        serde_json::from_str(r#"{"type":"Authenticate","data":{"app_id":"app"}}"#).unwrap();
    match parsed {
        ClientMessage::Authenticate {
            protocol_version,
            supported_transports,
            supported_topologies,
            ..
        } => {
            assert!(protocol_version.is_none());
            assert!(supported_transports.is_none());
            assert!(supported_topologies.is_none());
        }
        other => panic!("expected Authenticate, got {other:?}"),
    }
}

#[test]
fn authenticate_with_new_fields_round_trips() {
    let msg = ClientMessage::Authenticate {
        app_id: "app".to_string(),
        sdk_version: None,
        platform: Some("godot".to_string()),
        game_data_format: Some(GameDataEncoding::MessagePack),
        protocol_version: Some(3),
        supported_transports: Some(vec![Transport::Relay, Transport::Direct, Transport::WebRtc]),
        supported_topologies: Some(vec![Topology::Relay, Topology::Host, Topology::Mesh]),
    };

    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        value,
        json!({
            "type": "Authenticate",
            "data": {
                "app_id": "app",
                "platform": "godot",
                "game_data_format": "message_pack",
                "protocol_version": 3,
                "supported_transports": ["relay", "direct", "webrtc"],
                "supported_topologies": ["relay", "host", "mesh"]
            }
        })
    );

    // JSON round-trip preserves all fields.
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
    match parsed {
        ClientMessage::Authenticate {
            protocol_version,
            supported_transports,
            supported_topologies,
            ..
        } => {
            assert_eq!(protocol_version, Some(3));
            assert_eq!(
                supported_transports,
                Some(vec![Transport::Relay, Transport::Direct, Transport::WebRtc])
            );
            assert_eq!(
                supported_topologies,
                Some(vec![Topology::Relay, Topology::Host, Topology::Mesh])
            );
        }
        other => panic!("expected Authenticate, got {other:?}"),
    }

    // MessagePack round-trip preserves all fields.
    let mp = rmp_serde::to_vec_named(&msg).unwrap();
    let parsed_mp: ClientMessage = rmp_serde::from_slice(&mp).unwrap();
    assert!(matches!(
        parsed_mp,
        ClientMessage::Authenticate {
            protocol_version: Some(3),
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// ProtocolInfoPayload version fields: skipped when None, present when Some.
// ---------------------------------------------------------------------------

#[test]
fn protocol_info_version_fields_skipped_when_none() {
    let payload = ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![],
        player_name_rules: None,
        protocol_version: None,
        min_protocol_version: None,
        max_protocol_version: None,
    };
    let value = serde_json::to_value(&payload).unwrap();
    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("protocol_version"));
    assert!(!obj.contains_key("min_protocol_version"));
    assert!(!obj.contains_key("max_protocol_version"));
}

#[test]
fn protocol_info_version_fields_present_when_some() {
    let payload = ProtocolInfoPayload {
        platform: None,
        sdk_version: None,
        minimum_version: None,
        recommended_version: None,
        capabilities: vec![],
        notes: None,
        game_data_formats: vec![],
        player_name_rules: None,
        protocol_version: Some(3),
        min_protocol_version: Some(2),
        max_protocol_version: Some(3),
    };
    let value = serde_json::to_value(&payload).unwrap();
    assert_eq!(value["protocol_version"], json!(3));
    assert_eq!(value["min_protocol_version"], json!(2));
    assert_eq!(value["max_protocol_version"], json!(3));
}

// ---------------------------------------------------------------------------
// P2 signal-relay wire types (Appendix A): Signal / NewPeer round-trips.
// The `signal` payload is opaque and must be byte-preserved verbatim.
// ---------------------------------------------------------------------------

/// A representative opaque, matchbox-compatible signal payload.
fn sample_signal() -> serde_json::Value {
    json!({ "Offer": "v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n" })
}

#[test]
fn client_signal_round_trips_json_and_msgpack() {
    let to = PlayerId::new_v4();
    let msg = ClientMessage::Signal {
        to,
        signal: sample_signal(),
    };

    // Exact wire tag + field names.
    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["type"], json!("Signal"));
    assert_eq!(value["data"]["to"], json!(to.to_string()));
    assert_eq!(value["data"]["signal"], sample_signal());

    // JSON round-trip preserves the opaque payload byte-for-byte.
    let parsed: ClientMessage =
        serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
    match parsed {
        ClientMessage::Signal { to: rt_to, signal } => {
            assert_eq!(rt_to, to);
            assert_eq!(signal, sample_signal());
        }
        other => panic!("expected Signal, got {other:?}"),
    }

    // MessagePack round-trip.
    let mp = rmp_serde::to_vec_named(&msg).unwrap();
    let parsed_mp: ClientMessage = rmp_serde::from_slice(&mp).unwrap();
    match parsed_mp {
        ClientMessage::Signal { to: rt_to, signal } => {
            assert_eq!(rt_to, to);
            assert_eq!(signal, sample_signal());
        }
        other => panic!("expected Signal, got {other:?}"),
    }
}

#[test]
fn client_transport_status_round_trips_json_and_msgpack() {
    // Exact wire form (Appendix A): {"type":"TransportStatus",
    // "data":{"transport":"webrtc","connected":true}}. Cover all three transport
    // tokens and both `connected` values.
    let cases = [
        (Transport::WebRtc, "webrtc", true),
        (Transport::Direct, "direct", false),
        (Transport::Relay, "relay", true),
        (Transport::WebRtc, "webrtc", false),
    ];

    for (transport, token, connected) in cases {
        let msg = ClientMessage::TransportStatus {
            transport,
            connected,
        };

        // Exact tag, field names, and transport token.
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], json!("TransportStatus"));
        assert_eq!(value["data"]["transport"], json!(token));
        assert_eq!(value["data"]["connected"], json!(connected));

        // JSON round-trip.
        let parsed: ClientMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match parsed {
            ClientMessage::TransportStatus {
                transport: rt_transport,
                connected: rt_connected,
            } => {
                assert_eq!(rt_transport, transport);
                assert_eq!(rt_connected, connected);
            }
            other => panic!("expected TransportStatus, got {other:?}"),
        }

        // MessagePack round-trip (named fields).
        let mp = rmp_serde::to_vec_named(&msg).unwrap();
        let parsed_mp: ClientMessage = rmp_serde::from_slice(&mp).unwrap();
        match parsed_mp {
            ClientMessage::TransportStatus {
                transport: rt_transport,
                connected: rt_connected,
            } => {
                assert_eq!(rt_transport, transport);
                assert_eq!(rt_connected, connected);
            }
            other => panic!("expected TransportStatus, got {other:?}"),
        }
    }
}

#[test]
fn server_signal_round_trips_json_and_msgpack() {
    let from = PlayerId::new_v4();
    let msg = ServerMessage::Signal {
        from,
        signal: sample_signal(),
    };

    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["type"], json!("Signal"));
    assert_eq!(value["data"]["from"], json!(from.to_string()));
    assert_eq!(value["data"]["signal"], sample_signal());

    let parsed: ServerMessage =
        serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
    match parsed {
        ServerMessage::Signal {
            from: rt_from,
            signal,
        } => {
            assert_eq!(rt_from, from);
            assert_eq!(signal, sample_signal());
        }
        other => panic!("expected Signal, got {other:?}"),
    }

    let mp = rmp_serde::to_vec_named(&msg).unwrap();
    let parsed_mp: ServerMessage = rmp_serde::from_slice(&mp).unwrap();
    match parsed_mp {
        ServerMessage::Signal {
            from: rt_from,
            signal,
        } => {
            assert_eq!(rt_from, from);
            assert_eq!(signal, sample_signal());
        }
        other => panic!("expected Signal, got {other:?}"),
    }
}

#[test]
fn server_new_peer_round_trips_json_and_msgpack() {
    let peer_id = PlayerId::new_v4();
    let msg = ServerMessage::NewPeer {
        peer_id,
        you_initiate: true,
    };

    let value = serde_json::to_value(&msg).unwrap();
    assert_eq!(value["type"], json!("NewPeer"));
    assert_eq!(value["data"]["peer_id"], json!(peer_id.to_string()));
    assert_eq!(value["data"]["you_initiate"], json!(true));

    let parsed: ServerMessage =
        serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
    match parsed {
        ServerMessage::NewPeer {
            peer_id: rt_peer,
            you_initiate,
        } => {
            assert_eq!(rt_peer, peer_id);
            assert!(you_initiate);
        }
        other => panic!("expected NewPeer, got {other:?}"),
    }

    let mp = rmp_serde::to_vec_named(&msg).unwrap();
    let parsed_mp: ServerMessage = rmp_serde::from_slice(&mp).unwrap();
    match parsed_mp {
        ServerMessage::NewPeer {
            peer_id: rt_peer,
            you_initiate,
        } => {
            assert_eq!(rt_peer, peer_id);
            assert!(you_initiate);
        }
        other => panic!("expected NewPeer, got {other:?}"),
    }
}

#[test]
fn server_peer_transport_status_round_trips_json_and_msgpack() {
    // Exact wire form (mirrors the client `TransportStatus` shape, plus the
    // reporting peer): {"type":"PeerTransportStatus",
    // "data":{"peer_id":"<uuid>","transport":"webrtc","connected":true}}.
    // Cover all three transport tokens and both `connected` values.
    let cases = [
        (Transport::WebRtc, "webrtc", true),
        (Transport::Direct, "direct", false),
        (Transport::Relay, "relay", true),
        (Transport::WebRtc, "webrtc", false),
    ];

    for (transport, token, connected) in cases {
        let peer_id = PlayerId::new_v4();
        let msg = ServerMessage::PeerTransportStatus {
            peer_id,
            transport,
            connected,
        };

        // Exact tag, field names, and transport token.
        let value = serde_json::to_value(&msg).unwrap();
        assert_eq!(value["type"], json!("PeerTransportStatus"));
        assert_eq!(value["data"]["peer_id"], json!(peer_id.to_string()));
        assert_eq!(value["data"]["transport"], json!(token));
        assert_eq!(value["data"]["connected"], json!(connected));

        // JSON round-trip.
        let parsed: ServerMessage =
            serde_json::from_str(&serde_json::to_string(&msg).unwrap()).unwrap();
        match parsed {
            ServerMessage::PeerTransportStatus {
                peer_id: rt_peer,
                transport: rt_transport,
                connected: rt_connected,
            } => {
                assert_eq!(rt_peer, peer_id);
                assert_eq!(rt_transport, transport);
                assert_eq!(rt_connected, connected);
            }
            other => panic!("expected PeerTransportStatus, got {other:?}"),
        }

        // MessagePack round-trip (named fields).
        let mp = rmp_serde::to_vec_named(&msg).unwrap();
        let parsed_mp: ServerMessage = rmp_serde::from_slice(&mp).unwrap();
        match parsed_mp {
            ServerMessage::PeerTransportStatus {
                peer_id: rt_peer,
                transport: rt_transport,
                connected: rt_connected,
            } => {
                assert_eq!(rt_peer, peer_id);
                assert_eq!(rt_transport, transport);
                assert_eq!(rt_connected, connected);
            }
            other => panic!("expected PeerTransportStatus, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// P3 SessionPlan wire types (Appendix A/B): exact tag, field names, tokens, and
// the skip_serializing_if / default omissions.
// ---------------------------------------------------------------------------

#[test]
fn session_plan_mesh_wire_shape_and_tokens() {
    let peer = PlayerId::new_v4();
    let msg = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        topology: Topology::Mesh,
        transport: Transport::WebRtc,
        host: None,
        peers: vec![SessionPeer {
            player_id: peer,
            player_name: "P2".to_string(),
            is_authority: false,
            initiate: true,
        }],
        ice_servers: vec![IceServer {
            urls: vec![
                "stun:stun.l.google.com:19302".to_string(),
                "turn:turn.example.com:3478".to_string(),
            ],
            username: Some("user".to_string()),
            credential: Some("pass".to_string()),
        }],
        fallback: Transport::Relay,
    }));

    let value = serde_json::to_value(&msg).unwrap();
    // Exact envelope: {"type":"SessionPlan","data":{...}} (Appendix A).
    assert_eq!(value["type"], json!("SessionPlan"));
    let data = &value["data"];
    // Wire tokens.
    assert_eq!(data["topology"], json!("mesh"));
    assert_eq!(data["transport"], json!("webrtc"));
    assert_eq!(data["fallback"], json!("relay"));
    // Field names on the peer.
    assert_eq!(data["peers"][0]["player_id"], json!(peer.to_string()));
    assert_eq!(data["peers"][0]["player_name"], json!("P2"));
    assert_eq!(data["peers"][0]["is_authority"], json!(false));
    assert_eq!(data["peers"][0]["initiate"], json!(true));
    // ICE server byte-preserved (urls + auth fields present).
    assert_eq!(
        data["ice_servers"][0]["urls"],
        json!(["stun:stun.l.google.com:19302", "turn:turn.example.com:3478"])
    );
    assert_eq!(data["ice_servers"][0]["username"], json!("user"));
    assert_eq!(data["ice_servers"][0]["credential"], json!("pass"));
    // host is None => omitted.
    assert!(
        data.as_object().unwrap().get("host").is_none(),
        "host must be omitted when None"
    );
}

#[test]
fn session_plan_host_some_is_present_on_wire() {
    let host = PlayerId::new_v4();
    let msg = ServerMessage::SessionPlan(Box::new(SessionPlanPayload {
        topology: Topology::Host,
        transport: Transport::Direct,
        host: Some(host),
        peers: vec![],
        ice_servers: vec![],
        fallback: Transport::Relay,
    }));

    let value = serde_json::to_value(&msg).unwrap();
    let data = &value["data"];
    assert_eq!(data["topology"], json!("host"));
    assert_eq!(data["transport"], json!("direct"));
    assert_eq!(data["host"], json!(host.to_string()));
    // ice_servers empty => omitted (skip_serializing_if Vec::is_empty).
    assert!(
        data.as_object().unwrap().get("ice_servers").is_none(),
        "ice_servers must be omitted when empty"
    );
}

#[test]
fn ice_server_omits_credentials_when_none() {
    let server = IceServer {
        urls: vec!["stun:stun.l.google.com:19302".to_string()],
        username: None,
        credential: None,
    };
    let value = serde_json::to_value(&server).unwrap();
    let obj = value.as_object().unwrap();
    assert_eq!(value["urls"], json!(["stun:stun.l.google.com:19302"]));
    assert!(
        !obj.contains_key("username"),
        "username must be omitted when None"
    );
    assert!(
        !obj.contains_key("credential"),
        "credential must be omitted when None"
    );
}

#[test]
fn session_plan_round_trips_json_and_msgpack() {
    let host = PlayerId::new_v4();
    let peer = PlayerId::new_v4();
    let original = SessionPlanPayload {
        topology: Topology::Host,
        transport: Transport::WebRtc,
        host: Some(host),
        peers: vec![SessionPeer {
            player_id: peer,
            player_name: "Peer".to_string(),
            is_authority: true,
            initiate: true,
        }],
        ice_servers: vec![IceServer {
            urls: vec!["turn:turn.example.com:3478".to_string()],
            username: Some("u".to_string()),
            credential: Some("c".to_string()),
        }],
        fallback: Transport::Relay,
    };
    let msg = ServerMessage::SessionPlan(Box::new(original));

    // JSON round-trip.
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    assert_session_plan_eq(&msg, &parsed);

    // MessagePack round-trip (named, matching the project's wire convention).
    let mp = rmp_serde::to_vec_named(&msg).unwrap();
    let parsed_mp: ServerMessage = rmp_serde::from_slice(&mp).unwrap();
    assert_session_plan_eq(&msg, &parsed_mp);
}

#[test]
fn session_plan_default_ice_servers_when_field_absent() {
    // ice_servers has #[serde(default)] so an absent field deserializes to empty.
    let host = PlayerId::new_v4();
    let json = format!(
        r#"{{"type":"SessionPlan","data":{{"topology":"host","transport":"webrtc","host":"{host}","peers":[],"fallback":"relay"}}}}"#
    );
    let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
    match parsed {
        ServerMessage::SessionPlan(plan) => {
            assert_eq!(plan.topology, Topology::Host);
            assert_eq!(plan.host, Some(host));
            assert!(
                plan.ice_servers.is_empty(),
                "absent ice_servers must default to empty"
            );
        }
        other => panic!("expected SessionPlan, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Protocol v3 negotiation: the [2, 4] clamp matrix at the config level. The
// same matrix is exercised end-to-end (through Authenticate/ProtocolInfo) in
// `tests/v3_negotiation_e2e.rs`.
// ---------------------------------------------------------------------------

#[test]
fn v3_negotiation_clamp_matrix() {
    use signal_fish_server::config::{ProtocolConfig, SERVER_MAX_PROTOCOL_VERSION};

    assert_eq!(
        SERVER_MAX_PROTOCOL_VERSION, 3,
        "this build implements protocol v3 (the single unshipped current version: \
         WebRTC signaling + server-stamped GameData seq/epoch + RelayStats)"
    );

    // Default deployment range is [2, 3].
    let cfg = ProtocolConfig::default();
    assert_eq!(cfg.max_protocol_version, 3, "default ceiling is v3");
    assert!(cfg.validate().is_ok(), "default [2, 3] range validates");

    // client asks 3 => gets 3.
    assert_eq!(cfg.negotiate_protocol_version(Some(3)), 3);
    // client asks 4/5 (future, or a stale v3-era client) => clamped down to 3.
    assert_eq!(cfg.negotiate_protocol_version(Some(4)), 3);
    assert_eq!(cfg.negotiate_protocol_version(Some(5)), 3);
    // client asks 2 / omits => the v2 floor (v3 is opt-in, never forced).
    assert_eq!(cfg.negotiate_protocol_version(Some(2)), 2);
    assert_eq!(cfg.negotiate_protocol_version(None), 2);

    // Deployment clamped back to max 2 (pure v2) by config: a v3 client is
    // negotiated down to 2 and the narrowed range still validates.
    let clamped = ProtocolConfig {
        max_protocol_version: 2,
        ..ProtocolConfig::default()
    };
    assert!(clamped.validate().is_ok(), "[2, 2] stays a valid range");
    assert_eq!(clamped.negotiate_protocol_version(Some(3)), 2);
    assert_eq!(clamped.negotiate_protocol_version(Some(2)), 2);

    // The full [2, 3] range is accepted by config validation; above the
    // build ceiling is rejected.
    let full = ProtocolConfig {
        min_protocol_version: 2,
        max_protocol_version: 3,
        ..ProtocolConfig::default()
    };
    assert!(full.validate().is_ok());
    let beyond = ProtocolConfig {
        max_protocol_version: 4,
        ..ProtocolConfig::default()
    };
    assert!(
        beyond.validate().is_err(),
        "max above the build ceiling (3) must be rejected"
    );
}

fn assert_session_plan_eq(expected: &ServerMessage, actual: &ServerMessage) {
    match (expected, actual) {
        (ServerMessage::SessionPlan(a), ServerMessage::SessionPlan(b)) => {
            assert_eq!(a.topology, b.topology);
            assert_eq!(a.transport, b.transport);
            assert_eq!(a.host, b.host);
            assert_eq!(a.fallback, b.fallback);
            assert_eq!(a.peers.len(), b.peers.len());
            for (pa, pb) in a.peers.iter().zip(b.peers.iter()) {
                assert_eq!(pa.player_id, pb.player_id);
                assert_eq!(pa.player_name, pb.player_name);
                assert_eq!(pa.is_authority, pb.is_authority);
                assert_eq!(pa.initiate, pb.initiate);
            }
            assert_eq!(a.ice_servers.len(), b.ice_servers.len());
            for (ia, ib) in a.ice_servers.iter().zip(b.ice_servers.iter()) {
                assert_eq!(ia.urls, ib.urls);
                assert_eq!(ia.username, ib.username);
                assert_eq!(ia.credential, ib.credential);
            }
        }
        other => panic!("expected two SessionPlan messages, got {other:?}"),
    }
}
