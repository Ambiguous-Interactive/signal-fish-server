---
name: protocol-v3-session-plan
description: >-
  Apply project guidance for protocol v3 session-plan selection. Use when editing
  `server/session_policy.rs`, `server/signaling.rs`, or any code that reads a
  `SessionPlanDecision` / `SessionPlanPayload`.
---

# Protocol v3 Session-Plan Selection

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
3. `Host + Direct` — `desired ∈ {Mesh, Host}`, `enable_direct`, all support
   host+direct (LAN), and at least one capability-compatible member has a
   validated Direct endpoint from `ProvideConnectionInfo`.
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

`Signal` is WebRTC-signaling plumbing. `handle_signal` stays **transport-only**
(`supports_webrtc_signaling`: v3 + WebRTC transport, both sender and target) and
must not topology-gate or second-guess which pairs the plan brokered. The
session-level ICE/credential decision uses
`SessionPlanDecision::uses_webrtc_signaling` (`transport == WebRtc`), never
topology alone. `Host + Direct` is non-relay but is not WebRTC.

`NewPeer` remains a decodable compatibility shape, but current finalized
membership changes do not emit additive deltas. They refresh the complete
`SessionPlan` for every v3 member so additions and removals share one
authoritative peer-set transition.

Per path:

- `publish_finalized_join_membership` and the reconnect publication rehydrate
  the room's **stored** non-relay `ActiveSessionPlan` (never a ladder recompute),
  or derive Relay/Relay from its absence. They publish a complete tailored plan
  to every current v3 member. An incapable v3 member still gets an empty-peer
  plan; v2 members get none. If the stored host is invalid, the same transaction
  repairs it before publishing.
- Finalization advertises ICE only for a WebRTC transport and emits a plan to
  every v3 member, including an explicit Relay/Relay no-peer reset. Only a
  non-relay decision is stored in `active_session_plans`; relay removes stale
  state and is re-derived when membership refreshes.
- `handle_session_member_departure` re-elects + re-emits to every remaining v3
  member (via the shared `replan_host_session`) whenever the stored host is
  invalid (`ActiveSessionPlan::host_invalid`: absent from the current members,
  or still seated but failing the host predicate after a capability-downgrading
  reconnect or losing a usable Direct endpoint) — capability-aware: only v3
  members supporting the sticky pair are electable, and Direct candidates must
  additionally expose a validated endpoint (the authority preference passes the
  same filter). If none qualifies the stored entry is dropped with no
  emission and no `session_replans_emitted` increment. Topology/transport are
  sticky for the session lifetime.

## Peer lists are capability-filtered on both sides (`plan_for`)

New seat-fills cannot weaken this shape: a joiner into a finalized room that
runs a stored non-relay session is rejected at admission unless it negotiated
v3 plus the sticky topology and transport (`ROOM_SESSION_INCOMPATIBLE`, issue
#421). Re-issued and reconnect member lists can still contain members that
never negotiated the session's sticky pair — an incumbent reconnecting with
downgraded capabilities keeps its seat.
`SessionPlanDecision::plan_for` — the single peer-list seam every emission path
shares (finalize, failover/heal re-plan, late join / reconnect) — filters
`peers[]` with `SessionMember::supports_session` (v3 + sticky topology + sticky
transport): a member that did not
negotiate the session's pair still RECEIVES its v3-gated plan, but with an
**empty** `peers` list (`fallback: relay` is its data path; `host` stays as
elected, informational), and capable recipients never see it listed. The
At finalize the filter is vacuous for non-relay decisions (`all_support`
gates selection); an elected host always satisfies the predicate (debug-asserted
in `host_peers_for`, structurally unfireable: `host_invalid` re-plans away any
host that stops satisfying it before plans are built).

Host election deliberately uses the stricter `SessionMember::can_host`
predicate. For WebRTC this is equivalent to `supports_session`; for Direct it
also requires `direct_endpoint()` to validate the member's self-declared
host/port. The elected endpoint is copied into every Direct
`SessionPlan.direct_endpoint`, revalidated on membership refresh/reconnect, and
replaced on endpoint-aware failover. Do not require non-host Direct clients to
advertise an endpoint, and do not use endpoint readiness to filter peer lists.

The two predicates are distinct and must not be conflated: `is_relay()` decides
whether a plan is sticky room state (relay is explicit on the wire but not
stored), while `uses_webrtc_signaling()` decides whether ICE/signaling is
needed. Their truth table over the four legal pairs is pinned by the
`emission_gates_track_relay_topology_and_webrtc_transport` test.

Prefer the `is_relay()` / `uses_webrtc_signaling()` accessors over ad-hoc
`topology ==` comparisons at every decision point. When documenting these gates in
prose (module docs, CHANGELOG, wire-type docs), **defer to the accessor name**
rather than re-deriving the condition as "non-relay" / "non-WebRTC" — that
independent restatement is exactly what drifted (a "non-relay" plan is not the same
set as a "WebRTC" plan; `Host + Direct` is the discriminator). Likewise, document
`initiate` / `you_initiate` direction per topology: mesh follows the UUID glare rule,
host fixes it (client offers, host answers).

## Config validation (`SessionConfig::validate`)

Each `session.ice_servers[*]` is propagated verbatim to clients, so `urls` must be
non-empty and every entry non-blank — a blank or whitespace-only URL is rejected
even alongside valid ones (it breaks client-side `RTCIceServer` parsing). Error
messages point at the offending `ice_servers[i].urls[j]` index.
