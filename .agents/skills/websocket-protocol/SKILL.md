---
name: websocket-protocol
description: Design, implement, and validate Signal Fish WebSocket protocol, connection, room, peer, and session-plan behavior. Use for wire messages, protocol versions, serde shapes, topology or transport selection, connection lifecycle, broadcast behavior, compatibility, canonical JSONL samples, or protocol scenarios.
---

<!-- markdownlint-disable MD013 -->

# WebSocket Protocol

Treat serialized messages and canonical samples as compatibility contracts. Trace both client-to-server and server-to-client behavior before changing a wire shape.

## Workflow

1. Read [protocol-and-scenarios.md](references/protocol-and-scenarios.md) for current protocol flows and common scenarios.
2. Read [WebSocket-protocol-patterns.md](references/websocket-protocol-patterns.md) for lifecycle, message, broadcast, heartbeat, and close behavior.
3. Read [protocol-v3-session-plan.md](references/protocol-v3-session-plan.md) for topology, transport, late-join, or `SessionPlan` behavior.
4. Update the canonical [v2 client](references/v2-client-messages.jsonl), [v2 server](references/v2-server-messages.jsonl), [v3 client](references/v3-client-messages.jsonl), and [v3 server](references/v3-server-messages.jsonl) samples when wire examples change.
5. Update protocol implementation, client consumers, documentation, fuzz seeds, and drift tests as one atomic change.
6. Invoke `$web-service-security` for identity or reconnect semantics and `$testing-rust` for scenario coverage.
