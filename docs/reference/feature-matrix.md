# Feature Availability Matrix

Which protocol version each feature needs, the exact server config key(s) that gate it, and what the client must
send or handle. Every config key below exists in the server's configuration — see the
[Configuration Reference](../configuration.md) for defaults and environment-variable overrides, and
[Protocol v2 vs v3](../concepts/protocol-versions.md) for the version model.

"Both" in the protocol column means the feature works on a v2 (relay-only) and a v3 connection alike. Only
room-wide P2P upgrades require the all-members-v3 invariant; recipient-gated v3 features such as delivery classes
do not (see [the two invariants](../concepts/protocol-versions.md#the-two-invariants)).

| Feature | Protocol version | Server config required | Client support required |
| --- | --- | --- | --- |
| Lobby ready-up / start | both | none (always available); `max_players` (via `JoinRoom`) is a ceiling, not a required count | Send `PlayerReady` to toggle readiness; send `StartGame` to finalize once every current player is ready; handle `LobbyStateChanged` and `GameStarting` |
| Reconnection | both | `server.enable_reconnection` (default `true`); `server.reconnection_window`, `server.event_buffer_size` tune it | Store `player_id` / `room_id` / `auth_token`; send `Reconnect`; handle `Reconnected` / `ReconnectionFailed` |
| Spectators | both | none (always available) | Send `JoinAsSpectator` / `LeaveSpectator`; handle `SpectatorJoined` and related events |
| Authority | both | none; room created with `supports_authority` (via `JoinRoom`) | Send `AuthorityRequest`; handle `AuthorityChanged` / `AuthorityResponse` |
| Classified JSON delivery | v3, per recipient | none; queue/socket behavior is tuned by `websocket.send_queue_capacity`, `websocket.control_queue_capacity`, `websocket.slow_consumer_timeout_ms`, `websocket.max_sojourn_ms`, and `websocket.socket_send_buffer_bytes` | Send well-formed `GameData.class` / `key`; handle `DeliveryReport`, stamped `epoch` / `seq`, lifecycle overtaking, and reconnect baselines. Does **not** require an all-members-v3 room upgrade; v2 recipients remain reliable FIFO |
| Delivery diagnostics | v3 | `websocket.delivery_stats_interval_secs` enables periodic `RelayStats` and counter-only `DeliveryReport` snapshots; exact gap reports are always event-driven | Treat `RelayStats` and counters as diagnostics only; authorize a hole from the union of prior exact ranges (at most 256 ranges per report) |
| Mesh topology | v3 | `session.default_topology` / `session.game_topology_mappings` set to `mesh`; `session.enable_webrtc` (default `true`) | `Authenticate` with `supported_topologies` including `mesh` and `supported_transports` including `webrtc`; replace peer state from each `SessionPlan`; exchange `Signal` |
| Host topology | v3 | `session.default_topology` / `session.game_topology_mappings` set to `host`; `session.enable_webrtc` and/or `session.enable_direct` | `Authenticate` with `supported_topologies` including `host`; `supported_transports` with `webrtc` and/or `direct`; for Direct, an electable host must send `ProvideConnectionInfo` with a usable endpoint; replace peer state from each `SessionPlan` (+ `Signal` for WebRTC) |
| Direct transport | v3 | `session.enable_direct` (default `true`); requires a non-`relay` desired topology (`host`) and at least one endpoint-ready host | `Authenticate` with `supported_transports` including `direct`; an intended host sends `ProvideConnectionInfo { type: direct, host, port }`; consume `SessionPlan.direct_endpoint`; report failure and use `fallback: relay` when connection fails |
| WebRTC transport | v3 | `session.enable_webrtc` (default `true`) | `Authenticate` with `supported_transports` including `webrtc`; replace peer state from each `SessionPlan`; exchange `Signal` (`Offer` / `Answer` / `IceCandidate`) |
| TURN / ICE | v3 | STUN via `turn.stun_urls`; ephemeral TURN via `turn.enabled` + `turn.static_auth_secret` + `turn.urls` (+ `turn.credential_ttl_secs`); static servers via `session.ice_servers` | Apply `ice_servers` from `SessionPlan` (and pre-gather) when establishing WebRTC |
| ICE pre-gather | v3 | `session.enable_ice_pregather` (default `true`) **and** `session.enable_webrtc`; non-relay desired topology; non-`Finalized` room | Negotiate `webrtc` transport + the game's desired topology; read optional `ice_servers` on `RoomJoined` / `Reconnected`; prefer the latest set (`SessionPlan` supersedes) |
| Metrics | both | `/metrics` and `/metrics/prom` always exposed; protect with `security.require_metrics_auth` + `security.metrics_auth_token` | Scrape `metricsSnapshot.connections.delivery_by_class` or `signal_fish_websocket_delivery_class_outcomes_total{class,outcome}`; `TransportStatus` reports feed v3 transport counters |
| Batching | both | `websocket.enable_batching` (default `true`); `websocket.batch_size`, `websocket.batch_interval_ms` | None (transparent to clients) |
| TLS | both | `security.transport.tls.enabled` + `security.transport.tls.certificate_path` + `security.transport.tls.private_key_path` (optional `client_ca_cert_path`, `client_auth`) | Connect over `wss://`; present a client cert if `client_auth` requires it |
| Token binding | both | `security.transport.token_binding.enabled` (+ `required`, `subprotocol`, `scheme`); `require_client_fingerprint` additionally requires `required=true`, built-in TLS, and `tls.client_auth=optional` or `require` | Negotiate v2, derive a connection key from the server challenge, and sequence every RFC 8785 JSON or versioned MessagePack proof; certificate mode also binds reconnect credentials to the authenticated leaf SHA-256 |
| MessagePack game data | both | `protocol.enable_message_pack_game_data` (default `true`) | `Authenticate` with `game_data_format` of `message_pack`; token-bound clients wrap exact legacy payload bytes in the signed v2 binary envelope |

## Notes

- **Topology vs transport.** Topology (`relay` / `host` / `mesh`) and transport (`relay` / `direct` / `webrtc`)
  are independent axes; only four pairs are legal — `mesh + webrtc`, `host + webrtc`, `host + direct`, and the
  `relay` floor. A WebRTC plan always advertises STUN; the `direct` transport carries no ICE.
- **`desired` topology is a ceiling.** `session.default_topology` (and per-game `session.game_topology_mappings`)
  set the richest plan a game may select. The server still falls back down the ladder, and ultimately to the
  relay floor, whenever a richer rung is disabled or not supported by every member.
- **All-members-v3 gate.** Every room-wide topology/transport upgrade row above requires that _every_ room member
  negotiated v3 and supports the chosen topology and transport; otherwise the room stays on the relay floor and
  sends each v3 member an explicit no-peer `relay`/`relay` `SessionPlan`. V2
  members receive no plan. Classified delivery is independently selected for each recipient.
- **TURN is self-hosted only.** `turn.static_auth_secret` is the coturn `--static-auth-secret`; it is
  server-only and never sent to clients — only short-lived per-player username/credential pairs derived from it
  reach a client.

## See also

- [Configuration Reference](../configuration.md) — full key list, defaults, and environment overrides.
- [Protocol v2 vs v3](../concepts/protocol-versions.md) — the version model and migration checklist.
- [Features](../features.md) — narrative overview of each capability.
- [Protocol Reference](../protocol.md) — message-level detail.
