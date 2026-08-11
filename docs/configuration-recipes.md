# Configuration Recipes

Per-feature configuration examples. Each recipe shows a short JSON snippet, the
environment-variable equivalent, and when to use it. Every key here is verified
against the server's config types — see the
[configuration reference](configuration.md#configuration-reference) for the full
table of keys, defaults, and env spellings, and [Run Modes](run-modes.md) for
the matching run commands.

Environment overrides use the `SIGNAL_FISH__` prefix with double underscores
between nested keys (see
[environment variable format](configuration.md#environment-variable-format)).
JSON and environment overrides can be mixed; environment variables win.

## Application ID allowlist (`allowed_apps`)

Require every WebSocket client to submit a configured public app ID, and set
per-app accounting limits. This does not authenticate a hostile client.

```json
{
  "security": {
    "enforce_app_id_allowlist": true,
    "cors_origins": "https://yourgame.com",
    "allowed_apps": [
      {
        "app_id": "my-game",
        "app_name": "My Awesome Game",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}
```

Environment equivalent (the app list is a JSON array):

```bash
export SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=true
export SIGNAL_FISH__SECURITY__CORS_ORIGINS="https://yourgame.com"
export SIGNAL_FISH__SECURITY__ALLOWED_APPS='[
  {"app_id":"my-game","app_name":"My Awesome Game","max_rooms":100,
   "max_players_per_room":16,"rate_limit_per_minute":60}
]'
```

When to use: any non-local deployment. `max_rooms` limits an app's persisted
rooms across all game names, while `max_players_per_room` caps both new room
capacity and future admission to existing rooms. Both are enforced only when
app-ID allowlisting is enabled; omit them to use server-wide admission limits.
`rate_limit_per_minute` is an optional per-app override. See
[Application identification](authentication.md) for the exact trust boundary.

## TURN/STUN

Self-hosted TURN: the server mints short-lived coturn REST credentials from a
shared secret and advertises STUN servers for hole-punching. It never contacts a
third-party cloud.

```json
{
  "turn": {
    "enabled": true,
    "static_auth_secret": "",
    "urls": ["turn:turn.yourgame.com:3478"],
    "stun_urls": ["stun:stun.l.google.com:19302"],
    "credential_ttl_secs": 3600
  }
}
```

Environment equivalent (set the secret from the environment, never the file):

```bash
export SIGNAL_FISH__TURN__ENABLED=true
export SIGNAL_FISH__TURN__STATIC_AUTH_SECRET="$(openssl rand -hex 32)"
export SIGNAL_FISH__TURN__URLS='["turn:turn.yourgame.com:3478"]'
export SIGNAL_FISH__TURN__STUN_URLS='["stun:stun.l.google.com:19302"]'
export SIGNAL_FISH__TURN__CREDENTIAL_TTL_SECS=3600
```

When to use: enable TURN once you have real users behind symmetric NAT, CGNAT,
or restrictive firewalls (roughly 15–20% of P2P connections). STUN alone (TURN
disabled) handles the rest. The same `static_auth_secret` must be set on both
coturn and the signaling server — it is the single source of truth both sides
derive credentials from. The whole `[turn]` block is inert when `enabled` is
`false`; only `stun_urls` is advertised. URLs in `urls` must use
`turn:`/`turns:` and those in `stun_urls` must use `stun:`/`stuns:` (validated at
startup). To depend on no third-party STUN, set `"stun_urls": []`.

> **Credential rotation.** `static_auth_secret` is the only long-lived
> credential in the scheme; rotate it on a schedule and immediately on suspected
> exposure. coturn accepts multiple valid secrets at once, so rotation is
> zero-downtime: add the new secret to coturn alongside the old, flip the
> signaling server to the new secret and redeploy, wait at least
> `credential_ttl_secs` for old minted credentials to expire, then remove the old
> secret from coturn. Full procedure:
> [rotating the shared secret](deployment-turn.md#rotating-the-shared-secret).

## TLS

Terminate TLS in the server itself (built-in) when you are not fronting it with a
reverse proxy.

```json
{
  "security": {
    "transport": {
      "tls": {
        "enabled": true,
        "certificate_path": "/etc/ssl/signal-fish/fullchain.pem",
        "private_key_path": "/etc/ssl/signal-fish/privkey.pem",
        "client_ca_cert_path": null,
        "client_auth": "none"
      }
    }
  }
}
```

Environment equivalent:

```bash
export SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED=true
export SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH=/etc/ssl/signal-fish/fullchain.pem
export SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH=/etc/ssl/signal-fish/privkey.pem
```

When to use: built-in TLS suits single-node deployments or when you do not want a
proxy in the path. If you already run nginx or Caddy, terminate TLS there instead
(leave `tls.enabled=false`) — see
[reverse proxy setup](deployment.md#reverse-proxy-setup). Either way the public
signaling endpoint must be `wss://`, which is load-bearing for WebRTC security
(see [signaling must run over wss://](deployment-turn.md#signaling-must-run-over-wss)).
`client_auth` accepts `none`, `optional`, or `require`; `require` plus
`client_ca_cert_path` enables mTLS.

## Per-game topology mapping

Route specific games to a WebRTC or host topology while everything else stays on
the relay floor.

```json
{
  "session": {
    "default_topology": "relay",
    "game_topology_mappings": {
      "chess": "mesh",
      "BoardGame": "host"
    },
    "enable_webrtc": true,
    "enable_direct": true
  }
}
```

Environment equivalent (the mapping is a JSON object of game name to topology):

```bash
export SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY=relay
export SIGNAL_FISH__SESSION__GAME_TOPOLOGY_MAPPINGS='{"chess":"mesh","BoardGame":"host"}'
```

When to use: when different games want different transports — e.g. a low-latency
title on `mesh` (full WebRTC peer mesh) and a turn-based title on `host` while
unmapped games default to `relay`. Valid topologies are `relay`, `host`, and
`mesh`. Every upgrade gracefully degrades to the relay floor, so this is always
safe: if both `enable_webrtc` and `enable_direct` are `false`, mapped games
simply fall back to relay (the server warns but still starts).

## Token binding

Token binding cryptographically ties each WebSocket frame to the connection that
sent it, so a stolen session token cannot be replayed on a different connection.
It is an optional zero-trust hardening layer, **off by default**, and is
currently undocumented elsewhere.

```json
{
  "security": {
    "transport": {
      "token_binding": {
        "enabled": true,
        "required": false,
        "require_client_fingerprint": false,
        "subprotocol": "signalfish.tokenbinding.v1",
        "scheme": "sec_websocket_key_sha256"
      }
    }
  }
}
```

Environment equivalent:

```bash
export SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__ENABLED=true
export SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRED=false
export SIGNAL_FISH__SECURITY__TRANSPORT__TOKEN_BINDING__REQUIRE_CLIENT_FINGERPRINT=false
```

What it does and how to enable it:

- `enabled` turns on negotiation of the token-binding WebSocket subprotocol
  (`subprotocol`, default `signalfish.tokenbinding.v1`). With `enabled=true` and
  `required=false`, clients that advertise the subprotocol get per-frame proofs;
  clients that do not still connect normally — a safe, opt-in rollout.
- `required=true` rejects clients that do not advertise the subprotocol. Set this
  only after every client in your fleet supports token binding, or you will lock
  out existing clients.
- `require_client_fingerprint=true` is reserved for binding proofs to a verified
  mTLS peer certificate. The built-in listener currently rejects this setting:
  its authenticated rustls peer certificate is not yet exposed to the
  WebSocket handler, and client-supplied forwarding headers are deliberately
  not trusted as identity. Track issue #344's verified extraction work before
  enabling this knob.
- `scheme` selects the per-frame signing scheme (default
  `sec_websocket_key_sha256`).

Client impact: when `enabled` but not `required`, none — non-participating
clients are unaffected. When `required`, clients **must** implement the
subprotocol, so roll that out fleet-wide first. Start with `enabled=true,
required=false`, confirm clients negotiate it, then tighten.

## Metrics auth

Protect the metrics endpoints with a shared bearer token.

```json
{
  "security": {
    "require_metrics_auth": true,
    "metrics_auth_token": "REPLACE_WITH_A_STRONG_TOKEN"
  }
}
```

Environment equivalent (keep the token out of the file):

```bash
export SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=true
export SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN="$(openssl rand -hex 32)"
```

When to use: any deployment whose `/metrics`, `/v1/metrics`, or `/metrics/prom`
endpoints are reachable beyond a trusted network. With `require_metrics_auth`
on, requests must send `Authorization: Bearer <token>` matching
`metrics_auth_token`; the server fails to start if the token is missing and warns
if it is shorter than 16 characters (32 or more recommended — `openssl rand -hex 32`).
Configure Prometheus with the token:

```yaml
scrape_configs:
  - job_name: 'signal-fish'
    metrics_path: /metrics/prom
    authorization:
      type: Bearer
      credentials: "REPLACE_WITH_A_STRONG_TOKEN"
    static_configs:
      - targets: ['signal-fish:3536']
```

If you instead restrict metrics at a reverse proxy (an IP allowlist, see the
[nginx example](deployment.md#reverse-proxy-setup)), you may leave
`require_metrics_auth=false`.

## Batching tuning

Outbound WebSocket batching is **opt-in** (`enable_batching` defaults to
`false`) because the flush timer adds up to `batch_interval_ms` of latency to
every relay hop. Enable it for bulk/throughput deployments, then tune the batch
size and flush interval.

```json
{
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16,
    "auth_timeout_secs": 10,
    "idle_timeout_secs": 300,
    "socket_send_buffer_bytes": 65536,
    "send_queue_capacity": 1024,
    "control_queue_capacity": 128,
    "slow_consumer_timeout_ms": 5000,
    "max_sojourn_ms": 15000,
    "delivery_stats_interval_secs": 0
  }
}
```

Environment equivalent:

```bash
export SIGNAL_FISH__WEBSOCKET__ENABLE_BATCHING=true
export SIGNAL_FISH__WEBSOCKET__BATCH_SIZE=10
export SIGNAL_FISH__WEBSOCKET__BATCH_INTERVAL_MS=16
export SIGNAL_FISH__WEBSOCKET__SOCKET_SEND_BUFFER_BYTES=65536
export SIGNAL_FISH__WEBSOCKET__SEND_QUEUE_CAPACITY=1024
export SIGNAL_FISH__WEBSOCKET__CONTROL_QUEUE_CAPACITY=128
export SIGNAL_FISH__WEBSOCKET__SLOW_CONSUMER_TIMEOUT_MS=5000
export SIGNAL_FISH__WEBSOCKET__MAX_SOJOURN_MS=15000
```

When to use: enable batching only for throughput-oriented deployments (heavy
fan-out of bulk data) where fewer, larger writes matter more than per-hop
latency. It is **off by default** so real-time relay traffic (rollback game data
is `reliable`) is never held by the timer. When enabled, only `latest` traffic
waits up to `batch_interval_ms` to coalesce same-key values; `reliable`,
`volatile`, and control are still flushed immediately. Raise `batch_size` and
`batch_interval_ms` to favor throughput. `batch_interval_ms` must be `> 0` and
`max_sojourn_ms` must exceed it when `enable_batching` is `true` (both enforced
at startup). Keep `idle_timeout_secs` positive in production — it reclaims zombie
sockets (`0` disables it); `auth_timeout_secs` must be between 5 and 60.

High-rate game data (rollback netcode): choose the v3 JSON delivery class by
meaning. Keep commands and critical events `reliable`; send frequently replaced
state as `latest` with a stable per-stream key; use `volatile` only when an
omission is acceptable. Raw binary frames are always reliable. Reliable traffic
waits when `send_queue_capacity` fills, while `latest` and `volatile` never pace
the sender and account for every omission in a prior exact `DeliveryReport`.

Size `send_queue_capacity` for burst headroom, but keep it consistent with
`max_sojourn_ms`: reliable traffic closes once its oldest reliable queued or
batched item cannot complete within 15 seconds. Control uses its own enqueue
age; latest/volatile queue age is handled by the class policy and gets a
15-second write-progress budget only after selection.
For a measured drain rate and encoded WebSocket-frame size, check
`(socket_bytes_ahead + capacity * frame_bytes) / drain_bytes_per_second <=
max_sojourn_seconds`.
Apply the data capacity to reliable data and the separate 128-slot control
capacity to reports. A capacity larger than this bound remains valid burst and
memory protection, but a completely full queue is expected to hit maximum
sojourn before it drains. Close `4002 slow_consumer` reports that bounded
delivery-contract failure whether the recipient stopped entirely or merely
remained below the offered reliable rate; there is no separate
oversubscription close code. See the worked
[queue and freeze budget](architecture/scaling.md#queue-and-freeze-budget).
`slow_consumer_timeout_ms`
(default `5000`) controls how long reliable delivery may wait for capacity;
higher values ride out longer stalls but let one recipient pace reliable senders
longer. The authoritative failure signal is close code `4002 slow_consumer`;
the final `SLOW_CONSUMER` error is best effort.

Keep `socket_send_buffer_bytes` bounded so the operating system cannot accept a
large data tail ahead of later Ping/report frames. The default requests 65536
bytes (`0` restores the platform default); the effective value is logged at
listener startup because kernels may clamp or account it differently.

Keep `control_queue_capacity` large enough for bursts of lifecycle and exact
accountability traffic. V3 drains this lane before data within the active room
generation; the recipient's own room/spectator transitions are generation
barriers that drain old-room data first. The connection fails closed if it
cannot publish a required gap report before a successor. Setting
`delivery_stats_interval_secs=0` disables periodic aggregate snapshots only;
gap-bearing `DeliveryReport` frames remain event-driven. See
[delivery semantics](protocol.md#delivery-semantics).

## Rate limits

Per-player rate limits that bound abusive room creation, join, and signaling
traffic.

A room creation consumes one room-creation slot and one join-attempt slot, and
is rejected atomically when either budget is exhausted.

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

Environment equivalent:

```bash
export SIGNAL_FISH__RATE_LIMIT__MAX_ROOM_CREATIONS=5
export SIGNAL_FISH__RATE_LIMIT__TIME_WINDOW=60
export SIGNAL_FISH__RATE_LIMIT__MAX_SIGNALS=600
```

When to adjust:

- `max_room_creations` — raise for matchmaking services that legitimately create
  many rooms per player; lower to throttle room-spam abuse.
- `max_join_attempts` — raise if clients retry joins aggressively (flaky
  networks); lower to curb brute-forcing of room codes.
- `max_signals` — raise for v3 WebRTC games with heavy ICE/SDP exchange (mesh
  topologies signal more than host); lower for relay-only deployments that never
  signal.
- `max_signal_errors` — the ceiling on detailed rejected-signal error responses;
  lower it to replace validation errors with a generic rate-limit error sooner.
- `time_window` — the window (seconds) all the above counts apply over; must be
  `> 0`.

These are global defaults; an `allowed_apps` entry's `rate_limit_per_minute`
overrides per app (see [Application ID allowlist](#application-id-allowlist-allowed_apps)).

## See also

- [Configuration reference](configuration.md) — every key, default, and env override
- [Run Modes](run-modes.md) — how to run each mode, with commands
- [Pre-deployment checklist](pre-deployment-checklist.md) — verify before going live
- [Application identification](authentication.md) — public app-ID trust boundary and wire errors
- [TURN Deployment](deployment-turn.md) — self-hosted coturn and rotation
