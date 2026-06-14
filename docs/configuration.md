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

- `max_rooms_per_game` - Maximum concurrent rooms per game name
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
- `reconnection_window` - Seconds a reconnection token stays valid (default: 300)
- `event_buffer_size` - Max events buffered for replay (default: 100)

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
| `SIGNAL_FISH__SERVER__PING_TIMEOUT` | `server.ping_timeout` | `30` | Seconds before a silent client is dropped |
| `SIGNAL_FISH__SERVER__ROOM_CLEANUP_INTERVAL` | `server.room_cleanup_interval` | `60` | Seconds between room cleanup sweeps (must be > 0) |
| `SIGNAL_FISH__SERVER__MAX_ROOMS_PER_GAME` | `server.max_rooms_per_game` | `1000` | Max rooms allowed per game name |
| `SIGNAL_FISH__SERVER__EMPTY_ROOM_TIMEOUT` | `server.empty_room_timeout` | `300` | Seconds before an empty room is removed |
| `SIGNAL_FISH__SERVER__INACTIVE_ROOM_TIMEOUT` | `server.inactive_room_timeout` | `3600` | Seconds before an inactive room is removed |
| `SIGNAL_FISH__SERVER__RECONNECTION_WINDOW` | `server.reconnection_window` | `300` | Seconds a reconnection token stays valid |
| `SIGNAL_FISH__SERVER__EVENT_BUFFER_SIZE` | `server.event_buffer_size` | `100` | Max events buffered for reconnection replay |
| `SIGNAL_FISH__SERVER__ENABLE_RECONNECTION` | `server.enable_reconnection` | `true` | Enable reconnection support |
| `SIGNAL_FISH__SERVER__HEARTBEAT_THROTTLE_SECS` | `server.heartbeat_throttle_secs` | `30` | Min seconds between `last_seen` heartbeat writes |
| `SIGNAL_FISH__SERVER__REGION_ID` | `server.region_id` | `default` | Deployment region identifier; recorded in internal player and room state (not serialized to clients) |
| `SIGNAL_FISH__SERVER__ROOM_CODE_PREFIX` | `server.room_code_prefix` | `null` | Optional prefix for generated room codes |
| `SIGNAL_FISH__RATE_LIMIT__MAX_ROOM_CREATIONS` | `rate_limit.max_room_creations` | `5` | Max room creations per player per window |
| `SIGNAL_FISH__RATE_LIMIT__TIME_WINDOW` | `rate_limit.time_window` | `60` | Rate limit window in seconds (must be > 0) |
| `SIGNAL_FISH__RATE_LIMIT__MAX_JOIN_ATTEMPTS` | `rate_limit.max_join_attempts` | `20` | Max join attempts per player per window |
| `SIGNAL_FISH__RATE_LIMIT__MAX_SIGNALS` | `rate_limit.max_signals` | `600` | Max validated WebRTC Signal dispatch attempts per player per window |
| `SIGNAL_FISH__RATE_LIMIT__MAX_SIGNAL_ERRORS` | `rate_limit.max_signal_errors` | `60` | Max rejected WebRTC signal attempts per player per window |
| `SIGNAL_FISH__PROTOCOL__MAX_GAME_NAME_LENGTH` | `protocol.max_game_name_length` | `64` | Max characters in a game name |
| `SIGNAL_FISH__PROTOCOL__ROOM_CODE_LENGTH` | `protocol.room_code_length` | `6` | Length of generated room codes |
| `SIGNAL_FISH__PROTOCOL__MAX_PLAYER_NAME_LENGTH` | `protocol.max_player_name_length` | `32` | Max characters in a player name |
| `SIGNAL_FISH__PROTOCOL__MAX_PLAYERS_LIMIT` | `protocol.max_players_limit` | `100` | Hard ceiling on players per room |
| `SIGNAL_FISH__PROTOCOL__ENABLE_MESSAGE_PACK_GAME_DATA` | `protocol.enable_message_pack_game_data` | `true` | Enable MessagePack game-data frames |
| `SIGNAL_FISH__PROTOCOL__MIN_PROTOCOL_VERSION` | `protocol.min_protocol_version` | `2` | Lowest accepted protocol version |
| `SIGNAL_FISH__PROTOCOL__MAX_PROTOCOL_VERSION` | `protocol.max_protocol_version` | `3` | Highest negotiated protocol version |
| `SIGNAL_FISH__PROTOCOL__SDK_COMPATIBILITY__ENFORCE` | `protocol.sdk_compatibility.enforce` | `true` | Enforce SDK compatibility checks |
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
| `SIGNAL_FISH__SECURITY__CORS_ORIGINS` | `security.cors_origins` | `http://localhost:3000,http://localhost:5173` | Allowed CORS origins (comma-separated or `*`) |
| `SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH` | `security.require_websocket_auth` | `true` | Require app authentication on WebSocket connect |
| `SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH` | `security.require_metrics_auth` | `true` | Require auth token for metrics endpoints |
| `SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN` | `security.metrics_auth_token` | `null` | Bearer token for metrics endpoints |
| `SIGNAL_FISH__SECURITY__MAX_MESSAGE_SIZE` | `security.max_message_size` | `65536` | Max WebSocket message size in bytes |
| `SIGNAL_FISH__SECURITY__MAX_SIGNAL_BYTES` | `security.max_signal_bytes` | `16384` | Max serialized size in bytes of a v3 `Signal` payload (must be > 0 and ≤ `max_message_size`) |
| `SIGNAL_FISH__SECURITY__MAX_CONNECTIONS_PER_IP` | `security.max_connections_per_ip` | `10` | Max concurrent connections from one IP |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED` | `security.transport.tls.enabled` | `false` | Enable built-in TLS listener |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH` | `security.transport.tls.certificate_path` | `null` | Path to PEM certificate chain |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH` | `security.transport.tls.private_key_path` | `null` | Path to PEM private key |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_CA_CERT_PATH` | `security.transport.tls.client_ca_cert_path` | `null` | Path to trusted client CA bundle |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CLIENT_AUTH` | `security.transport.tls.client_auth` | `none` | TLS client auth mode (`none`, `optional`, `require`) |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED` | `security.transport.token_binding.enabled` | `false` | Enable token-binding negotiation |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED` | `security.transport.token_binding.required` | `false` | Require token-binding subprotocol |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRE_CLIENT_FINGERPRINT` | `security.transport.token_binding.require_client_fingerprint` | `false` | Require mTLS fingerprint in token-bound frames |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__SUBPROTOCOL` | `security.transport.token_binding.subprotocol` | `signalfish.tokenbinding.v1` | Token-binding WebSocket subprotocol |
| `SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__SCHEME` | `security.transport.token_binding.scheme` | `sec_websocket_key_sha256` | Token-binding signing scheme |
| `SIGNAL_FISH__SECURITY__AUTHORIZED_APPS` | `security.authorized_apps` | `[]` | JSON array of authorized app entries |
| `SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_CLEANUP_INTERVAL_SECS` | `auth.rate_limit_cache_cleanup_interval_secs` | `300` | Auth rate-limit cache cleanup interval |
| `SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_RETENTION_SECS` | `auth.rate_limit_cache_retention_secs` | `172800` | Auth rate-limit cache retention window |
| `SIGNAL_FISH__AUTH__RATE_LIMIT_CACHE_ALERT_ROWS` | `auth.rate_limit_cache_alert_rows` | `100000` | Auth rate-limit cache warning threshold |
| `SIGNAL_FISH__COORDINATION__DEDUP_CACHE__CAPACITY` | `coordination.dedup_cache.capacity` | `100000` | Cross-instance dedup cache capacity |
| `SIGNAL_FISH__COORDINATION__DEDUP_CACHE__TTL_SECS` | `coordination.dedup_cache.ttl_secs` | `60` | Cross-instance dedup cache TTL |
| `SIGNAL_FISH__COORDINATION__DEDUP_CACHE__CLEANUP_INTERVAL_SECS` | `coordination.dedup_cache.cleanup_interval_secs` | `30` | Cross-instance dedup cache cleanup interval |
| `SIGNAL_FISH__COORDINATION__MEMBERSHIP_SNAPSHOT_INTERVAL_SECS` | `coordination.membership_snapshot_interval_secs` | `30` | Membership snapshot interval |
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
| `SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING` | `websocket.enable_batching` | `true` | Enable outbound message batching |
| `SIGNAL_FISH__WEBSOCKET__BATCH_SIZE` | `websocket.batch_size` | `10` | Max messages per batch |
| `SIGNAL_FISH__WEBSOCKET__BATCH_INTERVAL_MS` | `websocket.batch_interval_ms` | `16` | Batch flush interval in milliseconds (must be > 0 when `enable_batching` is true) |
| `SIGNAL_FISH__WEBSOCKET__AUTH_TIMEOUT_SECS` | `websocket.auth_timeout_secs` | `10` | Seconds to wait for auth after connect |
| `SIGNAL_FISH__WEBSOCKET__IDLE_TIMEOUT_SECS` | `websocket.idle_timeout_secs` | `300` | Seconds without any inbound frame before an authenticated connection is closed (`0` disables) |
| `RUST_LOG` | -- | `info` | Standard `tracing` log filter used when `logging.level` is `null` |

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
    "require_websocket_auth": false
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
    "require_websocket_auth": true,
    "max_connections_per_ip": 10
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

- `max_room_creations` - Max room creations per player per time window
- `max_join_attempts` - Max room join attempts per player per time window
- `max_signals` - Max validated WebRTC Signal dispatch attempts per player per time window
- `max_signal_errors` - Max rejected WebRTC signal attempts per player per time window
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

## WebSocket Settings

```json

