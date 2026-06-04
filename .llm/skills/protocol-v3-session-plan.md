# Skill: Protocol v3 Session-Plan Selection

<!--
  trigger: session plan, topology, transport, choose_session_plan, NewPeer, SessionPlan, late join, ICE servers
  | Keep protocol-v3 topology/transport selection correct: desired-as-ceiling ladder, legal pairs, transport-gated signaling
  | Core
-->

**Trigger**: When editing `server/session_policy.rs`, `server/signaling.rs`, or any
code that reads a `SessionPlanDecision` / `SessionPlanPayload`.

---

## The selection ladder (`choose_session_plan`)

The room-wide plan is chosen from the intersection of every member's negotiated
capabilities (ADR-0001 §1). The `desired` topology — the per-game override, else
`default_topology` — is a **ceiling, not an exact match**:

- The room settles on the richest `UPGRADE_LADDER` rung whose topology is no richer
  than `desired`, whose transport is enabled, and which **every** member supports.
- A rung fails if any single member lacks its topology/transport (or the transport
  is disabled); the walk then continues to the next rung.
- The universal `RELAY_FLOOR` is reached only when no rung fits.

Richest-first, that is:

1. `Mesh + WebRtc` — `desired == Mesh`, `enable_webrtc`, all support mesh+webrtc.
2. `Host + WebRtc` — `desired ∈ {Mesh, Host}`, `enable_webrtc`, all support host+webrtc.
3. `Host + Direct` — `desired ∈ {Mesh, Host}`, `enable_direct`, all support host+direct (LAN).
4. `Relay + Relay` — the universal floor (always available).

So a `Mesh`-preferring room that cannot run mesh **falls back to a host topology
before the relay floor** — it never collapses straight to relay. Do not re-gate the
host rungs on `desired == Host`; that is the bug this data-driven ladder replaced.

## Only four `(topology, transport)` pairs are legal

The three `UPGRADE_LADDER` rungs plus `(Relay, Relay)`. `is_valid_pair` is the
single source of truth, backed by a `debug_assert!` in `choose_session_plan` and
the exhaustive `selection_only_ever_yields_a_legal_pair` test. Never hand-build a
`SessionPlanDecision` with any other combination (for example `Mesh + Direct` or
`Host + Relay`). `Relay` topology and `Relay` transport always coincide.

## Gate WebRTC signaling on transport, never topology

`Signal` and `NewPeer` are WebRTC-signaling control messages. Decisions to emit
them must gate on the **transport**, via `SessionPlanDecision::uses_webrtc_signaling`
(`transport == WebRtc`) — never on topology alone. `Host + Direct` is a non-relay
_topology_ whose transport is **not** WebRTC, so keying off topology would wrongly
start WebRTC negotiation for a LAN session.

- `handle_webrtc_late_join` checks `uses_webrtc_signaling()` before shaping pairing
  by topology (mesh pairs all peers; host pairs clients with the host only).
- `emit_session_plan` advertises ICE servers only for a WebRTC transport, and skips
  emission entirely on the relay floor (`is_relay()`).

Prefer the `is_relay()` / `uses_webrtc_signaling()` accessors over ad-hoc
`topology ==` comparisons at every decision point.

## Config validation (`SessionConfig::validate`)

Each `session.ice_servers[*]` is propagated verbatim to clients, so `urls` must be
non-empty and every entry non-blank — a blank or whitespace-only URL is rejected
even alongside valid ones (it breaks client-side `RTCIceServer` parsing). Error
messages point at the offending `ice_servers[i].urls[j]` index.
