# Changelog

## 0.1.0 — Initial Release

- Core WebSocket signaling server with in-memory state
- Room creation, joining, leaving with room codes
- Lobby state machine (waiting -> countdown -> playing)
- Player ready-state and authority management
- Spectator mode
- Reconnection with token-based event replay
- In-memory rate limiting
- Prometheus-compatible metrics endpoint
- JSON config file + environment variable configuration
- Docker image support
- Optional TLS/mTLS via rustls (feature: tls)
- Optional legacy full-mesh mode (feature: legacy-fullmesh)
