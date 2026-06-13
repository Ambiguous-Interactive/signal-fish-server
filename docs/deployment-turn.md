# TURN Deployment

How to give Signal Fish Server's WebRTC sessions a TURN relay, and how to run the
signaling side securely. Signal Fish Server never relays WebRTC media itself — it
only mints and advertises ICE credentials, in `SessionPlan`s and pre-gather
`RoomJoined` / `Reconnected` ICE lists. TURN is **self-hosted**: the server mints
credentials locally for an operator-run coturn and never contacts a third-party
cloud (see [Self-hosted by design](#self-hosted-by-design-no-managed-cloud-integration)
if you want to point clients at a managed service instead).

## When you need TURN

Roughly 15–20% of real-world P2P connections cannot connect directly (symmetric
NAT, CGNAT/mobile networks, restrictive firewalls) and must fall back to a TURN
relay. Plan for that share of your WebRTC traffic to flow through TURN.

Even without TURN, no player is ever stranded: a client whose P2P path fails falls
back to `GameData` over the WebSocket relay — see the
[Transport Fallback Contract](architecture/transport-fallback.md). TURN simply
keeps those players on the lower-latency peer-to-peer path instead of the floor.

## Self-hosted coturn quick start

The repository's `docker-compose.yml` ships a `coturn` service behind the `turn`
compose profile. A plain `docker compose up` is unchanged; opting in brings up
coturn alongside the signaling server:

```bash
export TURN_STATIC_AUTH_SECRET="$(openssl rand -hex 32)"
export TURN_REALM="turn.yourgame.com"

docker compose --profile turn up -d
```

The **same secret** must be configured on the signaling server — it is the single
source of truth both sides derive credentials from:

```json
{
  "turn": {
    "enabled": true,
    "urls": ["turn:turn.yourgame.com:3478"],
    "credential_ttl_secs": 3600
  }
}
```

```bash
# Prefer the environment over the config file for the secret itself.
export SIGNAL_FISH__TURN__STATIC_AUTH_SECRET="$TURN_STATIC_AUTH_SECRET"
```

See the [TURN / STUN configuration reference](configuration.md#turn--stun-ice-credentials-protocol-v3)
for every `[turn]` key.

Three production notes on the compose profile:

- **Fail-fast on a missing secret.** The coturn service refuses to start when
  `TURN_STATIC_AUTH_SECRET` is unset or empty: an entrypoint guard exits with a
  clear error instead of letting coturn run `--use-auth-secret` with an empty
  HMAC key — which would let anyone mint valid TURN credentials (an open
  relay). The check runs at container start rather than at
  `docker compose config` time because Compose interpolates variables file-wide
  even when the `turn` profile is inactive, so a hard `${VAR:?}` requirement
  would break a plain `docker compose up` for deployments that never use TURN.
- **Relay port range.** Besides listening port 3478, coturn allocates one relay
  port per TURN allocation from its `--min-port`/`--max-port` range. Every port in
  that range must be reachable by remote peers, so the whole range has to be
  published — an unpublished relay port means allocations succeed but media never
  flows. The compose file carries a commented range mapping; for large ranges
  prefer `network_mode: host` on Linux over publishing thousands of ports.
- **No secrets in the repository.** `TURN_STATIC_AUTH_SECRET` is interpolated from
  the environment (or an uncommitted `.env` file); nothing secret is checked in.

## How the ephemeral credential scheme works

When TURN is enabled the server implements the coturn REST API
("ephemeral credentials") scheme — coturn's `--use-auth-secret` mode:

```text
expiry     = now_unix + credential_ttl_secs
username   = "{expiry}:{player_id}"
credential = base64( HMAC-SHA1( static_auth_secret, username ) )
```

Operationally this means:

- **The secret never reaches clients.** `turn.static_auth_secret` lives only on
  the signaling server and in coturn. Clients receive only the derived,
  short-lived `username`/`credential` pair inside their `SessionPlan` or
  pre-gather `ice_servers`; coturn recomputes the HMAC to authenticate them.
- **Each player gets their own credential.** The username embeds the player's id,
  so credentials are per-player and individually expiring. Members finalized
  together share one expiry timestamp.
- **TTL is `credential_ttl_secs`** (default 3600 = 1 hour). coturn rejects the
  pair once the embedded expiry passes, so a leaked credential has a bounded
  lifetime. Size the TTL to comfortably exceed your longest expected session,
  because mid-session ICE restarts need a still-valid credential; late joiners
  and host-failover re-plans always receive freshly minted credentials.
- **ICE pre-gather mints at join time.** With `session.enable_ice_pregather`
  (the default), every eligible v3 client — one that negotiated the WebRTC
  transport and the game's desired topology — joining or reconnecting into a
  non-relay-desired game's lobby receives a freshly minted credential on
  `RoomJoined` / `Reconnected` — before, and in addition to, the one in its
  eventual `SessionPlan`. For capacity planning that means **issuance scales
  with joins, not just finalizes** (a player that joins, leaves, and rejoins
  mints each time; an abandoned lobby still minted for everyone eligible who
  entered). Minted-but-never-used credentials cost coturn nothing until a client
  allocates with them, but each one is a live credential for its full TTL —
  `enable_ice_pregather: false` is the kill switch if join-time issuance is
  unwanted, and the TTL is the exposure lever either way. The shared
  `signal_fish_transport_turn_credentials_issued_total` counter includes
  pre-gather minting, so dashboards see total issuance.
- **No coturn user database is needed** for this scheme — the shared secret is
  the entire trust relationship.

## Rotating the shared secret

coturn accepts **multiple valid secrets at once** — `--static-auth-secret` can be
passed more than once, and database-backed deployments can hold several rows in
the `turn_secret` table (managed on the fly with `turnadmin`). That makes
zero-downtime rotation straightforward:

1. **Add the new secret to coturn** alongside the old one (a second
   `--static-auth-secret` flag, or `turnadmin` for database-backed setups).
   Credentials minted from either secret now authenticate.
2. **Flip the signaling server** to the new secret
   (`SIGNAL_FISH__TURN__STATIC_AUTH_SECRET`) and restart or redeploy it. All new
   `SessionPlan`s now carry credentials derived from the new secret.
3. **Wait at least `credential_ttl_secs`** so every credential minted from the old
   secret has expired.
4. **Remove the old secret from coturn.** Nothing still in flight depends on it.

Rotate on a schedule (and immediately on suspected exposure) — the static secret
is the only long-lived credential in the scheme.

## Self-hosted by design (no managed-cloud integration)

TURN here is **self-hosted only**, on purpose: the signaling server never calls a
third-party cloud and needs no external credentials. There is no `managed` mode —
the server mints credentials locally for a coturn instance **you** operate, so the
static secret stays inside your own infrastructure.

If you nonetheless want to use a managed TURN service (e.g. one priced per relayed
gigabyte), the server does not integrate with one directly. You can still wire it
in **out-of-band**, treating it as just another ICE server: either have clients
fetch short-lived credentials from the provider themselves, or place
provider-issued credentials in the static `[session].ice_servers` list, which is
advertised to WebRTC plans verbatim ahead of any self-minted TURN entry. Neither
path involves the signaling server contacting the provider.

## Signaling must run over wss://

In production the signaling endpoint **must** be served over `wss://` (TLS). This
is load-bearing for WebRTC security, not a generic hardening tip: the DTLS
certificate fingerprints that WebRTC uses to authenticate its encrypted transport
travel inside the SDP offers and answers — that is, through the signaling
channel. An on-path attacker who can read and rewrite plaintext `ws://` signaling
can substitute their own fingerprints and silently machine-in-the-middle both
DTLS handshakes, defeating WebRTC's encryption entirely. WebRTC verifies the
fingerprints it was given; it cannot detect that they were swapped in transit.

Terminating TLS at a reverse proxy in front of the server — the nginx and Caddy
examples in the [reverse proxy setup](deployment.md#reverse-proxy-setup) — fully
satisfies this: clients connect to `wss://signal.yourgame.com/v3/ws` and the
proxy forwards to the server over the loopback interface. Pair it with the
hardening items in the [security checklist](deployment.md#security-checklist).

## Capacity planning

- **TURN bandwidth dominates cost.** Budget for 15–20% of your total P2P game
  traffic to be relayed through TURN; at managed per-GB rates (or your own egress
  pricing) this dwarfs the cost of running the signaling server itself.
- **Signaling is light.** The signaling server only brokers small JSON messages
  (session plans, SDP/ICE signals, relay-floor `GameData`); see the
  [scaling architecture notes](architecture/scaling.md) for how to scale it
  horizontally.
- **STUN is effectively free.** Public or self-hosted STUN handles the ~80–85% of
  connections that can hole-punch; only the remainder consumes TURN relay
  bandwidth.

## Related documents

- [Deployment guide](deployment.md) — Docker, reverse proxies, cloud providers
- [TURN / STUN configuration reference](configuration.md#turn--stun-ice-credentials-protocol-v3)
- [Transport Fallback Contract](architecture/transport-fallback.md) — the relay floor
- [Scaling architecture notes](architecture/scaling.md) — multi-node signaling
- [Protocol v3 ICE / TURN credentials](protocol.md#ice--turn-credentials)
