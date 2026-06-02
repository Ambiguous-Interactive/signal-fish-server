# Summary

This PR introduces protocol v3 in a fully backward-compatible way and adds targeted WebRTC signaling for v3 clients.

## What Users Get

- Protocol version and capability negotiation during authentication.
- A new `/v3/ws` endpoint alias (same handler as v2) with v3 defaulting when the client omits a
  version.
- Targeted peer-to-peer signaling via opaque payload relay (`Offer`, `Answer`, `IceCandidate`)
  between peers in the same room.
- Deterministic initiator assignment (`you_initiate`) to avoid offer glare when peers are paired.
- New protocol/config guardrails:
  - `protocol.min_protocol_version` and `protocol.max_protocol_version`
  - `rate_limit.max_signals`
  - Explicit error codes for unsupported transport, cross-room signaling, target-not-found, and
    signal rate limits.

## Compatibility

- Existing v2 clients remain unchanged.
- If clients do not send new v3 fields, behavior stays relay-only v2.
- v2 wire behavior is preserved and explicitly frozen with golden tests.

## Validation

- Added/expanded negotiation and signaling integration/e2e tests.
- Added v2 wire golden snapshot tests (JSON + MessagePack).
- Added ADRs documenting protocol v3 design and matchbox-compatible signal payload decisions.

## Additional Notes

- Includes development environment updates in `.devcontainer` to improve contributor setup consistency.
