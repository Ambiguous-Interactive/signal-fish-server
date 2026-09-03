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
`rate_limit_per_minute` is an optional per-app override. All three must be
greater than 0 when set — a zero value rejects every creation, join, or
authentication for that app, so startup validation refuses it (omit the field
instead). See [Application identification](authentication.md) for the exact
trust boundary.

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

Token binding v2 requires every JSON or binary client message to carry an HMAC
under a connection key derived from both the WebSocket handshake key and a
server-fresh challenge. One sequence covers both frame formats, so a proof
cannot be replayed, reordered, or moved to another connection. Fingerprint mode
additionally binds proofs and reconnect credentials to the authenticated mTLS
leaf certificate. It is **off by default** and is also summarized in the
configuration reference and feature matrix.

```json
{
  "security": {
    "transport": {
      "token_binding": {
        "enabled": true,
        "required": false,
        "require_client_fingerprint": false,
        "subprotocol": "signalfish.tokenbinding.v2",
        "scheme": "server_nonce_hkdf_sha256"
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
  (`subprotocol`, default `signalfish.tokenbinding.v2`). With `enabled=true` and
  `required=false`, clients that advertise the subprotocol send proofs on each
  JSON or binary message; clients that do not still connect normally — a safe,
  opt-in rollout. Custom non-reserved aliases are supported, but a name in the
  `signalfish.tokenbinding.*` namespace must be exactly
  `signalfish.tokenbinding.v2`; legacy or future reserved version names fail
  startup rather than relabeling the v2 wire contract.
- `required=true` rejects clients that do not advertise the subprotocol and
  requires `enabled=true`. Set this only after every client in your fleet
  supports token binding, or you will lock out existing clients.
- `require_client_fingerprint=true` binds every proof to the lowercase
  hexadecimal SHA-256 digest of the authenticated mTLS leaf certificate's DER
  bytes. It requires built-in TLS and `tls.client_auth` set to `optional` or
  `require`, plus `token_binding.required=true` so a client cannot opt out by
  omitting the subprotocol. Under `optional`, a WebSocket client that presents
  no certificate is rejected because it cannot satisfy this setting. Clients
  include the exact 64-character fingerprint in `token_binding.fingerprint`
  and append those ASCII bytes when computing the frame HMAC. Direct
  client-supplied fingerprint and forwarding headers are ignored and cannot
  override the rustls identity.
- `scheme` selects the per-frame signing scheme (default
  `server_nonce_hkdf_sha256`). The legacy `sec_websocket_key_sha256` value is
  still parsed to produce an actionable migration error, but an enabled server
  rejects it because it has no server freshness.
- Every token-bound binary frame uses the signed MessagePack envelope described
  below. A raw legacy binary frame is unauthorized whether or not fingerprint
  binding is enabled.

> **Token binding requires TLS to authenticate.**
> The connection key is derived from the WebSocket handshake key plus a
> server-fresh challenge. Over plaintext `ws://`, both inputs are visible on
> the wire, so a passive observer can derive the key and forge every proof —
> in that deployment proofs provide replay ordering only, not authentication.
> Serve token-bound traffic over built-in TLS or reverse-proxy-terminated
> wss://, and use `required=true` where clients must not be able to opt out.
> The startup warning fires whenever binding is enabled but optional without
> _built-in_ TLS — including safe reverse-proxy deployments, which the server
> cannot see; treat it as a reminder to verify the wire is encrypted.

### v2 wire contract

The first server application message after a token-bound WebSocket upgrade is:

```json
{"type":"TokenBindingChallenge","data":{"version":2,"scheme":"server_nonce_hkdf_sha256","nonce":"BASE64_32_BYTES","first_sequence":1}}
```

Base64-decode the 16-byte `Sec-WebSocket-Key` and the challenge's 32-byte
`nonce`. Derive a 32-byte connection key with HKDF-SHA-256 using the handshake
key as input keying material, the nonce as the salt, and
`signalfish.tokenbinding.v2/session-key` as the info value. Start at
`first_sequence` and increment by exactly one after every accepted JSON **or**
binary frame. The server rejects gaps, duplicates, and reordered sequences; a
rejected proof does not advance the sequence.

The nonce is generated independently for each accepted WebSocket with the
operating system's cryptographic RNG (256 bits, making accidental reuse
negligible), is valid only for that connection, and is discarded when the
connection closes. A reconnect is a new WebSocket and always receives a new
challenge; no challenge or sequence state resumes across reconnects.

For a JSON message, remove its top-level `token_binding` member and encode the
remaining value using RFC 8785 (JCS). This includes ECMAScript-compatible number
serialization, recursive UTF-16 code-unit property ordering, no insignificant
whitespace, and UTF-8 output. Duplicate members and non-finite numbers are not
valid inputs. To keep the parsed application value identical across
implementations, every integer anywhere in the envelope
(including `sequence`) must be within
`-9007199254740991..=9007199254740991`; the server rejects values outside that
safe range before proof verification. Negative zero, fractional, and
exponent-form numbers are rejected on token-bound connections because the
server's ordinary JSON parser intentionally retains its faster
non-`float_roundtrip` behavior; this keeps every accepted proof portable
without changing non-token traffic. HMAC-SHA-256 covers, in order:

