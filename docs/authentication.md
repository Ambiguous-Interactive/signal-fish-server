# Authentication

Authentication is **disabled by default**. Enable it to secure your server and enforce per-app rate limits.

## Enabling Authentication

Set `require_websocket_auth` to `true` and add authorized apps:

```json
{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "my-game",
        "app_secret": "your-secret-here",
        "app_name": "My Game",
        "max_rooms": 100,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 60
      }
    ]
  }
}

```

**Important:** Change the default `app_secret` before deploying to production. The example value
`CHANGE_ME_BEFORE_PRODUCTION` in `config.example.json` is intentionally insecure.

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

  if (message.type === 'Error' && message.data.error_code === 'AUTHENTICATION_REQUIRED') {
    console.error('Authentication failed');
  }
};

```

## Per-App Settings

Each authorized app has its own limits:

```json

{
  "app_id": "my-game",
  "app_secret": "secret-key",
  "app_name": "My Game",
  "max_rooms": 100,
  "max_players_per_room": 16,
  "rate_limit_per_minute": 60
}

```

- `app_id` - Unique identifier for the app
- `app_secret` - Secret key for authentication
- `app_name` - Human-readable name (for logging/metrics)
- `max_rooms` - Maximum concurrent rooms for this app
- `max_players_per_room` - Max players per room for this app
- `rate_limit_per_minute` - Max requests per minute per IP for this app

## Auth Timeout

Clients must authenticate within the configured timeout:

```json

{
  "websocket": {
    "auth_timeout_secs": 10
  }
}

```

If the client doesn't send `Authenticate` within this window, the connection is closed.

## Metrics Authentication

Protect the `/metrics` endpoints:

```json

{
  "security": {
    "require_metrics_auth": true
  }
}

```

When enabled, metrics endpoints require an `Authorization` header:

```bash

curl -H "Authorization: Bearer my-game:your-secret-here" \
  http://localhost:3536/metrics

```

Format: `Bearer <app_id>:<app_secret>`

## Error Codes

Common auth-related errors:

- `AUTHENTICATION_REQUIRED` - Authentication is required but not provided
- `INVALID_APP_ID` - Invalid app ID
- `AUTHENTICATION_TIMEOUT` - Client did not authenticate in time
- `MAX_ROOMS_PER_GAME_EXCEEDED` - App has reached its max rooms limit

## Example: Multiple Apps

```json

{
  "security": {
    "require_websocket_auth": true,
    "authorized_apps": [
      {
        "app_id": "production-game",
        "app_secret": "prod-secret-key",
        "app_name": "Production Game",
        "max_rooms": 1000,
        "max_players_per_room": 16,
        "rate_limit_per_minute": 100
      },
      {
        "app_id": "dev-game",
        "app_secret": "dev-secret-key",
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

1. **Never commit secrets** - Use environment variables in production
2. **Generate strong secrets** - Use a password generator or `openssl rand -base64 32`
3. **Rotate secrets regularly** - Update secrets periodically
4. **Use HTTPS in production** - Protect credentials in transit
5. **Monitor failed auth attempts** - Watch for brute-force attacks

## Environment Variables

Override app secrets via environment:

```bash
# Not recommended - shown for reference only
SIGNAL_FISH_SECURITY__AUTHORIZED_APPS='[{"app_id":"my-game","app_secret":"env-secret",...}]'

```

Better approaches for production secrets management:

### Docker Secrets (Docker Swarm / Compose)

```yaml
# docker-compose.yml
version: '3.8'
services:
  signal-fish:
    image: ghcr.io/ambiguousinteractive/signal-fish-server:latest
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

        image: ghcr.io/ambiguousinteractive/signal-fish-server:latest
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
        "app_secret": "${APP_SECRET}",
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
