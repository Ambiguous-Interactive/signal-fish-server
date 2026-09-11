# Application identification and access

Signal Fish Server can restrict WebSocket use to a configured set of public
application IDs. This is an allowlist and accounting boundary, not client
authentication: shipped clients send `app_id` in cleartext, and any client that
knows an allowed value can reuse it.

The compiled default enforces the allowlist. The example configuration disables
it for local development.

## Configure the app-ID allowlist

Set `enforce_app_id_allowlist` to `true` and register each allowed application:

```json
{
  "security": {
    "enforce_app_id_allowlist": true,
    "allowed_apps": [
      {
        "app_id": "my-game",
        "app_name": "My Game",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}
```

The two policy modes are:

- `false`: open mode. Clients may omit `Authenticate`; any supplied app ID gets
  the default limits.
- `true`: allowlist mode. The first client message must be `Authenticate`, and
  its public `app_id` must appear in `allowed_apps`.

By default the app-ID handshake stays credential-free: `app_id` is a public
label. Deployments that need protocol-level tenant authentication can enable
the optional `connect_token` mode — see
[Optional tenant connect tokens](#optional-tenant-connect-tokens) below.

## Frozen wire names

Protocol v2 is frozen and protocol v3 is additive, so the existing wire names
remain `Authenticate`, `Authenticated`, and `AuthenticationError`. In this
server they mean “submit an app label,” “label accepted and protocol negotiated,”
and “handshake rejected”; the label itself never proves the caller's identity.
When the optional `connect_token` mode is enabled, the signed token — not the
label — is the credential.

```javascript
const ws = new WebSocket('wss://signal.example/v2/ws');

ws.onopen = () => {
  ws.send(JSON.stringify({
    type: 'Authenticate',
    data: { app_id: 'my-game' }
  }));
};

ws.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.type === 'Authenticated') {
    // The public app label was accepted; room operations may now begin.
  }
};
```

If another message arrives first in allowlist mode, the server sends a generic
`MISSING_APP_ID` error and closes the connection. Unknown IDs receive
`INVALID_APP_ID`. The same code rejects IDs that cannot be accepted safely in
operator-facing logs — control characters such as newlines or ANSI escapes, or
lengths over 256 bytes — in every mode. A protocol maximum below the deployment
minimum receives `UNSUPPORTED_PROTOCOL_VERSION`.

## Exact trust boundary

Once an app ID is accepted, the server attaches its application context to the
connection. Room creation, seated joins, spectator joins, reconnects, ready
state, quotas, and per-app rate limits all use that connection-bound context;
later messages cannot claim a different app ID.

This provides accounting and accidental-collision isolation only. A client that
knows another allowed ID can:

- consume that label's rate and room quota;
- create rooms attributed to that label;
- join that label's rooms when it also knows their room codes; and
- appear in logs and metrics under that label.

Room ownership remains non-enumerating: a different label receives the same
`ROOM_NOT_FOUND` result for seated, spectator, and reconnect admission. That
does not turn the public label into a credential.

### What the application UUID means

The connection-bound application context carries an internal UUID, and its
provenance differs by policy. Under the enforced allowlist it is always a
deterministic SHA-256 derivative of the public app ID string, so nothing about
it is client-chosen. Under the open policy it is derived the same way but
hashed under an open-policy-only namespace (issue #518), so an open-mode UUID
does not equal a configured application's UUID and a client cannot claim
another application's identity by sending that application's UUID as its
`app_id`. The same label still always yields the same UUID, so attribution and
room membership stay stable across handshakes and restarts.

Open-mode application identity does scope room admission (issue #520): a
created room is stamped with the creator's application UUID, and an owned
room admits only same-application seats, spectators, and reconnects. That
boundary remains a soft one — an attacker who knows (or guesses) a victim's
public app label can present it and pass the gate — so it must not be
treated as a tenancy guarantee. Deployment-grade isolation requires
allowlist enforcement plus the optional `connect_token` mode, where a
control-plane-signed credential is verified before admission.

## Per-app settings

- `app_id` — public identifier sent by clients.
- `app_name` — human-readable name returned after the handshake.
- `max_rooms` — maximum concurrent persisted rooms owned by the label across
  all game names.
- `max_players_per_room` — maximum requested capacity for newly created rooms.
- `rate_limit_per_minute` — handshake requests per minute, counted across every
  connection using the same public ID. Enforced only when an entry configures
  an explicit value; omitting it is the "unlimited" configuration — the
  `Authenticated.rate_limits` numbers are then projections only, and unknown-ID
  rejections never consume any budget.
  Enforcement is split into two sliding windows: the application-wide ceiling
  above, plus a per-source (IP) share of half that budget (at least one) — so
  one source that knows a configured `app_id` can never continuously exhaust
  the app's budget and lock out legitimate handshakes (issue #502). Rejected
  handshakes consume no budget in either window. Note a single source can
  therefore admit at most half of the advertised `per_minute` figure (for an
  entry limited to 1 per minute, the share is that single handshake); clients
  sharing one NAT egress share that source budget. A botnet spanning many
  sources is bounded by the application-wide ceiling itself.
- `max_relay_bytes` — optional per-sender game-data relay byte budget for this
  application (issue #530): admitted relay payload charges draw from this
  budget instead of the server-wide `rate_limit.max_relay_bytes` default, so
  hosted tiers can bound tenants independently. The fixed rate-limit window
  stays anchored to the sender, so a client cannot reset its window by
  switching app labels; must be `> 0` when set.

## Reload the allowlist at runtime (SIGHUP)

Allowlist enforcement is on or off for the life of the process, but the
configured set of applications is reloadable without a restart (issue #522).
Send `SIGHUP` to the server process:

```bash
kill -HUP "$(pgrep -f signal-fish-server)"
```

On each `SIGHUP` the server:

1. Re-reads the configuration from the same sources as startup
   (`config.json`, `security.app_auth_path`, `SIGNAL_FISH_CONFIG_JSON`,
   environment overrides).
2. Validates the new `security.allowed_apps` set with the exact startup
   rules (unique IDs, log-safe IDs, registry-file contract).
3. Swaps the set atomically. New handshakes resolve against the new set;
   handshakes already in flight complete against the set they resolved.
4. Logs the change (`added`/`removed` app IDs).

Failure behavior:

- A configuration that fails to load or fails security validation keeps the
  running allowlist. The error is logged. Validation covers the whole config
  document, so an unrelated invalid edit (a removed TLS cert file, for
  example) also blocks the allowlist update. To force fail-closed admission,
  restart the process instead.
- Removing an application stops NEW handshakes for that label immediately.
  Connections that already resolved it keep their context; revoking a live
  connection stays a restart or tenant-level action. The kept context includes
  the admission powers the context carries: a pre-reload socket can still join
  rooms and create new rooms under the revoked application's identity until it
  disconnects. New handshakes — including any reconnect from a fresh socket —
  fail with the unknown-app-ID refusal.
- A reload that TIGHTENS a per-app cap (`max_rooms`, `max_players_per_room`)
  reaches fresh handshakes immediately. Live sockets keep their resolved
  context until they re-handshake (reconnect from a new socket), so the
  stricter limit can coexist with rooms created by pre-reload sockets until
  those sockets recycle. The room-count check itself is always live; only the
  limit value is snapshotted.
- A reload can introduce rate limits for the first time; new budgets apply
  to new handshakes at once.
- In open mode (`enforce_app_id_allowlist: false`) the reload is a logged
  no-op: there is no configured set to swap.

Only `security.allowed_apps` and the `security.connect_token` verification key
are applied live. Port, TLS, limits, and every other configuration field still
require a restart; the reload log says so.

## External app-registry file (`security.app_auth_path`)

For deployments where a control plane owns the live app registry (onboarding,
suspension, per-app caps), the registry can live outside the main config
document. Set `security.app_auth_path` to a JSON file of the shape
`{"apps": [...]}` where each entry has exactly the same fields as an
`allowed_apps` entry:

```json
{
  "apps": [
    {
      "app_id": "my-game",
      "app_name": "My Game",
      "max_rooms": 100,
      "max_players_per_room": 16,
      "rate_limit_per_minute": 60
    }
  ]
}
```

The file's entries are appended to `security.allowed_apps` at config load
time, before any enforcement or `--validate-config` checks, so the two lists
form one registry. Semantics:

- **Fail-closed admission.** When the knob is set, a missing, unreadable,
  malformed, or contract-violating file is a startup error naming the file —
  never a silent fall-back to the config-file subset. A deployment whose
  registry mount disappears must fail loudly, not quietly start rejecting
  every handshake (or accounting for none).
- **Strict keys.** Unknown keys — at the top level, or inside any entry — are
  rejected, so a typo'd cap fails loudly instead of silently widening.
  `app_secret` is rejected outright: the registry is a public-label list and
  must never carry credential material. (The optional `connect_token`
  verification key is server-side public configuration, not a client
  credential, and it lives under `security.connect_token`, never in the
  registry.)
- **No duplicates.** `app_id` collisions between the two lists (or within one
  list) are startup validation errors naming the duplicate entry
  (`allowed_apps[N].app_id`).
- **Empty is valid.** `{"apps": []}` loads no entries — the shape a control
  plane writes for a freshly provisioned host.

The path can also be supplied without a config document via the environment:

```bash
SIGNAL_FISH__SECURITY__APP_AUTH_PATH=/etc/signal-fish/app-auth.json
```

This is the intended contract for the cloud deployment's read-only-mounted
`/etc/signal-fish/app-auth.json` file: the provisioner regenerates the file on
its own schedule and the server picks it up at the next startup or at the
next `SIGHUP` reload (see "Reload the allowlist at runtime"). At startup the
fail-closed rule applies: a missing or unreadable registry refuses to boot.
At reload time the running allowlist is kept and the error is logged, so a
transiently missing mount cannot silently change admission; restart to force
the fail-closed refusal.

## Optional tenant connect tokens

The `connect_token` mode adds protocol-level tenant authentication
(issue #517). The operator's control plane signs tokens with an Ed25519
private key; the server verifies them against the matching public key. The
private key never reaches this process, and verification is stateless: no
callouts, no store.

### Configure

```json
{
  "security": {
    "connect_token": {
      "public_key": "<base64 of the 32-byte Ed25519 public key>"
    }
  }
}
```

- Set exactly one of `public_key` (inline base64) or `public_key_path`
  (a file whose trimmed contents are the same base64 key; the loader folds
  the file into `public_key`). Configuring both is a startup error.
- The key is public material. The path option exists for mount and rotation
  convenience, not secrecy.
- A key that does not parse (not base64, not 32 bytes, not a valid curve
  point) is a startup error.
- The key reloads on `SIGHUP` with the allowlist. A reload with a corrupt
  key keeps the running key; removing the block removes verification, and
  presented tokens are then refused.

### Wire contract

A client that holds a credential adds one optional field to `Authenticate`:

```json
{
  "type": "Authenticate",
  "data": {
    "app_id": "my-game",
    "connect_token": "sfct_v1.C..."
  }
}
```

Token format (minted by the control plane):

```text
token   = "sfct_v1" "." payload_b64 "." signature_b64
payload = UTF-8 JSON {"app_id": string, "exp": unix-seconds, "nonce": string}
```

Both base64 encodings are URL-safe without padding. The 64-byte Ed25519
signature covers the exact ASCII bytes of `"sfct_v1." ++ payload_b64` — the
encoded form is signed, so there is no JSON canonicalization anywhere.

The server verifies, in order: encoding, signature, expiry, validity window,
then that the payload's `app_id` equals the presented `app_id`. Every failure
reports the same error code, `CONNECT_TOKEN_INVALID`; the `error` text carries
the reason, and the token is never logged or echoed.

### Semantics and limits

- **Absent field.** No token: the handshake behaves exactly as before, in
  every mode. Released SDKs and self-hosted deployments are unaffected.
- **Presented without a configured key.** Refused with
  `CONNECT_TOKEN_INVALID` (fail closed). A client that expects credentials to
  matter must not be silently downgraded to public-label semantics.
- **Token lifetime.** The server accepts a token only while `exp` is in the
  future and its remaining validity is at most 300 seconds plus a fixed
  60-second clock-skew allowance. A minter that sets a longer window fails;
  the replay window stays bounded by policy, not minter discipline.
- **Replay is accepted within the window.** The server is stateless and does
  no single-use tracking. Send tokens only over TLS. A leaked token replays
  until expiry — this is the ratified trade-off. Use the signed `nonce` at
  the control plane or edge if single-use enforcement is needed.
- **Retryable refusal.** A rejected token keeps the connection open, so the
  client can fetch a fresh token and re-send `Authenticate` on the same
  socket. The refusal charges the per-connection error-reply budget, like
  every polite per-frame reply (close code `4006` bounds the total).
- **Redaction.** The server never logs the token and never echoes it — not in
  `Authenticated`, `ProtocolInfo`, room snapshots, or reconnect payloads.
  Client SDKs must apply the same rule to their own logs.

## Legacy configuration

Existing configuration remains loadable:

- `require_websocket_auth` aliases `enforce_app_id_allowlist`.
- `authorized_apps` aliases `allowed_apps`.
- `app_secret` is accepted only as deprecated input and discarded without being
  retained, logged, validated, or emitted by `--print-config`.

Migrate to the canonical names. Supplying both a canonical and legacy name in
the same JSON source is a startup error naming the source and both keys, so
the server never boots on an ambiguous allowlist and no lower-priority open
config can fail open.
Canonical individual-field environment overrides still have final precedence.
Duplicate `app_id` entries — and entries that could never authenticate (control
characters such as newlines or ANSI escapes, or more than 256 bytes) — are
rejected at startup rather than using last-entry-wins limits or failing every
later handshake silently. The canonical environment override is:

```bash
SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=true
SIGNAL_FISH__SECURITY__ALLOWED_APPS='[{"app_id":"my-game","app_name":"My Game","rate_limit_per_minute":60}]'
```

## Handshake timeout

`websocket.auth_timeout_secs` is also a frozen legacy name. It is the exclusive
deadline for receiving the initial `Authenticate` protocol-negotiation frame.
Input observed at or after the boundary is rejected with close code
`4001 auth_timeout`.

## Metrics authentication

Metrics authentication is separate and does validate a real bearer secret:

```json
{
  "security": {
    "require_metrics_auth": true,
    "metrics_auth_token": "<generated-token>"
  }
}
```

Generate the token through a secret manager or environment variable. The server
compares `Authorization: Bearer <metrics_auth_token>` in constant time and
redacts the configured token from `--print-config`.

## Operational guidance

- Use `wss://` in production so app labels and signaling are protected in
  transit.
- Treat rejected-ID spikes as probing or configuration drift.
- Treat allowed IDs as public; do not grant billing, administrative, or secret
  access based on them.
- Protect active TURN and metrics secrets through environment or secret-manager
  injection.

See [Configuration](configuration.md), [Deployment](deployment.md), and
[Error codes](reference/error-codes.md).
