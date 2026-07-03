//! Machine-readable event stream (the client's primary output contract).
//!
//! stdout carries exactly one JSON object per line (JSONL), each with a stable
//! snake_case `"event"` tag. Harnesses and scripts consume this stream;
//! human-oriented logging goes exclusively to stderr via `tracing`. Every event
//! is emitted from the single orchestrator task, so lines never interleave and
//! per-client ordering is causal.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use signal_fish_server::protocol::{LobbyState, PlayerId, RoomId, Topology, Transport};

/// One JSONL stdout event. Variant names serialize as the snake_case `event`
/// tag (e.g. `P2pPairConnected` -> `"p2p_pair_connected"`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// WebSocket connection to the server established. Carries the run's
    /// runtime configuration so starved-runtime harnesses can assert the
    /// intended fault-injection shape was actually in effect: `runtime` is the
    /// `--runtime` token (`multi`/`current`) and `tick_stall_ms` the
    /// `--tick-stall-ms` value (0 = no stall).
    Connected { runtime: String, tick_stall_ms: u64 },
    /// Server accepted `Authenticate`.
    Authenticated,
    /// Negotiation result echoed by the server (v2 connections report 2).
    ProtocolInfo { negotiated_version: u16 },
    /// This client created the room; the harness scrapes `room_code` to start joiners.
    RoomCreated { room_code: String },
    /// This client is seated in the room (creators emit `room_created` first).
    /// `lobby_state` is the room's state at entry (`waiting`/`lobby`/
    /// `finalized`); `finalized` marks a late join into a running session.
    RoomJoined {
        room_id: RoomId,
        player_id: PlayerId,
        lobby_state: LobbyState,
    },
    /// Another player joined the room.
    PeerJoined { player_id: PlayerId },
    /// Another player left the room.
    PlayerLeft { player_id: PlayerId },
    /// Lobby finalized; `is_authority` is this client's own flag from `GameStarting`.
    GameStarting { is_authority: bool },
    /// The per-recipient v3 session directive (peers list this recipient's pairings).
    SessionPlan {
        topology: Topology,
        transport: Transport,
        host: Option<PlayerId>,
        peers: Vec<PlanPeer>,
        ice_servers_count: usize,
        fallback: Transport,
    },
    /// Late-join pairing delta for an already-running session.
    NewPeer {
        peer_id: PlayerId,
        you_initiate: bool,
    },
    /// An outbound `Signal` envelope was relayed toward `to`.
    SignalSent { to: PlayerId, kind: SignalKind },
    /// An inbound `Signal` envelope arrived from `from`.
    SignalReceived { from: PlayerId, kind: SignalKind },
    /// RTCPeerConnection state transition (`new`/`connecting`/`connected`/...).
    PcState { peer: PlayerId, state: String },
    /// One data channel reached the open state.
    ChannelOpen { peer: PlayerId, label: String },
    /// An `--exchange` message was sent over an open channel.
    ChannelMessageSent {
        peer: PlayerId,
        label: String,
        text: String,
    },
    /// A data-channel text message was received.
    ChannelMessage {
        peer: PlayerId,
        label: String,
        text: String,
    },
    /// Both channels (`reliable` + `unreliable`) are open toward `peer`.
    P2pPairConnected { peer: PlayerId },
    /// The single overall `TransportStatus` report was sent (Appendix G).
    TransportStatusSent {
        transport: Transport,
        connected: bool,
    },
    /// A same-room peer's reported transport state changed (server fan-out).
    PeerTransportStatus {
        peer: PlayerId,
        transport: Transport,
        connected: bool,
    },
    /// The `--relay-payload` GameData message was sent over the relay floor.
    GameDataSent,
    /// A relayed GameData payload arrived over the WebSocket (the floor).
    GameDataReceived {
        from: PlayerId,
        payload: serde_json::Value,
    },
    /// The P2P window resolved with zero connected pairs; relay carries the session.
    FallbackEngaged,
    /// A non-fatal or fatal error (fatal errors are followed by `exiting`).
    Error { message: String },
    /// Final event before process exit with the given code.
    Exiting { code: i32 },
}

/// Peer entry inside a [`Event::SessionPlan`] event.
#[derive(Debug, Clone, Serialize)]
pub struct PlanPeer {
    pub player_id: PlayerId,
    pub initiate: bool,
}

/// Classification of an opaque matchbox-shaped signal payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Offer,
    Answer,
    IceCandidate,
    /// Anything not matching the matchbox `PeerSignal` convention.
    Other,
}

impl SignalKind {
    /// Classify an opaque signal value by its single matchbox-convention key.
    pub fn classify(signal: &serde_json::Value) -> Self {
        let Some(object) = signal.as_object() else {
            return Self::Other;
        };
        if object.len() != 1 {
            return Self::Other;
        }
        match object.keys().next().map(String::as_str) {
            Some("Offer") => Self::Offer,
            Some("Answer") => Self::Answer,
            Some("IceCandidate") => Self::IceCandidate,
            _ => Self::Other,
        }
    }
}

/// Set once a stdout write has failed (e.g. the consumer closed the pipe);
/// every later [`emit`] becomes a no-op so the client can finish its bounded
/// run instead of panicking on `EPIPE` the way `println!` would.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

