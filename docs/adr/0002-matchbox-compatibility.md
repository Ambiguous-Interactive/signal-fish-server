# Matchbox-Compatible Signal Payloads

## Status

ADR-0002 - Accepted

## Context

Protocol v3 ([ADR-0001](0001-protocol-v3-two-axis.md)) introduces opaque WebRTC
signaling: `ClientMessage::Signal` / `ServerMessage::Signal` carry a `signal`
field that the server routes by `to` / `from` and never parses. The wire shape
of that opaque payload is therefore a _client-to-client_ convention, not a server
contract -- but the protocol should still recommend one shape so reference and
third-party clients interoperate without bespoke adapters.

The upstream `matchbox` project (Johan Helsing) -- which this server was
originally extracted from -- defines a well-known `PeerSignal` shape that both
its native (`webrtc-rs`) and browser (`web-sys` / WASM) `matchbox_socket` clients
already speak:

```text
{ "Offer": "<sdp>" } | { "Answer": "<sdp>" } | { "IceCandidate": "<candidate>" }
```

All three variants carry a single `String`. matchbox's server is itself
payload-agnostic: it forwards `Signal { receiver -> sender, data }`, emits
`IdAssigned` / `NewPeer` / `PeerLeft`, and lets existing peers offer to
newcomers. Our v3 routing model (Appendix E glare rule, `NewPeer`, opaque
forwarding) is deliberately the same shape.

The open decision (PLAN.md Appendix L, item 1): should the **recommended** native
signal payload be matchbox-shaped?

## Decision

**YES.** The recommended convention for the opaque `signal` field is the
matchbox `PeerSignal` shape:

```json
{ "Offer": "<sdp-string>" }
{ "Answer": "<sdp-string>" }
{ "IceCandidate": "<candidate-string>" }
```

Crucially, this is **convention, not coupling**:

- The server stores and forwards the `signal` field as an opaque
  `serde_json::Value`. It **never** deserializes it into a `PeerSignal`, never
  imports a matchbox type, and never validates SDP/ICE.
- The server has **no compile-time or runtime dependency** on `matchbox` for
  signaling. The shape lives only in client code and reference samples.
- A client may put any JSON in `signal`; the server routes it identically. The
  matchbox shape is the documented default so that independent clients interop.

This makes the payload **matchbox-shaped but not matchbox-coupled**.

### Leverage on reference clients (P7)

Choosing the matchbox shape means a `matchbox_socket` client (native via
`webrtc-rs` or browser via `web-sys`/WASM) can talk to this server through a
**thin adapter** -- mapping our `Signal` / `NewPeer` envelope onto matchbox's
`PeerSignal` / `NewPeer` events -- rather than a full from-scratch WebRTC client.
matchbox supports native <-> browser interop, which directly covers the largest
and most expensive line item in the plan (P7: reference clients + cross-platform
interop matrix). The native reference client in P7 can therefore be a
`matchbox_socket` adapter instead of a hand-rolled `webrtc-rs` integration,
substantially cutting that cost.

## Consequences

### Positive

- **Large P7 leverage:** existing matchbox native + browser clients work via a
  thin adapter; mature, interop-tested WebRTC stacks come "for free".
- **No new server dependency:** the server stays zero-dependency for signaling
  and payload-agnostic; opaque routing is unchanged from ADR-0001.
- **Interoperable default:** independent clients that follow the documented shape
  interop without negotiating a private payload format.
- **Future-proof:** because the field is opaque, clients can extend or replace
  the shape later without a server change.

### Negative

- **Convention is unenforced:** the server cannot reject a malformed or
  non-conforming `signal`; conformance is a client/documentation concern (bounded
  by the rate-limit, size cap, and same-room checks from ADR-0001).
- **Soft tie to an upstream shape:** if matchbox changes `PeerSignal`, our
  _recommended_ convention may drift, though server behavior is unaffected.

### Mitigations

- Document the shape in `docs/protocol.md` (v3 additions) and ship canonical
  samples under `.llm/code-samples/protocol/` (P6).
- Keep the server assertion that `signal` is opaque covered by tests (a signal
  with arbitrary JSON must round-trip byte-preserved through the relay).

## Alternatives Considered

### 1. Define a bespoke Signal Fish payload shape

**Rejected:** gains nothing over the matchbox shape and forfeits the ability to
reuse `matchbox_socket` clients, raising P7 cost with no benefit.

### 2. Couple the server to the matchbox `PeerSignal` type (parse and validate)

**Rejected:** violates the ADR-0001 opaque-signal invariant, adds a dependency
and attack surface, and provides no value since clients interpret the payload
regardless.

### 3. Leave the payload entirely unspecified

**Rejected:** without a recommended default, independent clients cannot interop,
and reference clients lose the matchbox adapter shortcut. A documented convention
costs nothing and unlocks interop.

## References

- Internal protocol v3 implementation plan - Appendix A (opaque payload convention), Appendix L (item 1)
- [Protocol v3 Two-Axis (ADR-0001)](0001-protocol-v3-two-axis.md) - opaque-signal invariant
- [Matchbox](https://github.com/johanhelsing/matchbox) - `PeerSignal` shape and payload-agnostic signaling