1. the literal bytes `signalfish.tokenbinding.v2\0json\0`;
2. the proof sequence as an unsigned 64-bit big-endian integer;
3. the RFC 8785 payload bytes; and
4. when fingerprint mode is active, the 64 lowercase hexadecimal certificate
   fingerprint bytes.

Insert the standard-base64 MAC as the signature:

```json
{"type":"Ping","token_binding":{"version":2,"scheme":"server_nonce_hkdf_sha256","sequence":1,"signature":"BASE64_MAC","fingerprint":null}}
```

For binary data, retain the existing inner MessagePack game-data bytes exactly.
HMAC the same fields with the domain
`signalfish.tokenbinding.v2\0binary\0`, then send a named-field MessagePack map
with this shape (where `payload` is a MessagePack `bin` value):

```text
{"token_binding": <the same v2 proof map>, "payload": <binary>}
```

The sequence namespace is shared: for example, JSON sequence 1 followed by
binary sequence 2 is valid; another JSON sequence 1 is not.

Normative JSON golden vector (no fingerprint):

- handshake key: `MDEyMzQ1Njc4OWFiY2RlZg==`
- challenge nonce: `AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=`
- derived key (hex): `abb5860d3be9f16fad2763e718d0e9a038b7196fd136f24d62cb3ab6fc631da7`
- input: `{"zzz":{"y":2,"x":"é"},"type":"Ping","aaa":[3,1]}`
- canonical payload: `{"aaa":[3,1],"type":"Ping","zzz":{"x":"é","y":2}}`
- sequence: `1`
- signature: `HobFBjbmzHgNF/QoXXFpqNy5s4/InE7+tCYO56+Dqig=`

Normative binary golden using the same key/nonce, sequence 2, inner payload hex
`81a46461746101`:

- signature: `7/4RSNk/Euc4JnPJqlrMVDqp1l8oLXl+A8jAqDZIt1A=`
- complete named-field MessagePack envelope (hex):
  `82ad746f6b656e5f62696e64696e6785a776657273696f6e02a6736368656d65b87365727665725f6e6f6e63655f686b64665f736861323536a873657175656e636502a97369676e6174757265d92c372f3452534e6b2f457563344a6e504a716c724d56447170316c386f4c586c2b41386a4171445a497431413dab66696e6765727072696e74c0a77061796c6f6164c40781a46461746101`

Client impact: when `enabled` but not `required`, none — non-participating
clients are unaffected. When `required`, clients **must** implement the
subprotocol, so roll that out fleet-wide first. Start with `enabled=true,
required=false`, confirm clients negotiate it, then tighten.

Fingerprint binding is a separate tightening step: first deploy trusted client
certificates, enable `tls.client_auth`, and verify mTLS connectivity. Then set
`require_client_fingerprint=true` and update proof generation with the actual
leaf-certificate fingerprint. Reconnect tokens issued to certificate A remain
claimable only with A. Presenting certificate B returns an invalid-token result
without consuming A's token; after rotation to B, use the normal join flow to
obtain a new B-bound token. Per-frame integrity, connection replay resistance,
and reconnect-credential identity binding are distinct checks and all remain
enforced during that transition.

```json
{
  "security": {
    "transport": {
      "tls": {
        "enabled": true,
        "certificate_path": "/run/secrets/server-chain.pem",
        "private_key_path": "/run/secrets/server-key.pem",
        "client_ca_cert_path": "/run/secrets/client-ca.pem",
        "client_auth": "require"
      },
      "token_binding": {
        "enabled": true,
        "required": true,
        "require_client_fingerprint": true
      }
    }
  }
}
```

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
`batch_interval_ms` to favor throughput. When `enable_batching` is `true`,
`batch_interval_ms` must be `> 0` and at most 60000 (1 minute), and
`max_sojourn_ms` must exceed it (all enforced at startup). Keep
`idle_timeout_secs` positive in production — it reclaims zombie
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
    "max_signal_errors": 60,
    "max_relay_bytes": 268435456,
    "max_room_relay_bytes": 1073741824
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
- `max_relay_bytes` — the per-sender game-data relay byte budget per window;
  size it against the games' real submit rates (a full roster of 8 relaying
  64 KiB at 30 Hz submits roughly 2 MB/s), and lower it to cap one player's
  share of the host's egress bill.
- `max_room_relay_bytes` — the per-room aggregate game-data relay byte ceiling
  per window; bounds many individually under-budget senders jointly
  (a room's admitted submit volume can no longer multiply past the ceiling),
  and lower it to cap a whole room's share of the host's egress bill. Keep it
  at or above the per-sender budgets your rooms legitimately use in aggregate.
- `allowed_apps[].max_relay_bytes` — the per-tenant override of
  `max_relay_bytes` for one application (issue #530); raise it for a paid tier
  or lower it for a trial tier without moving the server-wide default.
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
