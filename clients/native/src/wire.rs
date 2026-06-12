//! WebSocket wire layer: JSON text frames carrying the server crate's own
//! `ClientMessage` / `ServerMessage` enums.
//!
//! Envelope types are NEVER hand-rolled here — they come straight from
//! `signal_fish_server::protocol` via the path dependency, so the reference
//! client cannot drift from the server's wire contract. Only the opaque
//! `signal` payload (matchbox `PeerSignal` shape per ADR-0002) is built by the
//! client, because the server deliberately does not model it.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use signal_fish_server::protocol::{ClientMessage, ServerMessage};
use tokio_tungstenite::tungstenite::Message;

/// The concrete stream type produced by `tokio_tungstenite::connect_async`.
pub type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Time allowed for the initial TCP + WebSocket handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-message ceiling during the sequential handshake phase (authenticate /
/// join). Generous: a CI scheduling budget, not an expected wait.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Connect to the signaling endpoint (full `ws://.../v3/ws` URL).
pub async fn connect(server_url: &str) -> Result<WsStream> {
    let (stream, _response) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(server_url),
    )
    .await
    .map_err(|_elapsed| anyhow!("websocket connect to {server_url} timed out"))?
    .with_context(|| format!("websocket connect to {server_url} failed"))?;
    Ok(stream)
}

/// Serialize `message` and send it as one WebSocket text frame.
pub async fn send_client_message(ws: &mut WsStream, message: &ClientMessage) -> Result<()> {
    let json = serde_json::to_string(message).context("serialize ClientMessage")?;
    ws.send(Message::Text(json.into()))
        .await
        .context("send ClientMessage frame")?;
    Ok(())
}

/// Read the next `ServerMessage`, skipping WebSocket control frames
/// (tungstenite answers pings internally) and bounding the wait.
///
/// Errors distinguish a closed/failed transport (connection error) from a
/// malformed text frame (protocol error) via the message text; the caller maps
/// them onto exit codes.
pub async fn next_server_message(ws: &mut WsStream, timeout: Duration) -> Result<ServerMessage> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .map_err(|_elapsed| anyhow!("timed out waiting for a ServerMessage"))?;
        match frame {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text)
                    .with_context(|| format!("invalid ServerMessage text frame: {text}"));
            }
            // Binary frames are MessagePack/rkyv game data for clients that
            // negotiated those formats; this client always negotiates JSON.
            Some(Ok(other)) => {
                tracing::debug!(frame = ?other, "skipping non-text frame");
            }
            Some(Err(error)) => {
                return Err(anyhow!("websocket transport error: {error}"));
            }
            None => {
                return Err(anyhow!("websocket closed by server"));
            }
        }
    }
}
