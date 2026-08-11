# Deployment

Guide for deploying Signal Fish Server in production.

## Docker Deployment

### Pull and Run

```bash
docker pull ghcr.io/ambiguous-interactive/signal-fish-server:latest

docker run -d \
  --name signal-fish \
  -p 3536:3536 \
  -v ./config.json:/app/config.json:ro \
  ghcr.io/ambiguous-interactive/signal-fish-server:latest

```

### Custom Config

Mount your config file:

```bash

docker run -d \
  -p 3536:3536 \
  -v ./config.json:/app/config.json:ro \
  -v ./logs:/app/logs \
  ghcr.io/ambiguous-interactive/signal-fish-server:latest

```

### Environment Variables

```bash

docker run -d \
  -p 3536:3536 \
  -e SIGNAL_FISH__PORT=8080 \
  -e SIGNAL_FISH__SERVER__DEFAULT_MAX_PLAYERS=16 \
  -e SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=true \
  -e 'SIGNAL_FISH__SECURITY__ALLOWED_APPS=[{"app_id":"my-game","app_name":"My Game"}]' \
  ghcr.io/ambiguous-interactive/signal-fish-server:latest

```

## Docker Compose

```yaml

services:
  signal-fish:
    image: ghcr.io/ambiguous-interactive/signal-fish-server:latest
    ports:

      - "3536:3536"

    volumes:

      - ./config.json:/app/config.json:ro
      - ./logs:/app/logs

    environment:

      - RUST_LOG=info
      - SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=true
      - 'SIGNAL_FISH__SECURITY__ALLOWED_APPS=[{"app_id":"my-game","app_name":"My Game"}]'

    restart: unless-stopped
    healthcheck:
      test:
        - CMD-SHELL
        - >-
          curl -fsS --max-time 2 http://localhost:3536/v2/health ||
          curl -fkSs --max-time 2
          --config "$${SF_HEALTHCHECK_CURL_CONFIG:-/dev/null}"
          https://localhost:3536/v2/health
      interval: 30s
      timeout: 10s
      retries: 3

```

The image health check supports both the default HTTP listener and built-in
HTTPS. When `client_auth` is `require`, mount a curl config readable by the
container user and set `SF_HEALTHCHECK_CURL_CONFIG` to its path. The
config must name the health probe's client certificate and key, for example:

```text
cert = "/run/secrets/health-client.pem"
key = "/run/secrets/health-client-key.pem"
```

### TURN Relay Profile

For WebRTC sessions (Protocol v3), the repository's `docker-compose.yml` includes
an optional coturn service behind the `turn` compose profile:

```bash
export TURN_STATIC_AUTH_SECRET="$(openssl rand -hex 32)"
docker compose --profile turn up -d
```

A plain `docker compose up` is unchanged. See the
[TURN deployment guide](deployment-turn.md) for the matching server `[turn]`
configuration, the ephemeral credential scheme, secret rotation, and why TURN is
self-hosted only.

## Production Configuration

```json

{
  "port": 3536,
  "server": {
    "default_max_players": 8,
    "ping_timeout": 30,
    "room_cleanup_interval": 60,
    "max_rooms_per_game": 1000,
    "empty_room_timeout": 180,
    "inactive_room_timeout": 1800,
    "reconnection_window": 300,
    "enable_reconnection": true
  },
  "rate_limit": {
    "max_room_creations": 5,
    "time_window": 60,
    "max_join_attempts": 20,
    "max_signals": 600,
    "max_signal_errors": 60
  },
  "logging": {
    "dir": "logs",
    "enable_file_logging": true,
    "rotation": "daily",
    "format": "json"
  },
  "security": {
    "cors_origins": "https://yourgame.com",
    "enforce_app_id_allowlist": true,
    "allowed_apps": [{"app_id": "my-game", "app_name": "My Game"}],
    "max_message_size": 65536,
    "max_connections_per_ip": 24
  },
  "websocket": {
    "enable_batching": true,
    "batch_size": 10,
    "batch_interval_ms": 16
  }
}

```