/// Emit one event line to stdout.
///
/// The locked stdout handle is held for the whole line (JSON + newline), so
/// events from this process never interleave mid-line. This function never
/// panics:
///
/// - Serialization of [`Event`] cannot realistically fail (no non-string map
///   keys, no fallible impls); if it ever does, the failure is reported on
///   stderr and the event is dropped.
/// - A failed stdout write (typically `BrokenPipe` after the consumer hung
///   up) is reported on stderr once and latches [`STDOUT_CLOSED`]: event
///   emission shuts down gracefully while the client continues toward its
///   normal bounded exit (`--run-for-secs` / `--max-runtime-secs`).
pub fn emit(event: &Event) {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    let line = match serde_json::to_string(event) {
        Ok(line) => line,
        Err(error) => {
            tracing::error!(%error, ?event, "failed to serialize stdout event");
            return;
        }
    };
    if let Err(error) = write_event_line(&mut std::io::stdout().lock(), &line) {
        STDOUT_CLOSED.store(true, Ordering::Relaxed);
        tracing::error!(
            %error,
            "stdout write failed (consumer closed?); suppressing all further events"
        );
    }
}

/// Write one event line (JSON + trailing newline) and flush, surfacing any
/// I/O failure (such as `BrokenPipe`) to the caller instead of panicking.
fn write_event_line(target: &mut impl Write, line: &str) -> std::io::Result<()> {
    target.write_all(line.as_bytes())?;
    target.write_all(b"\n")?;
    target.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_tags_are_snake_case() {
        let cases: Vec<(Event, &str)> = vec![
            (
                Event::Connected {
                    runtime: "multi".to_string(),
                    tick_stall_ms: 0,
                },
                "connected",
            ),
            (
                Event::ProtocolInfo {
                    negotiated_version: 3,
                },
                "protocol_info",
            ),
            (
                Event::P2pPairConnected {
                    peer: PlayerId::nil(),
                },
                "p2p_pair_connected",
            ),
            (
                Event::TransportStatusSent {
                    transport: Transport::WebRtc,
                    connected: true,
                },
                "transport_status_sent",
            ),
            (Event::GameDataSent, "game_data_sent"),
            (Event::FallbackEngaged, "fallback_engaged"),
            (Event::Exiting { code: 0 }, "exiting"),
        ];
        for (event, expected_tag) in cases {
            let value = serde_json::to_value(&event).expect("event serializes");
            assert_eq!(
                value.get("event").and_then(|tag| tag.as_str()),
                Some(expected_tag),
                "tag for {event:?}"
            );
        }
    }

    #[test]
    fn connected_event_carries_runtime_configuration() {
        let event = Event::Connected {
            runtime: "current".to_string(),
            tick_stall_ms: 750,
        };
        let value = serde_json::to_value(&event).expect("event serializes");
        assert_eq!(value["event"], "connected");
        // The starved-runtime harness keys on these exact fields.
        assert_eq!(value["runtime"], "current");
        assert_eq!(value["tick_stall_ms"], 750);
    }

    #[test]
    fn room_joined_event_carries_lobby_state_tokens() {
        let event = Event::RoomJoined {
            room_id: RoomId::nil(),
            player_id: PlayerId::nil(),
            lobby_state: LobbyState::Finalized,
        };
        let value = serde_json::to_value(&event).expect("event serializes");
        assert_eq!(value["event"], "room_joined");
        // Late-join harnesses key on this exact token (snake_case serde).
        assert_eq!(value["lobby_state"], "finalized");
        assert_eq!(
            serde_json::to_value(LobbyState::Waiting).expect("serializes"),
            serde_json::json!("waiting")
        );
        assert_eq!(
            serde_json::to_value(LobbyState::Lobby).expect("serializes"),
            serde_json::json!("lobby")
        );
    }

    #[test]
    fn session_plan_event_carries_wire_tokens() {
        let event = Event::SessionPlan {
            topology: Topology::Mesh,
            transport: Transport::WebRtc,
            host: None,
            peers: vec![PlanPeer {
                player_id: PlayerId::nil(),
                initiate: true,
            }],
            ice_servers_count: 1,
            fallback: Transport::Relay,
        };
        let value = serde_json::to_value(&event).expect("event serializes");
        assert_eq!(value["topology"], "mesh");
        assert_eq!(value["transport"], "webrtc");
        assert_eq!(value["fallback"], "relay");
        assert_eq!(value["host"], serde_json::Value::Null);
        assert_eq!(value["peers"][0]["initiate"], true);
        assert_eq!(value["ice_servers_count"], 1);
    }

    #[test]
    fn write_event_line_appends_exactly_one_newline() {
        let mut sink: Vec<u8> = Vec::new();
        write_event_line(&mut sink, r#"{"event":"connected"}"#).expect("vec writes succeed");
        assert_eq!(sink, b"{\"event\":\"connected\"}\n");
        // A round-trip parse proves the line is consumable as JSONL.
        let line = std::str::from_utf8(&sink).expect("utf8").trim_end();
        let value: serde_json::Value = serde_json::from_str(line).expect("line parses");
        assert_eq!(value["event"], "connected");
    }

    /// A writer that fails every write with `BrokenPipe` (a closed consumer).
    struct BrokenPipeWriter;

    impl std::io::Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn write_event_line_surfaces_broken_pipe_instead_of_panicking() {
        let error = write_event_line(&mut BrokenPipeWriter, r#"{"event":"connected"}"#)
            .expect_err("a closed pipe must surface as an error");
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn signal_kind_classification_covers_convention_and_garbage() {
        assert_eq!(
            SignalKind::classify(&json!({"Offer": "sdp"})),
            SignalKind::Offer
        );
        assert_eq!(
            SignalKind::classify(&json!({"Answer": "sdp"})),
            SignalKind::Answer
        );
        assert_eq!(
            SignalKind::classify(&json!({"IceCandidate": "x"})),
            SignalKind::IceCandidate
        );
        assert_eq!(SignalKind::classify(&json!("Offer")), SignalKind::Other);
        assert_eq!(
            SignalKind::classify(&json!({"Offer": "a", "Answer": "b"})),
            SignalKind::Other
        );
        assert_eq!(SignalKind::classify(&json!({})), SignalKind::Other);
    }
}
