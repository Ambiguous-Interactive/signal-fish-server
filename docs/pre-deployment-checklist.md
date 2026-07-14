# Pre-deployment Checklist

Run through this list before exposing Signal Fish Server to the internet. Every
item names the real configuration key (or command) involved — see the
[configuration reference](configuration.md#configuration-reference) for defaults,
[Configuration recipes](configuration-recipes.md) for per-feature snippets, and
[Run Modes](run-modes.md) for the run commands.

Start by letting the server validate itself: it exits non-zero on an insecure or
malformed config, so this is the single best gate.

```bash
cargo run -- --validate-config
# or against your built image:
docker run --rm -v ./config.json:/app/config.json:ro \
  ghcr.io/ambiguous-interactive/signal-fish-server:latest --validate-config
```

Then confirm the resolved posture (secrets are redacted):

```bash
cargo run -- --print-config
```

## Authentication

- [ ] WebSocket auth enabled — `security.require_websocket_auth=true`.
- [ ] At least one real app registered in `security.authorized_apps` with a
  meaningful `app_id` / `app_name`.
- [ ] CORS locked down — `security.cors_origins` set to your origin(s), **not**
  `*`.
- [ ] `security.max_connections_per_ip` set to a sane ceiling (default `24`).

## Secrets changed

- [ ] No default secrets remain. The example `app_secret`
  (`CHANGE_ME_BEFORE_PRODUCTION`) in
  [`config.example.json`](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/config.example.json)
  is intentionally insecure — replace every `authorized_apps[*].app_secret`.
- [ ] `turn.static_auth_secret` (if TURN is enabled) is a strong random value,
  supplied via `SIGNAL_FISH__TURN__STATIC_AUTH_SECRET`, not committed to the
  repo.
- [ ] `security.metrics_auth_token` (if metrics auth is enabled) is a strong
  random value (`openssl rand -hex 32`).
- [ ] `--print-config` shows every set secret as `<redacted>` and no plaintext
  secret leaks into logs or CI output.

## TLS or reverse proxy

- [ ] Signaling is served over `wss://` — **mandatory** for WebRTC, because DTLS
  fingerprints travel through the signaling channel (see
  [signaling must run over wss://](deployment-turn.md#signaling-must-run-over-wss)).
- [ ] Either built-in TLS is on (`security.transport.tls.enabled=true` with
  `certificate_path` and `private_key_path`) **or** TLS terminates at a reverse
  proxy (see [reverse proxy setup](deployment.md#reverse-proxy-setup)).
- [ ] If using mTLS: `security.transport.tls.client_auth="require"` with a valid
  `client_ca_cert_path`.

## Metrics auth

- [ ] `/metrics`, `/v1/metrics`, and `/metrics/prom` are not openly reachable —
  either `security.require_metrics_auth=true` with a `security.metrics_auth_token`,
  or the endpoints are restricted at the reverse proxy (IP allowlist).
- [ ] The metrics token is at least 32 characters (the server warns below 16).
- [ ] Prometheus is configured with the bearer token if auth is on (see the
  [metrics-auth recipe](configuration-recipes.md#metrics-auth)).

## Rate limits sane

- [ ] `rate_limit.*` values fit your traffic — `max_room_creations`,
  `max_join_attempts`, `max_signals`, `max_signal_errors`, and `time_window`
  (see [when to adjust](configuration-recipes.md#rate-limits)).
- [ ] Per-app overrides (`authorized_apps[*].rate_limit_per_minute`,
  `max_rooms`, `max_players_per_room`) set where an app needs different limits.
- [ ] `security.max_signal_bytes` and `security.max_message_size` left at sane
  caps (`max_signal_bytes` must be `> 0` and `≤ max_message_size`).

## TURN secret shared and rotated

- [ ] If TURN is enabled (`turn.enabled=true`): `turn.urls` lists at least one
  reachable `turn:`/`turns:` URL and `turn.static_auth_secret` is set.
- [ ] The **same** `static_auth_secret` is configured on coturn and the signaling
  server (the compose `turn` profile reads `TURN_STATIC_AUTH_SECRET`).
- [ ] `turn.credential_ttl_secs` comfortably exceeds your longest expected
  session (default `3600`).
- [ ] A rotation procedure is in place (see
  [rotating the shared secret](deployment-turn.md#rotating-the-shared-secret)).
- [ ] coturn's relay port range is published/reachable (see the
  [TURN quick start](deployment-turn.md#self-hosted-coturn-quick-start)).

## Log rotation

- [ ] File logging configured for production —
  `logging.enable_file_logging=true`, `logging.dir`, `logging.filename`.
- [ ] `logging.rotation` set (`daily`, `hourly`, or `never`) to bound disk usage.
- [ ] `logging.format` chosen (`json` for log pipelines, `text` for humans) and a
  level set via `logging.level` or `RUST_LOG`.

## Room cleanup intervals

- [ ] `server.room_cleanup_interval` is `> 0` (default `60`) so the cleanup task
  actually runs.
- [ ] `server.empty_room_timeout` and `server.inactive_room_timeout` tuned to
  your session lengths so stale rooms are reclaimed.
- [ ] If reconnection is enabled (`server.enable_reconnection=true`):
  `server.reconnection_window` and `server.event_buffer_size` match how long and
  how much you want to buffer for replay.

## Deployment topology

- [ ] One active Signal Fish process serves each routing domain; no generic
  round-robin or cookie-sticky load balancer fronts interchangeable processes.
- [ ] If operating several isolated deployments, an application-owned directory
  assigns the room home before the WebSocket upgrade and routes every initial
  connection and reconnect for that room to the same process.
- [ ] `server.room_code_prefix` and `server.region_id`, if set, are treated as
  routing/observability metadata rather than shared-state coordination.
- [ ] The load balancer stops sending new connections before `SIGTERM`, and its
  timeout allows at least `server.drain_grace_secs` for the close boundary.
- [ ] Review the
  [single-instance deployment contract](architecture/single-instance-deployment.md)
  and [scaling architecture notes](architecture/scaling.md).

## Final smoke test

- [ ] `--validate-config` passes.
- [ ] Health check responds: `curl https://your-host/v2/health` returns `200`.
- [ ] A real client can authenticate, create/join a room, and (for v3) negotiate
  its `SessionPlan`.

## See also

- [Run Modes](run-modes.md) — how to run each mode
- [Configuration recipes](configuration-recipes.md) — per-feature config
- [Configuration reference](configuration.md) — every key and default
- [Deployment](deployment.md) — Docker, reverse proxies, cloud providers
- [Authentication](authentication.md) — client handshake and error codes
- [TURN Deployment](deployment-turn.md) — self-hosted coturn and rotation
