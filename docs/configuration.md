# Configuration

Signal Fish Server uses a JSON config file with environment variable overrides.

## Config File

On startup, the server looks for `config.json` in the working directory.

See [`config.example.json`](../config.example.json) for a local-development
reference, and the table below for all available overrides.

## Essential Settings

### Port

```json
{
  "port": 3536
}

```

Environment override:

```bash

SIGNAL_FISH__PORT=8080 cargo run

```

### Max Players

```json

{
  "server": {
    "default_max_players": 8
  }
}

```

Environment override:

```bash

SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS=16 cargo run

```

### Room Limits

```json

{
  "server": {
    "max_rooms_per_game": 1000,
    "empty_room_timeout": 300,
    "inactive_room_timeout": 3600
  }
}

```

- `max_rooms_per_game` - Maximum concurrent rooms per game name (must be > 0)
- `empty_room_timeout` - Seconds before an empty room is cleaned up (default: 300)
- `inactive_room_timeout` - Seconds before an inactive room is removed (default: 3600)

### Reconnection

```json

{
  "server": {
    "enable_reconnection": true,
    "reconnection_window": 300,
    "event_buffer_size": 100
  }
}

```

- `enable_reconnection` - Enable token-based reconnection (default: true)
- `reconnection_window` - Seconds a reconnection token stays valid (default: 300; must be > 0)
- `event_buffer_size` - Max events buffered for replay (default: 100; maximum: 65,536)

## Environment Variable Format

All config fields use the `SIGNAL_FISH__` prefix. Nested fields use double underscores (`__`).

Examples:

```bash
# Top-level field
SIGNAL_FISH__PORT=3536

# Nested field: server.default_max_players
SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS=8

# Nested field: rate_limit.max_room_creations
SIGNAL_FISH__RATE_LIMIT__MAX_ROOM_CREATIONS=10

```

## Configuration Reference

Complete reference of all configuration options with environment variable overrides:

| Environment Variable | Config Path | Default | Description |
| --- | --- | --- | --- |
| `SIGNAL_FISH__PORT` | `port` | `3536` | Server listen port |
| `SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS` | `server.default_max_players` | `8` | Default max players per room |
| `SIGNAL_FISH__SERVER__PING_TIMEOUT` | `server.ping_timeout` | `30` | Seconds before a silent client is dropped (`0` disables the activity reaper) |
| `SIGNAL_FISH__SERVER__ROOM_CLEANUP_INTERVAL` | `server.room_cleanup_interval` | `60` | Seconds between room cleanup sweeps (must be > 0) |
| `SIGNAL_FISH__SERVER__DRAIN_GRACE_SECS` | `server.drain_grace_secs` | `30` | Seconds between shutdown drain start and forced close `4000`; `0` closes immediately |
| `SIGNAL_FISH__SERVER__MAX_ROOMS_PER_GAME` | `server.max_rooms_per_game` | `1000` | Max rooms allowed per game name (must be > 0) |
| `SIGNAL_FISH__SERVER__EMPTY_ROOM_TIMEOUT` | `server.empty_room_timeout` | `300` | Seconds before an empty room is removed |
| `SIGNAL_FISH__SERVER__INACTIVE_ROOM_TIMEOUT` | `server.inactive_room_timeout` | `3600` | Seconds before an inactive room is removed and assigned clients close with `4005 room_inactive` |
| `SIGNAL_FISH__SERVER__RECONNECTION_WINDOW` | `server.reconnection_window` | `300` | Seconds a reconnection token stays valid (must be > 0) |
| `SIGNAL_FISH__SERVER__EVENT_BUFFER_SIZE` | `server.event_buffer_size` | `100` | Max events buffered for reconnection replay (maximum: 65,536) |
| `SIGNAL_FISH__SERVER__ENABLE_RECONNECTION` | `server.enable_reconnection` | `true` | Enable reconnection support |
| `SIGNAL_FISH__SERVER__HEARTBEAT_THROTTLE_SECS` | `server.heartbeat_throttle_secs` | `30` | Min seconds between `last_seen` heartbeat writes |
| `SIGNAL_FISH__SERVER__REGION_ID` | `server.region_id` | `default` | Deployment region identifier; recorded in internal player and room state (not serialized to clients) |
| `SIGNAL_FISH__SERVER__ROOM_CODE_PREFIX` | `server.room_code_prefix` | `null` | Optional ASCII-alphanumeric generated-code prefix; must be shorter than `protocol.room_code_length` |
| `SIGNAL_FISH__RATE_LIMIT__MAX_ROOM_CREATIONS` | `rate_limit.max_room_creations` | `5` | Max room creations per player per window (must be > 0) |
| `SIGNAL_FISH__RATE_LIMIT__TIME_WINDOW` | `rate_limit.time_window` | `60` | Rate limit window in seconds (must be > 0) |
| `SIGNAL_FISH__RATE_LIMIT__MAX_JOIN_ATTEMPTS` | `rate_limit.max_join_attempts` | `20` | Shared max room-creation, seated-join, and spectator-join attempts per player per window (must be > 0) |
| `SIGNAL_FISH__RATE_LIMIT__MAX_SIGNALS` | `rate_limit.max_signals` | `600` | Max validated WebRTC Signal dispatch attempts per player per window (must be > 0) |
| `SIGNAL_FISH__RATE_LIMIT__MAX_SIGNAL_ERRORS` | `rate_limit.max_signal_errors` | `60` | Detailed WebRTC rejection errors per player per window before generic rate-limit errors |
| `SIGNAL_FISH__PROTOCOL__MAX_GAME_NAME_LENGTH` | `protocol.max_game_name_length` | `64` | Max bytes (UTF-8) in a game name (must be > 0) |
| `SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH` | `protocol.room_code_length` | `6` | Nonzero length of generated room codes |
| `SIGNAL_FISH__PROTOCOL__MAX_PLAYER_NAME_LENGTH` | `protocol.max_player_name_length` | `32` | Max bytes (UTF-8) in a player name (must be > 0) |
| `SIGNAL_FISH__PROTOCOL__MAX_PLAYERS_LIMIT` | `protocol.max_players_limit` | `100` | Hard ceiling on players per room (must be > 0) |
| `SIGNAL_FISH__PROTOCOL__ENABLE_MESSAGE_PACK_GAME_DATA` | `protocol.enable_message_pack_game_data` | `true` | Enable MessagePack game-data frames |
| `SIGNAL_FISH__PROTOCOL__MIN_PROTOCOL_VERSION` | `protocol.min_protocol_version` | `2` | Lowest accepted protocol version |
| `SIGNAL_FISH__PROTOCOL__MAX_PROTOCOL_VERSION` | `protocol.max_protocol_version` | `3` | Highest negotiated protocol version (clamp back to `2` to disable v3 features) |
| `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE` | `protocol.sdk_compatibility.enforce` | `false` | Enforce SDK platform/version checks (opt-in: the default platform list would otherwise reject unregistered/custom clients) |
| `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__MINIMUM_VERSIONS` | `protocol.sdk_compatibility.minimum_versions` | `platform defaults` | JSON object of minimum SDK versions |
| `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__RECOMMENDED_VERSIONS` | `protocol.sdk_compatibility.recommended_versions` | `platform defaults` | JSON object of recommended SDK versions |
| `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__CAPABILITIES` | `protocol.sdk_compatibility.capabilities` | `platform defaults` | JSON object of advertised capability lists |
| `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__NOTES` | `protocol.sdk_compatibility.notes` | `platform defaults` | JSON object of platform-specific notes |
| `SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOW_UNICODE_ALPHANUMERIC` | `protocol.player_name_validation.allow_unicode_alphanumeric` | `true` | Allow Unicode alphanumeric player names |
| `SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOW_SPACES` | `protocol.player_name_validation.allow_spaces` | `true` | Allow internal spaces in player names |
| `SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOW_LEADING_TRAILING_WHITESPACE` | `protocol.player_name_validation.allow_leading_trailing_whitespace` | `false` | Allow leading or trailing whitespace in player names |
| `SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ALLOWED_SYMBOLS` | `protocol.player_name_validation.allowed_symbols` | `["-","_"]` | Symbol allowlist for player names |
| `SIGNAL_FISH__PROTOCOL__PLAYER_NAME_VALIDATION__ADDITIONAL_ALLOWED_CHARACTERS` | `protocol.player_name_validation.additional_allowed_characters` | `null` | Optional extra player-name characters |
| `SIGNAL_FISH__LOGGING__DIR` | `logging.dir` | `logs` | Directory for rolling log files |
| `SIGNAL_FISH__LOGGING__FILENAME` | `logging.filename` | `server.log` | Rolling log filename |
| `SIGNAL_FISH__LOGGING__ROTATION` | `logging.rotation` | `daily` | File rotation policy (`daily`, `hourly`, `never`) |
| `SIGNAL_FISH__LOGGING__LEVEL` | `logging.level` | `null` | Log level override (`trace`, `debug`, `info`, `warn`, `error`) |
| `SIGNAL_FISH__LOGGING__ENABLE_FILE_LOGGING` | `logging.enable_file_logging` | `true` | Enable rolling file logs |
| `SIGNAL_FISH__LOGGING__FORMAT` | `logging.format` | `json` | Log output format (`json` or `text`) |
| `SIGNAL_FISH__SECURITY__CORS_ORIGINS` | `security.cors_origins` | `http://localhost:3000,http://localhost:5173` | Allowed HTTP and browser WebSocket origins (comma-separated or `*`) |
| `SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST` | `security.enforce_app_id_allowlist` | `true` | Require the public client app ID to appear in `allowed_apps` |
| `SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH` | `security.require_metrics_auth` | `true` | Require auth token for metrics endpoints |
| `SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN` | `security.metrics_auth_token` | `null` | Bearer token for metrics endpoints |
| `SIGNAL_FISH__SECURITY__MAX_MESSAGE_SIZE` | `security.max_message_size` | `65536` | Max inbound WebSocket message size in bytes |
| `SIGNAL_FISH__SECURITY__MAX_OUTBOUND_MESSAGE_SIZE` | `security.max_outbound_message_size` | `8388608` | Max aggregate encoded server WebSocket application payload in bytes (`1..=67108864`); oversized messages close the affected connection with code `1009` |
| `SIGNAL_FISH__SECURITY__MAX_SIGNAL_BYTES` | `security.max_signal_bytes` | `16384` | Max serialized size in bytes of a v3 `Signal` payload (must be > 0 and ≤ `max_message_size`) |
| `SIGNAL_FISH__SECURITY__MAX_CONNECTIONS_PER_IP` | `security.max_connections_per_ip` | `24` | Max concurrent connections from one IP (covers a 16-player NAT/LAN session plus spectators and reconnect churn; must be > 0 — a zero cap rejects every registration) |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED` | `security.transport.tls.enabled` | `false` | Enable built-in TLS listener |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH` | `security.transport.tls.certificate_path` | `null` | Path to PEM certificate chain |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH` | `security.transport.tls.private_key_path` | `null` | Path to PEM private key |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_CA_CERT_PATH` | `security.transport.tls.client_ca_cert_path` | `null` | Path to trusted client CA bundle |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_AUTH` | `security.transport.tls.client_auth` | `none` | TLS client auth mode (`none`, `optional`, `require`) |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED` | `security.transport.token_binding.enabled` | `false` | Enable token-binding negotiation |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED` | `security.transport.token_binding.required` | `false` | Require token-binding subprotocol |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRE_CLIENT_FINGERPRINT` | `security.transport.token_binding.require_client_fingerprint` | `false` | Require each proof to carry the authenticated mTLS leaf certificate's lowercase hex SHA-256 fingerprint and privately bind each newly issued reconnect credential to that identity; requires `token_binding.required=true`, built-in TLS, and `client_auth=optional` or `require` |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__SUBPROTOCOL` | `security.transport.token_binding.subprotocol` | `signalfish.tokenbinding.v2` | Replay-resistant token-binding WebSocket subprotocol |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__SCHEME` | `security.transport.token_binding.scheme` | `server_nonce_hkdf_sha256` | Server-fresh token-binding signing scheme |
| `SIGNAL_FISH__SECURITY__ALLOWED_APPS` | `security.allowed_apps` | `[]` | JSON array of public app-ID registrations and accounting limits (`max_rooms`, `max_players_per_room`, and `rate_limit_per_minute` must be > 0 when set) |
| `SIGNAL_FISH__COORDINATION__MEMBERSHIP_SNAPSHOT_INTERVAL_SECS` | `coordination.membership_snapshot_interval_secs` | `30` | Reserved membership-snapshot seam; the shipped coordinator is process-local |
| `SIGNAL_FISH__METRICS__DASHBOARD_CACHE_REFRESH_INTERVAL_SECS` | `metrics.dashboard_cache_refresh_interval_secs` | `5` | Dashboard metrics refresh interval |
| `SIGNAL_FISH__METRICS__DASHBOARD_CACHE_TTL_SECS` | `metrics.dashboard_cache_ttl_secs` | `30` | Dashboard metrics cache TTL |
| `SIGNAL_FISH__METRICS__DASHBOARD_CACHE_HISTORY_WINDOW_SECS` | `metrics.dashboard_cache_history_window_secs` | `300` | Dashboard history window |
| `SIGNAL_FISH__METRICS__DASHBOARD_CACHE_HISTORY_FIELDS` | `metrics.dashboard_cache_history_fields` | `["active_rooms","rooms_by_game","player_percentiles","game_percentiles"]` | Dashboard history fields |
| `SIGNAL_FISH__RELAY_TYPES__DEFAULT_RELAY_TYPE` | `relay_types.default_relay_type` | `matchbox` | Default relay integration label |
| `SIGNAL_FISH__RELAY_TYPES__GAME_RELAY_MAPPINGS` | `relay_types.game_relay_mappings` | `{}` | JSON object mapping game names to relay labels |
| `SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY` | `session.default_topology` | `relay` | Preferred topology for unmapped games (`relay`, `host`, `mesh`) |
| `SIGNAL_FISH__SESSION__GAME_TOPOLOGY_MAPPINGS` | `session.game_topology_mappings` | `{}` | JSON object mapping game names to topologies |
| `SIGNAL_FISH__SESSION__ENABLE_WEBRTC` | `session.enable_webrtc` | `true` | Permit the WebRTC transport for `mesh`/`host` upgrades |
| `SIGNAL_FISH__SESSION__ENABLE_DIRECT` | `session.enable_direct` | `true` | Permit the Direct (LAN/routable) transport for `host` upgrades |
| `SIGNAL_FISH__SESSION__ENABLE_ICE_PREGATHER` | `session.enable_ice_pregather` | `true` | Surface the composed ICE list on `RoomJoined`/`Reconnected` for eligible v3 WebRTC clients |
| `SIGNAL_FISH__SESSION__ICE_SERVERS` | `session.ice_servers` | `[]` | JSON array of static ICE servers advertised in a WebRTC plan |
| `SIGNAL_FISH__TURN__ENABLED` | `turn.enabled` | `false` | Mint and advertise self-hosted TURN credentials |
| `SIGNAL_FISH__TURN__STATIC_AUTH_SECRET` | `turn.static_auth_secret` | `""` | coturn `--static-auth-secret` (server-only; never sent to clients) |
| `SIGNAL_FISH__TURN__URLS` | `turn.urls` | `[]` | JSON array of TURN server URLs (e.g. `turn:turn.example.com:3478`) |
| `SIGNAL_FISH__TURN__STUN_URLS` | `turn.stun_urls` | `["stun:stun.l.google.com:19302"]` | JSON array of STUN URLs advertised on WebRTC plans |
| `SIGNAL_FISH__TURN__CREDENTIAL_TTL_SECS` | `turn.credential_ttl_secs` | `3600` | Lifetime in seconds of a minted TURN credential |
| `SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING` | `websocket.enable_batching` | `false` | Opt-in outbound message batching (off keeps real-time relay latency low; on trades up to `batch_interval_ms` per hop for fewer writes) |
| `SIGNAL_FISH__WEBSOCKET__BATCH_SIZE` | `websocket.batch_size` | `10` | Max messages per batch (must be > 0 when `enable_batching` is true; maximum: 65,536) |
| `SIGNAL_FISH__WEBSOCKET__BATCH_INTERVAL_MS` | `websocket.batch_interval_ms` | `16` | Batch flush interval in milliseconds (must be > 0 when `enable_batching` is true) |
| `SIGNAL_FISH__WEBSOCKET__AUTH_TIMEOUT_SECS` | `websocket.auth_timeout_secs` | `10` | Exclusive deadline for the initial app-ID/protocol handshake after connect (legacy key name) |
| `SIGNAL_FISH__WEBSOCKET__IDLE_TIMEOUT_SECS` | `websocket.idle_timeout_secs` | `300` | Exclusive inbound-frame deadline after handshake completion (`0` disables; values beyond the platform `Instant` range remain later than the process can represent) |
| `SIGNAL_FISH__WEBSOCKET__SERVER_PING_INTERVAL_SECS` | `websocket.server_ping_interval_secs` | `10` | Cadence for server-initiated RFC 6455 Ping frames (`0` disables; must be ≤ `3600`) |
| `SIGNAL_FISH__WEBSOCKET__PONG_TIMEOUT_SECS` | `websocket.pong_timeout_secs` | `5` | Seconds allowed for the matching Pong before close `4003 activity_timeout` (must be > `0` and ≤ `3600`) |
| `SIGNAL_FISH__WEBSOCKET__SOCKET_SEND_BUFFER_BYTES` | `websocket.socket_send_buffer_bytes` | `65536` | Requested TCP send-buffer bound ahead of WebSocket control traffic (`0` keeps the platform default; must be ≤ `16777216`) |
| `SIGNAL_FISH__WEBSOCKET__SEND_QUEUE_CAPACITY` | `websocket.send_queue_capacity` | `1024` | Per-connection data queue capacity (must be ≥ 1); only reliable delivery waits when full |
| `SIGNAL_FISH__WEBSOCKET__CONTROL_QUEUE_CAPACITY` | `websocket.control_queue_capacity` | `128` | Per-connection v3 priority control queue capacity (must be ≥ 2) |
| `SIGNAL_FISH__WEBSOCKET__SLOW_CONSUMER_TIMEOUT_MS` | `websocket.slow_consumer_timeout_ms` | `5000` | Milliseconds reliable delivery may wait for data-queue space before closing the recipient with `4002 slow_consumer` (must be > 0 and ≤ `600000`) |
| `SIGNAL_FISH__WEBSOCKET__MAX_SOJOURN_MS` | `websocket.max_sojourn_ms` | `15000` | Exclusive reliable/control sojourn and selected socket-write completion deadline before `4002 slow_consumer` (must be > `0` and exceed `batch_interval_ms` when batching is enabled) |
| `SIGNAL_FISH__WEBSOCKET__DELIVERY_STATS_INTERVAL_SECS` | `websocket.delivery_stats_interval_secs` | `0` | Seconds between v3 aggregate `RelayStats` and counter-only `DeliveryReport` snapshots (`0` disables periodic snapshots, not exact gap reports; must be ≤ `3600`) |
| `RUST_LOG` | -- | `info` | Standard `tracing` log filter used when `logging.level` is `null` |

