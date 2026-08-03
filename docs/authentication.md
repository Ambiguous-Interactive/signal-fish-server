# Authentication

The compiled config default requires WebSocket authentication. The example
configuration opts out for local development; production deployments should keep
auth enabled and enforce per-app rate limits.

## Enabling Authentication

Set `require_websocket_auth` to `true` and add authorized apps:

```json
{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "my-game",
        "app_secret": "RESERVED_NOT_USED_BY_CLIENTS",
        "app_name": "My Game",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}

```

`app_secret` remains a reserved configuration field, but the current WebSocket
client handshake does not send or validate it. Do not embed it in a shipped
client or treat changing it as client credential rotation.

## Client Authentication

When auth is enabled, clients must send an `Authenticate` message immediately after connecting:

```javascript

const ws = new WebSocket('ws://localhost:3536/v2/ws');

ws.onopen = () => {
  ws.send(JSON.stringify({
    type: 'Authenticate',
    data: {
      app_id: 'my-game'
    }
  }));
};

ws.onmessage = (event) => {
  const message = JSON.parse(event.data);

  if (message.type === 'Authenticated') {
    console.log('Authenticated successfully');
    // Now you can create/join rooms
  }

  if (message.type === 'AuthenticationError') {
    // e.g. INVALID_APP_ID for an unknown app_id
    console.error('Authentication failed:', message.data.error_code);
  }
};

```

A failed `Authenticate` is reported as a dedicated `AuthenticationError` message. If the
client instead sends any other message before authenticating, the server replies with a
generic `Error` carrying `error_code === 'MISSING_APP_ID'` and closes the connection.

> **Current trust boundary:** the WebSocket handshake validates the public
> `app_id`; clients do not send `app_secret`. Application ownership and quotas
> therefore prevent accidental cross-application room collisions and provide
> accounting isolation, but they are not a hostile-client security boundary.
> Do not embed `app_secret` in shipped clients. A replay-resistant client
> credential design is tracked in issue #250.

## Per-App Settings

Each authorized app has its own limits:

```json

{
  "app_id": "my-game",
  "app_secret": "RESERVED_NOT_USED_BY_CLIENTS",
  "app_name": "My Game",
  "max_rooms": 100,
  "max_players_per_room": 16,
  "rate_limit_per_minute": 60
}

```

- `app_id` - Unique identifier for the app
- `app_secret` - Reserved server-side field; not consumed by the current
  WebSocket client handshake
- `app_name` - Human-readable name (for logging/metrics)
- `max_rooms` - Maximum concurrent persisted rooms owned by this app, counted
  across every game name. Empty rooms continue to count until cleanup removes
  them.
- `max_players_per_room` - Maximum requested capacity for newly created rooms.
  Existing rooms admit at most the lower of the room's stored capacity and the
  app's current limit; lowering the limit does not eject existing players.
- `rate_limit_per_minute` - Max requests per minute for this app (counted per
  `app_id`, across all of its connections), not per IP

## Auth Timeout

Clients must authenticate within the configured timeout:

```json

{
  "websocket": {
    "auth_timeout_secs": 10
  }
}

```

`Authenticate` must be read by the server strictly before this exclusive
deadline. Input that is ready but not observed until the boundary or later is
rejected, and the connection closes with `4001 auth_timeout`.

## Metrics Authentication

Protect the `/metrics` endpoints with a single shared bearer token:

```json

{
  "security": {
    "require_metrics_auth": true,
    "metrics_auth_token": "<generated-token>"
  }
}

```

`metrics_auth_token` is one static token shared by every metrics caller; it is not tied to
any app's `app_id`/`app_secret`. Enabling `require_metrics_auth` without also setting
`metrics_auth_token` is a hard startup error. Generate a strong token, ideally via an
environment variable:

```bash

SIGNAL_FISH__SECURITY__METRICS_AUTH_TOKEN=$(openssl rand -hex 32)

```

When enabled, metrics endpoints require an `Authorization` header:

```bash

curl -H "Authorization: Bearer <your-metrics-token>" \
  http://localhost:3536/metrics

```

Format: `Bearer <metrics_auth_token>` - a single shared static token compared in constant
time, not per-app `app_id:app_secret` credentials.

## Error Codes

Common auth-related errors:

