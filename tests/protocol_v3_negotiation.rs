//! Protocol v3 (P1) capability-negotiation wire + helper tests.
//!
//! Covers Transport/Topology serde tokens (Appendix A), `Authenticate`
//! round-tripping with and without the new optional fields, and the
//! `ProtocolInfoPayload` version fields. The byte-for-byte v2 freeze lives in
//! `v2_wire_golden.rs`; this file asserts the *additive* v3 surface.

use serde_json::json;
use signal_fish_server::protocol::{
    ClientMessage, GameDataEncoding, PlayerId, ProtocolInfoPayload, ServerMessage, Topology,
    Transport,
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
