use crate::coordination::{
    deliver_or_disconnect, ClientDeliveryHandle, CloseReason, ConnectionCloseSignal,
    DeliveryOutcome,
};
use crate::protocol::{
    ClientMessage, ErrorCode, GameDataEncoding, PlayerId, PlayerNameRulesPayload,
    ProtocolInfoPayload, RateLimitInfo, ServerMessage, Topology, Transport,
};
use crate::server::{EnhancedGameServer, NegotiatedProtocol, RegisterClientError};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::time::Instant;

use super::batching::{send_batch, MessageBatcher};
use super::sending::{send_immediate_server_message, send_single_message};
use super::token_binding::{parse_client_message, TokenBindingHandshake};

/// Upper bound on the farewell error frame + close handshake writes issued
/// while force-closing a connection. The client's TCP window is often already
/// full when this runs (that is usually why its queue overflowed), so these
/// writes are strictly best-effort.
const CLOSE_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Enqueue a message on this connection's own outbound queue, honoring the
/// server-wide no-silent-drop contract: backpressure while the queue is
/// momentarily full, then a loud slow-consumer close if it never drains.
/// Used for control messages (auth responses, protocol info, timeout errors)
/// that are sent outside the message coordinator.
async fn enqueue_connection_message(
    tx: &mpsc::Sender<Arc<ServerMessage>>,
    close_signal: &ConnectionCloseSignal,
    server: &Arc<EnhancedGameServer>,
    slow_consumer_timeout: Duration,
    player_id: &PlayerId,
    message: ServerMessage,
    context: &'static str,
) {
    let handle = ClientDeliveryHandle {
        sender: tx.clone(),
        close: close_signal.clone(),
    };
    let outcome = deliver_or_disconnect(
        &server.metrics(),
        slow_consumer_timeout,
        player_id,
        &handle,
        Arc::new(message),
    )
    .await;
    if outcome != DeliveryOutcome::Delivered {
        tracing::warn!(
            %player_id,
            ?outcome,
            context,
            "Connection control message was not delivered"
        );
    }
}

/// Final actions of the send task once a server-side close was requested.
///
/// - Slow consumer: the queue contents are abandoned **by design** (the
///   recipient proved unable to drain them); they are counted as dropped, and
///   a best-effort farewell error tells the client why it is being closed.
/// - Unregistration (or all delivery handles dropped): the recipient is
///   healthy — flush whatever is already queued (e.g. a final error emitted
///   just before unregistering), then close.
async fn finalize_closed_connection(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    rx: &mut mpsc::Receiver<Arc<ServerMessage>>,
    batcher: &mut MessageBatcher,
    reason: Option<CloseReason>,
    player_id: &PlayerId,
    server: &Arc<EnhancedGameServer>,
) {
    match reason {
        Some(CloseReason::SlowConsumer) => {
            // Undercounts by at most one in-flight batch cancelled mid-write;
            // the connection-level disconnect signal is the primary indicator.
            let abandoned = rx.len() + batcher.len();
            server
                .metrics()
                .add_websocket_messages_dropped(abandoned as u64);
            tracing::warn!(
                %player_id,
                abandoned_messages = abandoned,
                "Closing slow-consumer connection; abandoning its undeliverable queue"
            );

            let farewell = ServerMessage::Error {
                message: format!(
                    "Disconnected as a slow consumer: this connection's outbound queue \
                     stayed full for more than {} ms",
                    server.config().websocket_config.slow_consumer_timeout_ms
                ),
                error_code: Some(ErrorCode::SlowConsumer),
            };
            match tokio::time::timeout(
                CLOSE_WRITE_TIMEOUT,
                send_immediate_server_message(sender, &farewell),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::debug!(%player_id, error = %err, "Failed to write slow-consumer farewell frame");
                }
                Err(_elapsed) => {
                    tracing::debug!(%player_id, "Timed out writing slow-consumer farewell frame");
                }
            }
        }
        Some(CloseReason::Unregistered) | None => {
            // Drain whatever is already buffered and flush it, bounded by the
            // close-write budget: an unregistered connection is usually
            // healthy (flush completes in milliseconds), but if its socket is
            // wedged this must not pin the task — the whole point of the
            // close path is to reclaim the connection. Both terminal channel
            // states end the drain: `Empty` means the flush is complete, and
            // `Disconnected` means every delivery handle is gone AND the
            // buffer is empty — also a completed flush.
            let flush = tokio::time::timeout(CLOSE_WRITE_TIMEOUT, async {
                // Not a `while let`: the repo-wide try_recv policy
                // (tests/async_timeout_policy_scan.rs) requires both terminal
                // channel states to be matched explicitly.
                #[allow(clippy::while_let_loop)]
                loop {
                    match rx.try_recv() {
                        Ok(message) => batcher.queue(message),
                        Err(
                            mpsc::error::TryRecvError::Empty
                            | mpsc::error::TryRecvError::Disconnected,
                        ) => break,
                    }
                }
                if batcher.is_empty() {
                    return Ok(());
                }
                send_batch(sender, batcher, player_id, server).await
            })
            .await;
            match flush {
                Ok(Ok(())) => {}
                Ok(Err(())) => {
                    tracing::debug!(
                        %player_id,
                        "Failed to flush queued messages while closing unregistered connection"
                    );
                }
                Err(_elapsed) => {
                    tracing::debug!(
                        %player_id,
                        "Timed out flushing queued messages while closing unregistered connection"
                    );
                }
            }
        }
    }

    match tokio::time::timeout(CLOSE_WRITE_TIMEOUT, sender.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::debug!(%player_id, error = %err, "WebSocket close handshake failed");
        }
        Err(_elapsed) => {
            tracing::debug!(%player_id, "Timed out closing WebSocket sink");
        }
    }
}