`relay_types.default_relay_type` and `game_relay_mappings` configure legacy
integration labels emitted as `relay_type`. Changing them changes that
informational string only; it does not select a TCP, UDP, WebSocket, WebRTC, or
external relay path. Executable v3 routing comes from `session` negotiation,
and the authenticated WebSocket relay floor remains available independently.

## Common Configurations

### Development

```json
{
  "port": 3536,
  "server": {
    "default_max_players": 8,
    "enable_reconnection": true
  },
  "logging": {
    "enable_file_logging": false
  },
  "security": {
    "cors_origins": "*",
    "enforce_app_id_allowlist": false
  }
}

```

### Production

```json

{
  "port": 3536,
  "server": {
    "default_max_players": 8,
    "empty_room_timeout": 180,
    "inactive_room_timeout": 1800
  },
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20,
    "max_signals": 600,
    "max_signal_errors": 60
  },
  "logging": {
    "enable_file_logging": true,
    "rotation": "daily"
  },
  "security": {
    "cors_origins": "https://yourgame.com",
    "enforce_app_id_allowlist": true,
    "max_connections_per_ip": 24
  }
}

```

## Rate Limiting

```json

{
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20,
    "max_signals": 600,
    "max_signal_errors": 60
  }
}

```

- `max_room_creations` - Max room creations per player per time window (must be > 0)
- `max_join_attempts` - Shared max room-creation, `JoinRoom`, and `JoinAsSpectator` attempts
  per player per time window (must be > 0)
