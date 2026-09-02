# Run Modes

How to run Signal Fish Server in each of its operating modes, with a
copy-pasteable command for every row. Every key referenced here is a real
configuration key — see the full
[configuration reference](configuration.md#configuration-reference) for defaults
and environment-variable spellings, and [Deployment](deployment.md) for
container, reverse-proxy, and cloud-provider details.

The server is a single self-contained binary with **zero external runtime
dependencies**. It starts with safe defaults and reads `config.json` from the
working directory when present; every key can also be overridden with a
`SIGNAL_FISH__...` environment variable (double underscores separate nested
keys — see [environment variable format](configuration.md#environment-variable-format)).

## How modes compose

The modes below are **not** mutually exclusive — they are feature layers you
combine. A production deployment is typically "Prod relay v2 + app allowlist" **plus**
"TLS (or reverse proxy)" **plus** "Metrics + Prometheus", and optionally the v3
WebRTC / TURN layers on top. Every v3 WebRTC upgrade gracefully degrades to the
v2 relay floor, so enabling v3 never breaks a v2-only client.

## Mode reference

| Mode | What it does | Key config (file keys + env overrides) | Exact command |
| --- | --- | --- | --- |
| Dev (relay v2, open app IDs) | Plain v2 WebSocket relay, open CORS, no app-ID restriction — for local development | `security.enforce_app_id_allowlist=false`, `security.cors_origins="*"`, `logging.enable_file_logging=false` (env: `SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=false`) | `cargo run` |
| Prod relay v2 + app allowlist | v2 relay with public app-ID allowlisting and locked-down CORS | `security.enforce_app_id_allowlist=true`, `security.allowed_apps=[…]`, `security.cors_origins="https://yourgame.com"`, `security.max_connections_per_ip=10` (env: `SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=true`) | `cargo run -- --validate-config && cargo run` |
| v3 WebRTC mesh/host + STUN | Advertises a v3 `SessionPlan` so peers connect over WebRTC, using public/self-hosted STUN to hole-punch | `session.default_topology="mesh"` (or `"host"`), `session.enable_webrtc=true`, `turn.stun_urls=["stun:…"]` (env: `SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY=mesh`) | `SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY=mesh cargo run` |
| v3 + TURN | Adds a self-hosted coturn relay for the ~15–20% of peers that cannot hole-punch; the server mints ephemeral coturn credentials | `turn.enabled=true`, `turn.urls=["turn:turn.yourgame.com:3478"]`, `turn.static_auth_secret=<shared>`, `turn.credential_ttl_secs=3600` (env: `SIGNAL_FISH__TURN__STATIC_AUTH_SECRET`) | `export TURN_STATIC_AUTH_SECRET="$(openssl rand -hex 32)"`<br>`export SIGNAL_FISH__TURN__STATIC_AUTH_SECRET="$TURN_STATIC_AUTH_SECRET"`<br>`docker compose --profile turn up -d` |
| TLS (built-in) | Terminates HTTPS/`wss://` in the server itself | `security.transport.tls.enabled=true`, `security.transport.tls.certificate_path`, `security.transport.tls.private_key_path` (env: `SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED=true`) | `cargo run` (with the three TLS keys set) |
| TLS (reverse proxy) | Terminates TLS at nginx/Caddy; server stays plain `ws://` on loopback | none on the server; configure the proxy (see [reverse proxy setup](deployment.md#reverse-proxy-setup)) | `cargo run` (server) + proxy in front |
| Metrics + Prometheus | Exposes JSON + Prometheus metrics, optionally behind a bearer token | `security.require_metrics_auth=true`, `security.metrics_auth_token=<token>` (env: `SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN`) | `SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN="$(openssl rand -hex 32)" SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=true cargo run` |
| Externally routed isolated deployments | An application-owned directory chooses one independent, single-process room home before any peer opens its WebSocket; Signal Fish does not share or hand off room state | Optional observability metadata: `server.room_code_prefix="USE"`, `server.region_id="us-east"` (env: `SIGNAL_FISH__SERVER__ROOM_CODE_PREFIX=USE`) | After deploying the external directory: `SIGNAL_FISH__SERVER__ROOM_CODE_PREFIX=USE SIGNAL_FISH__SERVER__REGION_ID=us-east cargo run` |

Per-feature JSON snippets (with env equivalents and "when to use") live in the
[configuration recipes](configuration-recipes.md).

## Mode details

### Dev (relay v2, open app IDs)

The fastest way to run the server. The compiled defaults fail closed:
metrics authentication is enabled with no token, so a bare `cargo run`
refuses to start. The example config disables metrics auth and app-ID
allowlisting for local use (file logging stays on; disable it with
`logging.enable_file_logging=false` if unwanted):

```bash
cp config.example.json config.json
cargo run
```

The example file [`config.example.json`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/config.example.json)
ships with `security.enforce_app_id_allowlist=false` and `security.cors_origins="*"`,
which is convenient for development but must never reach production. Health
check: `curl http://localhost:3536/v2/health`.

### Prod relay v2 + app allowlist

Turn on public app-ID allowlisting, register your app labels, and lock down CORS. Validate
first, then run:

```bash
cargo run -- --validate-config && cargo run
```

See the [application-ID recipe](configuration-recipes.md#application-id-allowlist-allowed_apps)
for the `allowed_apps` shape, and [Application identification](authentication.md) for the
client handshake. `--validate-config` exits non-zero on a bad config, so it is
safe to chain with `&&`.

### v3 WebRTC mesh/host + STUN

Set a non-`relay` topology (globally via `session.default_topology` or per game
via `session.game_topology_mappings`) and keep `session.enable_webrtc=true`. The
server then emits a v3 `SessionPlan` and advertises ICE servers (the configured
`turn.stun_urls`, plus any static `session.ice_servers`) when a WebRTC topology
is actually selected:

```bash
SIGNAL_FISH__SESSION__DEFAULT_TOPOLOGY=mesh cargo run
```

For a per-game mapping such as `{"chess":"mesh"}`, see the
[topology recipe](configuration-recipes.md#per-game-topology-mapping). STUN is
effectively free and handles roughly 80–85% of connections; the rest need TURN.

### v3 + TURN

TURN is **fully self-hosted**: the server mints short-lived coturn REST
credentials from a shared `turn.static_auth_secret` and never contacts a
third-party cloud. The repository's
[`docker-compose.yml`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/docker-compose.yml)
ships a `coturn` service behind the `turn` compose profile. The **same secret**
must be set on both coturn and the signaling server:

```bash
export TURN_STATIC_AUTH_SECRET="$(openssl rand -hex 32)"
export TURN_REALM="turn.yourgame.com"
export SIGNAL_FISH__TURN__STATIC_AUTH_SECRET="$TURN_STATIC_AUTH_SECRET"
export SIGNAL_FISH__TURN__ENABLED=true

docker compose --profile turn up -d
```

A plain `docker compose up` (no profile) is unchanged and starts only the
signaling server. The coturn service refuses to start with an empty secret. See
the [TURN deployment guide](deployment-turn.md) for the ephemeral-credential
scheme, the relay port range, secret rotation, and capacity planning, and the
[TURN/STUN recipe](configuration-recipes.md#turnstun) for the `[turn]` block.

### TLS: built-in vs reverse proxy

Signaling **must** run over `wss://` in production: WebRTC DTLS fingerprints
travel through the signaling channel inside the SDP, so plaintext `ws://` lets an
on-path attacker defeat WebRTC encryption (see
[signaling must run over wss://](deployment-turn.md#signaling-must-run-over-wss)).
You have two ways to satisfy this.

Built-in TLS terminates HTTPS in the server:

```bash
SIGNAL_FISH__SECURITY__TRANSPORT__TLS__ENABLED=true \
  SIGNAL_FISH__SECURITY__TRANSPORT__TLS__CERTIFICATE_PATH=/etc/ssl/signal-fish/fullchain.pem \
  SIGNAL_FISH__SECURITY__TRANSPORT__TLS__PRIVATE_KEY_PATH=/etc/ssl/signal-fish/privkey.pem \
  cargo run
```

Reverse-proxy TLS terminates at nginx or Caddy and forwards plain `ws://` to the
server on loopback; the server needs no TLS keys. This is the common deployment —
see the [reverse proxy setup](deployment.md#reverse-proxy-setup). The
[TLS recipe](configuration-recipes.md#tls) shows both as JSON.

### Metrics + Prometheus

Metrics are served at `/metrics` (and `/v1/metrics`) as JSON and at
`/metrics/prom` for Prometheus. Protect them with a bearer token: when
`security.require_metrics_auth=true`, requests must send
`Authorization: Bearer <token>` matching `security.metrics_auth_token`:

```bash
SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=true \
  SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN="$(openssl rand -hex 32)" \
  cargo run
```

Validation fails fast if `require_metrics_auth` is on but no token is set. Point
Prometheus at `/metrics/prom` (see
[Prometheus configuration](deployment.md#prometheus-configuration)). The
[metrics-auth recipe](configuration-recipes.md#metrics-auth) covers the scrape
config with a bearer token.

### Externally routed isolated deployments

Signal Fish does not support a fleet of interchangeable active processes behind
a generic load balancer. Room state, reconnect tokens, routes, and sequence
counters are process-local, and the room code arrives only after the WebSocket
upgrade. Neither cookie stickiness nor `server.room_code_prefix` can establish
the required room home by itself.

You may operate several isolated deployments only when an application-owned
directory chooses the home **before** clients connect and sends every initial
connection and reconnect for that room to the same process. A prefix can be part
of that external routing scheme, and `server.region_id` can label metrics, but
both are metadata rather than coordination:

```bash
SIGNAL_FISH__SERVER__ROOM_CODE_PREFIX=USE \
  SIGNAL_FISH__SERVER__REGION_ID=us-east \
  cargo run
```

The prefix is normalized to uppercase and must contain only ASCII letters or
digits. It must be shorter than `protocol.room_code_length`; hyphenated region
names such as `us-east-` are invalid room-code prefixes.

See the
[single-instance deployment contract](architecture/single-instance-deployment.md)
for the exact boundary and failure catalog, and the
[scaling architecture notes](architecture/scaling.md) for capacity drivers and
future extension seams.

## CLI flags

The binary accepts a small set of flags (run `--help` for the full list):

| Flag | Effect |
| --- | --- |
| `--validate-config` (`-c`) | Load and validate the config, print a summary, and exit. Non-zero exit on failure. |
| `--print-config` | Print the resolved config (file + environment overrides) as JSON, then exit. Secrets are redacted. |
| `--version` | Print the version and exit. |

`--validate-config` and `--print-config` are mutually exclusive.

### `--validate-config` in CI

Run it as a pre-deployment gate — it exits non-zero on an invalid or insecure
config, so it fails the pipeline before a bad config ships:

```bash
cargo run -- --validate-config
# or against a built image:
docker run --rm -v ./config.json:/app/config.json:ro \
  ghcr.io/ambiguous-interactive/signal-fish-server:latest --validate-config
```

It prints a short summary (port, TLS enabled, metrics auth required,
reconnection enabled, max players, region) so CI logs capture the effective
posture.

### `--print-config` redaction

`--print-config` serializes the **resolved** config (after environment
overrides) so you can confirm what the server actually loaded. Every secret is
replaced with `<redacted>` before printing — this covers
`security.metrics_auth_token`, `session.ice_servers[*].credential`, and
`turn.static_auth_secret`. A _set_
secret shows as `<redacted>`; an _unset_ one stays empty/`null`, so you can
still tell "configured" from "missing". TLS certificate and key _paths_ are file
locations, not secrets, and stay visible.

```bash
cargo run -- --print-config
```

## See also

- [Configuration reference](configuration.md) — every key, default, and env override
- [Configuration recipes](configuration-recipes.md) — per-feature JSON + env snippets
- [Pre-deployment checklist](pre-deployment-checklist.md) — what to verify before going live
- [Deployment](deployment.md) — Docker, reverse proxies, cloud providers
- [TURN Deployment](deployment-turn.md) — self-hosted coturn and credential rotation