- `AUTHENTICATION_REQUIRED` - Authentication is required but not provided
- `INVALID_APP_ID` - Invalid app ID
- `AUTHENTICATION_TIMEOUT` - Client did not authenticate in time
- `MAX_ROOMS_PER_GAME_EXCEEDED` - App has reached its max rooms limit

When auth is enabled, a room created by an authenticated client is owned by
that app. Another app receives the same `ROOM_NOT_FOUND` result for seated,
spectator, and reconnect admission, so ownership is not disclosed. For legacy
unowned rooms, the first successful authenticated seated admission claims the
room; spectator admission never claims it. Rooms created while WebSocket auth
is disabled remain unowned.

## Example: Multiple Apps

```json

{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "production-game",
        "app_secret": "RESERVED_NOT_USED_BY_CLIENTS",
        "app_name": "Production Game",
        "max_rooms": 1000,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 100
      },
      {
        "app_id": "dev-game",
        "app_secret": "RESERVED_NOT_USED_BY_CLIENTS",
        "app_name": "Development Game",
        "max_rooms": 10,
        "max_players_per_room": 4,
        "rate_limit_per_minute": 20
      }
    ]
  }
}

```

## Security Best Practices

1. **Do not ship server secrets** - A game client needs only its public
   `app_id`; never embed `app_secret`, TURN secrets, or metrics tokens.
2. **Use HTTPS in production** - Protect signaling and public app identity in
   transit.
3. **Monitor rejected app IDs** - Treat spikes as configuration drift or
   probing, while remembering that app IDs are not hostile-client credentials.
4. **Protect active secrets** - Generate and rotate TURN secrets and metrics
   bearer tokens through environment or secret-manager injection.

## Environment Variables

If deployment policy requires supplying the reserved `app_secret` field, keep
it out of the checked-in file:

```bash
# Not recommended - shown for reference only
SIGNAL_FISH__SECURITY__AUTHORIZED_APPS='[{"app_id":"my-game","app_secret":"RESERVED_NOT_USED_BY_CLIENTS",...}]'

```

Better approaches for production secrets management:

### Docker Secrets (Docker Swarm / Compose)

```yaml
# docker-compose.yml
services:
  signal-fish:
    image: ghcr.io/ambiguous-interactive/signal-fish-server:latest
    secrets:

      - signal_fish_config

    entrypoint: sh -c "cp /run/secrets/signal_fish_config /app/config.json && /app/signal-fish-server"

secrets:
  signal_fish_config:
    file: ./config.secret.json

```

### Kubernetes ConfigMap and Secrets

```bash
# Create secret from file
kubectl create secret generic signal-fish-config --from-file=config.json=./config.secret.json
```

```yaml
# deployment.yaml
apiVersion: v1
kind: Deployment
metadata:
  name: signal-fish-server
spec:
  template:
    spec:
      containers:

      - name: signal-fish

        image: ghcr.io/ambiguous-interactive/signal-fish-server:latest
        volumeMounts:

        - name: config

          mountPath: /app/config.json
          subPath: config.json
          readOnly: true
      volumes:

      - name: config

        secret:
          secretName: signal-fish-config

```

### Environment Variable Templating

Generate config.json at runtime from environment variables:

```bash
#!/bin/bash
# entrypoint.sh
cat > /app/config.json <<EOF
{
  "port": ${PORT:-3536},
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "${APP_ID}",
        "app_secret": "RESERVED_NOT_USED_BY_CLIENTS",
        "app_name": "${APP_NAME}",
        "max_rooms": ${MAX_ROOMS:-100},
        "max_players_per_room": ${MAX_PLAYERS:-16},
        "rate_limit_per_minute": ${RATE_LIMIT:-60}
      }
    ]
  }
}
EOF

exec /app/signal-fish-server

```

### AWS Secrets Manager

```bash
# Fetch secret and write to config file
aws secretsmanager get-secret-value \
  --secret-id signal-fish-config \
  --query SecretString \
  --output text > /app/config.json

# Start server
/app/signal-fish-server

```

### HashiCorp Vault

```bash
# Fetch secret from Vault
vault kv get -field=config secret/signal-fish > /app/config.json

# Start server
/app/signal-fish-server

```

## Next Steps

- [Configuration](configuration.md) - Full configuration reference
- [Deployment](deployment.md) - Production deployment guide