- `max_signals` - Max validated WebRTC Signal dispatch attempts per player per time window (must be > 0)
- `max_signal_errors` - Detailed rejected-signal errors per player per window before generic rate-limit errors
- `time_window` - Rate limit window in seconds

## Protocol Settings

```json

{
  "protocol": {
    "max_game_name_length": 64,
    "room_code_length": 6,
    "max_player_name_length": 32,
    "max_players_limit": 100,
    "enable_message_pack_game_data": true
  }
}

```

`room_code_length` must be greater than zero. When
`server.room_code_prefix` is configured, surrounding whitespace is trimmed,
the prefix must contain only ASCII letters or digits, and it must be shorter
than the total code length so at least one random character remains. Startup
rejects settings that could generate a code the explicit join path would not
accept.

The prefix consumes part of the 32-character clean-code namespace. If `s` is
the remaining random suffix length, each game has `32^s` generated suffixes.
Keep the expected active-room count below 1% of that space (for example, a
four-character suffix has 1,048,576 possibilities and comfortably covers the
default 1,000-room cap). Automatic creation makes at most eight independent
attempts, counts them as one rate-limited client operation, and reports
collisions through `race_conditions.room_code_collisions`. The adjacent
`room_code_retry_*` fields report logical retry operations, recoveries,
exhaustions, and their recovery rate without mixing them into infrastructure
retry accounting. Exhausting the budget fails the creation without exposing
candidate codes or changing the behavior of explicit room-code requests.