## Reverse Proxy Setup

### nginx

```nginx

upstream signal_fish {
    server 127.0.0.1:3536;
}

server {
    listen 443 ssl http2;
    server_name signal.yourgame.com;

    ssl_certificate /path/to/cert.pem;
    ssl_certificate_key /path/to/key.pem;

    location ~ ^/(v2|v3)/ws$ {
        proxy_pass http://signal_fish;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket timeouts
        proxy_read_timeout 86400s;
        proxy_send_timeout 86400s;
    }

    location /v2/health {
        proxy_pass http://signal_fish;
        proxy_set_header Host $host;
    }

    location /metrics {
        proxy_pass http://signal_fish;
        proxy_set_header Host $host;

        # Optional: restrict metrics access
        allow 10.0.0.0/8;
        deny all;
    }
}

```

### Caddy

```text

signal.yourgame.com {
    @websocket path /v2/ws /v3/ws
    reverse_proxy @websocket localhost:3536 {
        header_up X-Real-IP {remote_host}
    }

    reverse_proxy /v2/health localhost:3536
    reverse_proxy /metrics localhost:3536
}

```

## Cloud Providers

### AWS (ECS Fargate)

```json

{
  "family": "signal-fish-server",
  "networkMode": "awsvpc",
  "requiresCompatibilities": ["FARGATE"],
  "cpu": "256",
  "memory": "512",
  "containerDefinitions": [
    {
      "name": "signal-fish",
      "image": "ghcr.io/ambiguous-interactive/signal-fish-server:latest",
      "portMappings": [
        {
          "containerPort": 3536,
          "protocol": "tcp"
        }
      ],
      "environment": [
        {
          "name": "SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST",
          "value": "true"
        },
        {
          "name": "SIGNAL_FISH__SECURITY__ALLOWED_APPS",
          "value": "[{\"app_id\":\"my-game\",\"app_name\":\"My Game\"}]"
        }
      ],
      "logConfiguration": {
        "logDriver": "awslogs",
        "options": {
          "awslogs-group": "/ecs/signal-fish",
          "awslogs-region": "us-east-1",
          "awslogs-stream-prefix": "ecs"
        }
      }
    }
  ]
}

```

### Google Cloud Run

```bash

signal_fish_env_vars='^@^SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=true'
signal_fish_env_vars+='@SIGNAL_FISH__SECURITY__ALLOWED_APPS='
signal_fish_env_vars+='[{"app_id":"my-game","app_name":"My Game"}]'

gcloud run deploy signal-fish \
  --image ghcr.io/ambiguous-interactive/signal-fish-server:latest \
  --platform managed \
  --region us-central1 \
  --port 3536 \
  --set-env-vars "$signal_fish_env_vars" \
  --allow-unauthenticated \
  --max-instances 10

```

### Kubernetes

```yaml

apiVersion: apps/v1
kind: Deployment
metadata:
  name: signal-fish-server
spec:
  replicas: 3
  selector:
    matchLabels:
      app: signal-fish
  template:
    metadata:
      labels:
        app: signal-fish
    spec:
      containers:

      - name: signal-fish

        image: ghcr.io/ambiguous-interactive/signal-fish-server:latest
        ports:

        - containerPort: 3536

        env:

        - name: SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST

          value: "true"
        - name: SIGNAL_FISH__SECURITY__ALLOWED_APPS

          value: '[{"app_id":"my-game","app_name":"My Game"}]'
        volumeMounts:

        - name: config

          mountPath: /app/config.json
          subPath: config.json
        livenessProbe:
          httpGet:
            path: /v2/health
            port: 3536
          initialDelaySeconds: 10
          periodSeconds: 30
        readinessProbe:
          httpGet:
            path: /v2/health
            port: 3536
          initialDelaySeconds: 5
          periodSeconds: 10
      volumes:

      - name: config

        configMap:
          name: signal-fish-config
---
apiVersion: v1
kind: Service
metadata:
  name: signal-fish-service
spec:
  selector:
    app: signal-fish
  ports:

  - protocol: TCP

    port: 80
    targetPort: 3536
  type: LoadBalancer

```