/// Resolve the negotiated transport/topology capability sets for a connection.
///
/// Relay is the universal floor, so it is always present in both sets. A
/// connection negotiated below v3 is **relay-only** regardless of what it
/// advertised — peer-to-peer transports/topologies are a v3+ upgrade and must
/// never be recorded for a v2 peer. For v3+, the client's advertised sets are
/// used with `Relay` forced in (if the client omitted it) and duplicates
/// removed while preserving the client's stated preference order.
fn negotiate_capabilities(
    negotiated_version: u16,
    supported_transports: Option<Vec<Transport>>,
    supported_topologies: Option<Vec<Topology>>,
) -> (Vec<Transport>, Vec<Topology>) {
    if negotiated_version < 3 {
        return (vec![Transport::Relay], vec![Topology::Relay]);
    }

    let mut transports = supported_transports.unwrap_or_else(|| vec![Transport::Relay]);
    if !transports.contains(&Transport::Relay) {
        transports.push(Transport::Relay);
    }
    dedup_preserving_order(&mut transports);

    let mut topologies = supported_topologies.unwrap_or_else(|| vec![Topology::Relay]);
    if !topologies.contains(&Topology::Relay) {
        topologies.push(Topology::Relay);
    }
    dedup_preserving_order(&mut topologies);

    (transports, topologies)
}

/// Remove duplicate entries while preserving the first occurrence of each.
///
/// Unlike [`Vec::dedup`], which only collapses *consecutive* duplicates, this
/// drops every later repeat regardless of position. The lists it operates on
/// (negotiated transports/topologies) have at most three variants, so the
/// quadratic scan is trivially cheap and keeps the relative ordering the client
/// advertised (which conveys preference for the P3 selection logic).
fn dedup_preserving_order<T: PartialEq>(items: &mut Vec<T>) {
    let mut unique = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    *items = unique;
}