## WebSocket Settings

```json

{
  "websocket": {
    "enable_batching": false,
    "batch_size": 10,
    "batch_interval_ms": 16,
    "auth_timeout_secs": 10,
    "idle_timeout_secs": 300,
    "server_ping_interval_secs": 10,
    "pong_timeout_secs": 5,
    "socket_send_buffer_bytes": 65536,
    "send_queue_capacity": 1024,
    "control_queue_capacity": 128,
    "slow_consumer_timeout_ms": 5000,
    "max_sojourn_ms": 15000,
    "delivery_stats_interval_secs": 0
  }
}

```

- `enable_batching` - Opt-in outbound batching (off by default; on adds up to `batch_interval_ms` latency per hop)
- `batch_size` - Max messages per batch (must be > 0 when batching is enabled; maximum: 65,536)
- `batch_interval_ms` - Batch flush interval
- `auth_timeout_secs` - Exclusive deadline for app-ID handshake input after
  connect
- `idle_timeout_secs` - Exclusive post-handshake inbound-frame deadline
  (default: 300; `0` disables). A handshake-complete connection that produces no inbound WebSocket
  frame of any kind (including Ping/Pong) for this long receives a
  `CONNECTION_IDLE_TIMEOUT` error and is closed through the normal disconnect
  path, so the reconnection grace period still applies. The error is delivered
  on the connection's own outbound channel, so it reaches the client even
  though the `server.ping_timeout` state reaper (default 30s) has usually
  already removed a silent client's server-side registration by then. Clients
  that heartbeat (which `server.ping_timeout` already requires) are never
  affected by the 300s default. Keep this enabled in production — it reclaims
  zombie sockets that would otherwise hold file descriptors open indefinitely.