## Monitoring

### Health Checks

```bash

curl http://localhost:3536/v2/health

```

Returns `200 OK` when healthy.

### Metrics

JSON metrics:

```bash

curl http://localhost:3536/metrics

```

Prometheus metrics:

```bash

curl http://localhost:3536/metrics/prom

```

Protocol-v3 delivery outcomes are exported as
`signal_fish_websocket_delivery_class_outcomes_total{class,outcome}`. The
corresponding raw JSON snapshot is available with
`/metrics?includeSnapshot=true` at
`metricsSnapshot.connections.delivery_by_class`. At quiescence, each class's
`attempted` count equals the sum of its terminal outcomes; sustained
`abandoned`, `dropped_full`, or `unsupported_format` values deserve operator
attention, while `superseded` and `dropped` may be intentional class policy.

### Prometheus Configuration

```yaml

scrape_configs:

  - job_name: 'signal-fish'

    scrape_interval: 15s
    static_configs:

      - targets: ['signal-fish:3536']

    metrics_path: /metrics/prom

```

## Scaling Considerations

Signal Fish Server's supported topology is one active, in-memory process per
routing domain. A room, its WebSocket routes, reconnect records, and relay
counters all live on that process; losing it loses the room. Generic load-
balancer stickiness is not enough because the room code arrives after the
WebSocket upgrade.

Scale the process vertically, or place an application-owned room directory in
front of separate deployments so it selects the room home **before** clients
connect. Do not put interchangeable active processes behind a round-robin or
cookie-sticky load balancer: a misrouted join can silently create a second room
with the same public code.

See the
[single-instance deployment contract](architecture/single-instance-deployment.md)
for the proven two-process failure catalog and drain procedure, and the
[scaling architecture notes](architecture/scaling.md) for capacity drivers and
future extension seams.

## Resource Requirements

Typical resource usage per instance:

- **CPU**: 0.25-0.5 cores (idle), 1-2 cores (active)
- **Memory**: 128-512 MB (depends on room count)
- **Network**: Low bandwidth (WebSocket messages are small)

Scale based on:

- Active rooms per instance (recommend < 500)
- Active players per instance (recommend < 2000)
- Messages per second (recommend < 10000)

## Logging

Set log level:

```bash
RUST_LOG=info cargo run

```

Levels: `trace`, `debug`, `info`, `warn`, `error`

Enable file logging:

```json

{
  "logging": {
    "enable_file_logging": true,
    "dir": "logs",
    "filename": "server.log",
    "rotation": "daily",
    "format": "json"
  }
}

```

## Security Checklist

- [ ] Enable app-ID allowlisting (`enforce_app_id_allowlist: true`) where per-app
      admission policy is needed
- [ ] Treat app IDs as public labels; deprecated `app_secret` input is discarded
      and is not a client credential
- [ ] Configure CORS origins (not `*`)
- [ ] Serve signaling over `wss://` (TLS) — mandatory for WebRTC: DTLS fingerprints travel in the SDP through
      the signaling channel, so plaintext `ws://` lets an on-path attacker defeat WebRTC encryption (see the
      [TURN deployment guide](deployment-turn.md#signaling-must-run-over-wss))
- [ ] Set rate limits appropriately
- [ ] Limit max_connections_per_ip
- [ ] Enable metrics authentication
- [ ] Use a reverse proxy (nginx/Caddy)
- [ ] Monitor rejected public app-ID attempts
- [ ] Rotate TURN and metrics secrets regularly

## Next Steps

- [Configuration](configuration.md) - Full configuration reference
- [Application identification](authentication.md) - Public app-ID trust boundary
- [TURN Deployment](deployment-turn.md) - TURN relay for WebRTC sessions
- [Scaling Architecture](architecture/scaling.md) - Single-process capacity and
  externally routed isolated deployments
