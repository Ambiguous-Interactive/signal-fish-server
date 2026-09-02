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

There is no optional or required client-credential mode. Credential
provisioning, rotation, expiry, and replay protection therefore do not apply to
the app-ID handshake. Use a separate trusted identity service if hostile-client
tenant isolation is required.

## Frozen wire names

Protocol v2 is frozen and protocol v3 is additive, so the existing wire names
remain `Authenticate`, `Authenticated`, and `AuthenticationError`. In this
server they mean “submit an app label,” “label accepted and protocol negotiated,”
and “handshake rejected”; they do not prove the caller's identity.

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
it is client-chosen. Under the open policy, a well-formed UUID sent as
`app_id` is used verbatim, so the client chooses the application UUID. No
current open-mode feature consumes that identity as an authority (per-app
quotas and the cross-app join gate are allowlist-only); a future feature that
does must not treat an open-mode application UUID as unspoofable.

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
  handshakes consume no application-wide budget. A botnet spanning many
  sources is bounded by the application-wide ceiling itself.

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