- `server_ping_interval_secs` / `pong_timeout_secs` - The server sends an RFC
  6455 Ping after an otherwise-idle 10-second interval by default and requires
  its matching Pong within 5 seconds. Recent decoded inbound non-Pong activity
  skips the probe. A completed outbound application write still allows the Ping
  to be sent, so a read-only client can return an automatic Pong and refresh the
  inbound-activity reaper, but it supersedes that probe's deadline because the
  Ping may be queued behind constrained output. The probe is written directly
  by the socket layer, outside the application data/control queues; a silent
  authoritative miss closes with `4003 activity_timeout`. Queue sojourn and
  selected-write deadlines independently close an outbound path that stops
  draining with `4002 slow_consumer`. Completed outbound writes do not prove
  that the client-to-server path works, so keep `server.ping_timeout` nonzero
  when its independent inbound-activity bound is required. Size that timeout
  above `server_ping_interval_secs` plus the worst measured Ping queue/write
  delay and operational jitter; otherwise the activity reaper can legitimately
  win before an automatic Pong arrives. The defaults provide nominal
  headroom, but do not turn an arbitrarily backlogged network into a fixed
  delivery guarantee.
  Set the interval to `0` to disable server probes. The Pong timeout must remain
  greater than `0`; both fields are capped at 3600 seconds.
