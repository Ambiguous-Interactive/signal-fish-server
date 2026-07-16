---
name: web-service-security
description: Threat-model and harden Signal Fish WebSocket and container security. Use for authentication, authorization, input validation, CSWSH, replay or session hijacking, reconnect tokens, session lifecycle, TLS, secrets, headers, unsafe code, rate limiting, DDoS controls, connection limits, or container hardening.
---

<!-- markdownlint-disable MD013 -->

# Web Service Security

Define assets, trust boundaries, attacker capabilities, and failure modes before selecting controls. Add negative tests that demonstrate the original exploit or abuse path is closed.

## Route the task

- Read [web-service-security-auth.md](references/web-service-security-auth.md) for authentication, authorization, origin, and input validation.
- Read [web-service-security-hardening.md](references/web-service-security-hardening.md) for TLS, secrets, headers, audit behavior, and operational hardening.
- Read [WebSocket-session-hijacking.md](references/websocket-session-hijacking.md) and [WebSocket-session-lifecycle.md](references/websocket-session-lifecycle.md) for identity, replay, reconnect, fixation, expiration, and invalidation.
- Read [ddos-rate-limiting-connections.md](references/ddos-rate-limiting-connections.md) and [ddos-infrastructure-monitoring.md](references/ddos-infrastructure-monitoring.md) for abuse and load controls.
- Read [container-security.md](references/container-security.md) for image and runtime hardening; invoke `$deployment-operations` for deployment mechanics.

Avoid claiming infrastructure protection from application-only controls. State residual risks and external dependencies explicitly.
