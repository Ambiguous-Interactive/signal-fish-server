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

`Signal` and `NewPeer` are WebRTC-signaling control messages. The **per-session**
decision to emit them must gate on the **transport**, via
`SessionPlanDecision::uses_webrtc_signaling` (`transport == WebRtc`) — never on
topology alone. `Host + Direct` is a non-relay _topology_ whose transport is
**not** WebRTC, so keying off topology would wrongly start WebRTC negotiation for
a LAN session. Two further layers sit on top of (and below) that session gate:

- The **per-member** `NewPeer` pairing gate is the FULL session predicate
  (`SessionPlanDecision::recipient_pairable` / `pairable`: v3 + the sticky
  topology AND transport), applied to both ends of every announcement — the same
  rule as plan peer lists and host election. A v3 member with the WebRTC
  transport but without the session's topology is plan-filtered, so it must be
  `NewPeer`-silent too.
- `handle_signal` stays **transport-only** (`supports_webrtc_signaling`: v3 +
  WebRTC transport, both sender and target) — deliberately weaker: it is dumb
  plumbing per Appendix K and must not topology-gate or second-guess which pairs
  the plan brokered.

Per path:

- `handle_active_session_late_join` rehydrates the room's **stored**
  `ActiveSessionPlan` (never a ladder recompute), sends the joiner its tailored
  `SessionPlan` (v3-gated only — an incapable v3 joiner still gets its
  empty-`peers` plan), and announces via `NewPeer` only when
  `uses_webrtc_signaling()` AND the joiner passes `recipient_pairable` (mesh
  announces to all session-capable members; host along the star edge only —
  the announce helpers re-check the predicate per member). The joiner is never
  sent `NewPeer`. If the stored host is invalid (`host_invalid`: missing, or
  seated but unpairable) it first self-heals via `replan_host_session` and skips
  the joiner plan + `NewPeer` (the heal re-plan covers every v3 member, joiner
  included — even a joiner that cannot run the session: the heal is about the
  room).
- `emit_session_plan` advertises ICE servers only for a WebRTC transport, skips
  emission entirely on the relay floor (`is_relay()`), and records the non-relay
  decision in `active_session_plans` (relay removes any stale entry).
- `handle_session_member_departure` re-elects + re-emits to every remaining v3
  member (via the shared `replan_host_session`) whenever the stored host is
  invalid (`ActiveSessionPlan::host_invalid`: absent from the current members,
  or still seated but failing the session predicate after a
  capability-downgrading reconnect) — capability-aware: only v3 members
  supporting the sticky pair are electable (the authority preference passes the
  same filter), and if none qualifies the stored entry is dropped with no
  emission and no `session_replans_emitted` increment. Topology/transport are
  sticky for the session lifetime.

## Peer lists are capability-filtered on both sides (`plan_for`)

Re-issued and late-join member lists can contain members that never negotiated
the session's sticky pair (`add_player_to_room` gates only on fullness — e.g. a
v3 relay-only client can seat-fill a `mesh + webrtc` room).
`SessionPlanDecision::plan_for` — the single peer-list seam every emission path
shares (finalize, failover/heal re-plan, late join / reconnect) — filters
`peers[]` with the same predicate host election uses (v3 + sticky topology +
sticky transport, `SessionMember::supports_session`): a member that did not
negotiate the session's pair still RECEIVES its v3-gated plan, but with an
**empty** `peers` list (`fallback: relay` is its data path; `host` stays as
elected, informational), and capable recipients never see it listed. The
`NewPeer` announce helpers apply this same predicate (via
`recipient_pairable` / `pairable`) to both ends of every announcement, so
clients are never told to attempt WebRTC pairs the plan excludes (or that
`handle_signal` would reject). At finalize the filter is vacuous (`all_support`
gates selection); an elected host always satisfies the predicate (debug-asserted
in `host_peers_for`, structurally unfireable: `host_invalid` re-plans away any
host that stops satisfying it before plans are built).

The two gates are distinct and must not be conflated: `is_relay()` (topology) gates
`SessionPlan` emission — which a `Host + Direct` room _does_ receive — while
`uses_webrtc_signaling()` (transport) gates `NewPeer` / `Signal`, which it does
**not**. Their truth table over the four legal pairs is pinned by the
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