- `socket_send_buffer_bytes` - Requested TCP send buffer inherited by accepted
  sockets (default: 65536; `0` keeps the operating-system default; maximum 16
  MiB). This bounds bytes application data can hand to TCP ahead of a later
  WebSocket Ping or priority report. Operating systems may clamp the request or
  report a larger accounting value (Linux commonly reports twice the request).
- `send_queue_capacity` - Per-connection data queue capacity in messages
  (default: 1024; must be ≥ 1). Reliable messages wait for space and apply
  sender backpressure. V3 `latest` and `volatile` messages never wait: their
  class policy accounts for any omission in a prior exact `DeliveryReport`.
  Larger values absorb bigger relay bursts; queue slots hold pointers, so the
  configured capacity costs little until messages actually queue.
- `control_queue_capacity` - Per-connection control-plane queue capacity in
  messages (default: 128; must be ≥ 2). Within the active recipient generation,
  negotiated v3 drains this dedicated lane strictly before data, keeping exact
  delivery reports, peer lifecycle events, errors, and heartbeats from starving
  behind game data. The recipient's own room/spectator transitions are
  generation barriers that drain old-room data first. If exact accountability
  cannot be queued, the connection fails closed before later data is exposed.
- `slow_consumer_timeout_ms` - How long (milliseconds) delivery may wait for
  reliable space in a full data queue before the recipient is disconnected as
  a slow consumer (default: 5000; must be > 0 and ≤ 600000). A connection that
  cannot absorb reliable traffic for this long is closed with authoritative
  WebSocket code `4002 slow_consumer`; the `SLOW_CONSUMER` error is best effort.
  Capacity that becomes available strictly before the exclusive deadline and
  remains continuously available may be claimed after a delayed producer poll;
  capacity first available at or after the deadline may not.
  `latest` and `volatile` do not use this wait.
- `max_sojourn_ms` - Reliable messages are closed loudly with `4002
  slow_consumer` when the oldest reliable queue/batch item cannot complete its
  socket write within this end-to-end deadline (default: 15000; must be > 0).
  Control messages use their own enqueue timestamp, so stale lossy data cannot
  age a fresh report. Latest/volatile queue age is resolved by their explicit
  drop/coalesce policy; once selected for the socket, this value bounds write
  progress so a transport that stops draining cannot wedge the writer. Write
  completion must be observed strictly before this exclusive deadline; a
  completion ready at or after it expires. The value must exceed
  `batch_interval_ms` when batching is enabled.
- `delivery_stats_interval_secs` - Periodic v3 aggregate/counter snapshot
  cadence (default: 0, disabled; must be ≤ 3600). This does not suppress exact
  gap-bearing `DeliveryReport` frames, which are emitted whenever a lossy class
  or unsupported format omits a sequence range.