// `default_protocol_version` is the fallback used when the client omits
// `Authenticate.protocol_version`: `2` for the `/v2/ws` path, `3` for `/v3/ws`.
pub(super) async fn handle_socket(
    socket: WebSocket,
    server: Arc<EnhancedGameServer>,
    addr: SocketAddr,
    token_binding: Option<TokenBindingHandshake>,
    default_protocol_version: u16,
) {
    let (mut sender, mut receiver) = socket.split();
    // Validated >= 1 at startup; clamp anyway because `mpsc::channel` panics on 0.
    let queue_capacity = server.config().websocket_config.send_queue_capacity.max(1);
    let slow_consumer_timeout = Duration::from_millis(
        server
            .config()
            .websocket_config
            .slow_consumer_timeout_ms
            .max(1),
    );
    let (tx, mut rx) = mpsc::channel::<Arc<ServerMessage>>(queue_capacity);

    // One close signal per connection: the delivery layer requests closes
    // through its registered handle (slow consumer) or via unregistration;
    // both socket tasks listen and tear the connection down.
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let mut send_task_close = close_listener.clone();
    let mut receive_task_close = close_listener;

    // Keep a clone of tx for sending auth responses
    let tx_clone = tx.clone();

    // Register client with server
    let player_id = match server
        .register_client_with_close(tx, close_signal.clone(), addr)
        .await
    {
        Ok(player_id) => {
            tracing::info!(%player_id, client_addr = %addr, "WebSocket connection established");
            player_id
        }
        Err(RegisterClientError::IpLimitExceeded { current, limit }) => {
            let error_message = ServerMessage::Error {
                message: format!("Too many connections from your IP ({current}/{limit})"),
                error_code: Some(ErrorCode::TooManyConnections),
            };
            if let Err(err) = send_immediate_server_message(&mut sender, &error_message).await {
                tracing::debug!(
                    client_addr = %addr,
                    error = %err,
                    "Failed to send IP limit error frame"
                );
            }
            let _ = sender.close().await;
            return;
        }
    };

    // Track authentication state.
    let mut authenticated = !server.config().auth_enabled; // Auto-authenticated if auth disabled
    let mut authenticate_processed = false;
    let mut received_application_message = false;

    // When auth is disabled, legacy clients may skip Authenticate entirely. In
    // that mode the endpoint default still applies, so `/v3/ws` starts as v3
    // relay-only while `/v2/ws` remains pure v2. A later first Authenticate can
    // still refine transports/topologies.
    if authenticated {
        let cfg = server.protocol_config();
        let negotiated_version = cfg.negotiate_protocol_version(Some(default_protocol_version));
        let (negotiated_transports, negotiated_topologies) =
            negotiate_capabilities(negotiated_version, None, None);
        server.set_client_protocol(
            &player_id,
            NegotiatedProtocol {
                version: negotiated_version,
                transports: negotiated_transports,
                topologies: negotiated_topologies,
            },
        );
    }

    // Track connection time for authentication timeout
    let connection_start = Instant::now();
    let auth_timeout = Duration::from_secs(server.config().websocket_config.auth_timeout_secs);

    let effective_player_id = Arc::new(RwLock::new(player_id));

    // Spawn task to handle outgoing messages
    let server_clone = server.clone();
    let effective_player_id_for_send = Arc::clone(&effective_player_id);
    let send_task = tokio::spawn(async move {
        let config = server_clone.config();
        let batching_enabled = config.websocket_config.enable_batching;
        let batch_size = config.websocket_config.batch_size;
        let batch_interval_ms = config.websocket_config.batch_interval_ms;

        let mut batcher = MessageBatcher::new(batch_size, batch_interval_ms);

        // The write loop runs INSIDE this select so a close request interrupts
        // it at ANY await point — including a socket write wedged against a
        // peer that stopped reading, which is precisely the slow-consumer
        // state that triggers the close. Handling the close as a sibling loop
        // arm would only observe it BETWEEN writes, and a wedged write never
        // finishes; the sink half would then never drop and the connection
        // would linger as a zombie socket.
        let close_request: Option<Option<CloseReason>> = tokio::select! {
            reason = send_task_close.closed() => Some(reason),
            () = async {
                if batching_enabled {
                    // Batching mode: collect multiple messages and send together.
                    // Clamp to a 1ms floor: `batch_interval_ms` is validated `> 0`
                    // (when batching is enabled) at startup, but
                    // `tokio::time::interval` panics on a zero period, so guard
                    // the timer regardless of how this config was constructed.
                    let mut flush_interval =
                        tokio::time::interval(Duration::from_millis(batch_interval_ms.max(1)));
                    flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                    loop {
                        tokio::select! {
                            // Receive new message from channel
                            message_opt = rx.recv() => {
                                if let Some(message) = message_opt {
                                    batcher.queue(message);

                                    // Flush if batch is full or time threshold exceeded
                                    if batcher.should_flush()
                                        && {
                                            let current_player_id =
                                                *effective_player_id_for_send.read().await;
                                            send_batch(
                                                &mut sender,
                                                &mut batcher,
                                                &current_player_id,
                                                &server_clone,
                                            )
                                            .await
                                            .is_err()
                                        }
                                    {
                                        break;
                                    }
                                } else {
                                    // Channel closed, flush remaining messages and exit
                                    if !batcher.is_empty() {
                                        let current_player_id =
                                            *effective_player_id_for_send.read().await;
                                        let _ = send_batch(
                                            &mut sender,
                                            &mut batcher,
                                            &current_player_id,
                                            &server_clone,
                                        )
                                        .await;
                                    }
                                    break;
                                }
                            }
                            // Periodic flush based on time interval
                            _ = flush_interval.tick() => {
                                if !batcher.is_empty()
                                    && batcher.should_flush()
                                    && {
                                        let current_player_id =
                                            *effective_player_id_for_send.read().await;
                                        send_batch(
                                            &mut sender,
                                            &mut batcher,
                                            &current_player_id,
                                            &server_clone,
                                        )
                                        .await
                                        .is_err()
                                    }
                                {
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    // Non-batching mode: send each message immediately (legacy behavior)
                    while let Some(message) = rx.recv().await {
                        let current_player_id = *effective_player_id_for_send.read().await;
                        if send_single_message(
                            &mut sender,
                            message,
                            &current_player_id,
                            &server_clone,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                }
            } => None,
        };

        // A close was requested (slow consumer, unregistration): the write
        // loop above was cancelled wherever it was; run the bounded farewell/
        // flush/close sequence. When the loop ended by itself (socket error or
        // channel closed after a full flush) there is nothing more to write.
        if let Some(reason) = close_request {
            let current_player_id = *effective_player_id_for_send.read().await;
            finalize_closed_connection(
                &mut sender,
                &mut rx,
                &mut batcher,
                reason,
                &current_player_id,
                &server_clone,
            )
            .await;
        }

        // Cleanup when send task ends
        let current_player_id = *effective_player_id_for_send.read().await;
        server_clone.unregister_client(&current_player_id).await;
    });

    // Handle incoming messages
    let token_binding_for_receive = token_binding.clone();
    let server_clone = server.clone();
    let effective_player_id_for_receive = Arc::clone(&effective_player_id);
    let auth_timeout_secs = server.config().websocket_config.auth_timeout_secs;
    let close_signal_for_receive = close_signal.clone();
    let receive_task = tokio::spawn(async move {
        let mut active_player_id = player_id;
        let token_binding = token_binding_for_receive;
        let close_signal = close_signal_for_receive;
        // Create authentication timeout timer
        let auth_deadline = tokio::time::sleep_until(connection_start + auth_timeout);
        tokio::pin!(auth_deadline);

        // Post-auth idle timeout (0 = disabled). Wrapping each `receiver.next()`
        // means ANY inbound frame — Text, Binary, Ping, Pong, Close — counts as
        // activity and resets the window. The pre-auth phase below is bounded
        // by the (stricter) auth deadline instead.
        let idle_timeout_secs = server_clone.config().websocket_config.idle_timeout_secs;
        let idle_timeout = (idle_timeout_secs > 0).then(|| Duration::from_secs(idle_timeout_secs));

        loop {
            let msg = if authenticated {
                // If authenticated, enforce only the idle timeout (when enabled)
                tokio::select! {
                    // Server-side close request (slow consumer, unregistration):
                    // stop reading; the send task writes the farewell and
                    // closes the sink.
                    reason = receive_task_close.closed() => {
                        tracing::debug!(
                            %active_player_id,
                            ?reason,
                            "Connection close requested; ending receive task"
                        );
                        break;
                    }
                    next_frame = async {
                        match idle_timeout {
                            Some(window) => tokio::time::timeout(window, receiver.next()).await,
                            None => Ok(receiver.next().await),
                        }
                    } => match next_frame {
                        Ok(Some(msg)) => msg,
                        Ok(None) => break, // Connection closed
                        Err(_elapsed) => {
                            // Idle timeout: deliver the error through this
                            // connection's OWN outbound channel, not the
                            // coordinator. With production defaults the
                            // `server.ping_timeout` reaper (30s) unregisters a
                            // silent client from the coordinator long before
                            // the idle window (300s) elapses, so a
                            // coordinator-routed error would be unroutable.
                            // The send task flushes this channel before the
                            // socket is torn down, so the frame reaches the
                            // client regardless of registration state. Then
                            // close via the normal disconnect path so the
                            // reconnection grace period still applies.
                            tracing::info!(
                                %active_player_id,
                                timeout_secs = idle_timeout_secs,
                                "Idle timeout - no frames received, closing connection"
                            );
                            enqueue_connection_message(
                                &tx_clone,
                                &close_signal,
                                &server_clone,
                                slow_consumer_timeout,
                                &active_player_id,
                                ServerMessage::Error {
                                    message: format!(
                                        "Idle timeout - no messages received for {idle_timeout_secs} seconds"
                                    ),
                                    error_code: Some(ErrorCode::ConnectionIdleTimeout),
                                },
                                "idle timeout error",
                            )
                            .await;
                            break;
                        }
                    }
                }
            } else {
                // If not authenticated, enforce timeout
                tokio::select! {
                    reason = receive_task_close.closed() => {
                        tracing::debug!(
                            %active_player_id,
                            ?reason,
                            "Connection close requested during authentication; ending receive task"
                        );
                        break;
                    }
                    msg_opt = receiver.next() => {
                        match msg_opt {
                            Some(msg) => msg,
                            None => break, // Connection closed
                        }
                    }
                    () = &mut auth_deadline => {
                        // Authentication timeout
                        tracing::warn!(%active_player_id, timeout_secs = auth_timeout_secs, "Authentication timeout, closing connection");
                        let _ = server_clone
                            .send_error_to_player(
                                &active_player_id,
                                format!("Authentication timeout - must authenticate within {auth_timeout_secs} seconds"),
                                Some(ErrorCode::AuthenticationTimeout),
                            )
                            .await;
                        break;
                    }
                }
            };

            // Process the message
            let msg = match msg {
                Ok(msg) => msg,
                Err(e) => {
                    tracing::warn!(%active_player_id, "WebSocket error: {}", e);
                    break;
                }
            };

            match msg {
                Message::Text(text) => {
                    // Check message size limit
                    let max_size = server_clone.config().max_message_size;
                    if text.len() > max_size {
                        tracing::warn!(
                            %active_player_id,
                            size = text.len(),
                            max = max_size,
                            "Message exceeds size limit"
                        );
                        let _ = server_clone
                            .send_error_to_player(
                                &active_player_id,
                                format!(
                                    "Message too large ({} bytes, max {} bytes)",
                                    text.len(),
                                    max_size
                                ),
                                Some(ErrorCode::MessageTooLarge),
                            )
                            .await;
                        continue;
                    }

                    let client_message = match parse_client_message(&text, token_binding.as_ref()) {
                        Ok(message) => message,
                        Err(err) => {
                            tracing::warn!(
                                %active_player_id,
                                error = %err,
                                "Rejected client WebSocket frame"
                            );
                            let _ = server_clone
                                .send_error_to_player(
                                    &active_player_id,
                                    err.user_message().to_string(),
                                    Some(err.error_code()),
                                )
                                .await;
                            if err.should_disconnect() {
                                break;
                            }
                            continue;
                        }
                    };

                    match client_message {
                        ClientMessage::Authenticate {
                            app_id,
                            sdk_version,
                            platform,
                            game_data_format,
                            protocol_version,
                            supported_transports,
                            supported_topologies,
                        } => {
                            if server_clone.config().auth_enabled && authenticated {
                                tracing::warn!(%active_player_id, "Client already authenticated");
                                continue;
                            }
                            if !server_clone.config().auth_enabled
                                && (authenticate_processed || received_application_message)
                            {
                                tracing::warn!(
                                    %active_player_id,
                                    "Authenticate must be the first client message"
                                );
                                continue;
                            }

                            // Validate App ID
                            match server_clone.auth_middleware.validate_app_id(&app_id).await {
                                Ok(info) => {
                                    let compatibility = match server_clone
                                        .protocol_config()
                                        .sdk_compatibility
                                        .evaluate(platform.as_deref(), sdk_version.as_deref())
                                    {
                                        Ok(report) => report,
                                        Err(err) => {
                                            let error_message = err.to_string();
                                            tracing::warn!(
                                                %active_player_id,
                                                app_id = %app_id,
                                                ?sdk_version,
                                                ?platform,
                                                error = %error_message,
                                                "SDK compatibility check failed"
                                            );
                                            enqueue_connection_message(
                                                &tx_clone,
                                                &close_signal,
                                                &server_clone,
                                                slow_consumer_timeout,
                                                &active_player_id,
                                                ServerMessage::AuthenticationError {
                                                    error: error_message,
                                                    error_code: ErrorCode::SdkVersionUnsupported,
                                                },
                                                "SDK compatibility error",
                                            )
                                            .await;
                                            continue;
                                        }
                                    };

                                    authenticated = true;
                                    authenticate_processed = true;
                                    server_clone
                                        .set_client_app_info(&active_player_id, info.clone());
                                    server_clone.apply_app_bandwidth_policy(&info);
                                    let supported_formats = server_clone
                                        .protocol_config()
                                        .supported_game_data_formats();
                                    let negotiated_format = match game_data_format {
                                        Some(format) if supported_formats.contains(&format) => {
                                            format
                                        }
                                        Some(format) => {
                                            let supported_list: Vec<String> = supported_formats
                                                .iter()
                                                .map(|format| format.as_wire_str().to_string())
                                                .collect();
                                            let error_message = format!(
                                                "Requested game data format '{}' is not supported. Server supports: {}. Falling back to JSON.",
                                                format.as_wire_str(),
                                                supported_list.join(", ")
                                            );
                                            tracing::warn!(
                                                %active_player_id,
                                                requested_format = format.as_wire_str(),
                                                supported_formats = %supported_list.join(", "),
                                                "Client requested unsupported game_data_format"
                                            );
                                            // Send error message to client about capability mismatch
                                            enqueue_connection_message(
                                                &tx_clone,
                                                &close_signal,
                                                &server_clone,
                                                slow_consumer_timeout,
                                                &active_player_id,
                                                ServerMessage::Error {
                                                    message: error_message,
                                                    error_code: Some(
                                                        ErrorCode::UnsupportedGameDataFormat,
                                                    ),
                                                },
                                                "game data format error",
                                            )
                                            .await;
                                            GameDataEncoding::Json
                                        }
                                        None => GameDataEncoding::Json,
                                    };
                                    server_clone.set_client_game_data_format(
                                        &active_player_id,
                                        negotiated_format,
                                    );

                                    // Protocol version + capability negotiation (P1).
                                    // A missing `protocol_version` on the /v3 path is
                                    // treated as the path default (3); on /v2 it stays
                                    // v2. Explicit client values always take precedence.
                                    let cfg = server_clone.protocol_config();
                                    let client_max =
                                        protocol_version.or(Some(default_protocol_version));
                                    let negotiated_version =
                                        cfg.negotiate_protocol_version(client_max);

                                    let (negotiated_transports, negotiated_topologies) =
                                        negotiate_capabilities(
                                            negotiated_version,
                                            supported_transports,
                                            supported_topologies,
                                        );

                                    let min_protocol_version = cfg.min_protocol_version;
                                    let max_protocol_version = cfg.max_protocol_version;

                                    server_clone.set_client_protocol(
                                        &active_player_id,
                                        NegotiatedProtocol {
                                            version: negotiated_version,
                                            transports: negotiated_transports.clone(),
                                            topologies: negotiated_topologies.clone(),
                                        },
                                    );

                                    tracing::info!(
                                        %active_player_id,
                                        app_name = %info.name,
                                        app_id = %app_id,
                                        ?sdk_version,
                                        ?platform,
                                        protocol_version = negotiated_version,
                                        ?negotiated_transports,
                                        ?negotiated_topologies,
                                        "Client authenticated"
                                    );

                                    // Send success response
                                    let auth_response = ServerMessage::Authenticated {
                                        app_name: info.name.clone(),
                                        organization: info.organization.clone(),
                                        rate_limits: RateLimitInfo {
                                            per_minute: info.rate_limits.per_minute,
                                            per_hour: info.rate_limits.per_hour,
                                            per_day: info.rate_limits.per_day,
                                        },
                                    };

                                    let player_name_rules =
                                        PlayerNameRulesPayload::from_protocol_config(
                                            server_clone.protocol_config(),
                                        );
                                    let (
                                        response_protocol_version,
                                        response_min_protocol_version,
                                        response_max_protocol_version,
                                    ) = if negotiated_version >= 3 {
                                        (
                                            Some(negotiated_version),
                                            Some(min_protocol_version),
                                            Some(max_protocol_version),
                                        )
                                    } else {
                                        (None, None, None)
                                    };
                                    let protocol_info =
                                        ServerMessage::ProtocolInfo(ProtocolInfoPayload {
                                            platform: compatibility.platform.clone(),
                                            sdk_version: compatibility.sdk_version.clone(),
                                            minimum_version: compatibility.minimum_version.clone(),
                                            recommended_version: compatibility
                                                .recommended_version
                                                .clone(),
                                            capabilities: compatibility.capabilities.clone(),
                                            notes: compatibility.notes.clone(),
                                            game_data_formats: supported_formats,
                                            player_name_rules: Some(player_name_rules),
                                            protocol_version: response_protocol_version,
                                            min_protocol_version: response_min_protocol_version,
                                            max_protocol_version: response_max_protocol_version,
                                        });

                                    enqueue_connection_message(
                                        &tx_clone,
                                        &close_signal,
                                        &server_clone,
                                        slow_consumer_timeout,
                                        &active_player_id,
                                        auth_response,
                                        "authentication success response",
                                    )
                                    .await;
                                    enqueue_connection_message(
                                        &tx_clone,
                                        &close_signal,
                                        &server_clone,
                                        slow_consumer_timeout,
                                        &active_player_id,
                                        protocol_info,
                                        "protocol info response",
                                    )
                                    .await;
                                }
                                Err(e) => {
                                    tracing::warn!(%active_player_id, %app_id, "Authentication failed: {:?}", e);

                                    // Send error response.
                                    // The AppIdExpired, AppIdRevoked, and AppIdSuspended
                                    // variants are not currently returned by
                                    // `validate_app_id`, but are retained for future
                                    // backend implementations (e.g., app status management
                                    // or admin-controlled app suspension).
                                    let error_code = match e {
                                        crate::auth::AuthError::InvalidAppId => {
                                            ErrorCode::InvalidAppId
                                        }
                                        crate::auth::AuthError::AppIdExpired => {
                                            ErrorCode::AppIdExpired
                                        }
                                        crate::auth::AuthError::AppIdRevoked => {
                                            ErrorCode::AppIdRevoked
                                        }
                                        crate::auth::AuthError::AppIdSuspended => {
                                            ErrorCode::AppIdSuspended
                                        }
                                        crate::auth::AuthError::RateLimitExceeded => {
                                            ErrorCode::RateLimitExceeded
                                        }
                                        _ => ErrorCode::InternalError,
                                    };

                                    enqueue_connection_message(
                                        &tx_clone,
                                        &close_signal,
                                        &server_clone,
                                        slow_consumer_timeout,
                                        &active_player_id,
                                        ServerMessage::AuthenticationError {
                                            error: format!("{e:?}"),
                                            error_code,
                                        },
                                        "authentication failure response",
                                    )
                                    .await;

                                    // Close connection after auth failure
                                    break;
                                }
                            }
                        }
                        other => {
                            if !authenticated {
                                tracing::warn!(%active_player_id, "Received message before authentication");
                                let _ = server_clone
                                    .send_error_to_player(
                                        &active_player_id,
                                        "Authentication required".to_string(),
                                        Some(ErrorCode::MissingAppId),
                                    )
                                    .await;
                                break;
                            }

                            received_application_message = true;
                            match other {
                                ClientMessage::Reconnect {
                                    player_id: reconnect_player_id,
                                    room_id,
                                    auth_token,
                                } => {
                                    if server_clone
                                        .handle_reconnect(
                                            &active_player_id,
                                            &reconnect_player_id,
                                            &room_id,
                                            &auth_token,
                                        )
                                        .await
                                    {
                                        active_player_id = reconnect_player_id;
                                        *effective_player_id_for_receive.write().await =
                                            reconnect_player_id;
                                    }
                                }
                                other => {
                                    server_clone
                                        .handle_client_message(&active_player_id, other)
                                        .await;
                                }
                            }
                        }
                    }
                }
                Message::Binary(payload) => {
                    if !authenticated {
                        tracing::warn!(%active_player_id, "Received binary message before authentication");
                        let _ = server_clone
                            .send_error_to_player(
                                &active_player_id,
                                "Authentication required before sending binary data".to_string(),
                                Some(ErrorCode::MissingAppId),
                            )
                            .await;
                        break;
                    }

                    received_application_message = true;
                    let encoding = server_clone.client_game_data_format(&active_player_id);
                    if encoding == GameDataEncoding::Json {
                        tracing::warn!(
                            %active_player_id,
                            "Client negotiated JSON game data but sent binary payload; dropping"
                        );
                        let _ = server_clone
                            .send_error_to_player(
                                &active_player_id,
                                "Binary payloads are disabled for this connection".to_string(),
                                Some(ErrorCode::InvalidInput),
                            )
                            .await;
                        continue;
                    }

                    // Payload from axum WebSocket is already Bytes - pass directly for zero-copy
                    server_clone
                        .handle_game_data_binary(&active_player_id, encoding, payload)
                        .await;
                }
                Message::Close(_) => {
                    tracing::info!(%active_player_id, "WebSocket connection closed");
                    break;
                }
                Message::Pong(_) => {
                    // Handle pong as ping response
                    server_clone
                        .handle_client_message(&active_player_id, ClientMessage::Ping)
                        .await;
                }
                _ => {
                    // Ignore other message types
                }
            }
        }

        // Cleanup when receive task ends
        server_clone.unregister_client(&active_player_id).await;
    });

    // Wait for either task to complete
    tokio::select! {
        _ = send_task => {
            let current_player_id = *effective_player_id.read().await;
            tracing::info!(%current_player_id, "Send task completed");
        }
        _ = receive_task => {
            let current_player_id = *effective_player_id.read().await;
            tracing::info!(%current_player_id, "Receive task completed");
        }
    }

    // Ensure cleanup
    let current_player_id = *effective_player_id.read().await;
    server.unregister_client(&current_player_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseConfig;
    use crate::protocol::{ClientMessage, ServerMessage};
    use crate::server::ServerConfig;
    use std::net::SocketAddr;
    use tokio_tungstenite::{connect_async, tungstenite::Message as TungsteniteMessage};

    #[test]
    fn negotiate_capabilities_v2_is_relay_only_even_if_p2p_advertised() {
        // A connection negotiated below v3 must be relay-only regardless of what
        // it advertised — P2P is a v3+ upgrade and must not leak to a v2 peer.
        let (transports, topologies) = negotiate_capabilities(
            2,
            Some(vec![Transport::WebRtc, Transport::Direct]),
            Some(vec![Topology::Mesh, Topology::Host]),
        );
        assert_eq!(transports, vec![Transport::Relay]);
        assert_eq!(topologies, vec![Topology::Relay]);
    }

    #[test]
    fn negotiate_capabilities_v3_forces_relay_floor_and_dedups() {
        // Client advertised P2P but omitted Relay and repeated WebRtc: Relay is
        // forced in, duplicates removed, preference order preserved.
        let (transports, topologies) = negotiate_capabilities(
            3,
            Some(vec![
                Transport::WebRtc,
                Transport::Direct,
                Transport::WebRtc,
            ]),
            Some(vec![Topology::Mesh, Topology::Mesh]),
        );
        assert_eq!(
            transports,
            vec![Transport::WebRtc, Transport::Direct, Transport::Relay]
        );
        assert_eq!(topologies, vec![Topology::Mesh, Topology::Relay]);
    }

    #[test]
    fn negotiate_capabilities_v3_defaults_to_relay_when_unspecified() {
        let (transports, topologies) = negotiate_capabilities(3, None, None);
        assert_eq!(transports, vec![Transport::Relay]);
        assert_eq!(topologies, vec![Topology::Relay]);
    }

    #[test]
    fn dedup_preserving_order_drops_non_adjacent_repeats() {
        // Non-adjacent duplicates (the case `Vec::dedup` misses) are removed,
        // first-occurrence order is preserved.
        let mut transports = vec![
            Transport::WebRtc,
            Transport::Relay,
            Transport::WebRtc,
            Transport::Direct,
            Transport::Relay,
        ];
        dedup_preserving_order(&mut transports);
        assert_eq!(
            transports,
            vec![Transport::WebRtc, Transport::Relay, Transport::Direct]
        );

        // Already-unique and empty inputs are unchanged.
        let mut unique = vec![Topology::Relay, Topology::Mesh, Topology::Host];
        dedup_preserving_order(&mut unique);
        assert_eq!(
            unique,
            vec![Topology::Relay, Topology::Mesh, Topology::Host]
        );

        let mut empty: Vec<Transport> = Vec::new();
        dedup_preserving_order(&mut empty);
        assert!(empty.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg_attr(miri, ignore)]
    async fn test_websocket_connection() {
        // Add overall test timeout to prevent infinite hanging
        let test_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(30),
            test_websocket_connection_impl(),
        )
        .await;

        match test_result {
            Ok(Ok(())) => {} // Test completed successfully
            // A failed setup/exchange MUST fail the test loudly — never a silent
            // `return` that lets CI pass while the server never became reachable.
            Ok(Err(error)) => panic!("websocket connection test failed: {error:#}"),
            Err(_) => panic!("Test timed out after 30 seconds"),
        }
    }

    // Returns `Result` so EVERY setup/exchange failure propagates and fails the
    // test (the wrapper panics on `Err`). A bare `return` here would let a broken
    // server silently pass CI — the exact regression this shape prevents (and the
    // `tests/loud_test_failures_scan.rs` guard enforces repo-wide).
    async fn test_websocket_connection_impl() -> anyhow::Result<()> {
        use anyhow::Context as _;

        // Start test server
        let addr: SocketAddr = "127.0.0.1:0".parse().context("parse test address")?;
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .context("bind test listener")?;
        let addr = listener
            .local_addr()
            .context("read local listener address")?;

        let game_server = EnhancedGameServer::new(
            ServerConfig::default(),
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::AuthMaintenanceConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .context("create game server")?;
        let app =
            super::super::routes::create_router("http://localhost:3000").with_state(game_server);

        tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!("Test server failed: {}", e);
            }
        });

        // Poll the server until it accepts a WebSocket connection, rather than a
        // fixed startup sleep (zero-flakiness policy, .llm/context-testing.md): a
        // fixed sleep flakes when an oversubscribed runner has not bound the
        // listener yet. The happy path connects on the first attempt (typically
        // within a few ms of the spawn); the generous deadline only bites under
        // pathological load.
        let url = format!("ws://{addr}/ws");
        let ready_deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);
        let ws_stream = loop {
            match tokio::time::timeout(tokio::time::Duration::from_secs(5), connect_async(&url))
                .await
            {
                Ok(Ok((stream, _response))) => break stream,
                outcome => {
                    anyhow::ensure!(
                        tokio::time::Instant::now() < ready_deadline,
                        "WebSocket server did not become ready within 30s: {outcome:?}"
                    );
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
            }
        };
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        // Send join room message
        let join_message = ClientMessage::JoinRoom {
            game_name: "test_game".to_string(),
            room_code: None,
            player_name: "TestPlayer".to_string(),
            max_players: Some(4),
            supports_authority: Some(true),
            relay_transport: None,
        };

        let json_message =
            serde_json::to_string(&join_message).context("serialize join message")?;
        ws_sender
            .send(TungsteniteMessage::Text(json_message.into()))
            .await
            .context("send join message")?;

        // Receive response with timeout — propagate the elapsed, closed-stream,
        // and transport-error cases instead of swallowing any of them.
        let msg = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws_receiver.next())
            .await
            .context("timed out waiting for join response after 5s")?
            .context("websocket closed before sending a join response")?
            .context("receive websocket message")?;

        let TungsteniteMessage::Text(text) = msg else {
            anyhow::bail!("expected a Text websocket frame, got {msg:?}");
        };
        let server_message: ServerMessage =
            serde_json::from_str(&text).context("deserialize server message")?;
        match server_message {
            ServerMessage::RoomJoined(_) => Ok(()),
            ServerMessage::RoomJoinFailed { reason, .. } => {
                anyhow::bail!("room join failed: {reason}")
            }
            other => anyhow::bail!("unexpected server message: {other:?}"),
        }
    }
}
