# Worked Protocol Scenarios

This section walks through complete, turn-by-turn protocol sessions. Each page is a numbered narrative: a
plain-English intent, the exact JSON message a client sends, the exact JSON response(s) the server returns, and
what each side does next.

These scenarios complement the [Protocol Reference](../protocol.md) (which documents every message shape in
isolation) and the canonical wire samples in
[`.llm/code-samples/protocol/`](https://github.com/Ambiguous-Interactive/signal-fish-server/tree/main/.llm/code-samples/protocol).
The reference tells you what a `SessionPlan` looks like; these pages show you when one arrives and what to do with
it.

## How to read these pages

- All identifiers are realistic but obvious placeholders. Player and room UUIDs use the
  `00000000-0000-0000-0000-00000000000a` form; room codes are short strings such as `ABC123`.
- Wire shapes are byte-accurate to the real protocol. The outer envelope is always
  `{"type": "...", "data": {...}}` (a message with no payload, such as `PlayerReady`, omits `data` entirely).
- TURN `username` / `credential` values shown in WebRTC plans are illustrative placeholders, not real HMAC
  credentials.
- Every WebSocket frame is JSON text unless a scenario explicitly notes a MessagePack binary frame.

## Scenario catalog

| Scenario | Version | What it demonstrates |
| --- | --- | --- |
| [Two-player relay](v2-two-player-relay.md) | v2 | The canonical session: authenticate, create/join a room, ready up, exchange `GameData`, leave. |
| [Mesh WebRTC](v3-mesh-webrtc.md) | v3 | Two peers negotiate a mesh + WebRTC plan, signal, connect, then fall back to the relay floor. |
| [Host topology](v3-host-topology.md) | v3 | A star session: every client signals only the elected host. |
| [Host failover](v3-host-failover.md) | v3 | The host departs, the server re-elects, and survivors get a fresh `SessionPlan`. |
| [Reconnection](reconnection.md) | v2 / v3 | A dropped client reconnects with its token and replays missed events (plus the failure case). |
| [Spectator](spectator.md) | v2 / v3 | A read-only observer joins, watches `GameData`, and leaves. |
| [Error handling](error-handling.md) | v2 / v3 | Worked recovery from `ROOM_FULL`, `RATE_LIMIT_EXCEEDED`, `SIGNAL_TOO_LARGE`, and the relay floor. |

## Which version do I need?

Protocol v3 is a purely additive layer over v2. If your client only needs server-relayed `GameData`, the
[v2 two-player relay](v2-two-player-relay.md) flow is everything you need and works unchanged on both the `/v2/ws`
and `/v3/ws` endpoints. Reach for the v3 scenarios only when you want peer-to-peer transports (WebRTC mesh or a
host star). A single v2 (or relay-only) member always forces the whole room to the relay floor, so the v2 flow is
the universal baseline.