See [Delivery semantics](protocol.md#delivery-semantics) for class policies and
the exact gap-authorization contract.

## Session Topology (Protocol v3)

```json

{
  "session": {
    "default_topology": "relay",
    "game_topology_mappings": {},
    "enable_webrtc": true,
    "enable_direct": true,
    "enable_ice_pregather": true,
    "ice_servers": []
  }
}

```

- `default_topology` - Preferred topology for games not in `game_topology_mappings` (`relay`, `host`, `mesh`; default: `relay`)
- `game_topology_mappings` - Per-game topology overrides, e.g. `{"FastFPS": "mesh", "BoardGame": "host"}`
- `enable_webrtc` - Permit the WebRTC transport for `mesh`/`host` upgrades (default: true)
- `enable_direct` - Permit the Direct (LAN/routable) transport for `host`
  upgrades (default: true). Selection still requires every member to advertise
  `host + direct` and at least one electable host to provide a validated Direct
  endpoint; otherwise the ladder continues to the relay floor.
- `enable_ice_pregather` - Surface the composed ICE list (static `ice_servers`, then STUN, then a freshly minted
  TURN credential) on `RoomJoined`/`Reconnected` so v3 WebRTC-capable clients can pre-gather ICE candidates during
  the lobby wait (default: true). Only fires for non-relay-desired games in non-finalized rooms; the `SessionPlan`
  ICE list supersedes it. Set `false` to mint TURN credentials only via `SessionPlan` emission
  (finalize / late-join / host-failover re-plan) (see [TURN deployment](deployment-turn.md))
- `ice_servers` - Static ICE (STUN/TURN) servers advertised in a WebRTC `SessionPlan` and in the pre-gather list;
  appended before any TURN-derived entries. Every URL must use a `stun:`/`stuns:`/`turn:`/`turns:` scheme
  (validated at startup)

Every upgrade gracefully degrades to the `relay` floor, so a fully-disabled
deployment keeps working exactly like v2. ICE servers are advertised only when a
WebRTC topology (`mesh`, or `host` with the WebRTC transport) is actually
selected — plus, with `enable_ice_pregather`, at join/reconnect time for v3
WebRTC-capable members of non-relay-desired games still in the lobby; under the
default `relay` topology each v3 member receives an explicit no-peer
`relay`/`relay` `SessionPlan`, while no ICE is pre-gathered at all. V2 members
receive no plan.

## TURN and STUN (ICE Credentials) (Protocol v3)

TURN is **fully self-hosted**: when enabled, the server self-mints short-lived
coturn REST credentials for a TURN server **you** run. No third-party cloud is
ever contacted and no external credentials are required.

```json

{
  "turn": {
    "enabled": false,
    "static_auth_secret": "",
    "urls": [],
    "stun_urls": ["stun:stun.l.google.com:19302"],
    "credential_ttl_secs": 3600
  }
}

```

- `enabled` - Mint and advertise self-hosted TURN credentials (default: false).
  When false, the block is inert and only `stun_urls` is advertised
- `static_auth_secret` - coturn `--static-auth-secret`. Required when `enabled`
- `urls` - TURN server URLs, e.g. `["turn:turn.example.com:3478"]`. Required (non-empty) when `enabled`
- `stun_urls` - Public STUN URLs advertised on WebRTC plans regardless of `enabled` (default: `["stun:stun.l.google.com:19302"]`)
- `credential_ttl_secs` - Lifetime in seconds of a minted TURN credential. Must be `> 0` when enabled (default: 3600)

### Security: `static_auth_secret` is server-only

`turn.static_auth_secret` is a **server-only secret** and is **never sent to
clients** — only the short-lived ephemeral username/credential pair derived from
it ever reaches a client. It must match the value passed to coturn via
`--static-auth-secret` (coturn `--use-auth-secret`). Prefer setting it via the
environment variable `SIGNAL_FISH__TURN__STATIC_AUTH_SECRET` rather than checking
it into the config file.

### STUN phone-home note

`turn.stun_urls` defaults to a public Google STUN server
(`stun:stun.l.google.com:19302`). It is only advertised to clients once a WebRTC
topology (`mesh`, or `host` with the WebRTC transport) is actually selected — or,
with `session.enable_ice_pregather`, when a v3 WebRTC-capable client joins a
non-relay-desired game's lobby — it is **never** sent under the default `relay`
topology. Operators who want no third-party STUN dependency should set
`stun_urls: []`.

## Validation

Validate your config without starting the server:

```bash

cargo run -- --validate-config

```

Print the resolved config (with environment overrides):

```bash

cargo run -- --print-config

```

## Next Steps

- [Application identification](authentication.md) - Configure the public app-ID allowlist
- [Deployment](deployment.md) - Production deployment guide
