//! Shared 16-player room test harness for the relay and WebRTC matrices.
//!
//! Shared helpers for the sixteen-player suites: bring up N real WebSocket
//! clients from loopback (all sharing IP `127.0.0.1`, so the per-IP admission
//! cap is genuinely exercised) and drive them through `Authenticate` +
//! `JoinRoom`.
//!
//! Built incrementally alongside its consumers (red-green, so every helper is
//! exercised by a test the same PR): the connect / authenticate / join
//! primitives land with the admission suite
//! (`tests/sixteen_player_admission_e2e.rs`). The per-client `ChaosProxy`
//! option and the fault-tuned `sixteen_player_server_config()` (raised send
//! queue / slow-consumer timeout) land with the slow-consumer / matrix suites
//! that actually inject faults — added when those tests exist rather than as
//! unexercised scaffolding.

use futures_util::SinkExt;
use signal_fish_server::protocol::{
    ClientMessage, ErrorCode, GameDataEncoding, PlayerId, RoomJoinedPayload, ServerMessage,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};

use super::{next_matching_server_message_within, WsStream};

/// Ceiling for one connect / auth / join step — generous (only a genuine wedge
/// spends it), never an expected wait.
const STEP_DEADLINE: tokio::time::Duration = tokio::time::Duration::from_secs(30);

/// A connected, handshake-complete, room-joined player: its server-assigned id and
/// the live socket. The socket is retained so the player stays in the room for
/// the lifetime of the handle (dropping it disconnects the player).
pub struct PlayerHandle {
    pub player_id: PlayerId,
    /// Players the server reported in the room at this player's join
    /// (`RoomJoined.current_players`, which includes this player). Lets a caller
    /// assert co-location — that N joiners share ONE room — without a second
    /// round-trip or draining every socket's `PlayerJoined` stream.
    pub room_player_count: usize,
    /// The exact join snapshot consumed by the harness. Delivery suites feed
    /// this into `ConformanceAuditor` before recording the later lifecycle and
    /// game-data frames on the retained socket.
    pub room_joined: RoomJoinedPayload,
    pub ws: WsStream,
}

/// Open a WebSocket to `addr`'s `/ws` endpoint.
pub async fn connect(addr: std::net::SocketAddr) -> WsStream {
    let url = format!("ws://{addr}/ws");
    let (ws, _) = tokio::time::timeout(STEP_DEADLINE, connect_async(&url))
        .await
        .expect("websocket connect timed out")
        .expect("websocket connect failed");
    ws
}

async fn send(ws: &mut WsStream, msg: &ClientMessage) {
    let json = serde_json::to_string(msg).expect("serialize client message");
    ws.send(Message::Text(json.into()))
        .await
        .expect("send client message");
}

/// Authenticate advertising `protocol_version`, asserting the full handshake:
/// `Authenticated` THEN `ProtocolInfo`. Both frames are required — a regression
/// that dropped or reordered `Authenticated` must fail here rather than pass
/// silently on the later `ProtocolInfo` (matching the auth helpers in the other
/// e2e suites).
pub async fn authenticate(ws: &mut WsStream, protocol_version: u16) {
    authenticate_with_encoding(ws, protocol_version, None).await;
}

/// Authenticate with an explicit relay encoding. The matrix uses this seam to
/// exercise both JSON text frames and MessagePack binary frames through the
/// same room/admission path.
pub async fn authenticate_with_encoding(
    ws: &mut WsStream,
    protocol_version: u16,
    game_data_format: Option<GameDataEncoding>,
) {
    send(
        ws,
        &ClientMessage::Authenticate {
            app_id: "room16".to_string(),
            sdk_version: None,
            platform: None,
            game_data_format,
            protocol_version: Some(protocol_version),
            supported_transports: None,
            supported_topologies: None,
        },
    )
    .await;
    next_matching_server_message_within(ws, STEP_DEADLINE, "Authenticated ack", |message| {
        matches!(message, ServerMessage::Authenticated { .. }).then_some(())
    })
    .await;
    next_matching_server_message_within(ws, STEP_DEADLINE, "ProtocolInfo after auth", |message| {
        matches!(message, ServerMessage::ProtocolInfo(_)).then_some(())
    })
    .await;
}

/// Attempt to join `room_code` as `player_name` on an already handshake-complete
/// socket. Returns the joined handle, or the server's `(reason, error_code)`
/// on refusal — so a caller can assert admission (the A3 regression) OR the
/// documented room-cap refusal without a panic in either direction.
pub async fn try_join(
    mut ws: WsStream,
    game_name: &str,
    room_code: &str,
    max_players: Option<u8>,
    player_name: &str,
) -> Result<PlayerHandle, (String, Option<ErrorCode>)> {
    send(
        &mut ws,
        &ClientMessage::JoinRoom {
            game_name: game_name.to_string(),
            room_code: Some(room_code.to_string()),
            player_name: player_name.to_string(),
            max_players,
            supports_authority: Some(false),
            relay_transport: None,
        },
    )
    .await;

    let outcome = next_matching_server_message_within(
        &mut ws,
        STEP_DEADLINE,
        "room join outcome",
        |message| match message {
            ServerMessage::RoomJoined(payload) => Some(Ok(payload)),
            ServerMessage::RoomJoinFailed { reason, error_code } => Some(Err((reason, error_code))),
            _ => None,
        },
    )
    .await;

    outcome.map(|room_joined| {
        let room_joined = *room_joined;
        PlayerHandle {
            player_id: room_joined.player_id,
            room_player_count: room_joined.current_players.len(),
            room_joined,
            ws,
        }
    })
}

/// Connect + authenticate + join `n` players from loopback into one room,
/// creating it on the first join at `max_players`. Every player shares IP
/// `127.0.0.1`, so the per-IP admission cap is exercised. `game_data_format`
/// selects JSON text or MessagePack binary relay for matrix consumers. Panics
/// on any refusal — use it for the success path; drive [`connect`] /
/// [`try_join`] directly to assert a refusal.
///
/// The returned sockets are NOT drained afterward. That is safe as long as the
/// caller uses a server started via `create_router` + `axum::serve` (no
/// `cleanup_task`, so no activity reaper) and the default `send_queue_capacity`
/// (1024) — each retained socket then accumulates only a handful of small
/// `PlayerJoined` frames, far below any slow-consumer threshold. A caller that
/// starts the maintenance loop or shrinks the queue must drain instead.
pub async fn join_n_players(
    addr: std::net::SocketAddr,
    game_name: &str,
    room_code: &str,
    max_players: Option<u8>,
    n: usize,
    protocol_version: u16,
    game_data_format: Option<GameDataEncoding>,
) -> Vec<PlayerHandle> {
    let mut handles = Vec::with_capacity(n);
    for i in 0..n {
        let name = format!("P{i}");
        let mut ws = connect(addr).await;
        authenticate_with_encoding(&mut ws, protocol_version, game_data_format).await;
        match try_join(ws, game_name, room_code, max_players, &name).await {
            Ok(handle) => handles.push(handle),
            Err((reason, code)) => {
                panic!("player {name} failed to join room {room_code}: {reason} ({code:?})")
            }
        }
    }
    handles
}