{
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16,
    "auth_timeout_secs": 10,
    "idle_timeout_secs": 300
  }
}

```

- `enable_batching` - Batch outbound messages for better throughput
- `batch_size` - Max messages per batch
- `batch_interval_ms` - Batch flush interval
- `auth_timeout_secs` - Seconds to wait for auth after connect
- `idle_timeout_secs` - Post-authentication idle timeout (default: 300; `0`
  disables). An authenticated connection that produces no inbound WebSocket
  frame of any kind (including Ping/Pong) for this long receives a
  `CONNECTION_IDLE_TIMEOUT` error and is closed through the normal disconnect
  path, so the reconnection grace period still applies. The error is delivered
  on the connection's own outbound channel, so it reaches the client even
  though the `server.ping_timeout` state reaper (default 30s) has usually
  already removed a silent client's server-side registration by then. Clients
  that heartbeat (which `server.ping_timeout` already requires) are never
  affected by the 300s default. Keep this enabled in production — it reclaims
  zombie sockets that would otherwise hold file descriptors open indefinitely.

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
- `enable_direct` - Permit the Direct (LAN/routable) transport for `host` upgrades (default: true)
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
default `relay` topology no `SessionPlan` is emitted and no ICE is pre-gathered
at all.

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

- [Authentication](authentication.md) - Set up app authentication
- [Deployment](deployment.md) - Production deployment guide
