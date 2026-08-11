# PLAN.md — Protocol v3: Cross-Platform P2P + WebRTC (Backward-Compatible)

> Action plan for evolving Signal Fish Server from a WebSocket **relay** into a
> capability-negotiated **signaling + relay** server that supports true
> peer-to-peer (WebRTC) connections across browser, native (Linux/Windows/macOS),
> mobile, and Steam — while keeping every existing v2 client working unchanged.
>
> Status: ACTIVE. **All in-repo work through P52, P54–P55, and P57–P82 is complete;
> P53 is collecting
> hosted evidence, P56 is validating the fix for a
> recurring H14 hosted control failure, and P66 preserved and published the
> reviewed 0.6.0 source after `main` advanced beyond the prepared commit. P53
> has four of 20 eligible scheduled
> allocations per OS; P56 has five of 20 eligible scheduled H14 attempts.** The M1
> server core, the M2
> production-P2P server work, the M3 protocol documentation, both reference
> clients, the full P10 bulletproofing campaign, the P11 release-integrity gate,
> the P12/P13 Fortress interop gates, P14 dependency hygiene, P15 portable Agent
> Skills, P16 measurement integrity, and the P17–P19 accountable-delivery
> follow-through, the P20 relay allocation baseline, P21 relay wire-frame
> reuse, P22 standalone dependency reproducibility, P23 consistency /
> durability contract with its bounded disconnect/outage-exposure proof, and
> P24's synchronous healthy-relay fast path and P25's strict control-delivery
> deadline sweep plus Docker healthcheck audit correction have landed. P26's
> strict WebSocket I/O deadline semantics are complete in reviewed PR #235.
> P27 repaired the adjacent rate-limit accounting and observability contract
> under issue #236 in reviewed PR #237. P28 closed the release-preparation
> package-graph gap exposed by the 0.5.2 release PR: every tracked standalone
> lockfile that embeds the server is now prepared and validated. P29 eliminated
> binary-envelope buffer growth while preserving exact wire output. P30 fixes
> spectator/room-GC lifecycle coherence, and P31 proves the production-minted
> TURN credential path through a pinned local coturn with a relay-only positive
> control and mismatched-secret WebSocket-fallback control. See the phase table in
> §3 for per-phase status and `progress/session-NNN-*.md` for the per-session
> record — that is the canonical history, and it is deliberately not duplicated
> here.
>
> **Milestone work remaining out-of-repo:** the mobile/Steam platform cells of
> the P7 matrix, and operating the self-hosted coturn infrastructure (the rest
> of P8).
>
> **Open follow-ups** (tracked as issues, not phases): #204 design-system
> assets, #205 broader safety work, #206
> distributed-resilience research, #207 further measured optimization, #213
> additional static analyzers, #220
> formal-verification work beyond the bounded exposure theorem and #318's
> remaining cross-platform developer-hook latency evidence. #268's
> intentional WebRTC 0.20 migration and coordinated Rust 1.91 MSRV raise is P46;
> #271's cross-platform and IPv6 native-client coverage, the residual risk that
> migration recorded, is P47. #276's routable ICE bind selection — the
> intermittent TURN-only lane failure — and the advertised-candidate half of
> #275 are P48. P49 closed the production half of #274: what that issue
> recorded as a macOS timing flake was a real teardown accountability defect, a
> closing connection writing the queue behind a socket write abandoned in
> flight. P49's diagnostics and measured reachability gate then closed #276.
> P50 takes #275's remaining gap by running a complete two-peer live WebRTC
> exchange on Windows and macOS, not only compiling the client there. #274's
> reconnect/teardown-guard failure predates P39's explicit teardown join in
> commit `263be4e`; the failing merge did not contain the join, so that semantic
> item is already resolved. #274 remains open only for hosted-platform timing
> thresholds and PR-lane placement. P51 closes #281 without weakening H14's
> workload: both throttled recipients now preserve terminal observations, and
> every RED run prints immutable proxy-throughput and pump diagnostics before
> asserting the original accountability and amplification oracles.
> P52 closes #283 by rejecting any automatic room-code configuration that can
> generate a code the join path refuses. P53 starts #274's remaining hosted
> timing evidence acquisition without weakening semantic relay oracles; its
> pre-registered decision remains open until the scheduled three-platform
> sample reaches the required size. P54 addresses #284 by retaining automatic
> creation intent and retrying generated-code collisions within a bounded
> budget. P55 resolves #250 by defining the deployed application boundary as a
> public app-ID allowlist, removing the unused client-secret surface, and
> retaining legacy config and frozen v2/v3 wire compatibility.
> P56 tracks #290 after P55's hosted validation produced the second intermittent
> H14 eviction of the equally throttled compatible control. P51's diagnostics
> falsified proxy under-delivery and the historical #212 amplification signature;
> the strong workload and production deadlines remain unchanged while the
> queue-progress boundary was investigated and repaired deterministically; its
> hosted evidence distribution remains under validation.
> P57 adds a non-vacuous exhaustive model of that boundary: continuous
> pre-deadline capacity survives scheduler delay, refills invalidate stale
> evidence, and deadline validation plus admission remain atomic.
> P73 continues the proof after admission: a classified control permit remains
> an exact reservation and producer capability until commit or release, cannot
> be overtaken by terminal EOF while still committable, and cannot cross a
> concurrent room-generation transition without commit-time scope validation.
> P74 composes that permit lifecycle at the production transaction boundary:
> two recipients and two ordered phases must reserve completely before a
> durable hook, revalidate exact routing, account every post-hook degradation,
> and invoke the phase-zero decision callback exactly once.
> P75 closes the remaining production-shape boundary in that composition:
> sparse and zero-frame member batches retain exact routing identity, and a
> stale pre-hook reservation releases the complete partial attempt and retries
> without consuming either one-shot callback.
> P76 proves that one room-event mutation guard transfers into one owned job and
> remains held through its terminal result, including error, panic, and caller
> cancellation. P77 closes the adjacent production routing-isolation gap: joins,
> reconnect baselines, replay hooks, and exact publication transactions now hold
> only a stable room-scoped routing fence across asynchronous work, so one room
> cannot stop unrelated rooms from routing or relaying.
> P78 closes the recurring native selected-path evidence race exposed again by
> PR #332: gameplay exchange no longer waits behind a fixed diagnostic window,
> while clean success retains and retries each connected physical link's exact
> selected ICE-pair obligation until evidence arrives or the existing run
> deadline fails closed. The session also incorporates PR #332's grouped GitHub
> Actions dependency refresh into the same delivery branch.
> P79 addresses the numeric-conversion safety gap found under #213: extreme
> reconnection windows no longer wrap into already-expired tokens, percentile
> ranks no longer round-trip through floating point, and all root-package
> production plus test targets reject casts that can truncate, wrap, or lose a
> sign. PR #336's complete hosted matrix closes the phase.
> P80 advances #318 by classifying the Rust panic fast gate from one
> zero-context diff: added panic macros and removed test guards retain the full
> line-aware scan, while safe added guards and removed macros avoid loading it.
> Worktree status and the Rust diff start concurrently with policy setup; the
> final repeated worktree and staged profiles are below the mandatory
> one-second budget without weakening fail-closed handling or adding hook
> dependencies.
> P81 closes #337 after the same direct peer-reflexive ICE shape recurred on
> both Windows and macOS. The family-agnostic no-STUN/no-TURN baseline accepts
> it only with both clients' host-only UDP candidate corroboration; IPv6 and
> TURN remain exact and gameplay remains independent from diagnostics. The
> phase also incorporates PR #341's browser type definitions and repairs the
> Cargo-only dependency classifier that made npm-only maintenance PRs fail the
> changelog gate.
> P61 removes carry-forward contract drift and mock-only performance evidence
> found by the session-103 adversarial sweep. P62 closes #300 by distinguishing
> the server-emitted error-code contract from six legacy Rust variants retained
> only for source/decode compatibility. P65 closes #307 with the fresh 0.6.0
> release preparation from the complete P64 tree. P66 fixes #314 so the
> default-branch publication workflow retains that exact prepared source even
> after later reviewed changes, then closes #312 by publishing and independently
> verifying every 0.6.0 artifact. P63 provided the first closure of #301 with complete native
> selected-pair failure evidence and an artifact-backed Fortress WASM stall
> boundary that rejects repetition without treating one recovered denied
> callback as a correctness failure. P64 closes #309 by advancing both exact
> Fortress compatibility graphs to 0.12.0 and removing the obsolete
> `RUSTSEC-2025-0141` exceptions without weakening either live-gameplay gate.
> P58 closes nine defects an exploratory audit of the room lifecycle, the
> classified outbound queue, and the v3 signaling surface found: a stale
> authority flag restored by reconnect, a departure that never announced the
> authority it cleared, readiness resurrected by a rejoin, a `Latest` key
> starved whenever supersession outpaced the batch interval, an omission whose
> exact range a cancelled write could drop, and TURN credentials minted for —
> and undercounted for — recipients that cannot pair. Its class sweep and review
> rounds added three more: room snapshots reading readiness from the wrong side
> of finalization, a member reconnecting into a running game reported unready,
> and a replayed authority event contradicting the snapshot that carried it.
> #260's
> room-generation-scoped transport-status deduplication was completed as P43 in
> session 086 / PR #263; #261's exact AsyncAPI v2/v3 accountability envelopes
> were completed as P44 in session 087 / PR #267. #255's production
> zero-panic policy was completed as P45 in session 088 / PR #269.
> Sessions 062
> and 066–070 advanced
> the broad #205/#207 campaigns without claiming that every future safety or
> hosted-path optimization is exhausted. #210's scoped
> CAP/resilience contract for #206 and one concrete #220 increment were
> completed together in session 069; this does not close #220's open-ended
> scope. #225's
> standalone-package dependency automation and reproducible fuzz graph were
> completed in session 068. #222's
> end-to-end relay serialization measurement and
> compatible-recipient reuse were completed in session 067. #211's coordinator
> allocation baseline and first measured optimization were
> completed in session 066. #217 was
> resolved in session 065: `4002 slow_consumer` remains the bounded delivery
> outcome for both stalls and sustained reliable oversubscription, and
> successful outbound writes now prevent a false Pong timeout while a
> constrained recipient is still progressing. #218, advancing the report
> counter frontier on write rather than on pop, was completed in session 064 /
> PR #219. Both were opened in session 063 from the #212 work. #212 (the H14
> slow-consumer eviction) was root-caused in session 063 and turned out to be a
> **production defect** — unsupported-format accountability cost the weakest
> recipient 5.4x the bytes of the payload it replaced — fixed by coalescing those
> reports. #209 (LeakSanitizer) was root-caused and fixed in session 062 — a
> detached `axum::serve` task held the last `Arc<EnhancedGameServer>` past test
> end. #226's multiline Docker healthcheck audit parser is completed in session
> 071. #204 (design-system migration) is front-end asset work off the gameplay path.
>
> Target protocol version: **v3** (additive over v2); P10 targets
> **v3** (mutable pre-release).

---

## 0. Goal & shape of the solution

Today the v2 protocol is a **server relay**: `GameData` flows client → server →
clients (`src/server/game_data.rs`, `broadcast_to_room_except`). The
`relay_type: "WebRTC"` strings are cosmetic labels (`src/server/relay_policy.rs`,
`should_use_relay()` always returns `false`). A real handoff _seam_ exists — the
lobby finalization barrier emits `GameStarting` with each peer's self-declared
`ConnectionInfo` and an `is_authority` host flag
(`src/coordination/room_coordinator.rs:354-377`) — but it only establishes
`ConnectionInfo::Direct{host,port}` (LAN / routable), never negotiates WebRTC, and
never stops relaying.

**v3 adds two independent, negotiated axes** and keeps relay as the universal floor:

```text
Axis 1 — data path:   relay (server fan-out)   |  signal (peers carry data)
Axis 2 — topology:    relay-hub | host (star)  |  mesh (everyone-to-everyone)

        ┌───────────────────────────────────────────────┐
upgrade │  mesh   + webrtc   (two browsers / natives)    │  ← opt-in, negotiated
        │  host   + webrtc   (star, NAT-friendly)        │
        │  host   + direct   (LAN / routable host)       │
        ├───────────────────────────────────────────────┤
 floor  │  relay  (WebSocket fan-out)  ← v2, ALWAYS on   │  ← universal, mandatory-capable
        └───────────────────────────────────────────────┘
                every upgrade gracefully falls back to the floor
```

The server **picks the plan per room** from the intersection of what all members
advertise, hands it out at the finalization handoff, brokers the WebRTC handshake
(targeted offer/answer/ICE relay), and always keeps the relay live as the fallback
tier. This is the industry hybrid pattern (P2P primary → TURN relay → app/WebSocket
relay last resort).

**Backward compatibility is achieved by additive, capability-gated versioning** —
not by forking the codebase. A client that does not advertise v3 sees byte-identical
v2 behavior and never receives a v3 message.

### Milestones

- **M1 (core):** v3 negotiation + targeted signal relay + `mesh+webrtc` working
  between two reference clients, with automatic relay fallback. (Phases P0–P3, P7 partial)
  — **server core P0–P3 ✅ done**; **in-repo conformance proven at N≥3 + across a real
  process boundary (session 007)**; **end-to-end demonstration with a real WebRTC
  stack ✅ done native↔native (session 009: `clients/native/` on webrtc-rs — live
  DTLS+SCTP data channels + automatic relay fallback, CI-enforced) and
  browser↔native (session 010: `clients/browser/` on headless Chromium)** —
  **M1 complete**.
- **M2 (production P2P):** `host` topology + STUN/TURN + ephemeral credentials +
  transport metrics. (Phases P4–P5) — **✅ server work done** (ICE/TURN minting +
  transport-status + metrics); TURN _infra/deployment_ is P8.
- **M3 (full matrix):** reference clients + interop test matrix across all target
  platforms; TURN deployment + security + scaling. (Phases P6–P8) — **P6 protocol
  docs ✅ done**; **P8 in-repo work ✅ done (session 008: security hardening,
  TURN deployment surface, scaling notes)**; **both reference clients ✅ done
  (sessions 009 + 010) with the native↔native, browser↔native, and
  browser↔browser matrix cells CI-enforced** — TURN infra _operation_ and the
  out-of-repo mobile/Steam matrix cells remain.

---

## 1. Locked design principles (decide in P0, then treat as invariants)

1. **Additive, capability-gated versioning.** New message variants are added to the
   existing `ClientMessage`/`ServerMessage` enums. The server **must not** emit a
   v3-only message to a connection that did not negotiate v3. v2 stays frozen.
2. **Relay is the floor.** Every client is relay-capable by definition. P2P is an
   opt-in upgrade that degrades back to relay on failure or capability mismatch.
3. **The server is payload-agnostic for signals.** The `signal` field is opaque
   (`serde_json::Value`); the server routes by `to`/`from` and **never parses SDP or
   ICE**. (Mirrors matchbox: server forwards, clients interpret.)
4. **Deterministic glare avoidance.** For each unordered peer pair, exactly one side
   is designated offerer by a stateless rule (lexicographically smaller `PlayerId`
   UUID, or "client offers to host" in star topology). No perfect-negotiation dance.
5. **Signaling integrity.** Signaling MUST run over `wss://` in production (an
   on-path attacker who can rewrite DTLS fingerprints defeats WebRTC encryption).
6. **Same-room enforcement.** A peer may only signal peers in its own room. Every
   relay hop validates room membership server-side.
7. **The signaling server stays zero-dependency.** STUN can be public; TURN is
   bring-your-own (managed or self-hosted coturn). The server only _mints/forwards_
   ICE credentials — it never relays media itself.

---

## 2. Backward-compatibility strategy (the "new protocol version")

### 2.1 Negotiation seam (reuse the `game_data_format` pattern)

`Authenticate` is the first message on the socket (`src/websocket/connection.rs:244`)
and already negotiates `game_data_format` against `supported_game_data_formats()`,
storing the result per-connection via `set_client_game_data_format` /
`client_game_data_format`. **Protocol-version + transport/topology negotiation copies
this exact mechanism.**

- Client adds **optional** fields to `Authenticate` (absent ⇒ pure v2):
  - `protocol_version: Option<u16>` — highest version the client speaks.
  - `supported_transports: Option<Vec<Transport>>` — defaults to `[Relay]`.
  - `supported_topologies: Option<Vec<Topology>>` — defaults to `[Relay]`.
- Server computes `negotiated_version = min(client_max, SERVER_MAX_VERSION)` and
  stores `{version, transports, topologies}` per-connection (new
  `set_client_protocol` / `client_protocol`, analogous to the format setters).
  If that result is below the deployment's configured minimum, authentication
  fails with `UNSUPPORTED_PROTOCOL_VERSION`; the server never raises a client
  above its declared maximum.
- `ProtocolInfo` (already emitted after `Authenticated`,
  `src/websocket/connection.rs:357-381`) advertises
  `protocol_version` (negotiated), `min_protocol_version`, `max_protocol_version`.

### 2.2 The gating invariant (the heart of back-compat)

A single helper gates all v3 emission:

```rust
fn client_supports_v3(&self, player_id: &PlayerId) -> bool   // negotiated_version >= 3
fn client_supports_transport(&self, player_id: &PlayerId, t: Transport) -> bool
```

- `Signal`, `NewPeer`, and `SessionPlan` are sent **only** to connections where
  `client_supports_v3` is true.
- `GameStarting` remains byte-identical and is sent to everyone (v2 + v3).
- A v3 plan is only _chosen_ for a room when **all** members are v3-capable for the
  required transport; otherwise the room is assigned `relay` and behaves like v2.

Result: a v2 client never sends a v3 message and never receives one. Verified by a
golden back-compat test (Appendix K).

### 2.3 Endpoint versioning

- Keep `/v2/ws` mounted and unchanged.
- Primary mechanism is **in-band negotiation** on the same handler (the message
  schema is a superset; gating prevents leakage).
- **Resolved:** `/v2/ws` and the top-level `/v3/ws` alias share one handler.
  Omitted `protocol_version` defaults to 2 and 3 respectively, while an explicit
  client value wins. There is no accidental `/v2/v3/ws` route.

---

## 3. Prioritized phases

Each phase ends with the mandatory workflow from `.llm/context.md`:
`cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features`
(zero warnings), a documented changelog classification (and an Unreleased entry
when user-visible), plus `./scripts/check-doc-consistency.sh`.
Sizes: **S** ≈ 1–2 days, **M** ≈ 3–5 days, **L** ≈ 1–2 weeks, **XL** ≈ multi-week.

| Phase | Title | Size | Depends on | Milestone | Status |
|---|---|---|---|---|---|
| P0 | Design lock & v2 freeze | S | — | M1 | ✅ Done |
| P1 | Version + capability negotiation | M | P0 | M1 | ✅ Done |
| P2 | Targeted signal relay (WebRTC enabler) | M | P1 | M1 | ✅ Done |
| P3 | Session plan / handoff directive + topology selection | M | P2 | M1 | ✅ Done |
| P4 | ICE servers + ephemeral TURN credentials | M | P3 | M2 | ✅ Done |
| P5 | Relay-fallback contract + transport status + metrics | S | P3 | M2 | ✅ Done |
| P6 | Docs, samples, `.llm` context | S | P3 | M3 | ✅ Done |
| P7 | Reference clients + interop test matrix | XL | P2–P4 | M1→M3 | ✅ In-repo done (s009 native, s010 browser); mobile/Steam cells remain |
| P8 | TURN infra + deployment + security + scaling | L | P4 | M3 | ✅ In-repo done (s008); infra ops remain |
| P11 | Git-tagged releases + versioned GHCR containers | S | Release workflows | Release | ✅ Done |
| P12 | Fortress Rollback relay interoperability regression | S | P7, protocol v3 relay | M3 | ✅ Done (s049) |
| P13 | Fortress + Rust-client single-threaded WASM interoperability | M | P12, browser interop harness | M3 | ✅ Done (s056; Chromium cell) |
| P14 | Dependency hygiene and WebSocket stack coherence | S | Current release graph | Maintenance | Done (s060) |
| P15 | Portable Agent Skills library | S | — | Maintenance | ✅ Done (s061) |
| P16 | Measurement integrity and enforced no-unsafe | S | — | Maintenance | ✅ Done (s062); #212 resolved by P17 |
| P17 | Unsupported-format accountability amplification (#212) | S | P5, P7 | Maintenance | ✅ Done (s063); client release → `client-rust#81` |
| P18 | Confirm delivery-report frontier on write (#218) | S | P17 | Maintenance | ✅ Done (s064) |
| P19 | Bounded recipient progress and liveness (#217) | S | P17, P18 | Maintenance | ✅ Done (s065) |
| P20 | Relay allocation baseline and measured optimization (#211) | S | P16 | Maintenance | ✅ Done (s066) |
| P21 | Relay socket serialization measurement and reuse (#222) | S | P20 | Maintenance | ✅ Done (s067) |
| P22 | Standalone dependency automation and reproducible fuzz graph (#225) | S | P14 | Maintenance | ✅ Done (s068) |
| P23 | Single-home consistency contract and one disconnect-exposure proof (#210, #220 increment) | S | P10, P19 | Maintenance | ✅ Done (s069) |
| P24 | Healthy relay synchronous fast path and H2 saturation validation (#207) | S | P20, P21 | Maintenance | ✅ Done (s070) |
| P25 | Strict control-capacity deadlines and Docker healthcheck audit (#226) | S | P19, P24 | Maintenance | ✅ Done (s071) |
| P26 | Strict WebSocket I/O deadline semantics (#233) | S | P19, P25 | Maintenance | ✅ Done (s072) |
| P27 | Rate-limit accounting and observability integrity (#236) | S | P26 | Maintenance | ✅ Done (s073) |
| P28 | Release preparation across standalone package graphs | S | P11, P22 | Release | ✅ Done (s074) |
| P29 | Pre-sized binary relay serialization (#207) | S | P21, P24 | Maintenance | ✅ Done (s075) |
| P30 | Spectator lifecycle and room-GC coherence (#241) | S | P10 | Maintenance | ✅ Done (s076) |
| P31 | Local TURN-only WebRTC interoperability (#239) | S | P4, P7, P8 | M3 | ✅ Done (s076) |
| P32 | Zero-copy v2 raw-binary relay projection (#207) | S | P21, P29 | Maintenance | ✅ Done (s077) |
| P33 | Current pinned nightly analysis baseline (#243) | S | P16, P22 | Maintenance | ✅ Done (s077) |
| P34 | Relay projection allocation and timing integrity (#207) | S | P21, P29, P32 | Maintenance | ✅ Done (s078) |
| P35 | Strict retry delay cap (#205) | S | P10 | Maintenance | ✅ Done (s078) |
| P36 | Historical GitHub Release retry integrity | S | P11 | Release | ✅ Done (s079) |
| P37 | Allocation-free relay builder handoff (#207) | S | P20, P24, P34 | Maintenance | ✅ Done (s080) |
| P38 | Allocation-free routed-recipient traversal (#207) | S | P24, P37 | Maintenance | Done (s081) |
| P39 | Atomic application room ownership and quotas (#249) | M | P23 | Maintenance | Done (s082) |
| P40 | Executable Host + Direct plans (#251) | M | P3, P7 | Maintenance | ✅ Done (s083) |
| P41 | Protocol-v3 edge and specification hardening (#257) | M | P1, P3, P5, P7 | Maintenance | ✅ Done (s084) |
| P42 | Wire-fenced retained WebRTC pair generations (#258) | M | P3, P7, P41 | Maintenance | ✅ Done (s085) |
| P43 | Membership-scoped transport-status deduplication (#260) | S | P5, P30 | Maintenance | ✅ Done |
| P44 | Exact AsyncAPI v2/v3 accountability envelopes (#261) | M | P41, P43 | Maintenance | ✅ Done |
| P45 | Enforced zero-panic production policy (#255) | M | P16 | Maintenance | ✅ Done (s088, PR #269) |
| P46 | Intentional WebRTC 0.20 migration and Rust 1.91 MSRV (#268) | M | P7, P22, P45 | Maintenance | ✅ Done (s089, PR #270) |
| P47 | Cross-platform and IPv6 native client coverage (#271) | S | P7, P46 | Maintenance | ✅ Done (s090) |
| P48 | Routable ICE bind selection and candidate visibility (#276, #275) | S | P31, P46, P47 | Maintenance | ✅ Done (s091) |
| P49 | Teardown write integrity and TURN-lane observability (#274, #276) | S | P17, P18, P26, P48 | Maintenance | ✅ Done (s092) |
| P50 | Live native WebRTC transport on Windows and macOS (#275) | S | P7, P47, P48 | Maintenance | ✅ Done (s093) |
| P51 | H14 terminal and proxy diagnostic integrity (#281) | S | P16, P17, P19 | Maintenance | ✅ Done (s094) |
| P52 | Generated room-code generation/admission closure (#283) | S | P39 | Maintenance | ✅ Done (s095) |
| P53 | Hosted relay timing evidence acquisition (#274) | S | P51 | Maintenance | 🟡 Collecting (s095) |
| P54 | Bounded generated room-code collision retries (#284) | S | P39, P52 | Maintenance | ✅ Done (s096) |
| P55 | Public app-ID trust-boundary closure (#250) | M | P39, P45 | Maintenance | ✅ Done (s097) |
| P56 | H14 compatible-control hosted eviction closure (#290) | S | P51 | Maintenance | 🟡 Validating (s098) |
| P57 | Classified queue capacity-deadline arbitration proof (#220, #290) | S | P56 implementation | Maintenance | ✅ Done (s099) |
| P58 | Membership, delivery, and session-plan state integrity | M | P39, P43, P48, P51 | Maintenance | ✅ Done (s101) |
| P59 | Durable-repair and failure-response contract closure (#296, #297, #298) | S | P55, P58 | Maintenance | ✅ Done (s102) |
| P60 | Release-preparation integrity and dependency incorporation (#302, #305) | S | P11, P28, P59 | Release | ✅ Done (s103) |
| P61 | Carry-forward protocol and evidence integrity (#296, #298) | S | P59, P60 | Maintenance | ✅ Done (s103) |
| P62 | Emitted error-code contract closure (#300) | S | P41, P59, P61 | Maintenance | ✅ Done (s103) |
| P63 | Live-interop diagnostic and stall-gate integrity (#301) | S | P13, P47, P50 | Maintenance | ✅ Done (s104) |
| P64 | Fortress Rollback 0.12 compatibility and supply-chain closure (#309) | S | P12, P13, P63 | Maintenance | ✅ Done (s105) |
| P65 | Signal Fish Server 0.6.0 release preparation (#307) | S | P60, P64 | Release | ✅ Done (s106) |
| P66 | Prepared-source release publication recovery (#314, #312) | S | P65 | Release | ✅ Done (s108) |
| P67 | Compact shared relay frame cache (#207) | S | P21, P34, P38 | Maintenance | ✅ Done (s109) |
| P68 | Native selected-pair and browser-origin integrity (#301, #319) | S | P47, P50, P63 | Maintenance | ✅ Done (s110) |
| P69 | Single-allocation shared relay carrier (#207) | S | P37, P38, P67 | Maintenance | ✅ Done (s111, PR #321) |
| P70 | Pre-sized JSON relay projection (#207) | S | P21, P34, P67 | Maintenance | ✅ Done (s112, PR #322) |
| P71 | Allocation-free healthy relay handoff (#207) | S | P24, P69 | Maintenance | ✅ Done (s113) |
| P72 | Borrowed healthy relay handles (#207) | S | P38, P71 | Maintenance | ✅ Done (s114) |
| P73 | Reserved control-permit lifecycle conservation (#220) | S | P25, P57, P58 | Maintenance | ✅ Done (s115) |
| P74 | Exact room-publication transaction refinement (#220) | M | P32, P57, P58, P73 | Maintenance | ✅ Done (s116) |
| P75 | Sparse room-transaction and pre-hook retry closure (#220) | S | P74 | Maintenance | ✅ Done (s117) |
| P76 | Exact room-event mutation handoff (#220) | S | P73, P75 | Maintenance | ✅ Done (s118) |
| P77 | Room-scoped routing isolation and async lock containment (#220, #329) | M | P74, P75, P76 | Maintenance | ✅ Done (s119) |
| P78 | Selected ICE-pair evidence lifecycle and dependency incorporation (#301, #332) | S | P50, P63, P68 | Maintenance | ✅ Done (s120) |
| P79 | Numeric-conversion safety and lint enforcement (#213) | S | P16, P45 | Maintenance | ✅ Done (s121, PR #336) |
| P80 | Direction-aware Rust panic fast gate (#318) | S | P79 | Maintenance | ✅ Done (s121, PR #336) |
| P81 | Direct peer-reflexive evidence and npm dependency maintenance (#337, #341) | S | P78, P80 | Maintenance | ✅ Done (s122) |
| P82 | Terminal inactive-room cleanup convergence | S | P30, P58, P76 | Maintenance | ✅ Done (s123) |
| P10 | Bulletproofing campaign: falsify → formalize → v3 revision | XL | P9 | v3 | ✅ Done |

---

### P16 — Measurement integrity and enforced no-unsafe (Size S) — ✅ DONE

Session 062 closed the Firefox WASM lane that had never passed, repaired a
suite that had been reporting numbers it never measured, and enforced the
no-unsafe property. One further `main` lane (the H14 experiment) was found red
and is carried to issue #212 rather than claimed fixed.

- [x] Restore the weekly Firefox cell of the P13 WASM interop gate, which had
  failed on every run since session 059 introduced it. **Three** independent
  causes, each masking the next: Firefox blocklists WebGL without an accelerated
  adapter (`webgl.force-enabled`); Playwright's Firefox dependency list installs
  no GL packages at all, unlike its Chromium and WebKit lists (the workflow now
  installs Mesa's rasterizer); and Firefox has no SwiftShader-equivalent, so even
  headless it resolves llvmpipe through an X display (the runner script wraps the
  Firefox harness in `xvfb-run`). The third is the whole environment dependence —
  a workstation with `DISPLAY` set passes while a clean runner fails. A WebGL2
  probe now runs before the export loads and reports the browser's own reason;
  before it, all three failures were indistinguishable
  `page.waitForFunction: Timeout 15000ms exceeded`, which is why the cell sat red
  with no diagnosis. Verified by reproducing the CI condition locally with
  `env -u DISPLAY` (red) and under `xvfb-run` (green), plus exact-head
  `workflow_dispatch` runs.
- [x] Repair `tests/load_tests.rs` (issue #207). Every room-based cell built a
  room code of the wrong length against the server's required six characters,
  and `handle_join_room` reports a rejected join only by leaving the player
  roomless. Two throughput cells sat at 0% success and were excluded from CI
  against an **incorrect** recorded diagnosis (`max_rooms_per_game` plus rate
  limits — the latter is keyed per player and never bound), while the nightly
  latency cell ran green while broadcasting into empty rooms. All three cells
  now measure real work and run in the nightly load lane.
- [x] Enforce the no-unsafe property (issue #205). Every package manifest
  declares an explicit `unsafe_code` policy, discovered and checked via
  `git ls-files` so a package added later cannot opt out by omission.
- [x] Incorporate Dependabot #203 and #202. The compatible refresh and the
  Actions bumps land; `tokio-tungstenite` 0.30, `serial_test` 4.0, `base64`
  0.23, and `syn` 3.0 are each rejected on measured evidence (duplicate
  WebSocket stack, MSRV violation, duplicate base64, and no dedup benefit
  respectively).
- [x] Symbolize sanitizer reports so the next LeakSanitizer occurrence is
  diagnosable; the observed 22-indirect/0-direct flake is tracked as issue #209
  rather than blind-fixed.
- [x] Require every workflow `apt-get update` to drop the Azure CLI and
  Microsoft prod source lists first, enforced by a sweep test over all
  workflows. Five call sites already did this by convention; a sixth did not.

**Acceptance:** `main` has no failing workflow, the nightly load lane measures
non-vacuous work with recorded baselines, and no package can acquire `unsafe`
without editing a manifest. Delivered in PR #208: every applicable workflow is
green (Copilot remains quota-blocked, as in every recent session), Cursor Bugbot
reports no new issues with all four of its findings resolved, and the ASan lane's
LeakSanitizer flake (#209) is root-caused and fixed with 36/36 clean verification
samples.

**Historical carry-forward (closed in P17):** the H14 mixed-encoding experiment
(`unsupported_message_pack_fallback_does_not_flap_weaker_recipient`) evicts its
throttled recipient as a `SlowConsumer` on GitHub runners while passing locally.
This is **pre-existing on `main`** (run 30187497311 at `a8020246`, 2026-07-26)
and reaches PR #208 only through the nightly workflow's `tests/load_tests.rs`
path filter. Two hypotheses were tested and falsified — fault-injector rate
accuracy (fixed anyway; the next run failed earlier) and kernel-buffer
absorption (bounding the proxy receive buffer to 4 KiB changed nothing
locally). Runtime starvation on a 4-core shared runner is the leading remaining
avenue. Tracked with full evidence in issue #212; the assertion now reports
counters, elapsed time, and the server's message so the next failing run is
decisive.

**Follow-ups at session 062:** issues #209 (LSan flake), #210 (scoped #206 CAP
research), #211 (allocation tracking — the unstarted half of #207), and #212
(the H14 flake above). Later sessions closed #209 and #212. Issue #204
(design-system migration) is front-end asset work off the gameplay path.

---

### P17 — Unsupported-format accountability amplification (Size S) — ✅ DONE

Session 063 established that the H14 "flake" (#212) was a production defect and
fixed it. Accounting for a payload a recipient's negotiated encoding cannot
represent emitted one `DeliveryReport` frame per omitted message, so the peer
least able to afford it drained **5.38x** the wire bytes of the compact frames
its room-mates received (2,096,502 against 389,618 for a 5,000-message burst).
Under an equal 32 KiB/s fault that difference evicted it as a slow consumer
after 703 of 5,000 messages while the compatible peer was unaffected.

- [x] Reproduce the CI-only failure deterministically. `taskset -c 0` fails at
  9.0 s against CI's 8.3–8.5 s; the previous session's two-core attempt passes,
  so contention — not link behaviour — is the discriminator.
- [x] Falsify the recorded `SO_SNDBUF` hypothesis: the harness already bounds the
  send buffer to 64 KiB through `bind_serve_listener`
  (`effective_send_buffer_bytes=131072`), which cannot absorb a 2.1 MB burst, and
  the run reports backpressure throughout. The green result was never vacuous.
- [x] Coalesce consecutive omissions from one sender and delivery class into one
  exact range, under the merge rule the queue already applied to its own gap
  reports. Write it before the next delivered frame, with the rate-limited
  advisory, immediately after a queued report, one second after the first
  omission when idle, and after the teardown drain.
- [x] Make that cancellation-safe (Cursor Bugbot): peek/write/commit, so the
  ranges retire and the frontier advances only after the socket send/flush
  succeeds, and build each report on the successful socket-write frontier, so a
  cancelled report write can neither strand ranges nor suppress a flush.
- [x] Restate the experiment's oracles to measure the property rather than the
  defect: accounted sequences, an amplification bound, an advisory ceiling
  anchored to the 1/s limiter, and a non-vacuity check.
- [x] Fix both in-repo reference clients, which required the old single-sequence
  shape and would have rejected the server's own frames (Cursor Bugbot).
- [x] Make the report-shape contract explicit in `docs/protocol.md`: no gap
  reason has a privileged shape.

**Acceptance:** the experiment passes with the fault genuinely applied and the
amplification measured, on the runner and under single-core contention.
Delivered in PR #216. The published client 0.9.0 carries the same over-strict
check and needs a release (`client-rust#81`); the window affects only the error
path that emits these reports.

**Follow-ups:** #217 — whether accountable-progress accounting should distinguish
a stalled recipient from an oversubscribed one, with the queue-geometry
arithmetic that makes the question concrete. #218 is promoted to P18 below.

---

### P18 — Confirm delivery-report frontier on write (Size S) — ✅ DONE

Session 064 removes the compensating two-frontier bookkeeping introduced while
fixing #212. A queued `DeliveryReport` previously advanced `wire_counters` when
the queue popped it, before the socket write. If that write was cancelled or
failed, a teardown report could build on counters whose socket send/flush never
completed.

- [x] Reproduce the defect with a regression assertion: popping a queued report
  advances the successful socket-write frontier before any write occurs.
- [x] Stamp a queued report against the last successfully written frontier
  immediately before the writer sends it, without mutating queue state.
- [x] Advance the frontier only after the socket send/flush succeeds.
- [x] Keep coalesced unsupported-format reports on the same peek/write/commit
  rule and remove `confirmed_wire_counters`; one frontier now means exactly
  "counters successfully written and flushed to the socket."
- [x] Complete the full local validation gauntlet, adversarial review, and green
  PR review/CI loop.

**Acceptance:** no report counter can describe a frame whose socket send/flush
did not succeed, the frontier moves only after that success, and no compensating
second snapshot or conservative refusal branch remains. Delivered in PR #219.

---

### P19 — Bounded recipient progress and liveness (Size S) — ✅ DONE

Session 065 resolved issue #217 with the existing H10/H14 real-socket
experiments and a focused liveness regression. A reliably oversubscribed
recipient is meaningfully progressing, but it still violates the bounded
delivery contract because reliable traffic cannot be dropped or buffered
forever. Close `4002 slow_consumer` therefore remains the stable outcome; no
new close code or negotiation surface is added.

The experiment also exposed a distinct production defect under single-core
contention: application writes continued to complete at 32 KiB/s, but a
WebSocket Ping queued behind that traffic could miss its five-second Pong
deadline and falsely close the progressing recipient with `4003
activity_timeout`.

- [x] Make the H10 termination oracle classify the primary cause and report
  exact delivered/dropped ranges, liveness counters, and proxy pump
  termination.
- [x] Reproduce the false `4003` deterministically with `taskset -c 0`.
- [x] Treat successful outbound application writes as probe progress that
  supersedes its pending Pong deadline while still sending the Ping so
  read-only clients refresh the independent inbound-activity reaper.
- [x] Prove that continuing outbound progress prevents the false timeout and
  that an actually idle recipient is still reclaimed after progress stops.
- [x] Restore the documented `server.ping_timeout=0` behavior: zero disables
  the inbound-activity reaper instead of expiring every client immediately,
  and document that disabling it removes the fixed client-to-server
  half-partition bound while outbound traffic continues.
- [x] Retain the existing bounded `4002` outcome for sustained reliable
  oversubscription and document lane-correct queue geometry, including the
  separate 1,024-slot data and 128-slot control capacities.

**Acceptance:** the single-core H10 run completes two reliable cycles and a
9,475-message volatile cycle with all messages exactly delivered or reported
dropped; no false Pong timeout occurs. The focused liveness regression records
outbound deadline supersession, then observes exactly one timeout after writes
stop.
ADR-0007 records the stable close-code and sizing decision.

---

### P20 — Relay allocation baseline and measured optimization (Size S) — ✅ DONE

Session 066 completed the unstarted allocation half of issue #207, tracked by
GitHub issue #211. The measurement isolates the steady-state protocol-v3 fan-out and
classified outbound queue on a current-thread runtime, after setup and
collection warm-up. Every sample proves the expected delivery-attempt,
successful-enqueue, and receiver-drain ledgers before reporting a number, so a
room-routing regression cannot turn the benchmark into the vacuous workload
that issue #207 exposed in the load suite.

- [x] Add a dev-only instrumented allocator and an opt-in benchmark for 2-, 8-,
  and 16-player rooms, repeated five times with exact deterministic counts.
- [x] Establish the pre-change coordinator fan-out baseline: 6, 7, and 8
  allocation operations per measured call respectively. The separately
  measured warmed classified reliable queue performs zero allocations and
  reallocations. Payload/stamp construction and the inbound handler's outer
  builder are outside this deliberately narrower measurement boundary.
- [x] Pre-size recipient snapshots from known room membership. The same harness
  holds allocation operations flat at 6 per coordinator fan-out for every
  measured room size and lowers bytes per call from 1,160 to 1,040, 5,160 to
  5,120, and 10,600 to 10,560 respectively.
- [x] Test and reject `SmallVec` for this async-trait path. Although it reduced
  operation counts to 5, 5, and 6, its inline recipient storage enlarged the
  boxed future and increased bytes per call to 1,608, 5,448, and 11,168. The
  measured trade is worse than the pre-sized `Vec`, so it does not land.
- [x] Keep machine-sensitive Criterion timing on demand. P29 later promotes the
  deterministic Linux allocator operation, reallocation, and byte counts to
  the hosted CI lane; the harness still fails on nondeterministic or vacuous
  samples.

**Acceptance:** a reproducible allocation baseline exists for the relay hot
path, the classified queue is independently measured, and the first
optimization carries before/after evidence from the identical non-vacuous
workload. The audit also proved the existing `broadcast.rs` pre-serialization
helpers have no production consumer; #222 tracks measurement through socket
serialization and compatible-recipient reuse rather than overstating this
queue-boundary benchmark. Delivered in session 066.

---

### P21 — Relay socket serialization measurement and reuse (Size S) — ✅ DONE

Session 067 completed issue #222 through a benchmark that continues past
coordinator fan-out and queue dequeue into the exact production frame
materializer immediately before the Axum socket write.

- [x] Measure 1,024 fresh ~1 KiB relays for 2-, 8-, and 16-player rooms across
  v3 JSON text, v3 MessagePack binary, and mixed
  v2/v3 × JSON/MessagePack recipient cohorts.
- [x] Prove every sample non-vacuous from delivery attempts, successful
  enqueues, dequeues, materialized frames, simulated successful-write
  accounting, cohort counts, wire bytes, codec-work counts, empty queues, and a
  fixed output digest.
- [x] Keep allocation and runtime evidence separate: `stats_alloc` supplies
  deterministic five-repeat heap counts, while uninstrumented Criterion
  compares timing against the pre-change `issue222-pre` baseline.
- [x] Bind one lazy relay-frame cache atomically to the logical message through
  queue full/retry and cancellation paths. The cache is created only when exact
  cohort work repeats or multiple fallback cohorts can share a decode; it then
  initializes only the v2/v3 text, direct-binary, or fallback-text cohorts that
  real recipients request.
- [x] Preserve per-recipient delivery accounting, deadlines, unsupported-format
  reporting, queue ordering, and frozen v2/v3 wire bytes. Every post-change
  output digest matches its pre-change counterpart.

**Acceptance:** at 16 players, allocation operations per relay fall from 81 to
12 for JSON text, 111 to 14 for MessagePack binary, and 159 to 40 for mixed
traffic; allocated bytes fall by 59–67%. Criterion reports statistically
significant median time improvements of 30.5%, 12.6%, and 25.9% respectively.
The same three scenarios improve at eight players, while the one-recipient
cells show no statistically significant runtime change. Delivered in session
067.

---

### P22 — Standalone dependency automation and reproducible fuzz graph (Size S) — ✅ DONE

Session 068 completed issue #225 after the scheduled Dependabot job exposed a
stale `/third_party/rmp` update directory that no longer contained a manifest.

- [x] Derive the required Dependabot Cargo directories from every tracked
  package manifest, monitor the root, native reference client, and fuzz
  package, and explicitly exclude only the two exact-release Fortress fixtures
  that move through deliberate interoperability requalification.
- [x] Remove the dead vendored-rmp target and every genuine stale documentation
  claim that the repository still patches that crate. Synthetic regression
  fixtures remain intact.
- [x] Commit a freshly resolved `fuzz/Cargo.lock`, add a Rust 1.89 locked stable
  build plus pinned-nightly locked metadata preflight, and keep the cargo-fuzz
  command unflagged because cargo-fuzz 0.13 rejects `--locked`.
- [x] Apply cargo-deny and cargo-audit CI coverage to the standalone fuzz graph;
  declare its package license/MSRV/path-dependency version and permit
  libFuzzer's OSI-approved NCSA license.
- [x] Extend the MSRV and nested-lockfile guards so a future standalone-package
  drift fails on the always-on policy suite.

**Acceptance:** root, native, and fuzz graphs pass their Rust 1.89 locked
checks and cargo-deny advisory/license/ban/source policy. A compatible refresh
leaves no fuzz-specific hold; root and native retain only their measured
WebSocket-stack, MSRV, codec, and policy-code holds. Delivered in session 068.

---

### P23 — Single-home consistency contract and disconnect-exposure proof (Size S) — ✅ DONE

Session 069 completed issue #210's concrete scope for #206 and supplied one
non-vacuous, bounded-scope quantitative proof toward issue #220.

- [x] State the shipped guarantee per room create/join, gameplay relay, signal,
  reconnect, planned drain, and unexpected process loss: exact local commit,
  connection durability, invalidating fault, and client-visible outcome.
- [x] Accept ADR-0008: one active home process per room remains the product
  boundary. An application-owned directory may shard new rooms before the
  WebSocket upgrade; consistent hashing, leases/fencing, CRDT membership, or
  consensus do not make live rooms portable and no coordination round trip
  enters the relay hot path.
- [x] Derive the additional disconnect/outage exposure cut
  `queue tail + complete client-unobserved post-queue pipeline + accepted
  outage traffic`, excluding delivery-class omissions already accounted before
  the cut. Under the explicit arrival-curve assumption
  `A(T) <= B + ceil(R*T)`, the conditional ceiling is
  `Q + P + B + ceil(R*T)`. Defaults alone provide neither the admission bound
  nor a complete post-queue frame cap.
- [x] Check the corresponding
  `QCAP + PCAP + BURST + RATE * WINDOW` invariant exhaustively in TLA+,
  including a zero-window burst edge. A checked expected-failure configuration
  omits the post-queue term and must report the reachable `7 > 6`
  counterexample in CI.
- [x] Repair the dependency automation exposed after session 068: every
  standalone Dependabot job that traverses the server path dependency inherits
  all root holds, scheduled RustSec scans cover every managed Cargo graph, and
  native deny policy cannot silently drift. The complete webrtc-rs 0.17.2
  family advances together instead of leaving `webrtc-sctp` at an incompatible
  0.17.1.

**Acceptance:** every documented guarantee maps to shipped behavior or an
executable proof; the full formal suite and split-brain real-process catalog
remain green; the native reference client compiles and passes its interop
matrices on Rust 1.89 with one coherent webrtc-rs 0.17.2 family; all CI and
review feedback are green. Delivered in session 069.

---

### P24 — Healthy relay synchronous fast path and H2 saturation validation (Size S) — ✅ DONE

Session 070 advanced the broad optimization issue #207 by testing one causal
hypothesis against both the isolated allocator ledger and the existing
16-player real-WebSocket H2 workload. A healthy recipient's bounded queue
already resolved through non-blocking `try_send`, but every fan-out still
constructed and polled a dynamic future for every recipient through
`join_all`. That async scaffolding sat on each connection's sequential ingress
path even when no recipient was backpressured.

- [x] Add a red allocation oracle: warmed healthy fan-out may use at most four
  allocation operations per relay. Current `main` failed at 6.0002, 7.0002,
  and 7.0002 operations for 2-, 8-, and 16-player rooms.
- [x] Split the single delivery state machine into a synchronous non-blocking
  attempt and an async continuation that exists only after `Full`. Room fan-out
  starts every recipient first, then awaits all actually-full recipients
  concurrently under absolute deadlines captured when each queue reports
  `Full`. Scalar sends, trace
  validation, shared relay-frame ownership, accounting, close reasons, and
  current-generation pruning use the same helpers rather than duplicated logic.
- [x] Prove the mixed case: a healthy recipient lands before either full queue
  drains, while two full recipients both enter backpressure before either is
  released. Existing classified retry, cancellation, timeout, close-race, and
  trace suites remain the semantic oracle.
- [x] Re-run the identical deterministic allocator workload. Operations per
  relay fall to 3.0002, 4.0002, and 4.0002; allocated bytes fall from 1,112,
  6,208, and 12,096 to 424, 1,344, and 1,664 respectively. The warmed
  classified queue remains allocation-free.
- [x] Apply the pre-registered H2 abort gate. The canonical debug-profile sweep
  remains exact and backpressure-free; three same-machine repeats show a modest
  ~3% throughput improvement at the 240 Hz boundary. Five alternating
  release-profile base/candidate pairs at a diagnostic 960 Hz saturation point
  raise median completed throughput from 220,493 to 226,951 deliveries/s
  (+2.9%) and lower median p99 from 39.1 ms to 14.6 ms (-62.6%). At 1,920 Hz,
  median throughput improves 2.0% and p99 3.8%. The diagnostic higher rates are
  local comparison points, not portable capacity claims or new CI thresholds.

**Acceptance:** healthy fan-out allocates no async wait set, actually-full
recipients still wait concurrently, every exact-delivery/conformance invariant
holds, and the same real-socket workload demonstrates a repeatable
release-profile latency/throughput gain. Delivered in session 070.

---

### P25 — Strict control-capacity deadlines and Docker healthcheck audit (Size S) — ✅ DONE

Session 071 swept the remaining control-message capacity waits after P24's
timer-first relay fix exposed the same Tokio boundary elsewhere. It also closes
issue #226, where the CI audit read Dockerfile physical lines and therefore
reported the production image's live multiline healthcheck as absent.

- [x] Reproduce the deterministic initial-transition defect: after a full
  control queue's grace expired, returning capacity before the waiting future's
  next poll produced `Delivered` instead of `SlowConsumer`.
- [x] Sweep all three control-capacity primitives: room-join/reconnect baseline
  reservation, conditional single-recipient delivery, and conditional
  reservation used by lifecycle broadcasts, session plans, replans, and
  two-phase room transactions. Drain cancellation retains precedence, expiry
  precedes returned capacity, and existing metrics/generation checks remain.
- [x] Add one paused-time data-driven oracle covering all three paths with
  legacy/classified queues, exact/post-deadline timing, terminal-state
  precedence, and exact attempt, backpressure, enqueue, drop, and close
  accounting.
- [x] Parse Dockerfile logical instructions without evaluating their contents:
  default-backslash continuations, CRLF, comments, stage boundaries, and the
  exact localhost curl probe are explicit; unsupported RUN/COPY/ADD heredocs,
  including `ONBUILD` wrappers, fail closed.
- [x] Add black-box fixtures for valid single/multiline shapes and invalid,
  commented, disabled, malformed, builder-only, heredoc-only,
  misleading-command/comment, wrong-host/path/port, and absent probes. The
  checked-in Dockerfile now produces no CI-audit warning.

**Acceptance:** capacity returned at or after the configured deadline cannot
revive any expired control-message wait; cancellation and exact delivery
accounting remain intact; the CI audit recognizes only an active, valid final
stage healthcheck on the production port; focused, full local, hosted CI, and
review gates are green. Published as PR #234 with the reviewed implementation
head and all applicable hosted workflows green.

---

### P26 — Strict WebSocket I/O deadline semantics (Size S) — ✅ DONE

Session 072 addresses issue #233 by sweeping the four adjacent I/O boundaries
identified in P25. Tokio's `timeout` and `timeout_at` poll their inner future
before the timer, while the pre-auth receive used an unbiased selection, so
work ready at or after a logical deadline could win when the task was polled
late.

- [x] Define one half-open contract: socket completion or inbound input must be
  observed strictly before its deadline; exact and post-deadline readiness
  expires.
- [x] Route selected reliable/control/lossy socket writes and server Ping writes
  through one absolute, timer-first primitive. Existing delivery accounting,
  Ping probe cleanup, metrics, and `4002 slow_consumer` versus `4003
  activity_timeout` ownership remain unchanged.
- [x] Route pre-authentication and authenticated idle reads through one
  close-first, timer-first primitive. Existing farewell messages and `4001
  auth_timeout` / `4004 idle_timeout` reasons remain unchanged, and
  `idle_timeout_secs = 0` still disables the idle boundary.
- [x] Prove just-before success, exact/post-deadline rejection, and
  already-requested-close precedence with paused time and gated futures after
  first polling each operation pending.

**Acceptance:** no selected socket write, Ping write, authentication input, or
idle input can be admitted at or after its configured boundary; just-before
completion remains healthy; and lifecycle cancellation retains precedence.
Focused and full local validation, all exact-head hosted workflows, and the
review loop are green on `cddb4f18bd166f5c230c3c7403abb74fb14c5ba0`.
Required link gates are also offline and deterministic after the observed
external-host timeout; the network-dependent audit is scheduled and
non-gating. Published as PR #235 and squash-merged to `main` at `60884be`,
closing #233.

---

### P27 — Rate-limit accounting and observability integrity (Size S) — ✅ DONE

Session 073 addresses issue #236, a bounded safety and observability increment
under #205. Failure-first tests proved that room creation could bypass the
shared join-attempt budget and overflow its counter, while a zero auth cleanup
period killed its background task.

- [x] Make room creation atomically require both room-creation and join-attempt
  capacity, with no partial counter changes on rejection.
- [x] Guard direct-library zero windows, cleanup-duration overflow, subsecond
  retry rounding, stale stats, and cleanup-task ownership.
- [x] Remove the never-consumed `AuthMaintenanceConfig` and its three
  database-shaped cache settings while tolerating legacy unknown input.
- [x] Retain the aggregate rejection counter and replace permanently-zero
  minute/hour/day/reset/cache telemetry with five production-wired rejection
  sources: auth, room creation, join attempt, signal, and detailed signal-error
  budget.
- [x] Correct docs for compound room/join accounting, `max_signal_errors`
  behavior, sliding-window auth, and legacy advisory hour/day fields.
- [x] Complete the full local gauntlet, adversarial review loop, exact-head
  hosted workflows, and publication review loop.

**Acceptance:** aggregate rejection telemetry equals the sum of the five real
decision sources; retired false series and JSON fields are absent; compound
budgets cannot partially consume or overflow; public edge inputs do not disable
enforcement or panic maintenance; and issue #236 closes only after local,
review, and hosted gates are green.

All 15 applicable hosted workflows and the complete reviewer loop are green on
final head `6bad0c0d41e8ca89b4e49dcfabf50ac8bb24782d`. Published as PR #237,
closing #236 and referencing the broader #205 safety umbrella.

---

### P28 — Release preparation across standalone package graphs (Size S) — ✅ DONE

Session 074 carries the 0.5.2 release PR to green after both CI and Advanced
Safety proved that release preparation left `fuzz/Cargo.lock` at 0.5.1. The
failure was deterministic and shared: the locked fuzz build rejected the stale
graph, and the workspace lockfile guard reported the same mismatch later under
ASan.

- [x] Add the fuzz lockfile to the 0.5.2 release commit.
- [x] Discover every tracked `Cargo.lock` that embeds the root path package,
  validate its sibling manifest before mutation, update it, and re-run locked
  metadata after mutation.
- [x] Run full locked metadata against every tracked manifest before lockfile
  contents select rewrite targets, so a future stale graph without a root
  package entry cannot disappear from release discovery.
- [x] Distinguish the unsourced local package from same-named registry entries;
  retain the fuzz graph's `cargo-deny`-required local version constraint and
  synchronize it so patch, minor, and major bumps all resolve.
- [x] Keep the root, native, and fuzz graphs as mandatory non-vacuous members
  while automatically covering future standalone packages.
- [x] Derive the release workflow's allowed diff and staged file set from the
  same package-graph rule instead of maintaining another hard-coded list.
- [x] Point stale-lock diagnostics to `Cargo.toml` manifests accepted by Cargo,
  rather than to lockfiles.
- [x] Restore every release file after a failed postflight and make workflow
  retries reuse only an exact-tree release branch and its open pull request.
- [x] Carry the developer toolchain's `rust-analyzer` component for VS Code
  compatibility and verify MSRV/tooling parity remains synchronized.

**Acceptance:** release preparation cannot omit a current or future tracked
standalone graph that embeds `signal-fish-server`; malformed mandatory graphs
fail before mutation; the generated PR contains the complete validated diff;
and the exact release head passes local, hosted, and reviewer gates.
Completed in PR #238 at exact head `dabafe13e3191eefc561c2f391f74a976c8f57be`:
17 hosted workflows passed, the Dependabot-only workflow skipped as expected,
Copilot reported no surfaced or suppressed comments, and all three adversarial
reviewers reported zero findings including minor issues.

---

### P29 — Pre-sized binary relay serialization (Size S) — ✅ DONE

Session 075 advances issue #207 at the exact production socket-projection seam
introduced in P21. The binary encoder previously built every MessagePack
envelope from an empty `Vec` even though the opaque payload length was already
known, paying three to five deterministic growth reallocations per encoded
cohort.

- [x] Add checked-in operation, reallocation, and allocated-byte ceilings for
  all nine JSON, direct-binary, and mixed-format benchmark cells; run them in
  the hosted CI lane so the scientific baselines fail closed.
- [x] Pre-size named MessagePack envelopes from the payload length plus bounded
  fixed-envelope headroom, without changing the v2 or v3 wire shape.
- [x] Prove identical output digests, wire-byte ledgers, codec work, delivery
  accounting, and empty queues across five exact allocation repeats.
- [x] Reduce direct-v3 binary allocation operations from 10–11 to 5–6 per relay
  and mixed 8-/16-player fan-out from 37 to 28; eliminate every encoder-output
  reallocation in those measured cells.
- [x] Compare the uninstrumented Criterion workloads against the exact
  pre-change encoder. Unchanged controls drifted significantly between the
  sequential runs, so P29 records no timing claim from that contaminated
  comparison; the deterministic allocator result is the acceptance evidence.
- [x] Complete the full local gauntlet, adversarial review loop, exact-head
  hosted workflows, and publication review loop.

**Acceptance:** all nine checked-in allocation ceilings pass deterministically;
direct binary and mixed-format encoder growth is absent; exact output stays
unchanged; and the phase closes only after local, review, and hosted gates are
green.

Completed in PR #240 at reviewed implementation head
`1d1d9919c8426f63b1a975798e952df5b0280d8c`: all 16 applicable hosted
workflows passed, the Dependabot-only workflow skipped as expected, Cursor
reported no issues, Copilot reported no new or suppressed comments after every
recommendation was incorporated, and the sole inline thread was resolved.

---

### P30 — Spectator lifecycle and room-GC coherence (Size S) — ✅ DONE

Session 076 found a production lifecycle defect while auditing the next
gameplay-path milestone: both room-GC sweeps treated an empty player map as an
empty room even while connected spectators remained. Deletion stranded each
spectator behind a process-local role that could neither detach from the
missing room nor join another one.

- [x] Reproduce the deletion with a failure-first, data-driven test over both
  GC paths and track the defect as issue #241.
- [x] Define occupancy uniformly as players or spectators for empty and
  inactive cleanup classification.
- [x] Refresh room activity on spectator join, detach, application traffic,
  and transport Pong through the existing throttled liveness seam.
- [x] Prove a spectator-only room uses the inactive timeout and becomes
  empty-GC eligible only after the final spectator detaches.

**Acceptance:** neither cleanup path can orphan a live spectator; active
spectator traffic keeps the occupied room alive; and the post-detach inverse
reclaims the room normally.

---

### P31 — Local TURN-only WebRTC interoperability (Size S) — ✅ DONE

Session 076 closes issue #239's in-repository operability gap. Earlier native
interop deliberately disabled TURN and therefore proved only direct host ICE,
while credential tests proved HMAC shape without moving data through coturn.

- [x] Force the native reference client to use relay-only ICE and surface the
  selected local/remote candidate types in its machine-readable event stream.
- [x] Start coturn from a version-and-digest-pinned image on explicit local
  listening and relay ports, with bounded readiness and cleanup.
- [x] Configure the real server through its production TURN block and assert
  both peers select relay candidates plus exact reliable/unreliable ledgers.
- [x] Exercise the WebSocket relay floor during the healthy TURN session.
- [x] Run a mismatched-secret negative control that selects no pair, reports
  WebRTC disconnected, engages fallback, and exchanges exact relay payloads.
- [x] Upload redacted coturn, server, client, and test diagnostics on hosted
  failure, and distinguish this proof from production infrastructure ops.

**Acceptance:** the pinned local lane needs no public STUN/TURN service, proves
actual relayed data rather than candidate gathering alone, and fails closed on
credential/configuration drift while preserving the universal WebSocket floor.

---

### P32 — Zero-copy v2 raw-binary relay projection (Size S) — ✅ DONE

Session 077 advances issue #207 on the universal v2 relay floor. JSON and Rkyv
binary payloads already live in shared `Bytes`, but direct v2 projection copied
the entire payload into a new `Vec` before converting it back into a WebSocket
`Bytes` frame.

- [x] Extend the production-seam allocation harness with homogeneous v2 JSON
  and Rkyv binary cells at 2, 8, and 16 players.
- [x] Record the failure-first baseline: 4/6/6 allocation operations per relay
  and 1,419/2,363/2,683 allocated bytes at 2/8/16 players.
- [x] Reuse the shared payload allocation while preserving exact frozen-v2
  output hashes, wire-byte ledgers, delivery accounting, and empty queues.
- [x] Enforce 3/4/4 operation ceilings and 425/1,345/1,665 byte ceilings in the
  required hosted allocation lane; v3 and MessagePack ceilings remain exact.
- [x] Complete the full local, adversarial-review, hosted-CI, and publication
  review gates.

**Acceptance:** both raw binary formats remove the payload-sized copy and one
allocation operation at two players or two operations at 8/16 players without
changing bytes or accounting, and all five benchmark scenarios remain
deterministic across five exact repeats.

Completed in PR #244 at reviewed implementation head `b189b45`: all hosted
allocation and platform lanes passed, Cursor reported zero findings, and no
review threads remained.

---

### P33 — Current pinned nightly analysis baseline (Size S) — ✅ DONE

Session 077 closes issue #243. The shared Miri, AddressSanitizer, cargo-fuzz,
and cargo-udeps pin reached the repository's six-month refresh threshold, while
the consistency policy omitted fuzz and inspected only the first date in each
workflow.

- [x] Select and install the available date-pinned `nightly-2026-08-01` with
  Miri, `rust-src`, and the x86_64 GNU target.
- [x] Update every operational analysis pin, the devcontainer, local fuzz
  instructions, workflow documentation, and agent reference material.
- [x] Make pin consistency inspect every live occurrence across all three
  workflows and the devcontainer while retaining the separate compatibility-
  pinned Fortress WASM toolchain.
- [x] Replace the frozen-reference freshness test with a real UTC age check at
  the same 180-day policy boundary used by workflow hygiene.
- [x] Make the nightly fuzz matrix derive its coverage contract from every
  declared fuzz binary, restoring smoke execution for both state-machine
  targets in addition to protocol decoding and input validation.
- [x] Refresh cargo-udeps to an Edition-2024-aware release, remove the two
  genuinely unused dev-dependencies it exposed, and make the full-target,
  full-feature analysis finish with zero findings.
- [x] Repair the newly exercised session-machine target's stale conservation
  oracle to include intentional cancellations and bounded self-stabilization
  for transiently unbalanced counter snapshots.
- [x] Prove the Miri, sanitizer, fuzz, and unused-dependency hosted lanes green
  on one exact commit, inspecting non-gating Miri/udeps step outcomes directly.

**Acceptance:** workflow hygiene reports no stale-nightly warning, all analysis
tools execute successfully on one explicit pin, and a partial future pin update
fails the local policy suite.

Completed in PR #244 at reviewed implementation head `b189b45`: the actual
Miri and cargo-udeps analysis steps passed alongside AddressSanitizer and all
four fuzz targets on the same pin.

---

### P34 — Relay projection allocation and timing integrity (Size S) — ✅ DONE

Session 078 advances issue #207 on the MessagePack-to-JSON fallback and repeated
frozen-v2 raw relay paths. The allocation harness also exposed whole-frame
SHA-256 work inside Criterion's timed loop even though production does not hash
relay output.

- [x] Record failure-first allocation ceilings at the production materializer
  seam for mixed-format and frozen-v2 raw relay cohorts.
- [x] Pre-size JSON fallback output from its known opaque payload length while
  retaining fallible growth, allocator context, and exact UTF-8 wire bytes.
- [x] Avoid allocating a relay-wide projection cache for repeated frozen-v2
  JSON/Rkyv passthrough frames that only clone shared `Bytes` handles.
- [x] Keep exact output hashing in benchmark validation while excluding it from
  Criterion's timed production-path samples.
- [x] Complete the full local, adversarial-review, hosted-CI, and publication
  review gates.

**Acceptance:** mixed 2-/8-/16-player relays use at most 15/22/22 allocation
operations with zero output reallocations, repeated frozen-v2 raw 8-/16-player
relays use three operations, every wire/accounting ledger remains exact, and
runtime samples do not time work absent from production.

Completed in green PR #245 at reviewed implementation head `31cb0d5`. All 19
hosted workflows reached terminal state (18 successes plus the expected
Dependabot skip), and Cursor Bugbot reported zero findings on the exact head.

---

### P35 — Strict retry delay cap (Size S) — ✅ DONE

Session 078 also closes a bounded safety defect under issue #205: both retry
execution paths capped exponential backoff before adding jitter, allowing the
documented persistent 5-second maximum to become a 6-second sleep.

- [x] Centralize initial and subsequent retry-delay calculation for both
  executor paths.
- [x] Treat `max_delay` as a strict cap on the complete jittered sleep.
- [x] Preserve sub-millisecond `Duration` precision and saturate invalid or
  overflowing public factor/duration combinations without panicking.
- [x] Complete the full local, adversarial-review, hosted-CI, and publication
  review gates.

**Acceptance:** deterministic boundary tests cover cap-before-jitter, remaining
headroom, ordinary jitter, sub-millisecond delays, overlarge initial delays,
and duration/factor overflow; no produced sleep exceeds `max_delay`.

Completed in green PR #245 at reviewed implementation head `31cb0d5`, with the
complete hosted matrix green and no Cursor findings.

---

### P36 — Historical GitHub Release retry integrity (Size S) — ✅ DONE

The v0.5.2 tag-push recovery proved the crate, GHCR manifest, and all six binary
builds, then GitHub rejected Release creation because the workflow redundantly
passed a historical `target_commitish` whose workflow files differ from the
current default branch. That operation requires workflow-write permission,
which a workflow-scoped `GITHUB_TOKEN` cannot receive.

- [x] Preserve the immutable annotated-tag identity and all existing preflight,
  package, container, and source-revision checks.
- [x] Create the GitHub Release from the already-verified tag without asking the
  Releases API to retarget or recreate it.
- [x] Skip identity mutation for an existing Release and retry SBOM/binary
  recovery through asset-only uploads.
- [x] Add workflow-policy and runbook documentation for historical idempotent
  retries.
- [x] Accept Cargo's canonical clean package metadata (`git.dirty` omitted) in
  the crates.io idempotency probe while continuing to fail closed on dirty,
  malformed, checksum-mismatched, or source-mismatched packages.
- [x] Validate an existing Release's public state, exact notes, source revision,
  and image digest before any SBOM or binary asset-only replacement.
- [x] Make SBOM and binary asset-only uploads name the GitHub repository
  explicitly instead of depending on ambient checkout discovery.
- [x] Complete a green hosted retry and verify the v0.5.2 Release, SBOM, and
  binary attachments.

**Acceptance:** a manual retry from a later default-branch revision reuses the
annotated release tag without workflow-write permission, publishes no different
crate or container bytes, and leaves the GitHub Release complete and verifiable.

Completed in green PRs #246 and #247. Historical Release run `30772076461`
reused crates.io source revision `09238c36ab8b086b13a5e50d679df51e32376134`
and GHCR digest
`sha256:efede9dbed5cba2d7f1c09b2143d568a91bc731e6510fd8fbdc81fe66d800d4c`,
then attached the CycloneDX SBOM plus all six archives and six matching checksum
files. Independent verification matched the exact Release notes digest, opened
every archive, and validated every checksum.

---

### P37 — Allocation-free relay builder handoff (#207) (Size S) — ✅ DONE

P34 proved the projection cells themselves; the next measured allocation seam
is the ingress-to-queue builder handoff in `src/server/game_data.rs`. The current
private builder path boxes both a one-shot payload builder and its result even
though each relay invocation consumes the builder at most once.

- [x] Replace the private double-boxed handoff with a generic helper and a
  one-shot `Option`/`FnMut` adapter, preserving cancellation and missing-stamp
  behavior.
- [x] Prove one-shot consumption, defensive double-call behavior, cancellation,
  missing-stamp handling, and builder-drop semantics with focused tests.
- [x] Measure real JSON and binary ingress-to-queue cells with payload creation
  outside the measured region and no background task noise.
- [x] Enforce room allocation-operation ceilings of 2, 3, and 3 and reduce the
  current byte ceilings by exactly the removed eight-byte box allocation.
- [x] Describe the result as an allocation-free builder handoff; do not claim a
  relay-latency improvement without an independent timing measurement.

**Acceptance:** the production handoff performs no builder-box allocation,
keeps exact delivery and cancellation semantics, and the data-driven measured
cells enforce the new operation and byte ceilings on both JSON and binary
ingress paths.

Completed in session 080. The public boxed coordinator method remains for
compatibility and an additive borrowed `FnMut` seam serves the hot path; the
private generic adapter uses `Option::take` to consume the original `FnOnce` at
most once. Real JSON and binary production-ingress cells, including envelope
and `Arc` construction, measure 3/4/4 operations at 2/8/16 players. A separate
like-for-like handoff cell measures 2/3/3 operations and
400/1,320/1,640 bytes, below ceilings tightened by exactly the removed
eight-byte builder allocation. Delivery, cancellation, builder-drop, reconnect
ordering, and message variants remain covered; no latency claim is made.

---

### P38 — Allocation-free routed-recipient traversal (#207) (Size S) — ✅ DONE

P37's isolated production handoff still measured 2/3/3 allocation operations
at 2/8/16 players. One operation and 48/288/608 bytes came from copying the
already-guarded routed recipients into a temporary `Vec` before beginning
delivery. That snapshot exists only to release routing guards before a
capacity wait; healthy classified queues complete synchronously and need no
owned recipient collection.

- [x] Record the unchanged failure-first baseline across five exact repeats:
  isolated handoff 2/3/3 operations and 400/1,320/1,640 bytes; production JSON
  and binary ingress 3/4/4 operations and 696/1,616/1,936 bytes.
- [x] Walk the guarded routing membership directly for projection-cohort
  discovery and synchronous delivery start, retaining owned state only for an
  exceptional backpressure or slow-consumer path.
- [x] Release both routing guards before awaiting capacity, and prove a late
  registration can complete during the wait without entering the already
  started recipient snapshot for both boxed and borrowed builders.
- [x] Tighten deterministic ceilings to isolated 1/2/2 operations and
  368/1,048/1,048 bytes, plus production 2/3/3 operations and
  648/1,328/1,328 bytes. Five exact observed samples are lower than or equal
  to every ceiling.
- [x] Complete the full local gauntlet, exact-head hosted CI, adversarial review,
  and publication review loop without weakening terminal-unroute, reconnect,
  delivery-accounting, slow-consumer, or wire-output evidence.

**Acceptance:** healthy relays allocate no routed-recipient vector, the
checked-in allocator cells enforce one fewer operation per relay and remove all
room-size-dependent handoff bytes, exceptional capacity waits release routing
guards and preserve the original snapshot, and no latency improvement is
claimed without an independent timing comparison.

---

### P39 — Atomic application room ownership and quotas (#249) (Size M) — COMPLETE

Configured application limits existed only as authentication metadata, while
room admission trusted a process-local owner cache and never compared it with
the persisted room owner. Cross-application seated, spectator, and reconnect
admission was therefore possible, and neither configured room nor player quotas
affected production admission.

- [x] Authorize seated, spectator, and reconnect admission from persisted
  `Room.application_id`, returning the same non-enumerating `ROOM_NOT_FOUND`
  outcome for another app's room and repopulating the cache only after an
  authoritative read.
- [x] Keep auth-disabled rooms unowned; let only a successful authenticated
  seated admission claim a legacy unowned room, with claim rollback when the
  admission cannot be published. Persist rollback intent across detach/storage
  failures without erasing ownership adopted by a later successful seated join
  or reconnect. Spectators never establish or adopt ownership.
- [x] Reject room creation above `max_players_per_room`, and cap future seats at
  the lower of the stored room capacity and the app's current limit without
  ejecting existing players.
- [x] Count persisted application-owned rooms across all game names and
  serialize count-plus-create under the documented room-code → application-cap
  → game-cap lock order. Lock and count failures deny creation without side
  effects.
- [x] Add scenario-matrix and deterministic concurrent proof for cache loss, cross-app seated /
  spectator / reconnect rejection, reconnect-token preservation, legacy claim
  races, exact capacity and room-count boundaries, independent applications,
  cleanup freeing quota, and fail-closed infrastructure errors.
- [x] Complete exact-head hosted CI and the full adversarial/reviewer loop, and
  wire PR #254 to close issue #249 on merge.

**Acceptance:** application ownership and configured quotas are authoritative,
atomic at every admission boundary, backward-compatible when the app-ID allowlist is disabled,
and fully green under local and hosted verification. This is accounting and
accidental-collision isolation; it does not claim hostile-client security while
the public-app-ID trust boundary is documented explicitly in P55.

---

### P40 — Executable Host + Direct plans (#251) (Size M) — COMPLETE

The negotiation ladder could select `Host + Direct` from capability strings
alone even when the elected authority had supplied no usable direct endpoint.
The emitted plan therefore could not tell a client where to connect, while the
reference clients treated the selection as though execution had started.

- [x] Validate self-declared direct endpoints at the planning boundary and
  exclude absent, malformed, zero-port, unspecified-address, or otherwise
  unusable endpoint claims from Direct-host eligibility.
- [x] Elect only an endpoint-bearing Direct host and carry its validated host
  and port in the v3 `SessionPlan`; when no eligible host exists, select the
  next executable ladder rung without changing v2 wire behavior.
- [x] Revalidate Direct eligibility during reconnect, departure, failover, and
  late membership refreshes while preserving the established sticky relay
  downgrade policy.
- [x] Make both reference clients reject an unsupported Direct plan explicitly
  and engage WebSocket relay fallback instead of implying that Direct
  execution succeeded.
- [x] Add exact wire, property, v2-compatibility, lifecycle, malformed-input,
  and real-WebSocket coverage for endpoint-bearing Direct plans.
- [x] Complete the full local validation gauntlet, adversarial review, and
  exact-head hosted CI and reviewer loop.

**Acceptance:** every selected `Host + Direct` plan names a validated endpoint
owned by its elected host; losing that eligibility deterministically re-elects
or drops to the next executable rung; reference clients fail closed to relay;
and focused, full local, hosted CI, and review gates are green.

---

### P41 — Protocol-v3 edge and specification hardening (#257) (Size M) — ✅ DONE

A fresh whole-surface v3 audit found three gameplay/lifecycle defects and three
machine-readable contract drifts: reconnect claims could outlive room-GC
protection, a v3-only deployment silently promoted explicit v2 clients, and a
required data channel could die while both reference clients continued to
report WebRTC connected. The AsyncAPI and canonical examples also admitted
wire shapes that production never emits or rejected nulls that it always emits.
Coordinated retained-pair generations require a wire barrier and continue in
follow-up issue #258.

- [x] Keep an actively claimed reconnect record GC-protected until the restore
  claim resolves, independently of the original reconnect-window deadline.
- [x] Cap negotiation only downward and reject a client below the deployment
  floor with append-only `UNSUPPORTED_PROTOCOL_VERSION`, including the
  auth-disabled endpoint-default path.
- [x] Observe required data-channel close/error callbacks in native and browser
  clients, generation-fence stale callbacks, tear down the unusable link, and
  publish the changed false transport state while retaining the relay floor.
- [x] Replace permissive `ConnectionInfo` and `SessionPlan` schemas with exact
  legal unions; require nullable authority fields that serde writes as null.
- [x] Pair `epoch` and `seq` in every canonical v3 room snapshot, align sample
  readiness, and enforce v2 omission/v3 presence in samples and real-WebSocket
  projection tests.
- [x] Complete the full local validation gauntlet, adversarial review, and
  exact-head hosted CI and reviewer loop.

**Acceptance:** reconnect restoration cannot race room deletion; no handshake
claims a protocol newer than the client declared; loss of either required data
channel visibly falls back to relay; generated clients see only executable
session-plan and exact connection-info shapes; all sample/runtime
baseline invariants are paired; and focused, full local, hosted CI, and review
gates are green.

---

### P42 — Wire-fenced retained WebRTC pair generations (#258) (Size M) — ✅ DONE

Authoritative WebRTC `SessionPlan` refreshes can carry new ICE/TURN credentials
while one or both endpoints retain an existing physical peer connection.
Endpoint-local link health cannot coordinate a safe rebuild: either endpoint
may replace its link first, and old offers, answers, or ICE candidates can still
be in flight. Make the server author one opaque generation per room-plan
publication, carry it on every signaling frame, and require both reference
clients to accept signals only for their current generation.

- [x] Add a server-authored generation to `SessionPlan` and both directions of
  `Signal`, preserving v2 bytes and making recipients reject stale or unknown
  generations without parsing the opaque WebRTC payload.
- [x] Advance the generation atomically with each finalized WebRTC membership
  publication and host re-plan, preserving plan-before-signal room ordering.
- [x] Rebuild every retained physical pair on a newer authoritative generation,
  use that plan's refreshed ICE servers, and retain the server-authored offerer
  role in native and browser clients.
- [x] Fence engine callbacks and inbound offer/answer/ICE by the wire
  generation while preserving logical exactly-once pair-connected,
  transport-status, and application-exchange observations.
- [x] Add deterministic asymmetric-health, delayed-plan/in-flight-signal,
  duplicate/stale-generation, and refreshed-TURN regressions across both
  reference clients and the real server publication seam.
- [x] Update the exact protocol schemas, canonical samples, client guides,
  changelog, and interop matrix; complete the full local, hosted CI, and
  reviewer loop.

**Acceptance:** every authoritative WebRTC plan refresh safely replaces
retained pairs at both endpoints; no stale signaling frame can reach a newer
physical connection; refreshed ICE/TURN credentials are used; the relay floor
remains live; and no logical connectivity or exchange event is duplicated.

---

### P43 — Membership-scoped transport-status deduplication (#260) (Size S) — ✅ DONE

Protocol-v3 transport status was deduplicated across the entire physical
WebSocket. Leaving one room and joining another on the same socket could
therefore suppress the new membership's first status when it matched the old
room, leaving peers without an initial health observation.

- [x] Reproduce the same-socket room-A to room-B suppression through the real
  WebSocket stack before changing production code.
- [x] Scope deduplication to an explicit room/spectator membership generation,
  including roomless, seated, spectator, same-room rejoin, reconnect, and
  prepared-transition rollback boundaries.
- [x] Preserve same-generation duplicate suppression, negotiated-v3 transport
  gates, metrics semantics, exact room routing, and frozen protocol-v2 bytes.
- [x] Document the seated-versus-spectator fan-out rule and add service,
  lifecycle-ordering, and real-socket coverage for same-room and cross-room
  transitions, spectator entry/leave/rollback, and collision-resistant opaque
  membership tokens.
- [x] Complete the full local validation gauntlet, adversarial review, exact-head
  hosted CI, and reviewer loop.

**Acceptance:** every membership generation admits its own first valid
`TransportStatus`; duplicates remain suppressed only within that generation;
old-room status cannot reach new-room peers; all local, hosted, and review gates
are green.

---

### P44 — Exact AsyncAPI v2/v3 accountability envelopes (#261) (Size M) — ✅ DONE

The codegen-facing AsyncAPI currently closes the inner v2/v3 `PlayerInfo` and
physical binary relay schemas, but several enclosing text-message schemas still
accept impossible hybrids: unpaired sequence/epoch fields, v3 delivery policy
on a v2 relay, mixed-version room snapshots, partial reconnect accountability,
and arbitrary replay objects.

- [x] Inventory every outer message carrying versioned relay accountability or
  snapshots and reproduce the missing schema fence with a failure-first guard.
- [x] Replace each broad public schema with closed, versioned wire-shape
  branches while retaining the stable public component names used by generators.
- [x] Preserve the valid shared `SpectatorJoined` shape when a spectator-only
  room has no players and therefore no wire-visible version discriminator.
- [x] Model `Reconnected.missed_events` as the exact version-specific replayable
  control-message subset instead of arbitrary objects.
- [x] Validate representative Rust-serialized v2/v3 frames and reject missing,
  extra, unpaired, mixed-version, and cross-version fields; keep all frozen v2
  bytes unchanged.
- [x] Validate canonical samples, update public protocol guidance and changelog,
  and complete local/full
  validation, hosted CI, and reviewer evidence.

**Acceptance:** generated models expose only executable v2/v3 accountability
shapes; the shared empty-spectator snapshot remains valid without a wire change;
reconnect replay cannot contain impossible message shapes; frozen v2 bytes are
unchanged; and all local, hosted, and review gates are green.

---

### P45 — Enforced zero-panic production policy (#255) (Size M) — ✅ DONE

The existing policy rejected explicit panic/unwrap/expect/indexing patterns in
the server crate, but still allowed production assertion macros, unchecked
arithmetic and time operators, byte-indexed strings, and did not run Clippy over
the standalone native reference client.

- [x] Extend the syntax-aware production scan to reject every `assert*` and
  `debug_assert*` macro while preserving nested and out-of-line test exemptions.
- [x] Replace all 12 production assertions with normalized inputs, fail-closed
  decisions, checked accounting, or types that cannot represent an invalid lane.
- [x] Enforce checked or saturating arithmetic and checked date/time deadlines
  across the server and native production targets.
- [x] Add the standalone native crate to the full Clippy panic policy and remove
  its pre-existing unchecked indexing and `expect` paths.
- [x] Preserve the manifest-level `unsafe_code = "forbid"` contract, add direct
  queue and stale-host regressions, and keep the complete locked suites green.
- [x] Incorporate the MSRV-compatible clap 4.6.5 update in both independent
  lockfiles; retain WebRTC 0.17 because the proposed 0.20 graph requires Rust
  1.91 while this repository supports Rust 1.89.

**Acceptance:** the syntax scan rejects the covered first-party production
panic/assertion functions and macros and cannot be bypassed by aliases, local
lint suppressions, unverified test filenames, or missing Cargo; Clippy denies
panic/unwrap/expect/todo/unimplemented/unreachable, unchecked
indexing and string slicing, and arithmetic side effects in both production
graphs; first-party unsafe code remains forbidden; supported Miri and root/native
AddressSanitizer suites are gating; supported builds, behavioral suites, hosted
CI, and review are green.

---

### P46 — Intentional WebRTC 0.20 migration and Rust 1.91 MSRV (#268) (Size M) — ✅ DONE

Issue #268's owner decision permits an intentional minimum-supported-Rust
raise so the native reference client can move to the maintained WebRTC stack.
This is a coordinated compatibility change rather than a lockfile-only update.

- [x] Raise the exact MSRV to Rust 1.91 across the root, native, fuzz, and
  Fortress manifests; toolchain, Clippy, container, workflow, development,
  badge, and policy references move in the same change.
- [x] Port the native client to webrtc-rs 0.20's peer-connection and
  data-channel traits, poll-driven event handlers, gathering completion, and
  statistics surface without weakening generation fencing or deterministic
  teardown.
- [x] Preserve the exact Matchbox/browser ICE-candidate wire projection by
  stripping the new local-only candidate provenance field, with an exact-JSON
  regression.
- [x] Prove the native-only, browser/native, crippled-ICE fallback, and local
  TURN-only scenarios on the migrated stack.
- [x] Pass locked Rust 1.91 root/native gates, all standalone supply-chain
  audits, the repository session-end gauntlet, hosted CI, and review.

**Acceptance:** every supported manifest declares the same exact Rust 1.91
MSRV; no webrtc-rs 0.17 package remains in the native lockfile; the migrated
client preserves exact signaling bytes, opens both required data channels,
selects relay candidates in the TURN proof, and retains deterministic fallback
and stale-generation behavior; all local, hosted, and reviewer gates are green.

---

### P47 — Cross-platform and IPv6 native client coverage (#271) (Size S) — ✅ DONE

Issue #271 records the residual risk PR #270 left behind. webrtc-rs 0.20 makes
the application supply its own UDP binds, so the native client now enumerates
interfaces itself — but no lane ever compiled `clients/native` off Linux, and
every multi-process cell selected IPv4, so both the non-Linux and the IPv6
branches of that new code were unproven.

- [x] Add a locked Linux/Windows/macOS matrix for the standalone crate on the
  exact repository MSRV: lockfile resolution, formatting, strict Clippy over
  all targets, and the library/binary unit suite. The Linux leg is the control
  for the same command set.
- [x] Add `--ip-family {any,ipv4,ipv6}`, which restricts the bound ICE sockets
  and therefore the advertised host-candidate family. A family the host cannot
  serve fails loudly instead of falling back to the other one. The pure
  selection rule is tested against a fixed mixed-interface sample, so the
  contract does not depend on the runner's own interfaces.
- [x] Report each selected candidate's address alongside its type, so a
  harness can prove which family actually carried the data channels rather
  than inferring it.
- [x] Prove one live IPv6 cell: two real client processes negotiate a
  host/host path over concrete dialable IPv6 and exchange the exact reliable
  and unreliable payloads with no relay fallback. A runner without IPv6 loopback fails the
  cell with an actionable message; it is never skipped.
- [x] Pin both proofs with a CI-configuration policy test whose markers are
  structural lines read from comment-stripped sources, and state in the client
  README which platforms are build-verified versus proved over live transport.
- [x] Exempt macOS from the 16-player matrix's wall-clock p99 ceiling
  (#274) after it failed that lane on this PR and the previous one; Linux and
  Windows still gate it. The number is a
  property of a shared-tenancy runner — 322ms/262ms on macOS against ~7ms on
  Linux, and 12x its own sibling cell in the same process — while every
  correctness oracle keeps running on all platforms.

**Acceptance:** the standalone crate compiles, lints, and passes its unit suite
on all three hosted platforms; one hosted cell proves an end-to-end IPv6
data-channel path with exact bidirectional messages on both labels; the
existing native, browser, and TURN-only suites and both cargo-deny policies
remain green.

---

### P48 — Routable ICE bind selection and advertised-candidate visibility (#276, #275) (Size S) — ✅ DONE

Session 091 root-caused the intermittent `TURN-only WebRTC Interop` failure that
issue #276 recorded on PR #273. The client chose its ICE bind set purely from
the interface table, and webrtc 0.20 starts a STUN binding and a TURN allocation
from **every** socket the application supplies. When no bound address routes to
the configured server, a relay-only session gathers no candidate at all — and
the only symptom the lane could report was
`expected exactly one p2p_pair_connected event, got 0: []`.

- [x] Root-cause from the run's own artifacts rather than a hypothesis: one
  `sendto` failed `EINVAL` from `127.0.0.1` toward the coturn container, two
  further allocations timed out, and coturn logged no Allocate at all — so
  every attempt left from an address the network could not accept.
- [x] Reproduce the mechanism directly against the lane's real topology.
  A datagram sent from an `--internal` network's own bridge address arrives; the
  identical datagram from any other host address is accepted by `sendto` and
  then silently dropped. Docker leaves that bridge `NO-CARRIER ... state DOWN`
  until a container attaches, and `if_addrs` filters on exactly that
  operational status — while `connect()` on an unbound socket still returns the
  bridge address as the source. Reachability is a routing fact; enumeration is a
  carrier-state snapshot.
- [x] Bind the kernel's own source address for every configured STUN/TURN
  server alongside the enumerated addresses. Both halves pass through the one
  existing selection rule, so a routing answer can never widen `--ip-family` or
  admit an address no peer could dial, and `--cripple-ice` — a transport that
  exists to be unreachable — is exempt.
- [x] Prove it behaviorally, on any host and with no external network: an
  enumeration that misses the route cannot reach a real socket the test owns,
  the merged set can, and the datagram is read back from the address the merge
  added. Confirmed red against the pre-change rule.
- [x] Report every advertised local ICE candidate as a `local_candidate` JSONL
  event, emitted after the relay so the set describes what the remote side can
  see. The relay-only positive control now requires relay candidates and only
  relay candidates; the mismatched-secret control requires none.
- [x] Close #275's gap 2 with the same event: the IPv6 cell asserts the
  advertised set carries no IPv4 entry, so an `--ip-family` regressed to a
  no-op cannot pass on a dual-stack host that happens to select IPv6 anyway.
  Falsified by making the flag a no-op, which the oracle rejects with the
  offending addresses.
- [x] Pin the production call site by AST, since
  `session_udp_addrs(self.settings, local_udp_addrs, Vec::new())` compiles,
  passes strict Clippy, and passes every live cell on a host whose interface
  table already contains the route. Four hollow-guard shapes were tried against
  the pin and each is rejected at its own assertion.

**Acceptance:** the bind set contains a routable source for every configured ICE
server whenever the host has one; no routing answer can defeat `--ip-family` or
the crippled transport; both live TURN controls and the IPv6 cell assert the
advertised candidate set; and the local gauntlet, hosted workflows and reviewer
loop are green.

---

### P49 — Teardown write integrity and TURN-lane observability (#274, #276) (Size S) — ✅ DONE

Session 092 closed a production delivery-accountability defect that issue #274
recorded as a macOS timing flake, and supplied the evidence the still-red
`TURN-only WebRTC Interop` lane needs.

- [x] Root-cause `reconnect_under_fire`'s `unexplained seq gap in epoch 1:
  expected 90, got 91`. The live writer loop runs inside the connection's close
  `select!`, so a close request cancels it wherever it is — including while a
  socket write owns one queued payload. `SendAccounting::drop` counted that
  payload, but `finalize_closed_connection`'s graceful branch then drained
  everything still queued **behind** it onto the same socket. The recipient
  therefore observed a delivered sequence skipping one no `DeliveryReport` ever
  described. `trace_validation.rs` already marked the schedule
  `Unsupported("live-write-cancelled-by-close")` because the formal model has
  no transition for a partially written frame; the production consequence had
  never been closed.
- [x] Establish that the payload's wire position is genuinely unknown, rather
  than assuming either outcome: tokio-tungstenite's `start_send` accepts the
  frame into its own buffer even on `WouldBlock`, while a cancellation inside
  `poll_ready` never hands it over. It can therefore be neither reported as an
  exact gap — a false gap for a frame that did reach the wire — nor assumed
  delivered.
- [x] Fence instead. An abandoned in-flight write latches the connection's
  queue; the graceful teardown abandons and counts the remainder rather than
  writing past it, so the stream ends as a gap-free prefix. Truncation at close
  is always legal; a hole is not. The loud slow-consumer path already abandoned
  its queue and is unchanged.
- [x] Prove it against a real upgraded WebSocket, asserting on the bytes the
  client received: a healthy teardown still flushes its whole queue, an
  abandoned in-flight write writes nothing behind it, and both keep exact drop
  accounting. Confirmed red with the fence removed.
- [x] Pin the other half data-driven over all three terminal accounting states,
  because a false positive would make every healthy teardown abandon its queue.
- [x] Sweep the class. Every other abandonment path already terminates the
  stream: deadline expiry and writer accountability failure both request
  `SlowConsumer`, a socket error breaks the loop, and a cancelled report leaves
  its ranges pending under the existing peek/write/commit rule.
- [x] Answer the still-red TURN lane with evidence rather than a second guess.
  The client reported the routing union only when it _added_ an address, so a
  run where the probe contributed nothing was indistinguishable from one where
  it never ran; it now reports the complete resolved bind set on every pairing
  and warns on an ICE server that neither resolves nor routes. The lane captures
  the host's own `ip route get`, address and route tables, and the Docker
  bridge's link state — the exact operational status `is_oper_up` reads — into
  its failure artifacts.

**Acceptance:** no recipient can observe a delivered sequence that skips one it
was never told about, on any teardown path; healthy teardown behaviour is
unchanged; and the next TURN-lane failure names which addresses ICE was given
and what the host's routing said, instead of only "no candidate pairs".

Delivered in PR #279 at final head `d9f1ff3`: the root suite passes 2,047 tests,
14 hosted workflows are green, the Dependabot-only workflow skips as designed,
and the sole failure is `Running Copilot Code Review` — the reviewer's own
account quota, which fails identically on every recent PR. The `TURN-only
WebRTC Interop` lane that is red on `main` is green here across **60
consecutive runs** of every head carrying the reachability gate. Cursor Bugbot
found two real defects in this session's own new code — the gate's errexit
collapse and the harness's Windows path interpolation, the latter confirmed
independently by the Windows lane — and reports no new issues on the final
head.

---

### P50 — Live native WebRTC transport on Windows and macOS (#275) (Size S) — ✅ DONE

Session 093 closes the remaining in-repo desktop-platform cell from P47. The
Windows/macOS matrix previously compiled and unit-tested the standalone native
client but never executed its interface enumeration, socket binding, ICE, DTLS,
SCTP, or multi-process harness on those operating systems.

- [x] Add the smallest complete platform-neutral scenario: two real native
  client processes and the real server negotiate a mesh session, select a
  direct host/host pair, and exchange exact traffic on both SCTP data channels.
- [x] Configure `windows-latest` and `macos-latest` to run that exact selector
  after building the root server binary with the locked repository toolchain.
  The full seven-scenario topology/fault matrix remains the Linux control.
- [x] Pin the workflow structurally: both non-Linux legs, the root server build,
  executable suffix, server-binary handoff, exact selector, and propagated
  failure are policy-tested.
- [x] Keep the evidence boundary explicit in the native client README and
  changelog: every desktop platform now proves live WebRTC, while the broader
  TURN/browser/IPv6/fault-injection matrix is still Linux-specific.
- [x] Complete the hosted Windows/macOS lanes and reviewer loop on PR #280.
  WebRTC Interop run 31031080742 passed the exact live selector on both hosted
  platforms; every substantive workflow on implementation head `4852482` is
  terminal green. Cursor Bugbot reported zero issues. Copilot was requested but
  could not review because the requesting account had exhausted its quota; no
  independent human reviewer was available to request from the connected
  self-authored identity. Session 093 records the full evidence and H14 retry
  RCA; follow-up diagnostic issue #281 preserves that unrelated experiment's
  strong production-default oracle.

**Acceptance:** Linux, Windows, and macOS each run two real native clients
through a direct host/host WebRTC pair and exact reliable/unreliable exchange;
the complete local gauntlet and every triggered hosted check are green; all
review feedback is resolved.

---

### P51 — H14 terminal and proxy diagnostic integrity (#281) (Size S) — ✅ DONE

Session 094 closes the diagnostic gap exposed by P50's intermittent H14
control failure. The experiment still sends the fixed 5,000-message burst to
two equally throttled recipients under the production queue, five-second
full-queue timeout, and 15-second maximum sojourn; only its failure evidence
changes.

- [x] Make both reader tasks return complete, timeout, EOF, WebSocket-error,
  close-code, unexpected-message, or task-failure observations with exact
  delivered/accounted counts, wire bytes, elapsed time, server errors, and
  `PlayerLeft` identities. Neither reader can erase the other's state.
- [x] Defer conformance-auditor assertions until both readers and transport
  evidence are preserved, preventing one assertion panic or poisoned mutex
  from cascading through the sibling reader.
- [x] Snapshot each proxy's server-to-client destination-write bytes and common
  measurement-clock elapsed time when that reader finishes and before lifting
  the throttle, then close the client streams and await retained pump
  termination causes. Report the immutable byte/time frontier, achieved rate,
  before/after-teardown terminations, control errors, backpressure events, and
  slow-consumer evictions before any oracle can fail.
- [x] Preserve every original H14 oracle: exact unsupported-format accounting,
  advisory rate limiting, no unexpected errors or lifecycle departures, no
  amplification, complete compatible delivery, non-vacuous backpressure, zero
  slow-consumer eviction, and full protocol conformance.
- [x] Add a deterministic RED-path formatting regression that proves both
  recipient terminals and both proxy outcomes survive in one diagnostic.

**Acceptance:** a RED run identifies the first failed control while retaining
both recipients' terminal states and actual proxy-throughput/pump evidence; a
GREEN run retains all 5,000-message workload semantics and production-default
timing. The focused RED-path regression and the unchanged ignored H14 selector
both pass.

---

### P52 — Generated room-code generation/admission closure (#283) (Size S) — ✅ DONE

Session 095 closes a gameplay-breaking configuration gap: the server previously
accepted zero-length codes and prefixes containing join-invalid punctuation (or
prefixes consuming the entire configured length), then created rooms whose
returned codes no second client could submit successfully.

- [x] Validate `protocol.room_code_length` as nonzero and constrain an optional
  `server.room_code_prefix` to a trimmed, nonblank ASCII-alphanumeric value
  shorter than the total generated-code length.
- [x] Apply the invariant both during top-level config validation and in
  `EnhancedGameServer::new`, so direct library construction cannot bypass it.
- [x] Prove accepted length/prefix combinations generate only join-valid codes,
  including lowercase and surrounding-whitespace normalization.
- [x] Extend the live two-client test so the second client joins with the exact
  prefixed code returned to the creator.
- [x] Correct the stale hyphenated deployment example and document the startup
  failure contract across the configuration and room references.

**Acceptance:** every accepted automatic generation setting is closed under the
join validator, invalid settings fail before serving traffic with a field-specific
error, and a generated prefixed code completes the real create/join path.

---

### P53 — Hosted relay timing evidence acquisition (#274) (Size S) — 🟡 COLLECTING

Session 095 adds a scheduled/manual three-platform observation lane for the
remaining #274 decision. It runs the exact six-cell 16-player relay matrix five
times per hosted allocation on Linux, Windows, and macOS while preserving exact
delivery, conformance, zero-backpressure, and zero-eviction oracles. Only the
wall-clock assertion is disabled in this diagnostic selector, so outliers are
recorded rather than censored.

- [x] Emit one versioned JSONL record per cell with run, runner, target, sample,
  encoding, player count, profile, toolchain/workload identity, and full
  semantic/timing observation data.
- [x] Retain raw logs, JSONL, and an explicit completeness/outcome manifest for
  the repository's 30-day maximum even on RED runs; use the GitHub run ledger
  for cancellations that cannot upload an artifact.
- [x] Guard the workflow's schedule, complete OS matrix, exact selector,
  repetition count, pipe failure semantics, artifact policy, and semantic-oracle
  unit test in `ci_config_tests`.
- [ ] Evaluate the first 20 consecutive eligible scheduled attempts per OS
  (100 requested observations per cell) under the fixed workload/toolchain
  cohort, counting RED, cancelled, missing, and incomplete attempts against
  timing-gate enablement as pre-registered in `docs/development.md`.
- [ ] Use that distribution to decide, and then enforce, platform-specific PR
  timing gates or correctness-only placement; keep #274 open until this step.

The first four eligible scheduled allocations span two implementation sources.
Semantic outcomes remain eligible across that boundary, but timing thresholds
or comparative timing claims must be stratified by exact source commit (or use
one implementation cohort); a mixed-source distribution must not be attributed
to a single implementation.

**Acceptance:** evidence collection is automated and reviewable now, but P53 is
not complete until the hosted sample threshold is met and the lane-placement
decision is committed from that evidence. Any resulting timing gate must be an
isolated all-feature job matching the observation context, never inferred for
the concurrently loaded broad Nextest lane.

---

### P54 — Bounded generated room-code collision retries (#284) (Size S) — ✅ DONE

Session 096 closes the automatic-creation collision gap left after P52. The
handler previously generated one code and erased the fact that the client had
omitted `room_code`; if that code already existed, the normal existing-room
branch could join an unrelated room instead of creating a new one.

- [x] Preserve automatic create-only intent through the coordinated room-code
  lookup while retaining explicit room-code join/create semantics.
- [x] Add a typed database collision result without breaking existing
  `GameDatabase::create_room` implementers, and retry only that classification.
- [x] Bound automatic generation to eight candidates, charge rate limiting once,
  and expose collisions, recovery, and exhaustion without logging candidate or
  player data.
- [x] Prove collision-then-success, bounded exhaustion, explicit-code behavior,
  concurrent creators, application ownership/quota integrity, and the typed
  storage outcome with deterministic tests.
- [x] Quantify the prefix/suffix namespace and document operational capacity
  guidance.
- [x] Close the adjacent npm audit blind spot discovered during the required
  dependency/CI sweep: refresh both vulnerable locked graphs and derive
  Dependabot plus scheduled-audit coverage from the tracked lockfile inventory.
- [x] Complete the full local validation gauntlet, adversarial/reviewer loop,
  and exact-head hosted CI before closing #284.

**Acceptance:** automatic creation never joins an occupied generated code;
collisions alone retry within a fixed budget as one client operation; ownership,
quotas, and explicit-code behavior remain unchanged; and collision/exhaustion
are observable without sensitive identifiers.

---

### P55 — Public app-ID trust-boundary closure (#250) (Size M) — ✅ DONE

Session 097 chooses #250's backward-compatible allowlist contract. The shipped
`Authenticate` envelope contains only a public `app_id`, so the server now says
exactly what it enforces instead of retaining an unused `app_secret` and dead
credential validator that implied hostile-client authentication.

- [x] Canonicalize configuration and Rust APIs around
  `enforce_app_id_allowlist`, `allowed_apps`, `AppRegistrationEntry`,
  `AppIdAllowlist`, and connection-bound `AppContext`.
- [x] Remove client-secret storage and validation; accept legacy `app_secret`
  only as discarded input that is never retained, logged, or serialized.
- [x] Normalize legacy JSON/file/environment keys before config-source merging,
  preserve direct serde aliases, apply documented source precedence, and fail
  closed on ambiguous same-source aliases.
- [x] Reject duplicate public IDs rather than silently selecting the last
  accounting policy, and keep production deployment examples usable by pairing
  enforced allowlists with an explicit registration.
- [x] Keep frozen v2/v3 `Authenticate`, `Authenticated`,
  `AuthenticationError`, and timeout wire behavior byte-identical.
- [x] Document the exact threat model: labels are replayable and provide access
  allowlisting, quota accounting, and accidental room isolation, not tenant
  identity against a hostile client.
- [x] Prove canonical and legacy config paths, replayable same-label context,
  unknown-label rejection, open mode, per-label rate limits, downstream
  connection-bound room isolation (including a real-socket two-label proof),
  and absence of retained secret material.

**Acceptance:** operator-facing configuration and documentation cannot imply a
credential the wire never carries; legacy deployments continue to load; no
client secret is retained or emitted; per-message room, spectator, reconnect,
and ready-state decisions continue to use the connection-bound app context; and
protocol-v2/protocol-v3 wire fixtures remain unchanged.

---

### P56 — H14 compatible-control hosted eviction closure (#290) (Size S) — 🟡 VALIDATING

Session 097's exact-head hosted validation produced the second intermittent
H14 eviction of the compact MessagePack compatible control on a Linux runner.
The JSON fallback completed all 5,000 exact outcomes with 0.01x amplification,
while the compatible proxy delivered at 32,702 B/s—essentially its configured
32 KiB/s rate—yet the connection closed as `4002 slow_consumer` after 3,818
deliveries. The identical-SHA retry passed.

- [x] Preserve decisive RED evidence through P51: both recipient terminals,
  exact semantic/wire counts, proxy destination-write rates, pump causes,
  control errors, backpressure, and eviction counters.
- [x] Falsify the historical #212 amplification signature and proxy
  under-delivery for this occurrence.
- [x] Add deterministic RED coverage for a slot released before the deadline
  whose waiting producer is not polled until afterward; preserve the existing
  exact/post-deadline expiry matrix for data and all control-capacity waits.
- [x] Implement classified-queue continuous-availability arbitration with
  lock-atomic deadline admission, and bound H14's two
  server-facing proxy receive windows while retaining the fixed burst, equal
  throttles, production queue/timeouts, sender-completion evidence, and every
  accountability/amplification/non-vacuity oracle.
- [x] Make every scheduled first attempt reviewable without survivor bias:
  preserve the exact Nextest/profile context and H14 selector after unrelated
  precursor failures, record outcome/completeness independently from
  eligibility under `h14-capacity-v1`, and immediately retain the raw log plus
  manifest for the repository's 30-day maximum. The manually audited first
  post-fix run remains in the unchanged cohort.
- [ ] Correlate the fix with first-attempt hosted behavior and validate the
  unchanged strong equal-fault oracle over at least 20 eligible scheduled H14
  attempts; buffered proxy throughput alone remains insufficient attribution.

**Acceptance:** H14 passes on the first attempt across the hosted evidence
window; a deterministic RED regression distinguishes the defect before the
fix; and the resolution explains how an on-rate compatible proxy reached
`SlowConsumer` while preserving exact fallback accounting, complete compatible
delivery, 0.01x-class amplification, and zero unintended evictions.

---

### P57 — Classified queue capacity-deadline arbitration proof (#220, #290) (Size S) — ✅ DONE

Session 099 adds an exhaustive state-space proof for the P56 boundary while
P56's hosted cohort continues independently. The model starts with one waiter
on a full classified lane, then explores writer drains before, exactly at, and
after the exclusive deadline; arbitrary producer scheduling delay; a competing
refill; and both orders of the refill-versus-admission race.

- [x] Mirror the queue-local `CapacityReleaseWitness`: retain the first
  full-to-non-full instant only while capacity remains continuously available.
- [x] Model validation plus enqueue/reservation as one atomic queue-lock action,
  with a competing producer able to take the slot first in the alternative
  interleaving.
- [x] Prove a pre-deadline continuous release cannot be falsely classified as
  `SlowConsumer` even when the producer runs after expiry.
- [x] Prove a release at/after the deadline cannot revive the wait, and a
  release→refill→release sequence cannot reuse stale evidence.
- [x] Prove lane capacity and the waiter's
  waiting/capacity-admitted/abandoned resolution are conserved across every
  reachable state, explicitly stopping before post-reservation permit
  send/drop/cancel lifecycle.
- [x] Pin the old timer-first behavior in an `_ExpectedFailure` configuration
  that passes only on the exact `NoFalseSlowConsumer` counterexample.

**Acceptance:** the corrected two-slot/three-tick model exhaustively checks all
76 distinct reachable states with no invariant violation; the seeded old
behavior reaches the intended delayed-poll false eviction and no unrelated
failure; the formal correspondence names every production action family. This
is one bounded #220 increment and does not replace P56's 20-attempt hosted
acceptance distribution.

---

### P58 — Membership, delivery, and session-plan state integrity (Size M) — ✅ DONE

Session 101 audited the room lifecycle, the classified outbound queue, and the
v3 signaling surface for state that survives a transition it should not, and
for contracts the documentation promises but no code emits. Six defects were
found directly, and three more by the sweep and review rounds that followed;
each is pinned by a deterministic test that fails on the previous behavior.

- [x] Derive the stored `is_authority` flag from the room's `authority_player`
  inside `add_player_to_room`, the single admission choke point, so a
  pre-disconnect snapshot cannot reinstate a role a successor now holds. Two
  flagged authorities would otherwise appear in `RoomJoined`, `Reconnected`,
  and `GameStarting`, and a client electing the host by scanning `is_authority`
  could pick the wrong peer.
- [x] Emit `AuthorityChanged { authority_player: null }` when a departure
  vacates the role, inside the same FIFO room event as its `PlayerLeft` and
  recorded for reconnect replay. `docs/concepts/authority.md` specified this
  from the start; the leave path — the choke point for both `LeaveRoom` and
  disconnects, including the forced-teardown branch that releases authority
  after a storage failure — emitted nothing, so the documented host-migration
  flow never fired.
- [x] Drop a joining id's stale readiness at admission. Reads filter the
  coordinator's ready set by current membership, an invariant that a rejoin by
  the same id silently breaks; reconnection, which resumes the same membership,
  deliberately keeps its readiness.
- [x] Carry a superseded `Latest` value's arrival instant onto its successor so
  the batched coalesce window measures the key's pendency. A key updated faster
  than `batch_interval` previously delivered nothing at all while still
  spending one `DeliveryReport` frame per update — bandwidth amplification with
  zero state sync, invisible to backpressure, sojourn, and eviction oracles.
- [x] Fence the queue when an omission is counted but its exact range is never
  held, which the full-pending-report path allows if the draining write is
  cancelled. The counter is clamped to the written frontier, so nothing would
  ever describe that hole.
- [x] Mint ICE only for a recipient that can pair in the session — nothing for a
  member whose empty peer list and relay fallback leave it nothing to gather
  against. This is _not_ the pre-gather rule: `ice_pregather_eligible` runs
  before a session exists and can only test the game's desired topology, so a
  member eligible there can still be non-pairable once the ladder settles.
  Count every committed TURN credential in `turn_credentials_issued` regardless
  of whether the joiner itself received a plan.

Three further fixes came out of the class sweep and the adversarial review
rounds:

- [x] Report readiness from the source that owns it for the room's state.
  Readiness is coordinator state while a room is open and moves into the room
  record at finalization, so `SpectatorJoined` (reading the record) showed an
  all-unready lobby and `RoomJoined` / `Reconnected` (reading the coordinator)
  showed an all-unready running game. All three now select by lobby state
  through one helper.
- [x] Keep a member that reconnects into a running game marked ready: removal
  prunes its id from the finalized room's ready list, so the restored
  membership carries the only surviving evidence, and a finalized room rejects
  `PlayerReady`. A rejoining member is still admitted unready — the join path
  constructs its record that way.
- [x] Drop a buffered `AuthorityChanged` from a reconnecting member's
  `missed_events` when the `Reconnected` snapshot already supersedes it, so the
  frame this branch's departure event newly makes possible cannot assert both
  "you are the authority" and "authority is vacant".

**Acceptance:** membership transitions leave no stale authority or readiness
behind, every room snapshot reports readiness from the source that owns it, the
documented authority-departure event reaches every remaining member in the order
that explains it, a continuously superseded `Latest` key keeps reaching the
socket within one coalesce window, no recipient receives credentials it cannot
use, and no omission can leave an undescribed hole. Frozen
v2/v3 wire shapes are unchanged: `AuthorityChanged` is an existing message and
no field was added, removed, or re-typed.

---

### P59 — Durable-repair and failure-response contract closure (#296, #297, #298) (Size S) — ✅ DONE

Session 102 closed the three issues session 101 filed but deliberately left
out of PR #295. Two are contract holes on paths a client depends on; the third
is the largest block of code in `src/` the server never executed.

- [x] Hand back the durable-detach retry a rejected reconnect took over
  (#297). Taking it over is required — maintenance must not delete a row the
  reconnect is about to make live — but a rejection that restored no membership
  of its own re-queued nothing, so the phantom row held a seat in every
  capacity check (`ROOM_FULL` for genuine joiners, never empty to cleanup)
  until the reconnection window expired. The uncommitted durable state is now
  one value that every rejection path unwinds, so a new early return cannot
  under-populate it, and the retry keeps its application-claim rollback
  provenance.
- [x] Answer a rejected `JoinAsSpectator` with `SpectatorJoinFailed` (#298).
  The message is documented and specified but had no emitter: failures produced
  a generic `Error`, so a client awaiting the documented pair — the contract
  `JoinRoom` already has with `RoomJoinFailed` — waited out its own timeout.
- [x] Report the documented cause of an authority denial (#298). Storage
  distinguished "unsupported room", "already held", and "not your role"
  internally and returned all three as an untyped string that the single
  coordinator site flattened to `AUTHORITY_DENIED` — telling a client that lost
  a race to stop retrying, and a client in a room that can never grant the role
  to retry forever. The reason and the code now come from one typed value.
- [x] Delete the unused zero-copy serialization surface and `rkyv` (#296),
  dropping 11 crates and ~1,347 `unsafe` occurrences from the shipped graph of
  a project that denies `unsafe_code` in its own manifests.

**Acceptance:** no failed reconnect can strand a seat that nothing will
reclaim; every `JoinAsSpectator` and every `AuthorityRequest` is answered by
the frame and code its documentation promises; and no shipped module
advertises an optimization that returns `NotImplemented`. Frozen v2/v3 wire
_shapes_ are unchanged — `SpectatorJoinFailed` and all four authority codes
already existed — while the frame and code a client receives for these two
failures change to the documented ones (recorded in the changelog with the
previous values named).

---

### P60 — Release-preparation integrity and dependency incorporation (#302, #305) (Size S) — ✅ DONE

Session 103 repaired the release pipeline after its successful workflow run
opened PR #304 in a deterministically test-red and semver-incompatible state.
The candidate changed the package identity to `0.5.3`, while a resolver test
still asserted `release/v0.5.2`; independently, Unreleased already contained
multiple explicitly breaking API changes whose recorded release floor is
`0.6.0`, but the workflow's default patch selection accepted them.

- [x] Derive release-resolver fixture identities and lockfile helper inputs from
  the package version being tested instead of the repository's previous
  version literal.
- [x] Run the release-preparation and standalone-lockfile contract tests on the
  prepared tree before any branch is pushed or pull request opened.
- [x] Fail before mutation when `**Breaking...:**` notes receive less than a
  minor bump during `0.x`, or less than a major bump after `1.0`.
- [x] Incorporate the open `lru` 0.18.2 dependency update (#302), including its
  upstream `pop` panic-safety fix on the coordination deduplication path.

**Acceptance:** release preparation cannot publish a candidate that fails its
own version-sensitive release tests, and a maintainer cannot accidentally label
explicitly breaking notes as a compatible patch/minor release. The generated
release diff remains restricted to canonical release files, and dependency
graphs stay locked and synchronized.

---

### P61 — Carry-forward protocol and evidence integrity (#296, #298) (Size S) — ✅ DONE

The adversarial review of P59 found two adjacent documentation/code mismatches
and one obsolete test surface. Frozen-v2 authority grants serialize
`reason: null`, but two guides claimed the field was absent and the canonical
sample invented a success string. A room disappearing during an authority
transition was also flattened to `AUTHORITY_DENIED` despite the typed denial
retaining `RoomNotFound`. Separately, the remaining performance test suite
defined local imitations of removed broadcast and `SmallVec` production types,
so it measured third-party primitives while claiming production coverage.

- [x] Make authority documentation and the canonical v2 sample match the frozen
  `reason: null` wire shape.
- [x] Map every typed authority denial exhaustively, including
  `RoomNotFound -> ROOM_NOT_FOUND`.
- [x] Delete mock-only broadcast/`SmallVec` characterization and timing tests,
  remove the direct `smallvec` dev dependency, and remove stale optimization
  guidance; real protocol round-trip coverage remains elsewhere.

**Acceptance:** every authority outcome carries the documented wire shape and
cause, the mapping cannot silently omit a typed denial, and performance claims
are backed by production paths rather than local lookalikes.

---

### P62 — Emitted error-code contract closure (#300) (Size S) — ✅ DONE

Six public Rust `ErrorCode` variants had no production emitter but were
advertised to code generators and clients as live server outcomes:
`INVALID_TOKEN` has no generic-auth emitter (reconnect token failures use
`RECONNECTION_TOKEN_INVALID`), `AUTHENTICATION_REQUIRED` duplicates the shipped
`MISSING_APP_ID` admission outcome, the three app-status codes belong to
future backends that the in-memory allowlist cannot produce, and
`SERVICE_UNAVAILABLE` exists as HTTP status 503 while WebSocket drain uses
`SERVER_DRAINING`.

- [x] Retain the Rust variants as documented decode/source-compatibility
  reserves, avoiding an unnecessary public API or deserialization break.
- [x] Remove the reserved tokens from the AsyncAPI emitted enum and client-facing
  outcome tables/examples; point guidance to the actual emitted outcomes.
- [x] Replace the one-way “every Rust variant appears somewhere” drift guard
  with exact equality between AsyncAPI and the Rust enum minus an explicit
  six-token reserved set, an exhaustive typed classification, and a
  production-source guard that rejects any unpinned reserved-variant reference.

**Acceptance:** code-generated clients expose exactly the codes Signal Fish
Server can emit, existing Rust consumers can still decode/match the legacy
variants, and adding either an undocumented emitter or a stale advertised token
fails the exact-set test.

---

### P63 — Live-interop diagnostic and stall-gate integrity (#301) (Size S) — ✅ DONE

Issue #301 recorded one nondeterministic hosted Windows native-WebRTC failure
and one Fortress WASM released-graph failure on a tree whose production code
was unchanged. Both passed on an identical-tree rerun. The first failure hid
the client process outcome and ICE evidence at the selected-pair assertion; the
second was caused solely by a single prediction-window-denied callback even
though every sustained-performance and correctness oracle passed.

- [x] Preserve each drained native client's exit code and include it, captured
  stderr, the complete emitted-event tag order, and every advertised local ICE
  candidate in a missing `selected_candidate_pair` failure.
- [x] Pin that diagnostic contract with a deterministic regression rather than
  relying on another hosted failure.
- [x] Classify the Fortress failure from its retained reports: one of 602
  joiner callbacks was denied at the eight-frame prediction boundary, while it
  confirmed frame 600, completed 1,317 messages at 123.1/s, kept its oldest
  queue entry to 17.5 ms, matched all nine checksums and both exact ledgers, and
  reported zero loss, overflow, retries, waits, or runtime errors. Smooth
  17.5/17.9/19.5 ms callback mean/p99/max does not support the original browser
  download / runner-I/O causal hypothesis.
- [x] Permit at most that one recovered denied callback in the hosted WASM
  released graph and fail on any repetition. Keep native P12 at zero stalls and
  retain every WASM progress, throughput, cadence, queue-age, lag, rollback,
  checksum, conservation, exact-zero error, and zero-wait oracle.
- [x] Make the one-stall boundary self-describing in the Rust report, exercise
  0/1/2 plus invalid counter values in the harness self-test, and structurally
  pin the production comparison and runner invocation. Bump the coupled Rust,
  browser-config, room-ready, bootstrap-error, and report contract to schema v3
  so an older exact v2 report cannot omit the new threshold and still pass.

**Acceptance:** the next native selected-pair failure names how far the client
got, what ICE candidates it advertised, its stderr, and its reaped exit code;
the WASM gate accepts zero or one recovered prediction-window denial, rejects
two, rejects stale v2 exports, and cannot pass by weakening any independent
gameplay/correctness oracle.

---

### P64 — Fortress Rollback 0.12 compatibility and supply-chain closure (#309) (Size S) — ✅ DONE

Issue #309 requested the current Fortress Rollback release. Both native and
Godot/no-thread WASM compatibility fixtures still exact-pinned 0.10.0, whose
graph directly used the unmaintained `bincode` 2.0.1 and therefore required a
narrow `RUSTSEC-2025-0141` exception in each standalone supply-chain policy.

- [x] Advance both exact registry pins and lockfiles to Fortress Rollback
  0.12.0 while keeping the released Signal Fish client, Godot, and toolchain
  identities unchanged.
- [x] Replace the old `bincode` graph with `bincode-next` 2.1.0 and remove both
  advisory exceptions.
- [x] Make the structural CI guard reject a stale Fortress pin, original
  `bincode`, a missing maintained codec, or a restored advisory exception in
  either standalone graph.
- [x] Extend the push, pull-request, and daily security inventory so every
  tracked Cargo graph receives both the `cargo deny` policy gate and the
  independent `cargo audit` RustSec scan, including exact-release fixtures that
  intentionally opt out of Dependabot.
- [x] Preserve the real native two-process 60 Hz rollback workload and every
  WASM progress, throughput, queue-age, rollback, checksum, conservation,
  cadence, runtime-identity, and expected-negative-control oracle.

**Acceptance:** both exact fixture graphs resolve Fortress Rollback 0.12.0
from crates.io without the original `bincode` or its advisory exception; the
native multiprocess game reaches its existing exact gameplay boundary, and the
pinned hosted Godot/Emscripten/Chromium gate proves the no-thread WASM graph.
Dynamic CI inventory rejects any tracked lockfile omitted from either security
scanner.

---

### P65 — Signal Fish Server 0.6.0 release preparation (#307) (Size S) — ✅ DONE

Session 106 completed the recovery required after the discarded 0.5.3
candidate. Prepare Release run `31270840717` started from the complete P64
`main` tree at `6a065e0`, used the required minor bump, passed its prepared-tree
release and standalone-lockfile gates, and produced the canonical eight-file
0.6.0 candidate. PR #311 merged that exact candidate as `3b1ad61` after its
required checks completed successfully.

- [x] Prepare from the complete green P64 tree with `bump=minor` and
  `dry_run=false`.
- [x] Produce exactly `0.6.0` on `release/v0.6.0`, restricted to the canonical
  release metadata, changelog, documentation, and lockfile set.
- [x] Pass `release_prepare_tests` and `workspace_lockfile_consistency` before
  publishing the release branch and PR.
- [x] Drive PR #311 through its required CI and merge the reviewed candidate.
- [x] Keep the discarded 0.5.3 candidate closed and unpublished.

**Acceptance:** P65 placed the internally consistent 0.6.0 package, changelog,
root and standalone lockfile, and public documentation identities on `main`.
Publication of crates, tags, releases, containers, and archives was the
separately reviewed second phase described by P11 and tracked by issue #312;
P66 has since completed it, while release preparation alone still does not
constitute publication evidence.

---

### P66 — Prepared-source release publication recovery (#314, #312) (Size S) — ✅ DONE

Session 107 found that P65's exact 0.6.0 source at `3b1ad61` was no longer the
default-branch tip after documentation PR #313 merged. With no `v0.6.0` tag,
the release resolver would have selected the later `5545daf` dispatch revision
and violated issue #312's cross-artifact source identity. Manual tag creation is
not an acceptable recovery because the release workflow owns that immutable
mutation.

- [x] Reproduce the unsafe source selection in the executable release fixture.
- [x] Resolve an absent tag to the unique first-parent commit that introduced
  the current Cargo package version.
- [x] Reject shallow histories and repeated version-introduction boundaries.
- [x] Preserve annotated-tag retry and direct-tag validation behavior.
- [x] Correct the stale development guide that instructed maintainers to create
  and push release tags manually.
- [x] Merge the reviewed workflow fix with all local and hosted checks green.
- [x] Dispatch and verify the separate 0.6.0 publication tracked by #312.

**Acceptance:** a default-branch dispatch after later documentation or workflow
changes still resolves `0.6.0` to prepared commit `3b1ad61`; ambiguous or
incomplete histories fail before mutation. Release run `31278681384` completed
successfully from merged PR #315. The annotated tag, unyanked crate, GitHub
Release assets and SBOM, and every versioned GHCR alias independently verify as
version `0.6.0` from source `3b1ad61`; session 108 records their immutable
identities and closes #312.

---

### P68 — Native selected-pair and browser-origin integrity (#301, #319) (Size S) — ✅ DONE

Session 110 found the current default branch red on the macOS native WebRTC
interop gate for the third occurrence of issue #301's
connected-before-selected-pair-statistics schedule. The live clients had both
data channels open and exchanged exact reliable and unreliable traffic, but an
immediate statistics snapshot omitted the already-selected ICE pair. The same
audit proved that `security.cors_origins` governed HTTP CORS responses but did
not authorize WebSocket upgrade `Origin` headers: a disallowed browser origin
received HTTP 101 on both enhanced protocol paths.

- [x] Observe the selected-pair statistics postcondition for a fixed one-second
  evidence budget without weakening the exact candidate-type/address oracle.
- [x] Add deterministic delayed-observation and deadline regressions at the
  native client seam.
- [x] Parse one strict origin policy for HTTP CORS and WebSocket upgrades;
  reject disallowed browser origins on both `/v2/ws` and `/v3/ws` before
  connection registration.
- [x] Reject malformed configured origins while deliberately preserving
  origin-less native-client compatibility and the development `*` policy.
- [x] Add real loopback upgrade regressions for explicit allow, explicit deny,
  absent origin, wildcard, and both enhanced protocol paths.
- [x] Merge the reviewed fixes with all local and hosted checks green.

**Acceptance:** after data channels connect, the native client emits exactly
one concrete selected candidate pair as soon as the WebRTC statistics
accumulator exposes it, or retains the loud failure after a bounded deadline.
For browser WebSockets, every enhanced upgrade with an `Origin` outside the
configured exact allowlist receives HTTP 403; the same parsed list controls
HTTP CORS, invalid configuration fails startup validation, and native clients
that omit `Origin` continue to connect.

---

### P69 — Single-allocation shared relay carrier (#207) (Size S) — ✅ DONE

Session 111 continues the measured relay optimization program at the production
ingress seam. P67 reduced the repeated-projection frame cache to 472 bytes, but
8- and 16-player relays still allocated the message and that cache in separate
`Arc` ownership domains. The two-player control needs no cache and therefore
showed the exact causal operation delta.

- [x] Register a failure-first production benchmark requiring two allocation
  operations per JSON and binary relay at every 2-/8-/16-player size; unchanged
  `main` fails at three operations for both larger rooms.
- [x] Add an owned-message coordinator compatibility seam and co-own newly built
  repeated-projection messages with their relay cache in one shared carrier.
- [x] Preserve that carrier through healthy enqueue, full/retry, batching,
  materialization, and trace correlation without cloning the message payload.
- [x] Exhaustively gate co-ownership on every actual recipient using a
  classified queue; pin the classified/classified/legacy ordering that exposed
  the original lazy-scan short circuit during adversarial review.
- [x] Retain the public boxed and borrowed-`Arc<ServerMessage>` compatibility
  seams and their existing allocation ceilings.
- [x] Measure the result: 8-/16-player production ingress falls from three to
  two allocation operations and from 1,120 to 1,104 bytes per relay; the
  two-player control remains two operations and 648 bytes.
- [x] Enforce both the production-ingress and exact projection allocation
  benchmarks in the hosted Relay Allocation Ceilings job.
- [x] Complete the full local validation, adversarial review, and exact-head
  hosted CI with zero findings.

**Acceptance:** every production relay allocates at most two times before queue
delivery regardless of room size; all exact wire digests, codec-work ledgers,
delivery accounting, retry identity, compatibility APIs, and the frozen-v2
floor remain unchanged. H14's one-direct/one-fallback workload does not enter
the repeated-projection path, so P56's four-attempt cohort remains intact. P53's
first three hosted allocations precede this source boundary; its records carry
the exact commit, and no mixed-source timing claim is permitted.

---

### P70 — Pre-sized JSON relay projection (#207) (Size S) — ✅ DONE

Session 112 continues the checked-in scientific optimization program at the
JSON socket-projection seam. The exact production benchmark showed that every
v3 JSON relay grew its output buffer three times: 2-player cells used seven
allocation operations and larger repeated-projection rooms used eight, while
the corresponding binary paths performed no reallocations.

- [x] Register a failure-first allocation ceiling requiring zero JSON relay
  growth reallocations; unchanged `main` fails at three per relay.
- [x] Derive a non-allocating capacity estimate from the existing JSON value
  and serialize directly into one pre-sized output buffer.
- [x] Retain exact growth for unusually escape-heavy values and pin empty,
  nested, Unicode, and escape-heavy wire equivalence.
- [x] Bound estimator work by both structure and float-format uncertainty so
  dense or float-heavy shapes use the one-pass reference serializer.
- [x] Preserve every checked-in wire digest and codec-work ledger.
- [x] Measure the deterministic result: JSON projection falls from 7/8 to 4/5
  allocation operations and from 2,538/3,010 to 1,627/2,099 allocated bytes per
  relay in 2-/8-/16-player rooms.
- [x] Compare the uninstrumented release benchmark against a same-session
  baseline; every JSON room-size cell improves, including the serialization-
  dominated two-player control.
- [x] Complete the full local validation, adversarial review, and exact-head
  hosted CI with zero findings.

**Acceptance:** JSON relay serialization uses no growth reallocations for the
representative production workload; empty, nested, Unicode, and unusually
escape-heavy values retain byte-identical output; frozen-v2 bytes, delivery
accounting, cache reuse, and codec-work counts remain unchanged; and the
uninstrumented runtime comparison shows no regressed room-size cell.

---

### P71 — Allocation-free healthy relay handoff (#207) (Size S) — ✅ DONE

Session 113 follows the remaining fixed allocation in the production ingress
benchmark past message/cache ownership and the zero-allocation classified
queue. A prebuilt two-player relay still allocated one 352-byte async-trait
completion state, and production ingress added that state to its one message
or co-owned message/cache allocation at every room size.

- [x] Register a failure-first production-ingress ceiling of one allocation per
  relay; unchanged `main` uses two and fails the proposed bound.
- [x] Add a hidden, object-safe coordinator try-start seam whose default retains
  compatibility for alternate coordinator implementations.
- [x] Acquire the process-local routing snapshot synchronously when uncontended
  and complete healthy queue delivery without constructing an async future.
- [x] Preserve the existing async path without consuming the one-shot message
  builder when either routing lock is contended.
- [x] Allocate and await completion only after a queue actually reports
  backpressure or a synchronous slow-consumer outcome; release routing locks
  before that wait.
- [x] Prove contention fallback and backpressured completion, including the
  original recipient snapshot, shared carrier identity, and late registration.
- [x] Measure the deterministic result: JSON and binary production ingress both
  fall from two allocations to one, and from 648/1,104 to 296/752 allocated
  bytes per relay in 2-player versus 8-/16-player rooms.
- [x] Tighten the existing hosted Relay Allocation Ceilings job to enforce the
  one-allocation and exact-byte boundary.

**Acceptance:** the healthy process-local JSON and binary relay handoff performs
one message/carrier allocation and no async-completion allocation at every room
size; contended routing and backpressured recipients retain the existing
ordering, retry, deadline, pruning, and one-shot-builder behavior; alternate
coordinator implementations retain their default async compatibility path; and
no wall-clock latency claim is inferred from an instrumented allocator.

---

### P72 — Borrowed healthy relay handles (#207) (Size S) — ✅ DONE

Session 114 follows the healthy relay handoff beyond heap allocation counts to
its per-recipient ownership work. The guarded routing walk cloned each
`ClientDeliveryHandle` before the delivery state machine discovered whether it
needed ownership, even though only backpressure or slow-consumer pruning can
outlive that synchronous attempt.

- [x] Extend the production relay allocation benchmark with a non-vacuous
  classified-sender clone ledger.
- [x] Prove the unchanged implementation RED at one handle clone per recipient
  per relay; the first two-player cell records 4,096 clones for 4,096 relays.
- [x] Borrow delivery handles during guarded healthy fan-out and clone only
  when backpressure state or slow-consumer cleanup must retain ownership.
- [x] Preserve the routing snapshot, shared message/cache carrier, delivery
  ordering, capacity deadline, and post-wait pruning behavior.
- [x] Prove zero handle clones across JSON and binary production ingress and
  prebuilt coordinator handoff at 2, 8, and 16 players without changing any
  allocation, byte, delivery, or wire ledger.
- [x] Compare the uninstrumented relay runtime with a same-session baseline;
  larger-room cells improve or remain within noise, and an isolated rerun of
  the initial two-player JSON outlier improves rather than regresses.

**Acceptance:** every healthy routed relay borrows registered delivery handles
from the guarded routing map and performs zero sender-handle clones; exceptional
backpressure and slow-consumer cleanup retain owned state only after the initial
queue attempt; existing delivery and allocation ceilings remain exact; and the
uninstrumented runtime comparison shows no reproducible regression.

---

### P73 — Reserved control-permit lifecycle conservation (#220) (Size S) — ✅ DONE

Session 115 closes the lifecycle boundary that P57 deliberately stopped at:
what happens after classified control capacity has been atomically reserved.
The production queue has separate reservation, permit-producer, accepting,
generation, room-scope, commit, cancellation, and destructor states; these
govern session-plan and lifecycle control delivery even though the common paths
already had focused unit coverage.

- [x] Add a two-permit bounded TLA+ model from ordinary and transition
  reservation through commit, explicit cancellation / `Drop`, receiver close,
  accountability failure, last-sender drop, receiver polling / wake, and a
  competing generation transition. Choose ordinary versus transition message
  kind nondeterministically at commit, matching production rather than
  predicting it at reservation.
- [x] Prove exact `queued + reserved` capacity, reservation / producer-count
  agreement with the permit's active state, logical-control conservation,
  no terminal EOF overtaking a committable permit, no stale-scope commit, and
  exact notification of an already-waiting receiver after permit progress.
- [x] Seed six independent expected failures: omit the permit producer from
  receiver liveness; omit release on `Drop`, failed send, or stale cancel; skip
  commit-time scope validation; and omit the receiver notification. Require
  each run to name only its intended invariant.
- [x] Extend the TLA runner to resolve the longest checked-in module prefix so
  one model can carry several named expected-failure configurations without
  weakening their exact diagnostic oracle; pin multiword and underscore-module
  resolution plus missing-module rejection in a hermetic script regression.
- [x] Add production-seam regressions for invalid payload release, two-permit
  mixed Drop / commit, `DeliveryPermit` transition commit, held-permit send
  after receiver close and accountability failure, and stale generation
  cancellation followed by a competing refill. Pin already-waiting receiver
  wakeups for ordinary stale, transition stale, and defensive unscoped
  transition cancellation separately.
- [x] Fix the divergence exposed by the deterministic wake proof: stale permit
  cancellation now notifies an already-parked receiver when consuming the
  final producer capability, so it observes terminal EOF instead of hanging.

**Acceptance:** the exhaustive two-permit, two-slot lifecycle model passes with
all bugs disabled; each seeded defect fails only its named invariant;
paused/concurrent Rust tests assert exact reservations, permit counts, queue
contents, terminal state, and already-pending receiver wakeups without sleeps;
the full formal and Rust suites pass; and P53/P56's hosted cohorts remain
unchanged because no hosted workload or timing threshold changed.

---

### P74 — Exact room-publication transaction refinement (#220) (Size M) — ✅ DONE

Session 116 composes P73's classified-permit lifecycle at
`commit_room_messages_if_members_with_hook`, the shared publication primitive
used by start-game, finalized join, reconnect, and host replan. The existing
proof stopped at one classified recipient's held permits, while the production
transaction reserves several recipients across ordered phases, validates their
routes, awaits a durable hook, and can degrade per permit after that hook.

- [x] Add a bounded two-recipient, two-phase TLA+ model covering arbitrary
  reservation order, membership or route-generation change before final
  validation, hook accept / reject / error, post-hook close or generation
  staleness, the phase-zero decision callback, dependent cancellation, and
  terminal permit release.
- [x] Prove complete reservation and exact routing validation before the hook,
  zero publication before hook acceptance, exact phase boundaries and one-shot
  callback input, per-frame terminal conservation, exact degraded-frame
  accounting, and zero terminal reservation or producer leak. Import P73's
  separately mutated no-stale-scope-commit lemma for synthetic composition at
  the transaction seam instead of claiming a second proof of it here.
- [x] Seed seven independent expected failures for premature hook invocation,
  skipped routing validation, phase inversion, duplicate callback, incorrect
  callback failure input, incorrect degraded-frame count, and omitted permit
  release. Require the runner to report each exact invariant name.
- [x] Add classified high-level Rust regressions for minimum two-slot progress,
  a synthetic permit-scope invalidation during the async hook with both
  callback decisions,
  route replacement before the hook, hook rejection and error, reusable
  capacity, and production `recv_batched` wake-to-EOF after final stale permit
  cancellation. Use deterministic synchronization and no sleeps.
- [x] Correct the formal correspondence documentation: process-local same-room
  mutation is serialized by the shared mutation guard and FIFO event lane;
  post-hook receiver closure remains a live degradation boundary, while normal
  sender replacement is blocked by the transaction's routing read guards.
- [x] Audit the production implementation against the model and regressions.
  The falsification pass found no production divergence, so this phase changes
  proof, tests, and correspondence documentation only.

**Acceptance:** the healthy model reaches 1,091 distinct states at depth 16;
all seven independently seeded defects fail their exact named invariants;
classified transaction tests assert ordering, callback input, degraded totals,
zero partial publication, capacity reuse, and terminal wakeups without sleeps;
the full formal and Rust suites pass; and P53/P56 retain their unchanged hosted
cohorts because no workload or timing threshold changed.

---

### P75 — Sparse room-transaction and pre-hook retry closure (#220) (Size S) — ✅ DONE

Session 117 adversarially rechecked P74 against every production batch shape
and reservation outcome. The symmetric two-recipient/two-frame proof was
sound for its stated bound, but it was not a reduction of the sparse shapes
the shared primitive accepts, and it stopped after permit admission rather
than modeling a canceled scoped waiter and the outer retry.

- [x] Generalize the bounded model to one or two members whose batches may be
  empty, phase-zero-only, phase-one-only, mixed across members, or full. Keep
  all-empty transactions excluded, matching production validation.
- [x] Preserve exact route and connection-generation validation for members
  that reserve no physical frame, and prove correct callback timing/input and
  `Committed` versus `CommittedDegraded` outcomes when either phase is empty.
- [x] Model one stale pre-hook reservation attempt that releases every held
  sibling permit, retains both one-shot callbacks, refreshes, and retries;
  model terminal closed/slow-consumer cleanup before a `RoutingChanged`
  outcome as well.
- [x] Add independent expected failures for omitted retry release, separately
  consumed callbacks, stale-snapshot reuse, and skipped empty-member routing
  validation. Retain all seven P74 mutants and their exact diagnostics.
- [x] Add deterministic classified production-seam coverage that commits a
  reserved next-generation transition while an old-generation transaction
  waiter is blocked, then proves six complete reservation attempts, exact
  three-way cancellation, one hook and callback invocation on retry, complete
  phase-ordered delivery, and reuse of both physical capacity slots.
- [x] Add a data-driven empty-member/phase-one regression proving healthy
  callback and delivery behavior plus fail-closed `RoutingChanged` when only
  the zero-frame member changes either legacy channel or classified generation.
- [x] Audit production against the expanded proof. No production divergence
  was found; this phase corrects proof completeness, deterministic coverage,
  and correspondence claims without changing runtime behavior.

**Acceptance:** the healthy model exhausts all 15 labeled nonempty
two-recipient frame plans at 228,040 distinct states and depth 25 plus all three
singleton plans at 972 states and depth 15; a 35-state fair witness forces the
complete stale-generation retry through commit; all twelve seeded bugs fail
with their exact named invariant; both new regressions pass without sleeps;
the full formal and Rust suites pass; and P53/P56 retain their unchanged hosted
cohorts because no workload or timing threshold changed.

---

### P76 — Exact room-event mutation handoff (#220) (Size S) — ✅ DONE

Session 118 proves the process-local boundary that makes the per-room session
models' atomic-event abstraction valid. The production lane is deliberately
not a general FIFO of independently mutated jobs: one mutation guard moves
synchronously into one owned job and remains held through that job's terminal
result before the next same-room producer may mutate.

- [x] Model acquire, mutation, synchronous guard transfer, queued/running job,
  success, explicit error, isolated panic, caller completion-drop, the
  drain-empty/new-enqueue race, and weak-registry replacement cleanup.
- [x] Prove exact mutation/admission/start/terminal order, guard ownership for
  every live job, zero mutated-event loss after caller cancellation, the
  same-room terminal barrier, exact queue/active-worker state, panic recovery,
  and active-generation registry protection.
- [x] Seed four independent bugs for early guard release, caller-driven job
  cancellation, panic-stranded drain state, and stale cleanup deleting a
  replacement entry; require each targeted configuration to report its exact
  diagnostic even though later invariants may cascade after the seeded break.
- [x] Add a non-vacuous recovery witness that reaches detached completion,
  isolated panic, weak-lane replacement, and successful next-event commit.
- [x] Add deterministic Rust regressions for dropped completions, error/panic
  recovery, cross-room independence, and stale pointer-identity cleanup.
- [x] Keep the theorem explicitly process-local: it assumes an active runtime,
  does not promise multi-node ordering, and makes no liveness claim for a job
  whose own future remains blocked.

**Acceptance:** the healthy model exhausts 1,704 distinct states at depth 19;
all four mutants fail on their exact named invariant; the recovery witness is
reachable; focused Rust regressions pass without sleeps; the full formal and
Rust suites pass; and P53/P56 retain their unchanged hosted cohorts because no
scheduled selector, workload, threshold, or contract version changed.

---

### P77 — Room-scoped routing isolation and async lock containment (#220, #329) (Size M) — ✅ DONE

Session 119 closes the production-composition gap behind P76's cross-room
independence claim. The in-memory coordinator previously retained two
process-global routing-map locks across room-local durable hooks, replay hooks,
and async join/reconnect baseline construction. One paused room could therefore
block route changes everywhere; Tokio's writer preference could then queue later
relay reads behind that unrelated writer.

- [x] Preserve the failure-first production regression: a paused room-A exact
  transaction on unchanged `main` prevents room-B registration from completing.
- [x] Replace the process-global asynchronous fence with stable keyed room gates;
  retain global maps only for brief snapshots and updates, and retain active gates
  so the healthy relay path does not allocate a new lock per frame.
- [x] Serialize one player's cross-room routing changes, acquire all affected room
  gates in canonical UUID order, and reclaim inactive room/player gate entries
  without stale-destructor deletion.
- [x] Move exact transactions, replay-hook broadcasts, sync/async initial
  registration, game-data snapshot/stamp builders, route mutation, exact routed
  lookups, terminal tails, and slow-consumer pruning under the same room fence.
- [x] Prove with deterministic barriers that paused transaction, replay, and
  baseline work blocks same-room mutation/publication but not unrelated-room
  routing or relay; prove opposite reroutes terminate with exact unique routes.
- [x] Preserve the P71 one-allocation relay ceiling, P72 zero healthy handle clones,
  P74/P75 transaction outcomes, reconnect replay/watermark ordering, terminal-tail
  ordering, mutation inventory, and every scheduled P53/P56 cohort contract.

**Acceptance:** routing-fenced async work for room A retains the existing exact
same-room membership/publication boundary while room B continues both lifecycle
mutation and relay; cross-room reroutes cannot deadlock or leave duplicate routes;
inactive gate state is reclaimed safely; deterministic tests use explicit
notifications rather than sleeps; all relay allocation/clone ceilings and the
full Rust, formal, Z3, policy, documentation, and hosted-CI gates pass; P53/P56
remain evidence-only at 4/20 per OS and 5/20 respectively.

---

### P78 — Selected ICE-pair evidence lifecycle and dependency incorporation (#301, #332) (Size S) — ✅ DONE

Session 120 closes the selected-path evidence race that recurred on PR #332's
macOS live WebRTC lane after P68's one-second statistics wait. ICE connected,
both data channels opened, exact reliable and unreliable traffic crossed in
both directions, and the client exited zero, but the wrapper statistics
snapshot still omitted the selected pair when that fixed evidence budget
expired. This falsified the timeout as a correctness boundary.

- [x] Preserve exact selected candidate types and concrete addresses as a
  fail-closed live-transport requirement; do not weaken the harness oracle or
  retry the hosted workflow into silence.
- [x] Take one immediate statistics snapshot when the physical pair opens, then
  let gameplay exchange and transport-status publication proceed without
  waiting behind diagnostic convergence.
- [x] Retain a generation-local evidence obligation for every connected pair,
  poll it on the normal event loop, and block clean success until it is emitted
  or the existing whole-run deadline fails with explicit unmet criteria.
- [x] Clear only the affected obligation on transport loss, retry, authoritative
  replacement, or removed membership so unrelated pairs remain accountable and
  stale generations cannot satisfy current evidence.
- [x] Replace the former bounded-wait tests with a paused-time regression that
  crosses the old one-second boundary for more than 100 retries, proves the
  client remains ineligible for exit zero, and proves observation or teardown
  closes exactly the intended obligation.
- [x] Incorporate PR #332's grouped `rust-cache`, `install-action`, and `typos`
  action updates into this single session PR and retain workflow-policy parity.

**Acceptance:** focused lifecycle tests and five consecutive live two-process
WebRTC exchanges pass locally; Linux, Windows, and macOS hosted live lanes emit
one selected pair per connected physical generation and pass exact exchange
oracles; every local and hosted gate is green; issue #301 closes only after the
cross-platform evidence passes; P53/P56 selectors and cohorts remain unchanged.

---

### P79 — Numeric-conversion safety and lint enforcement (#213) (Size S) — ✅ DONE

Session 121 enables Clippy's truncation, wrap, and sign-loss cast checks across
every root target. The failure-first sweep found 59 reports, including an
unrestricted `u64` reconnection window narrowed to `i64`; values above
`i64::MAX` wrapped negative and minted immediately expired reconnect tokens.

- [x] Keep reconnection-window state unsigned end to end and saturate durations
  beyond Chrono's representable range at `DateTime::MAX_UTC`.
- [x] Replace float-to-index dashboard percentile selection with exact
  per-mille integer arithmetic that remains bounded at `usize::MAX`.
- [x] Make duration, timestamp, capacity, counter, schema, and test-fixture
  narrowing explicit through checked or saturating conversions.
- [x] Enforce `cast_possible_truncation`, `cast_possible_wrap`, and
  `cast_sign_loss` for library, binary, unit, and integration-test targets.
- [x] Preserve wire formats, timeout values, percentile labels, and all
  scheduled P53/P56 cohort definitions.

**Acceptance:** an extreme reconnection window remains valid instead of
expiring immediately; percentile ranks match the prior nearest-rank results
without float-to-integer casts or overflow; all-target/all-feature Clippy emits
zero numeric-cast warnings; the complete local and hosted gates pass.

---

### P80 — Direction-aware Rust panic fast gate (#318) (Size S) — ✅ DONE

Session 121 follows the completed correctness sweep with the next usability
priority required by the repository's hook budget. Representative Rust
worktree preflights repeatedly took 2.6–2.8 seconds because any added
`#[test]`/`#[cfg(test)]` marker loaded the full line-aware panic scanner.

- [x] Use one zero-context Git diff to classify relevant panic-policy changes.
- [x] Load the full scanner for added panic macros and removed test guards.
- [x] Keep untracked production Rust and Git failures fail-closed.
- [x] Pin added/removed marker behavior in the PowerShell policy fixture.
- [x] Overlap worktree status and Rust-diff I/O with hook policy setup.
- [x] Preserve the PowerShell-and-Git-only hook dependency boundary.

**Acceptance:** after one warm-up and with no concurrent Cargo or hook process,
five consecutive representative worktree profiles and five consecutive staged
profiles complete below 1,000 ms; contention outliers remain diagnosed by the
per-check report rather than becoming a brittle CI wall-clock assertion. Added
panic macros and removed test guards still select the full line-aware scanner;
all hook policy tests and complete local and hosted gates pass.

---

### P81 — Direct peer-reflexive evidence and npm dependency maintenance (#337, #341) (Size S) — ✅ DONE

Session 122 closes the cross-platform selected-path failure first recorded on
Windows and then reproduced on macOS `main`. rtc can receive an inbound ICE
connectivity check before the sender's trickled candidate reaches the peer,
creating a real direct peer-reflexive remote candidate whose registry entry and
address never appear in rtc 0.20's public statistics.

- [x] Accept remote `prflx` only in the family-agnostic no-STUN/no-TURN smoke
  cell, with a selected local host and both clients' non-empty host-only UDP
  advertised-candidate sets.
- [x] Keep IPv6 exact host/host with concrete dialable IPv6 addresses and TURN
  exact relay/relay; reject srflx, relay, non-host local, absent peer evidence,
  and host-without-address shapes in the baseline.
- [x] Keep gameplay exchange independent from selected-pair statistics and
  report both clients' complete selected event, candidate set, exit code, and
  stderr on a field-level failure.
- [x] Incorporate PR #341's `@types/node` update and make dependency-only
  changelog classification cover every tracked Cargo and npm manifest/lock.
- [x] Remove nonexistent npm Dependabot labels that generated configuration
  errors while preserving dependency and automation routing.

**Acceptance:** the deterministic peer-reflexive fixture fails against the
former strict baseline and passes the scoped oracle; negative fixtures and the
unchanged IPv6/TURN paths fail closed; repeated live two-peer exchanges and the
complete local/hosted matrices pass without retry; npm-only dependency changes
skip only the changelog gate while any production-file co-change still requires
an Unreleased entry.

---

### P82 — Terminal inactive-room cleanup convergence (Size S) — ✅ DONE

Session 123 closes a storage/routing split-brain in occupied-room expiration.
The durable cleanup deleted a room after `inactive_room_timeout`, but its
count-only result left seated clients assigned and routed locally, so stale
JSON and binary gameplay could continue relaying through a room that no longer
existed and later teardown could attempt to preserve it for reconnect.

- [x] Sweep every connected room assignment against authoritative storage after
  room cleanup, with a final check under the connection lifecycle gate.
- [x] Reuse the room-event lane and terminal relay watermark transition to
  generation-fence assignment and routing removal.
- [x] Discard pre-issued and pending reconnect state before unregistering each
  member of a deleted room.
- [x] Send a best-effort `ROOM_NOT_FOUND` farewell and pin stable WebSocket close
  code `4005 room_inactive`, still allowing shutdown to win a concurrent drain.
- [x] Keep the all-client reconciliation pass retryable across task cancellation
  and transient storage errors.

**Acceptance:** a production-shaped maintenance tick deletes a two-player
inactive room, removes both connection-manager assignments and coordinator
routes, leaves no reconnect records, terminates both sockets with `4005`, and
cannot relay later gameplay from a former member. The test fails against the
former count-only cleanup path; close-code mappings, documentation, complete
local gates, and hosted CI pass.

---

### P11 — Git-tagged releases + versioned GHCR containers (Size S) — ✅ DONE

**Schedule this before the next crates.io publish or GitHub Release.** A public
release is not complete until its source and container artifacts share one
verifiable version identity.

The current workflows contain both halves of this behavior: `release.yml`
creates an annotated `vX.Y.Z` tag for a manual release, and
`docker-publish.yml` maps a `vX.Y.Z` tag-push event to GHCR tags such as
`:vX.Y.Z`, `:X.Y.Z`, and `:X.Y`. The historical missing guarantee was
orchestration: a tag pushed with a workflow's `GITHUB_TOKEN` does not start
another workflow. The completed implementation therefore invokes container
publication directly instead of relying on that suppressed event.

- [x] Use one canonical release version, validated against `Cargo.toml` and the
  changelog, and ensure the annotated Git tag `vX.Y.Z` points at the exact
  commit being published.
- [x] Invoke container publication directly from the release path (for example,
  through a reusable workflow or a release job) instead of depending on a
  second workflow being triggered by the release workflow's tag push. Direct
  human tag pushes must remain supported.
- [x] Build once for the supported multi-architecture manifest and publish that
  same digest as `ghcr.io/<owner>/<repo>:X.Y.Z` and
  `ghcr.io/<owner>/<repo>:vX.Y.Z`; retain the existing immutable `sha-*` tag and
  the documented rolling-tag policy.
- [x] Fail closed when the Git tag, crate/GitHub release version, container
  labels, or GHCR tags disagree. Make retries idempotent so a partial release
  can safely finish without moving an existing version tag to different bytes.
- [x] Add workflow-policy regression tests covering both manual releases and
  direct tag pushes, including an assertion that the release path does not rely
  on a `GITHUB_TOKEN`-generated event to start container publication.
- [x] Add a release-runbook verification step that resolves `:X.Y.Z` from GHCR,
  checks its manifest platforms and source revision, and records the digest in
  the GitHub Release notes or artifacts.
- [x] Backfill versioned GHCR tags for already-published releases where the
  corresponding Git tag and source commit are available; rebuild from that tag
  rather than aliasing a historical version to `latest`. The session-035 audit
  found `v0.4.0` / `0.4.0` absent; dispatch **Docker Publish** with
  `release_tag=v0.4.0`. The backfill deliberately
  preserves the existing `sha-50b28a9` digest because its historical OCI
  version label is `latest`, then rebuilds the version tags from annotated tag
  commit `50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8`.

**Acceptance:** releasing `X.Y.Z` produces an annotated Git tag `vX.Y.Z`, a
GitHub Release for that tag, and a pullable multi-architecture image at
`ghcr.io/<owner>/<repo>:X.Y.Z` (plus `:vX.Y.Z`) whose OCI source revision is the
tagged commit. The release gate fails if any one of those artifacts is missing
or inconsistent. The workflow/policy implementation meets this bar for the next
release and is fully green in PR #160 at `940ac28`. Session 053 closed the
historical gap with successful Docker Publish run `29602078436`: `v0.4.0`,
`0.4.0`, and `0.4` share release digest
`sha256:09b127664e8572b67ec9d9cbb84da30092c7721d3638b4b383bd2c41f7772389`.

**Operational finding (Session 053):** the first canonical dispatch, Actions
run `29595222223`, failed before building because its historical source checkout
did not contain `scripts/reuse-verified-image.sh`. The workflow now checks out
publication helpers from `job.workflow_sha` separately from the immutable
source checkout and passes only `source/` to Buildx. Retry run `29602078436`
succeeded from the reviewed workflow commit. Independent verification confirmed
exactly `linux/amd64`, `linux/arm64`, and `linux/arm/v7`, with revision
`50b28a9a13dc2b99d301bfb2482c5fd6f768a2e8` and version `0.4.0` on every
runtime manifest. The pre-existing `sha-50b28a9` digest remained
`sha256:8d9f52aa6c95eba858825ce3f238f9f7b92eecaea89f50c25662b81b8fbbbdd3`.

**Historical 0.5.0 audit (Session 058):** the published `latest` and `0.5.0`
indexes both contain exactly `linux/amd64`, `linux/arm64`, and `linux/arm/v7`.
The canonical verifier also proved that `v0.5.0`, `0.5.0`, and `0.5` share
digest `sha256:af1f3b965f8ec7e7f7678112bd260485d092669d1bba49f7dc0d4eb0849487c8`
and carry tagged source revision
`16ac09b042436b6dbc5cca0b68c462eb2a8ab33f` plus version `0.5.0` on every
runtime manifest. This fresh registry evidence closes issue #122 as completed;
the workflow and Dockerfile policy tests remain the regression guard.
The current 0.6.0 source, crate, release, binary, and GHCR identities are
recorded in P66 and `progress/session-108-signal-fish-060-publication.md`.

---

### P12 — Fortress Rollback relay interoperability regression (Size S) — ✅ DONE

Fortress Rollback issue 242 reported a Godot single-threaded WASM game becoming
extremely slow when a custom Signal Fish relay adapter fed its polling client.
The client repository owns browser/Godot transport behavior; this repository
owns the complementary release-client-to-current-server acceptance boundary.

- [x] Add a standalone `clients/fortress` crate outside the server package,
  initially exact-pinning `fortress-rollback = 0.10.0` and the published
  `signal-fish-client = 0.8.0` with its own lockfile and supply-chain policy.
  P64 later advanced the maintained Fortress pin to 0.12.0 without changing
  the released Signal Fish client boundary.
- [x] Spawn the current checkout's server plus two separate Fortress game
  processes using real loopback WebSockets, protocol-v3 MessagePack relay, and
  exactly one polling-client call per 60 Hz game callback.
- [x] Preserve the issue's destination-UUID framing while replacing global
  stop-and-wait behavior with a bounded, ordered non-blocking adapter. Retain
  enqueue timestamps until the client counter proves transport completion.
- [x] Advance at least 600 confirmed frames and require more than two outgoing
  game-data messages per callback so a one-send-per-frame regression cannot
  satisfy the test accidentally.
- [x] Gate at least 120 Fortress messages/s, complete send/receive ledgers,
  final queue drain, oldest-frame age at most 500 ms, rollback and confirmation
  lag at most eight frames, matching checksums, and zero stalls, wait
  recommendations, overflow, malformed frames, unknown senders, or decode loss.
- [x] Add a dedicated path-filtered CI workflow, MSRV parity, exact dependency
  pins, current-checkout binary guards, client documentation, and a changelog
  entry.

**Acceptance:** `bash scripts/run-fortress-interop.sh` builds the signaling
server from the checkout, lints the fixture warning-free, and passes the real
three-process regression. CI independently audits the fixture dependency graph.

---

### P13 — Fortress + Rust-client single-threaded WASM interoperability (Size M) — ✅ DONE

P12 proves the released Rust client and Fortress integration on a native
polling path. It does **not** answer the original issue-242 question: whether
the same released client remains healthy when Fortress and networking share a
single Godot browser thread. The existing P7 browser suite also cannot answer
that question because it runs the TypeScript reference client rather than the
Rust client compiled to WASM.

- [x] Add a standalone `clients/fortress-wasm` fixture with its own lockfile
  and supply-chain policy. Initially exact-pin `fortress-rollback = 0.10.0`
  (advanced to the maintained 0.12.0 in P64) and exact-pin
  `signal-fish-client = 0.9.0` and its lockstep
  `signal-fish-client-godot = 0.9.0` adapter; enable the core polling client and
  compile the fixture as a Godot 4.5 no-thread web extension for
  `wasm32-unknown-emscripten`. Do not use a path or Git override of either
  production dependency in the acceptance cell.
- [x] Pin the Godot editor/export-template version and verify downloaded
  artifacts before building. Add a compile-only target check, but do not count
  compilation as interoperability success.
- [x] Export a minimal deterministic Godot project that embeds the real
  Fortress session, the issue's destination-UUID relay framing, and
  `SignalFishPollingClient` over Godot `WebSocketPeer`. The exported page must
  expose its dependency versions, WASM target, build identity, and
  single-thread/no-worker status so a native or JavaScript fallback cannot pass
  unnoticed.
- [x] Start the current checkout's signaling-server binary and two independent
  headless-Chromium processes serving two real Godot web exports over HTTP.
  Each peer must advance networking only from the real main-thread game
  callback: one Signal Fish poll per nominal 60 Hz callback, no worker, no
  `SharedArrayBuffer`, and no synthetic tight-loop clock. Disable browser
  background throttling explicitly and record observed callback intervals.
- [x] Run the same deterministic input seed and bounded relay adapter in native
  P12 and WASM P13 modes. Emit a machine-readable per-peer report containing
  callbacks, wall time, Fortress messages admitted/completed/received,
  outstanding depth and oldest age, confirmed/current frame, rollback depth,
  confirmation lag, wait recommendations, stalls, loss/overflow/malformed
  counts, and checksum samples.
- [x] Require both WASM peers to reach at least 600 confirmed frames, sustain at
  least 120 completed Fortress messages/s during the active phase, complete
  more than two sends per game callback, drain all adapter/client queues, keep
  oldest outstanding age at most 500 ms and rollback/confirmation lag at most
  eight frames, compare at least eight matching checksums, and report zero
  waits, overflow, malformed frames, unknown senders, decode loss, or completion
  underflow. The hosted WASM graph may recover from one prediction-window-denied
  callback; a second stall fails the gate.
- [x] Make the verdict explicit and fail closed. A build failure, WASM trap,
  browser console/page error, WebSocket close, timeout, missed metric, native
  fallback, or threshold violation is `BUSTED`; only two complete reports that
  satisfy every gate are `HEALTHY`.
- [x] Add a bounded negative-control mode that admits at most one relay send per
  callback. The test harness must classify that run as `BUSTED` because it
  violates throughput/progress gates, proving the healthy cell cannot pass with
  the issue's stop-and-wait behavior. The expected negative verdict must not be
  represented as a failing CI job.
- [x] Add `scripts/run-fortress-wasm-interop.sh` and a dedicated path-filtered
  CI workflow. Cache only checksum-pinned Godot/Chromium artifacts, upload the
  two machine-readable reports plus browser/server logs on failure, enforce a
  bounded wall-clock timeout, and add structural CI tests that preserve the
  exact dependency pins, real WASM/Godot/Chromium execution, single-thread
  proof, current-checkout server build, negative control, and all verdict gates.
- [x] Document the result as a two-row native/WASM compatibility table. Do not
  generalize a Chromium result to every browser. The deterministic primary
  Godot/Chromium reproduction remains the PR gate; session 059 adds a weekly
  Firefox cell with the same released-client and negative-control oracles.

**Historical 0.8.0 result:** exact-head Chromium CI reached 566/568 confirmed frames in
32.5/34.2 seconds before the original diagnostic deadline. The peers admitted
1,455/1,476 relay messages with maxima of 3/5 admissions per callback, while
the released client completed 1,442/1,476 sends over 1,444/1,517 active
callbacks. That is the issue-242 completion bottleneck, not an adapter admission
cap. The characterization now allows the full 600-frame reports to settle and
requires this known completion failure (and no unrelated healthy-gate failure)
as an expected `BUSTED` result.

A subsequent exact-head run continued linearly to 403/408 confirmed frames at
the 50-second diagnostic deadline, with 1,233/1,275 completions and multi-send
admission intact. Its 37.5/37.6 ms callback means also missed the healthy
cadence bound, demonstrating runner-dependent cadence variation. The diagnostic
window is 90 seconds so both peers can settle full reports; cadence failure is
accepted only alongside the per-peer completion bottleneck.

The 90-second exact-head run produced two complete, clean reports at confirmed
frame 607. Creator/joiner completed 1,835/1,834 sends over 1,795/1,800 active
callbacks (27.2 messages/s) despite maxima of four admissions per callback;
callback means were 36.7 ms, oldest queue age was 1.24/1.22 seconds, stalls were
1,185/1,193, and wait recommendations were 1/0. Both pipelines drained, all
nine checksums matched, exact ledgers conserved in both directions, and every
loss/overflow/malformed/retry/runtime-error counter was zero. This is a full
released-client `BUSTED` characterization; the single wait is accepted only as
a downstream companion to the required per-peer completion bottleneck.

The following one-admission negative control ran the full 600 active callbacks
per peer and completed only 615/623 sends at 40.2/40.9 messages/s, reaching
confirmed frame 233/237 with three matching checksum samples. Both pipelines
drained with exact ledgers and every loss/overflow/malformed/retry/runtime-error
counter at zero. Its frame/advancement/checksum-sample shortfalls are therefore
cap-induced companions; CI still requires both per-peer completion predicates
plus at least 600 admitted/completed messages, exact checksum accounting, and an
observed maximum of exactly one admission per callback. It does not accept
shortened progress alone as `BUSTED` evidence.

**Acceptance:** `bash scripts/run-fortress-wasm-interop.sh` builds the current
server and exact released Fortress/client graph and launches two genuine
single-threaded Godot WASM peers in independent Chromium processes. The current
release must satisfy every healthy gate on both peers, including no repeated
prediction-window stall and zero wait recommendations, and print `HEALTHY`. The
same harness must print the expected `BUSTED` verdict for its one-send-per-
callback negative control. Scheduled CI repeats both oracles in Firefox and
fails loudly on browser-specific regressions.

**Release readiness (Session 052):** the client-side cause now has direct
red-green evidence. `signal-fish-client-rust` PR #62 changed Godot send
completion from socket-wide buffer drain to backend ownership transfer and
proved multiple transfers in one poll. PRs #64 and #65 added real
Fortress/Godot browser coverage plus queue-age, conservation, rollback, and
impaired-network oracles. The final reliability follow-up, PR #66, merged at
`37e8f8c5865aa16d18775ca3e1580e5f9bd3daa0`, with all eleven workflows passing,
Cursor reporting no issues, Copilot reporting quota exhaustion, and no review
threads. Those fixes are now published in 0.9.0. The acceptance cell uses only
exact crates.io pins—no Git/path override—and has been flipped to require
`HEALTHY`; the full real browser gate against the current server checkout
is the authoritative completion evidence. Exact-head run `29659394663` passed
the healthy primary cell and expected-busted negative control.

---

### P14 — Dependency hygiene and WebSocket stack coherence (Size S) — ✅ DONE

Dependabot #179 proposed eight compatible package refreshes plus a
`tokio-tungstenite` 0.29→0.30 transition. The latter is not coherent while
Axum 0.8.9 still requires 0.29: accepting it would retain 0.29 for the server
and add 0.30 solely for the direct test client, duplicating Tungstenite, its
SHA-1 implementation, and its random-number stack in the lockfile.

- [x] Refresh Tokio, UUID, Clap, Rustls, Regex, Syn, Trybuild, and Saphyr to the
  compatible versions proposed by the live dependency update.
- [x] Remove the redundant normal `tokio-tungstenite` declaration. Keep the
  direct test client in `dev-dependencies`, aligned with Axum's 0.29 stack.
- [x] Prove the locked graph contains one `tokio-tungstenite`/`tungstenite`
  version, passes advisory/license policy, and compiles the supported feature
  set plus focused WebSocket and workflow-parser regressions.
- [x] Fix the same-session audit failure exposed by the refresh:
  `check-advisories.sh --full` now places cargo-deny's global `--all-features`
  option before the `check` subcommand, with a structural regression guard.
- [x] Regenerate the native fixture lock after CI's fast `--locked` precheck
  caught its stale root path-package dependency list, then sweep all five
  repository manifests with locked metadata resolution.
- [x] Close the repeated lossy N=8 CI failure class: coordinate one bounded
  rebuild when ICE is connected but SCTP data channels never open, preserve
  glare roles and exact signal ledgers, and keep intentional permanent-fault
  controls retry-free.

**Acceptance:** the safe refresh lands without a second WebSocket protocol
implementation, the production package no longer declares a test-only direct
dependency, and CI is fully green on the exact PR head.

**Completed:** PR #191 is ready for review at exact head
`410ba10ab5d548770e649b09f5c6a33b809cc2c3` with every applicable workflow
green and all automated review feedback addressed.

---

### P15 — Portable Agent Skills library (Size S) — ✅ DONE

The repository's flat skill-file collection predated the portable Agent Skills
layout. Supporting documents and executable helpers lived beside entrypoints,
so agents could not reliably discover one capability from metadata and load
only the resources needed for that task.

- [x] Move every skill entrypoint to `.llm/skills/<name>/SKILL.md` with portable
  `name` and `description` frontmatter and matching directory names.
- [x] Colocate conditional knowledge under `references/` and deterministic
  helpers under `scripts/`, with direct routing links from each entrypoint.
- [x] Generate `skills/index.md` from discovery metadata and validate the
  package layout, resource links, catalog freshness, and file-size policy.
- [x] Update repository workflows, hooks, tests, ADRs, and context references
  to consume the packaged paths without retaining legacy flat-path aliases.
- [x] Keep the fast hook catalog repair path cross-platform and cover its new
  metadata parser and nested-path behavior with focused policy tests.
- [x] Pass all applicable exact-head CI, address Cursor Bugbot and GitHub
  Copilot feedback, and resolve every non-trivial review thread.

**Acceptance:** the portable validator and catalog freshness check pass, all
legacy skill-path references are absent, focused documentation/policy tests are
green, and the exact PR head passes all applicable CI and automated review.

**Completed:** PR #192 is ready for review at exact head
`8b026b8a18fb462b6b03f90b01e0f2934056f7e9`; all 13 applicable workflows are
green (with the intentional Dependabot auto-merge skip), Cursor Bugbot's one
hook-gating finding was fixed and resolved before its clean exact-head review,
and Copilot's exact-head request received the account quota response.

---

### P0 — Design lock & v2 freeze (Size S) — ✅ DONE

> Completed — see [`progress/session-001-p0-design-lock-v2-freeze.md`](progress/session-001-p0-design-lock-v2-freeze.md).
> ADR-0001 + ADR-0002 merged; 44 golden v2 wire snapshot tests (`tests/v2_wire_golden.rs`)
> freeze the JSON + MessagePack wire format and are enforced in CI.

**Goal:** Ratify Section 1 invariants and lock the wire contract before writing code.

#### Tasks

1. Write `docs/adr/0001-protocol-v3-two-axis.md` capturing: versioning strategy,
   relay-as-floor, opaque-signal decision, glare rule, topology set.
2. Write `docs/adr/0002-matchbox-compatibility.md` — **decide:** do native signal
   payloads use matchbox's `PeerSignal { Offer, Answer, IceCandidate }` (all
   `String`) inside the opaque `signal`? If yes, `matchbox_socket` (Rust native +
   WASM) clients work against this server with a thin adapter — large leverage on P7.
   Recommended: **yes**, keep `signal` opaque so it is matchbox-shaped but not
   matchbox-coupled.
3. **v2 freeze:** add golden serialization tests that snapshot the current v2
   `ClientMessage`/`ServerMessage` JSON + MessagePack wire format
   (`src/protocol/mod.rs` test module already covers many cases — extend to a
   locked golden file). Any future diff to these is a breaking change and must fail CI.

**Acceptance:** ADRs merged; golden v2 wire snapshots committed and enforced in CI.

---

### P1 — Version + capability negotiation (Size M) — ✅ DONE

> Completed — see [`progress/session-001-p1-version-capability-negotiation.md`](progress/session-001-p1-version-capability-negotiation.md).
> `Transport`/`Topology` enums, optional `Authenticate` fields, extended `ProtocolInfo`,
> per-connection `NegotiatedProtocol` state, the negotiation/relay-floor logic, config
> `min/max_protocol_version` + validation, and the `/v3/ws` alias all landed. v2 wire is
> provably byte-identical (golden tests green); no v3-only messages are emitted yet.

**Goal:** Negotiate and persist `{protocol_version, transports, topologies}`
per connection. No behavior change yet — pure plumbing.

#### Tasks

1. `src/protocol/types.rs`: add `Transport` and `Topology` enums (Appendix B).
2. `src/protocol/messages.rs`: add optional fields to `ClientMessage::Authenticate`
   (`protocol_version`, `supported_transports`, `supported_topologies`), all
   `#[serde(skip_serializing_if = "Option::is_none")]` / `#[serde(default)]`.
3. `src/protocol/types.rs`: extend `ProtocolInfoPayload` with `protocol_version`,
   `min_protocol_version`, `max_protocol_version`.
4. Per-connection state: add `set_client_protocol` / `client_protocol` /
   `client_supports_v3` / `client_supports_transport` mirroring
   `set_client_game_data_format` / `client_game_data_format` (find their owner via
   `grep -rn "set_client_game_data_format" src/` and co-locate).
5. `src/websocket/connection.rs:244-381`: parse the new `Authenticate` fields,
   compute `negotiated_version = min(client, SERVER_MAX_VERSION=3)`, store caps,
   populate the new `ProtocolInfo` fields.
6. `src/config/protocol.rs`: add `max_protocol_version` (3) and
   `min_protocol_version` (2) with defaults + validation.
7. `src/main.rs`: confirm `/v2` nesting; add `/v3/ws` alias (Section 2.3).

#### Acceptance

- A client advertising `protocol_version: 3` gets `ProtocolInfo.protocol_version = 3`.
- A client omitting the fields is recorded as v2, `[Relay]`, `[Relay]`.
- `client_supports_v3` returns correct values.

**Tests:** negotiation matrix (v3↔v3, v3↔v2-server clamp, v2↔v3-server clamp);
`Authenticate` round-trip with and without new fields; golden v2 snapshots still pass.

---

### P2 — Targeted signal relay (Size M) — ✅ DONE

> Completed — see the [P2 signal-relay progress notes](progress/session-002-p2-targeted-signal-relay.md).
> `ClientMessage::Signal` / `ServerMessage::Signal` / `ServerMessage::NewPeer`, the
> four signaling error codes, a payload-agnostic `src/server/signaling.rs` handler
> (same-room + WebRTC-transport + self-signal + per-sender rate-limit enforcement,
> all delivery gated to v3 + WebRTC peers), the deterministic glare helper, the
> `NewPeer` compatibility wire shape, and a per-connection signal rate limit (`rate_limit.max_signals`,
> default 600) all landed. Acceptance met: an over-the-wire e2e test
> (`tests/v3_signaling_e2e.rs`) drives two `/v3/ws` peers through a byte-preserved
> offer→answer→ICE round-trip with exactly-one-offerer pairing; cross-room is rejected;
> v2 clients neither send nor receive `Signal`/`NewPeer` (golden snapshots byte-identical).

---

### P3 — Session plan / handoff directive + topology selection (Size M) — ✅ DONE

> Completed — see [`progress/session-003-p3-session-plan-topology-selection.md`](progress/session-003-p3-session-plan-topology-selection.md).
> `IceServer` / `SessionPeer` / `SessionPlanPayload`, `ServerMessage::SessionPlan(Box<…>)`,
> a new `src/config/session.rs` (`SessionConfig`) threaded through all construction
> sites, and a new payload-agnostic `src/server/session_policy.rs` all landed.
> `choose_session_plan` implements the Appendix D ladder (`mesh → host → relay`,
> relay floor always wins, all-members-v3 required for any non-relay plan);
> `elect_host` prefers explicit `authority` → earliest-joiner → lowest UUID;
> `plan_for(recipient)` fills per-recipient `peers[].initiate` (Appendix E mesh +
> host-star rules). The plan is emitted from the `handle_player_ready` wrapper
> **after** the coordinator broadcasts the unchanged `GameStarting` — the coordinator
> now returns a `FinalizedRoom` snapshot, since it is capability-blind and the plan
> must be gated on per-connection caps. Finalized membership changes now publish a
> complete authoritative plan to every current v3 member; `NewPeer` remains decodable
> for compatibility but is not the current membership delta. Acceptance met: all-v3 mesh room ⇒
> per-recipient `SessionPlan { mesh, webrtc, fallback: relay }` after `GameStarting`;
> mixed/relay-only room ⇒ explicit no-peer `relay` plan for each v3 member and no plan
> for v2 members (their wire flow remains byte-identical); `host` topology
> names one host and marks it authoritative. 44 golden v2 snapshots byte-identical;
> reached 110/100 over two adversarial-review rounds.
>
> **Notable design choices** (vs. the original task sketch): topology selection lives
> in a dedicated `session_policy.rs` (not `relay_policy.rs`); the elected host is
> reflected in the `SessionPlan` (`host` + per-peer `is_authority`) rather than by
> mutating room state via `set_authority` (`GameStarting` is already broadcast by
> then, so a re-broadcast would be required); `SessionConfig.ice_servers` defaults
> empty (zero-dependency posture) and is the seam P4 fills with minted TURN creds.
>
> **Carried into P4/P5/P6:** TURN credential minting into `IceServer`;
> topology/transport + P2P-vs-relay metrics; `docs/protocol.md` v3 additions +
> `v3-server-messages.jsonl` `SessionPlan` sample.
>
> **Extended in session 007** (see
> [`progress/session-007-mid-session-replanning-host-failover.md`](progress/session-007-mid-session-replanning-host-failover.md)):
> lobby finalization is now actually **persisted** (it previously never reached the
> room store, leaving every `Finalized` gate dead in production); the finalize
> non-relay decision is recorded as a sticky per-room `ActiveSessionPlan` that
> late-join / reconnect consults instead of re-running the ladder (the relay floor
> remains represented by an absent sticky entry). Late joiners, reconnectors, and
> every current v3 incumbent receive fresh authoritative per-recipient
> `SessionPlan`s; and a departed/invalid stored host triggers
> capability-filtered re-election + fresh plans for every remaining v3 member
> (host failover), with the relay floor carrying the room when no member
> qualifies. One capability predicate (v3 + sticky topology + transport) now
> gates election, plan peer lists, and signal validation alike.

---

### P4 — ICE servers + ephemeral TURN credentials (Size M) — ✅ DONE

> Completed — see [`progress/session-004-p4-ice-turn-credentials.md`](progress/session-004-p4-ice-turn-credentials.md).
> New `src/security/turn_credentials.rs` mints coturn REST credentials
> (`username = "{expiry}:{player_id}"`, `credential = base64(HMAC-SHA1(secret,
> username))`; `sha1` is the one added dep, vector-pinned to RFC 2202). New
> `[turn]` config (`src/config/turn.rs`: `enabled`, `static_auth_secret`, `urls`,
> `stun_urls`, `credential_ttl_secs`) with
> validation, defaults, `config.example.json`, and `docs/configuration.md`. WebRTC
> `SessionPlan`s now carry per-recipient ICE — STUN always, freshly minted per-player
> TURN credentials when `[turn].enabled` (single expiry per finalize). Acceptance met:
> enabled ⇒ distinct time-limited creds (HMAC matches the RFC-2202 vector); disabled
> ⇒ public STUN only. The secret never leaves the server. The deferred `RoomJoined`
> ICE pre-gather refinement landed in **session 011** (see
> [`progress/session-011-ice-pregather-and-ice-url-validation.md`](progress/session-011-ice-pregather-and-ice-url-validation.md)):
> `RoomJoined` / `Reconnected` carry the composed ICE list, gated by the new
> `session.enable_ice_pregather` (default true) AND WebRTC enabled AND non-relay
> desired topology AND non-finalized room AND a v3 WebRTC-capable recipient whose
> negotiated topologies contain the desired one (empty ⇒ field skipped ⇒ v2 bytes
> untouched; the `SessionPlan` ICE supersedes it, and a `Finalized`-room
> join/reconnect mints only via its late-join `SessionPlan` — never twice; see
> `tests/v3_ice_pregather_e2e.rs` and the `ice_pregather_emitted` counter).
> Session 011 also closed the deferred ICE URL hardening: schemes
> (`stun:`/`stuns:`/`turn:`/`turns:`, case-insensitive) are validated as hard
> errors and duplicate URLs warn, via the shared `src/config/ice_url.rs`.
> **Session 012** removed the never-implemented `managed` (third-party-cloud)
> TURN mode entirely — TURN is self-hosted only (the server self-mints coturn
> credentials locally and never contacts an external cloud); see
> `progress/session-012-self-hosted-turn-only.md`. 44 golden v2 snapshots
> byte-identical; reached 110/100 over an adversarial round.

---

### P5 — Relay-fallback contract + transport status + metrics (Size S) — ✅ DONE

> Completed — see [`progress/session-005-p5-transport-status-metrics.md`](progress/session-005-p5-transport-status-metrics.md).
> New `docs/architecture/transport-fallback.md` documents the Appendix G client
> contract and the relay-floor-never-closes invariant. New v3-only
> `ClientMessage::TransportStatus { transport, connected }` (per-connection state +
> v3-gated handler; the deferred `ServerMessage::PeerTransportStatus` peer fan-out
> landed in a later session — accepted state changes, never duplicates, fan out to
> the sender's current room, per-recipient v3-gated, counted once per event by
> `transport_status_fanout`). New
> `TransportMetrics` group (`src/metrics.rs` + `src/websocket/prometheus.rs`): chosen
> topology/transport per finalized room, `p2p_established` vs `relay_fallback`,
> `signals_relayed`, `turn_credentials_issued`, `session_plans_emitted` — each counted
> once per logical event (no double-count across the late-join path, proven by test).
> Acceptance met: metrics expose P2P-vs-relay ratios; fallback documented; relay still
> works after a client reports P2P failure. 44 goldens byte-identical; 110/100.

---

### P6 — Docs, samples, `.llm` context (Size S) — ✅ DONE

> Completed — see [`progress/session-006-p6-docs-samples-llm-context.md`](progress/session-006-p6-docs-samples-llm-context.md).
> `docs/protocol.md` gains a "Protocol v3 additions" section (handshake, the four v3
> messages, the selection ladder, the transport-qualified late-join decision table,
> the glare rule, ICE/TURN, and mesh + host sequence diagrams) with v2 unchanged. New
> `docs/architecture/handoff-and-topologies.md`. New canonical samples
> `.llm/code-samples/protocol/v3-client-messages.jsonl` + `v3-server-messages.jsonl`,
> referenced from `.llm/context.md`, `.llm/context-protocol-and-scenarios.md`, and
> `README.md`, and **machine-verified** against the real types by
> `tests/v3_protocol_samples.rs` (round-trip `serde_json::Value` equality, which
> catches optional-field drift despite the absence of `deny_unknown_fields`).
> Acceptance met: doc-consistency + markdown + link-text + internal-link +
> llm-example gates all pass; samples match types. 110/100.

---

### P7 — Reference clients + interop test matrix (Size XL — in-repo complete; mobile/Steam out-of-repo)

**Goal:** Prove the protocol end-to-end across the platform matrix. This is the
largest body of work and the real cost of "cross-platform" — the server changes above
are the small part.

#### Tasks

1. ~~**Browser reference client** (TypeScript)~~ — ✅ **DONE (session 010)**:
   see `progress/session-010-p7-browser-reference-client.md` and
   [ADR-0005](docs/adr/0005-browser-reference-client.md). In-repo standalone
   npm package `clients/browser/` (`signal-fish-reference-browser`) driving a
   **real headless-Chromium `RTCPeerConnection`** via `playwright-core`: full
   v3 flow, one reliable + one unreliable
   (`{ ordered: false, maxRetransmits: 0 }`) channel per pair, trickle ICE
   (matchbox-shaped payloads per ADR-0002), mesh + host, Appendix G
   transport-status + relay fallback, deterministic `--cripple-ice`, pure-v2
   mode, and JSONL/exit-code parity with the native client so the session-009
   harness drives both client kinds unchanged. Chromium provably never
   outlives the CLI (graceful teardown + a pid-reuse-guarded orphan reaper,
   both CI-pinned).
2. ~~**Native reference client** (Rust)~~ — ✅ **DONE (session 009)**: see
   `progress/session-009-p7-native-reference-client.md` and
   [ADR-0004](docs/adr/0004-native-reference-client.md). In-repo standalone crate
   `clients/native/` (`signal-fish-reference-native`) on **`webrtc-rs` 0.20
   directly** (matchbox-shaped signal payloads per ADR-0002, incl.
   IceCandidate-as-serialized-`RTCIceCandidateInit`): full v3 flow, one
   reliable + one unreliable (`ordered:false, max_retransmits:0`) channel per
   pair, trickle ICE, Appendix G transport-status reporting, deterministic
   `--cripple-ice` fault injection, pure-v2 mode, JSONL stdout event contract,
   and a multi-process interop harness (real server binary + N≥3 client
   processes over loopback UDP, zero external network). CI:
   `.github/workflows/webrtc-interop.yml` via `scripts/run-webrtc-interop.sh`.
3. ~~**In-repo signaling conformance tests**~~ — ✅ **DONE (session 007, exceeded
   scope)**: see
   [`progress/session-007-multipeer-multiprocess-conformance.md`](progress/session-007-multipeer-multiprocess-conformance.md).
   `tests/v3_multipeer_e2e.rs` (8 tests over real WebSockets at N=3/4: global glare
   matrix, host star, failover + cascade failover, mixed v2/v3 floor, seat-fill
   late join, wire reconnect) and `tests/v3_multiprocess_e2e.rs` (the compiled
   binary as a real child process over TCP: full mesh session; SIGKILL + same-port
   restart invalidating reconnect tokens), plus a formal layer
   ([`progress/session-007-formal-verification-layer.md`](progress/session-007-formal-verification-layer.md)):
   TLA+/TLC model of the session core (4 configs, CI-wired), proptest invariant
   suites, and parser fuzz-hardening with a release-profile depth-bomb probe.
4. **Cross-platform interop matrix** (browser↔native, native↔native, mobile, Steam
   build) — **native↔native cells ✅ in-repo (session 009)**: mesh N=3 full
   WebRTC with live relay floor, host star N=3, crippled-ICE relay fallback,
   late-join full-plan refresh (seat fill), and mixed v2/v3 relay floor — all in
   `clients/native/tests/interop_e2e.rs` (scenario table in
   `clients/native/README.md`). **Browser↔native + browser↔browser cells ✅
   in-repo (session 010)**: mixed mesh N=3, browser↔browser mesh, host star
   with a browser client, crippled-ICE browser fallback, the Chrome `.local`
   mDNS trap (empirically pinned: P2P establishes via peer-reflexive
   candidates; webrtc-rs tolerates the unresolvable `.local` candidate),
   pure-v2 browser floor, mid-handshake-close contract, and SIGTERM/SIGKILL
   teardown reaping — all feature-gated in
   `clients/native/tests/browser_interop_e2e.rs`, CI-enforced via
   `.github/workflows/browser-interop.yml`. Trickle-ICE ordering and SCTP/DTLS
   interop are exercised live by both suites (real webrtc-rs↔Chromium
   sessions). The mobile/Steam platform cells remain (out-of-repo builds,
   Appendix H).
5. ~~**Per-platform integration notes** (Appendix H)~~ — ✅ **DONE (session 014)**:
   see [`progress/session-014-p7-platform-integration-guide.md`](progress/session-014-p7-platform-integration-guide.md).
   New user-facing guide `docs/guides/platform-integration.md` maps every platform to
   its WebRTC stack and integration steps — Godot (built-in / `webrtc-native` =
   libdatachannel — easiest, whole matrix), Unity (native via `com.unity.webrtc` but
   **NOT WebGL** — needs a JS bridge), Unreal (Pixel-Streaming-shaped, weak for P2P —
   embed libdatachannel), mobile (libdatachannel / Google libwebrtc), Steam (just a
   native build — embed a WebRTC stack; **Steam networking does not interop with
   browsers**), plus the universal v3 client contract (relay floor, opaque
   matchbox-shaped signal payloads, the two-channel layout, stateless glare) and the
   cross-stack interop traps. Wired into the MkDocs nav, the docs landing page, README,
   and reciprocally cross-linked from `protocol.md`, `transport-fallback.md`,
   `handoff-and-topologies.md`, and `deployment-turn.md`. The guide is explicit that
   only the browser/native rows are in-repo demonstrated; the mobile/Steam/engine rows
   are integration notes for out-of-repo builds.

**Acceptance:** ✅ **met for all in-repo platforms** — a documented green
interop matrix (`clients/native/README.md`), with browser↔native mesh + relay
fallback demonstrated and CI-enforced (sessions 009 + 010). Mobile/Steam rows
stay open until those platform builds exist (out-of-repo).

---

### P8 — TURN infra + deployment + security + scaling (Size L) — ✅ IN-REPO PORTIONS DONE

> In-repo portions completed in session 008 — see
> [`progress/session-008-p8-security-hardening.md`](progress/session-008-p8-security-hardening.md)
> and
> [`progress/session-008-p8-turn-deployment-scaling-docs.md`](progress/session-008-p8-turn-deployment-scaling-docs.md).

#### Tasks

1. **TURN deployment guide** — ✅ **DONE (session 008)**: `docker-compose.yml`
   gained a pinned `coturn` service behind the `turn` compose profile
   (`--use-auth-secret`, env-driven secret with a fail-fast empty-secret guard);
   new `docs/deployment-turn.md` covers the coturn quick start, the ephemeral
   credential scheme + TTL, **zero-downtime shared-secret rotation**, the
   self-hosted-only posture (no third-party-cloud integration; any managed
   service must be wired in out-of-band via `[session].ice_servers`), the wss://
   requirement, and capacity planning (plan for 15–20% relayed). _(Session 012
   removed the `managed` mode; the guide now documents self-hosted only.)_
2. **Security hardening** — ✅ **DONE (session 008)** for every in-repo item:
   `--print-config` secret redaction (`Config::redacted_for_display`); signal
   payload size cap (`security.max_signal_bytes`, default 16 KiB, new
   `SIGNAL_TOO_LARGE` error); post-auth **idle timeout**
   (`websocket.idle_timeout_secs`, default 300 s, error delivered via the
   connection's own channel — the `ping_timeout` reaper never closed sockets);
   startup **wss:// warning** when `turn.enabled` without TLS. Already done
   previously: authenticate-before-room-op, same-room signal enforcement (P2),
   per-connection signal rate-limit (P2), peers/room cap. **Security review —
   ✅ DONE (session 013):** a multi-agent adversarial audit of the entire v3
   signaling surface fixed three hardening findings red-green — (a) the
   `TransportStatus`→`PeerTransportStatus` 1→N fan-out is now bounded on the
   existing per-connection control-plane budget (it was the one un-rate-limited
   client-triggered v3 emit path); (b) zero-valued background-task interval
   configs (`server.room_cleanup_interval`, `rate_limit.time_window`,
   `websocket.batch_interval_ms`) are now rejected at startup instead of silently
   panicking their tasks (with defense-in-depth use-site clamps); (c) all secret
   comparison is consolidated into one constant-time helper, fixing the
   reconnection-token and metrics-bearer-token compares. The TURN credential
   minting and session-policy/host-election surfaces were independently confirmed
   clean. See
   [`progress/session-013-p8-security-review-hardening.md`](progress/session-013-p8-security-review-hardening.md).
   The `/security-review` slash command (a user-triggered, billed cloud review of
   the branch diff) remains available but the substantive review is complete.
3. **Scaling notes** — ✅ **DONE (session 008)**: new
   `docs/architecture/scaling.md` (room as the scaling unit / room-affinity
   consistent-hash, relay+signaling share the affinity constraint, Redis
   pub/sub only if rooms span nodes — out of scope today, the real `region_id`
   and cross-instance seams described accurately) cross-linked from
   `docs/deployment.md`.

**Acceptance:** reproducible TURN setup ✅ (compose profile, live-verified);
documented scaling path ✅; security review ✅ (session 013 — multi-agent
adversarial audit of the v3 surface, three findings fixed red-green; the
`/security-review` slash command remains available for a formal cloud pass).

**Remaining (out-of-repo / deferred):** operating real (self-hosted coturn) TURN
infrastructure; multi-node room-spanning fan-out (explicitly out of scope by
design). _(The `managed` third-party-cloud TURN mode was removed in session 012
— TURN is self-hosted only by design.)_

---

## P9 — Production-readiness hardening (v2/v3) — ✅ DONE

Covers the explicit-`StartGame` lobby change, the verification build-out (Z3 SMT
proofs, cargo-mutants, cargo-fuzz), self-referential docs + AsyncAPI client spec,
and the zero-flakiness audit. The tracked flakes below are closed; optional
long-soak ideas remain non-blocking confidence work under the zero-flakiness
policy (`.llm/context-testing.md`).

### Flakiness tracking (zero-tolerance — must close deterministically)

#### FLAKE-001 — `tests/v3_multiprocess_e2e.rs` timeout under full-suite saturation

- **Symptom.** During the production-readiness pass, one run of
  `cargo test --all-features --no-fail-fast` reported `1 passed; 1 failed` in the
  `v3_multiprocess_e2e` binary (2 tests). Every other full run that session was
  green, and the binary passes **2/2 in isolation** (`cargo test --test
  v3_multiprocess_e2e`).
- **Reproduction conditions (only under saturation).** Raw `cargo test`
  (NOT nextest) runs all ~48 test binaries' tests concurrently. On a 12-core
  machine that is ~4× oversubscription. These tests spawn the real
  `signal-fish-server` binary as **child OS processes** that bind ports and poll
  `/v2/health`; a subset run (4 heavy binaries × `--test-threads=16`, ×3) did
  **not** reproduce it — it needs the full-suite load. So it is a low-probability,
  load-dependent event.
- **Root-cause hypothesis.** CPU/port **starvation** of the spawned child server
  under extreme oversubscription pushes it past a fixed readiness/connect
  deadline (`HEALTH_DEADLINE` / `CONNECT_TIMEOUT`), not a logic bug — the child is
  progressing, just slowly. (The harness is otherwise sound: `kill_on_drop`,
  port-reservation retry, health polling, per-test temp config.)
- **Mitigation applied (this pass).**
  1. nextest **test-group** isolation (`.config/nextest.toml`):
     `process-spawning = { max-threads = 1 }` + an override routing
     `binary(v3_multiprocess_e2e)` into it with `threads-required = 4`, so under
     the canonical CI runner (`cargo nextest run --profile ci`) these tests run
     one-at-a-time and reserve slots, preventing the starvation.
  2. Generous, saturation-tolerant **ceilings** in the harness (see fix items)
     so even raw `cargo test` oversubscription cannot trip them.
- **Status: closed.** Both fixes applied and verified: a full
  `cargo nextest run --profile ci --all-features` is green (1031 passed), with the
  two multi-process tests now completing in ~1.4s / ~1.6s (serialized in the
  `process-spawning` group, so the child server is never CPU-starved). The
  generous ceilings additionally cover the raw `cargo test` path (which has no
  per-test timeout, so a starved-but-progressing child only runs slower, never
  killed).
- **Historical optional ideas (not current phase work):** a ≥20-run nextest
  soak, richer child-process timeout diagnostics if the failure ever recurs,
  and lower parallelism for raw `cargo test`. A non-starvation recurrence must
  be root-caused directly rather than handled by widening deadlines.

#### FLAKE-002 — `src/websocket/batching.rs` time-based unit tests — FIXED

- **Symptom.** `test_message_batcher_flush_on_time` panicked at
  `batching.rs:157` (`assert!(!batcher.should_flush())`) during a full
  `cargo nextest run`. `test_message_batcher_partial_batch` had the identical
  latent fragility.
- **Root cause (real timing flake, not the system).** The batcher's flush timer
  starts at `MessageBatcher::new()` and `should_flush()` reads the wall clock
  (`last_flush.elapsed() >= batch_interval`). The tests created a batcher with a
  small interval (50ms / 20ms), queued, then **immediately** asserted
  `!should_flush()`. Under load the thread is descheduled past the interval
  between construction and that check, so `should_flush()` returns `true` and the
  negated assertion fails. The companion "after sleep" assertions were fine (a
  sleep only overshoots).
- **Fix (deterministic).** Split each test into two robust legs: a **large
  interval (10s)** batcher for the "must NOT flush yet" assertion (10s cannot
  elapse before the immediate check, under any load), and a **tiny interval
  (5ms) plus generous sleep (50ms, 10×)** batcher for the "must flush on time" assertion
  (robust because the sleep only ever overruns). No production change; no fixed
  sleep used as an expected-timing oracle.
- **Superseded (session 017): now truly deterministic (paused clock).** As the
  B4 sweep (`MessageBatcher` already reads `tokio::time::Instant`), both tests
  were converted to `#[tokio::test(start_paused = true)]` driving the interval
  with `tokio::time::advance(..)` — no `thread::sleep` at all, so there is no
  wall-clock leg to overshoot or deschedule. Each is now a single batcher
  exercising the `>=` boundary (49 ms → not yet, +1 ms → flush), removing the
  large-interval/tiny-interval gymnastics entirely (simpler AND fully
  deterministic; runs in ~0 ms). No production change.
- **Status: closed** — verified by repeated runs; covered going forward by the
  Zero-Flakiness Policy guidance on time-based tests.

#### Timing-audit sweep (siblings of FLAKE-002)

Audited every `thread::sleep` / `tokio::time::sleep` / `Instant::elapsed` in
`src/` test code for the same fragility:

- **`src/websocket/connection.rs` WebSocket test — FIXED.** Replaced a fixed
  `sleep(2000ms)` "give the server time to start" wait + single connect attempt
  with a **poll-until-ready** loop (retry connect every 50ms up to a 30s ceiling).
  A fixed startup sleep can leave the listener unbound under load; polling is
  deterministic.
- **`src/rate_limit.rs` (×3) and `src/auth/rate_limiter.rs` (×1) — audited,
  robust, left as-is.** These sleep _past_ a short window (100ms / 1ms) and then
  assert the limit reset / entry cleanup. A sleep only ever overshoots, so the
  "more time elapsed" assertion direction cannot flake; the margins (150ms vs
  100ms; 5ms vs 1ms) are sufficient.

#### FLAKE-003 — trybuild UI test timeout under nextest — FIXED

- **Symptom.** `public_api_privacy_tests::websocket_binary_wire_internals_are_not_public_api`
  was killed by nextest's slow-timeout ("test timed out") during a full
  `cargo nextest run` — its stdout showed `Compiling signal-fish-server ... 2m43s`.
- **Root cause (timeout mis-read, not a hang).** This is a **trybuild** test: its
  runtime _is_ a full crate compile in trybuild's own target dir. That compile is
  cold whenever source changed (it was, after the flakiness fixes) and runs
  several minutes under a loaded runner — legitimately longer than the global
  120s `slow-timeout` (terminate-after), which then kills it.
- **Fix (deterministic).** A per-test nextest override
  (`.config/nextest.toml`) gives `binary(public_api_privacy_tests)` a generous
  `slow-timeout` (300s) and routes it through the serialized `process-spawning`
  group. The trybuild compile is full-CPU for minutes, so it also starved a
  _second_ already-slow test — `ci_config_tests::test_docs_relative_links_*` (a
  ~17s repo link-checker script) — past the timeout. Both are now routed into the
  same `process-spawning` group (max-threads = 1) so they never co-schedule, and
  the global `ci` `slow-timeout` terminate-after was raised 2→3 (240s→360s) as a
  net so any other merely-starved (not hung) subprocess test is not killed.
- **Status: closed** — verified by a clean full `cargo nextest run --profile ci`.

#### FLAKE-004 — macOS RST relay-chaos phase deadline — FIXED

- **Symptom.** `Nextest (macos-latest)` in Actions run `29443472699`, job
  `87448487806`, spent exactly 60 seconds in
  `rst_during_relay_heals_room_and_conserves` before its outer watchdog fired.
- **Root cause (test synchronization).** The watcher loop used a fresh relative
  30-second timeout for every frame while ignoring non-text protocol frames.
  Server WebSocket Pings therefore kept resetting the timeout when an expected
  post-RST condition was absent. The test also redundantly awaited natural
  client-side split-stream error delivery even after the watcher had already
  proved the server-facing reset through `PlayerLeft`; the proxy unit test owns
  the client-facing termination contract.
- **Fix (deterministic).** Every multi-frame phase in the suite now shares one
  absolute `timeout_at` deadline. At reset, the victim observer is aborted and
  joined before the auditor records the terminal injected fault, freezing the
  already-proven gap-free prefix and preventing buffered frames from racing the
  terminal marker. The watcher still requires the complete surviving stream and
  exact victim `PlayerLeft`; no production assertion was weakened.
- **Status: closed.** The exact regression passed 20/20 back-to-back, the RST
  proxy contract and same-class churn regression pass, the async-timeout policy
  scan is green, and current-main macOS Nextest is green.

#### MUTPERF-001 — mutation-testing shards exceed the 20-min timeout (all cancelled)

- **Symptom.** Every `cargo-mutants` shard in `.github/workflows/mutation.yml`
  (6 shards, 20-min timeout) ran past 20 minutes and was **cancelled** by the
  timeout, so the workflow produced no mutation verdict.
- **Root cause (build cost, not test cost).** `cargo-mutants` builds in a `/tmp`
  scratch dir and excludes `./target` from its copy, so each shard started with an
  empty `target/` and recompiled ALL ~428 dependencies cold (worse with
  `--all-features` pulling `rustls` / `matchbox` / `axum-server`). Compounding it,
  round-robin sharding scattered each shard's mutants across files, defeating
  incremental-compilation locality so per-mutant cost grew (~55s and climbing).
  Net: each shard paid a full cold build before testing a single mutant.
- **Fix/Mitigation (all centralised in `scripts/run-mutants.sh`).**
  1. `--in-place` builds in the Rust-cache-warmed `./target` (deps reused —
     measured **0 cold recompiles**); a `baseline` job warms a SHARED Rust cache
     (`shared-key: "mutants"`, same profile + RUSTFLAGS) and is the green-gate for
     `--baseline=skip`.
  2. `--sharding slice` gives each shard a CONTIGUOUS mutant range
     (incremental-compilation locality) — kept per-mutant ~stable at ~22s.
  3. `mold` linker via per-target `CARGO_TARGET_<triple>_LINKER=clang` +
     `RUSTFLAGS="-C link-arg=-fuse-ld=mold"` (NOT `-C linker=clang` in RUSTFLAGS,
     which cargo-mutants re-encodes and diverges from the warm build's
     fingerprint), mirrors Dockerfile / `.devcontainer`, for the per-mutant relink.
  4. `[profile.mutants]` (inherits `dev`; `debug=0`; `incremental=true` — NOT
     `profile.ci`, which sets `incremental=false`).
  5. `--lib`-only oracle via `.cargo/mutants.toml`
     (`additional_cargo_args = ["--lib"]`, applies to build AND test, so ~20
     integration-test binaries are no longer compiled per mutant); NO
     `--all-features` (the 7 scoped modules have zero feature gates;
     `cargo mutants --list` stays 319).
  6. Resharded **6 → 32** after CI measured 310.12s for the worst
     12-mutant shard. The conservative budget is now 29s/mutant (observed
     25.843s plus ~10% headroom), so `ceil(319/32) × 29s = 290s`
     (<5 min/shard); `timeout-minutes: 10`.
- **Standing rules.** Keep `--in-place` (never a `/tmp` scratch or
  `--copy-target`), `--sharding slice` (never round-robin), `mold`,
  `profile.mutants` (never `profile.ci`), `--lib` in `additional_cargo_args`
  (never `--all-features`), and a shard count sized for serial execution. Keep
  `{mutant-count 319, shard-count N, per-shard timeout, per-mutant budget}`
  feasible together; per-shard target < 5 min; timeout within `[8, 15]` min (floor
  avoids cold-cache flakes per the Zero-Flakiness Policy in
  `.llm/context-testing.md`; ceiling enforces the budget).
- **Enforcing tests.** `test_mutation_workflow_uses_fast_linker_and_in_place`,
  `test_mutation_workflow_uses_optimized_build_profile`,
  `test_mutation_shard_matrix_is_complete_contiguous_partition`,
  `test_mutation_shard_budget_is_feasible_vs_timeout`,
  `test_mutation_oracle_does_not_use_all_features`,
  `test_full_suite_caching_jobs_drop_trybuild_artifacts`,
  `test_mutation_scope_matches_workflow_path_filter` (all in
  `tests/ci_config_tests.rs`) plus `tests/run_mutants_script_tests.rs`. Full
  rationale: [`.llm/skills/mutation-testing-performance/SKILL.md`](.llm/skills/mutation-testing-performance/SKILL.md).
- **Status: closed** — PR #153's 32-shard run caught every mutant. Across the
  29 jobs exposed by GitHub's first-page jobs API, the measured worst shard was
  261.315s, below both the 290s conservative model and the five-minute target;
  the workflow-level success covers all 32 shards.

---

## P10 — Bulletproofing campaign: falsify → formalize → v3 revision — ✅ DONE

> Provenance: deep skeptical analysis (2026-07) of the protocol, server,
> reference clients, and real-world usage — codebase exploration, comparative
> industry/web research (GNS, ENet/KCP, QUIC/WebTransport, WebRTC/SCTP, Photon,
> Nakama, Colyseus, PlayFab/EOS, Aeron, matchbox; Appendix P), and TLA+ gap
> analysis. Two production bugs were verified by direct code inspection
> (P10.0). **Scope ruling: protocol v3 is NOT live — v3 wire shapes may be
> augmented/replaced in place** (`tests/v3_reliability_wire_golden.rs` fixtures may be
> rewritten). v2 remain frozen: `tests/v2_wire_golden.rs` stays
> byte-identical and every existing gating invariant (§2.2) holds.
>
> Method: falsification-first. Every experiment records its prediction before
> running (they are pre-registered in P10.C); every protocol change lands
> spec-first (TLA+ module before implementation, house seeded-bug convention);
> every fix lands red-green (failing test first).

### P10.0 — Evidence ledger (verified findings driving this campaign)

Confirmed bugs (fix in P10.A):

- **BUG-1 — every room is deleted 1 hour after creation, mid-game.**
  `Room.last_activity` is written exactly once, at creation
  (`src/database/mod.rs:388`). The refreshers exist but have **zero production
  call sites** (grep-verified): `GameDatabase::update_room_activity` (trait
  `src/database/mod.rs:120`, impl `:680`) and `Room::update_activity`
  (`src/protocol/room_state.rs:263`). `cleanup_expired_rooms`
  (`src/database/mod.rs:643-677`) deletes rooms where `Room::is_expired`
  (`src/protocol/room_state.rs:281`, `now - last_activity > inactive_timeout`,
  default 3600s) — **including non-empty rooms** (counted as
  `inactive_rooms_cleaned`; the caller comment in
  `src/server/maintenance.rs` even says "including non-empty ones").
  Corollary A: any session ≥1h dies with players in it. Corollary B:
  `cleanup_empty_rooms` (`src/database/mod.rs:629`,
  `players.is_empty() && last_activity <= now - empty_timeout` with
  `empty_timeout` 300s) destroys the documented 300s reconnection window
  (`docs/concepts/reconnection.md`) for any room older than 5 minutes whose
  members disconnect simultaneously — reconnects then fail `RoomNotFound`
  with still-valid tokens. Corollary C: coordinator `room_players` routing is
  not swept by this deletion path → mixed zombie state. E2E suites never
  caught it because test rooms are seconds old.
- **BUG-2 — a legal config gets a healthy sender evicted.**
  `websocket.slow_consumer_timeout_ms` validates only its own bounds
  (0 < x ≤ 600000, `src/config/websocket.rs:123-133`); there is no cross-field
  check against `server.ping_timeout` (default 30s). Sender activity is
  recorded at dispatch (`src/server/message_router.rs:15`) and the handler
  then awaits the broadcast `join_all` (`src/server.rs:675-716`) with the
  sender's receive loop parked. With `slow_consumer_timeout_ms ≥
  ping_timeout·1000` (legal, e.g. 60000), one slow recipient parks a healthy
  sender past the reaper deadline → healthy player evicted 4003.

Gaps (addressed across P10.A/C/E/F):

- **GAP-3 — "16 players" is false at defaults.** `max_connections_per_ip = 10`
  (`src/config/defaults.rs:233`) — 16 players behind one NAT (LAN party,
  venue, office) cannot connect; `default_max_players = 8`
  (`src/config/defaults.rs:24`) — the 9th join fails unless the room creator
  passed `max_players`. Every 16-player test harness must raise the IP cap or
  it tests the limiter, not the relay.
- **GAP-4 — sender↔recipient coupling (mitigated for state sync in session
  032).** `deliver_to_all` awaits `join_all`
  over all recipients (`src/server.rs:675`), so one backpressured recipient
  stalls every sender's receive loop up to `slow_consumer_timeout` (5s) per
  message → whole-room ~5s freezes per slow-consumer event. No surveyed peer
  (GNS/Aeron/Photon/Colyseus) couples connections this way (Appendix P).
  Session 032 shipped #136-A/B/E: `latest`/`volatile` avoid sender parking,
  exact priority reports account loss, and maximum sojourn fails closed.
  Reliable traffic deliberately retains backpressure.
- **GAP-5 — deploys were session-fatal and unexplained (closed in session
  028).** Before E3, no SIGTERM/ctrl_c handler made
  `CloseReason::Shutdown`/close code 4000 reachable, so deploys ended as
  fleet-wide 1006. The implemented drain now advertises `GoingAway`, closes
  4000, and avoids arming reconnect state. Industry floor is drain-with-notice (Agones
  SIGTERM+grace, HTTP/3 GOAWAY, WebTransport DRAIN_SESSION); live-session
  state handoff is NOT the bar (Nakama matches are in-memory-only; Cloudflare
  Durable Objects disconnect every WebSocket on deploy). Appendix P.
- **GAP-6 — half-open blind window is 30–90s.** The server never sends WS
  protocol pings (verified: no outbound `Message::Ping` in `src/websocket/`).
  The activity reaper (`ping_timeout` 30s) runs on the `cleanup_task` tick
  (`room_cleanup_interval` 60s, `src/server/maintenance.rs:69-139`), so a
  vanished peer (NAT rebind, cable pull) ghosts the room for 30–90s while
  also inflicting GAP-4 stalls.
- **GAP-7 — gap-accountability hole at truncation (closed by E5/E6).** The
  original contract could not explain a reconnect with `ReplayStatus::Truncated`
  after the replay ring evicted
  the `PlayerLeft`/`PlayerJoined` evidence for a sender that left+rejoined
  during the absence. Complete per-sender `Reconnected.sender_watermarks` (E5)
  and the mandatory reconnect re-baseline rule (E6) now close the hole.
- **GAP-8 — silent seq reset needed an epoch (closed by E1/E6).** `seq` resets to 1 on every
  room (re)assignment (`src/server/connection_manager.rs:222,360,448`),
  attributable today only because control and data share one FIFO. The
  #136-B control-priority split (P10.E2) broke that FIFO attribution. The E1
  `epoch` stamp plus E6 announced-epoch/stale-tail rules make resets explicit.
- **DOC-9 — protocol doc drift (closed).** Replay, negotiated-version, delivery
  class, spectator epoch, metrics, and exact-accountability docs are synchronized
  with the v3 wire artifacts and guarded by documentation/spec consistency tests.
- **ARCH-10 — multi-instance is silent split-brain.** All state is
  per-instance and in-memory; join-with-unknown-code **creates** the room
  (`src/server/room_service.rs:477-561`), so the same code on two instances
  = two live rooms; reconnect tokens are stranded on the other instance;
  `should_process_message` returns `Ok(true)` unconditionally
  (`src/server.rs:862`); `DedupCache`/`CircuitBreaker`/in-memory
  "distributed lock" are dead stubs. CAP verdict: the system is single-node
  CP by construction — the honest posture is doctrine + LB room-affinity
  docs (P10.F1), not sharding (Appendix P, Colyseus precedent).
- **MISC-11.** Committed fuzz artifacts `fuzz/artifacts/fuzz_reconnect_tokens/`
  (3 files) are untriaged. `metrics.rs` `game_data_messages` is exported to
  Prometheus but never incremented (permanently 0). `std::time::Instant` in
  `src/server/connection_manager.rs` / `src/rate_limit.rs` blocks
  paused-clock determinism (P10.B4).

What is already strong (do not regress): deliver-or-disconnect + the
conservation law + v3 seq observability exceed what commercial relay peers
promise (Nakama relayed: order-of-processing only; Photon-over-WS: bare
transport reliability); the five TLA+ specs with seeded-bug non-vacuity; the
golden-wire discipline; "resync is app-level" is backed by the end-to-end
argument and is the relay-industry norm (Appendix P).

### P10.A — P0 correctness fixes (Size M) — do first

Every item red-green: the failing test lands in the same PR as the fix.

> **Status (session 016):** A1–A5 landed as one PR (the code fixes, red-green
> Rust tests, doc corrections, and metric wiring) — see
> [`progress/session-016-p10a-p0-correctness-fixes.md`](progress/session-016-p10a-p0-correctness-fixes.md).
> A1 reuses `last_activity` (refreshed on join/leave/heartbeat-relay) instead of
> a new `emptied_at` field, with the reconnection-record veto as the
> authoritative window guard (simpler, no emptiness bookkeeping). **D1
> (`RoomLifecycleGC.tla`) landed in the same PR** — see P10.D.
> `default_max_players` was intentionally left at 8 (a gameplay product call)
> and documented rather than changed.

- [x] **A1 — BUG-1 fix + `RoomLifecycleGC.tla` (D1).** Red tests:
  (i) a room older than `inactive_room_timeout` with active players survives
  GC; (ii) after a mass disconnect of a >5-minute-old room, reconnection
  succeeds within the 300s window. Fix: refresh `last_activity` on join,
  leave, and relayed game data (throttle writes like the heartbeat throttle,
  `src/server/heartbeat.rs:26-44`); reuse `last_activity` for the empty-room
  clock rather than adding separate emptiness bookkeeping; and encode whether
  GC consults pending reconnection records. The shipped contract is:
  `cleanup_empty_rooms`/`cleanup_expired_rooms` must not delete a room with
  an unexpired, unclaimed reconnection record (`ReconnectionManager`
  exposure needed). The TLA+ module (Appendix O.1) lands in the same PR with
  `StaleActivityBug = TRUE` reproducing today's behavior (must violate
  `ActiveRoomNeverReaped` and `ReconnectWindowRespected`), `FALSE` in checked
  cfgs. Files: `src/database/mod.rs`, `src/server/maintenance.rs`,
  `src/protocol/room_state.rs`, `src/server/room_service.rs`,
  `formal/tla/RoomLifecycleGC.tla` + `_Small.cfg` + `_WindowBoundary.cfg`.
- [x] **A2 — BUG-2 fix.** Config cross-field validation rejects
  `websocket.slow_consumer_timeout_ms ≥ server.ping_timeout * 1000` at startup
  (guarded on `ping_timeout > 0`). Data-driven boundary test in
  `src/config/validation.rs`; constraint documented. **D3 (session 019)
  confirmed this strict `<` is the exact derived side condition** — the
  tick-exact safe region is `<=`, strengthened to `<` for continuous-time slop —
  so the interim inequality needed no change; the check comment and the boundary
  test now cite `formal/tla/SenderPacingReaper.tla`.
- [x] **A3 — 16-player defaults.** Raised `default_max_connections_per_ip`
  10 → 24 (16 players + spectators + reconnect churn); regression test registers
  16 same-IP clients at the default cap. `default_max_players` left at 8 (a
  gameplay product call) and documented alongside the IP cap rather than changed.
  Files: `src/config/defaults.rs`, `src/server.rs`, `config.example.json`,
  `docs/`, `tests/config_and_endpoints_tests.rs`.
- [x] **A4 — DOC-9 fixes.** Corrected `docs/protocol.md` reconnection sections
  (replay ring is real; describes `missed_events` + the `replay` completeness
  field) and `docs/concepts/protocol-versions.md` max-version (3 → 4). The
  close-4000 note was already accurate ("reserved; no in-process trigger today").
- [x] **A5 — MISC-11 triage.** Wired `increment_game_data_messages` at the relay
  funnel (`broadcast_game_data`) — the metric read a permanent 0. The three
  `fuzz/artifacts/fuzz_reconnect_tokens/` crash inputs were replayed against
  current code (see the session-016 note for the verdict).

### P10.B — Shared verification infrastructure (Size M)

- [x] **B1 — 16-player harness (completed session 036).**
  `tests/websocket_test_helpers/room16.rs` now provides `PlayerHandle`,
  `connect`, `authenticate`, `try_join` (→ `Result<PlayerHandle, (reason,
  ErrorCode)>` so callers assert admission OR refusal), and `join_n_players(addr,
  game, room, max_players, n, version) -> Vec<PlayerHandle>` (loopback clients
  share one IP, so the per-IP cap is genuinely exercised). Built red-green with
  its first consumer (H11/H12, below). **Deferred to their consumers** (not built
  as unexercised scaffolding): the per-client `ChaosProxy` option
  (`tests/relay_chaos_e2e.rs:445-449` pattern) and `sixteen_player_server_config()`
  (raised IP cap / `send_queue_capacity` / `slow_consumer_timeout_ms`) land with
  the slow-consumer / matrix suites (H1+H13, C-matrix) that inject faults; the
  `encoding` param lands with the GameData-sending suites. Reuses the auth/join
  shape from `tests/v3_game_data_sequencing_e2e.rs:99-190`.
  **Session 036 follow-through:** the shared harness now retains the exact
  `RoomJoined` snapshot for ConformanceAuditor baselining and exposes an
  encoding-aware authentication/join path; both JSON and MessagePack are
  exercised by C-matrix. Its matrix consumer accepts one endpoint per player,
  providing the per-client `ChaosProxy` seam for all twelve fault cells. The
  proposed fault-tuned server config was deliberately not added: the full
  relay grid passes at the production queue/timeout defaults, so a widened
  test-only config would weaken the evidence and add unused policy.
- [x] **B2 — extract the real-binary harness (session 017).** Landed — see
  [`progress/session-017-p10b2-real-binary-harness-dedupe.md`](progress/session-017-p10b2-real-binary-harness-dedupe.md).
  `ServerProcess` (+ `Drop`/`kill_and_wait`/`captured_output`), `reserve_port`,
  the health-poll (`wait_until_healthy`), the spawn helpers (`spawn_server`,
  `spawn_server_on_fixed_port`, `try_spawn_server`), and the timeout constants
  moved to the new `tests/websocket_test_helpers/server_process.rs` (single
  copy). Config is now a **`serde_json::Value` deep-merge overlay** over a
  suite-independent `base_config(port)` — each suite passes its own overlay
  (v3: `session.default_topology`; delivery: the retained `DeliveryKnobs` →
  `session`/`websocket.*`), reproducing each suite's exact original effective
  config (proven programmatically + by the passing suites). `NativeClientProcess`
  was checked and is NOT duplicated (lives only in `multiprocess_delivery_e2e`),
  so it stayed put per the PLAN's conditional. Both suites migrated
  (`v3_multiprocess_e2e` 620→398, `multiprocess_delivery_e2e` 1253→1049).
  Verified: both suites green in isolation (25/25, 2 nightly-ignored), full
  suite 1446/1446 exit 0, `cargo test --no-run` compiles all binaries, clippy
  clean, LF-normalized. Implemented by a verify-or-revert-gated sub-agent, then
  independently re-verified + main-thread adversarial review of the deep-merge.
- [x] **B3 — ConformanceAuditor** (`tests/websocket_test_helpers/conformance.rs`)
  — landed in session 030. The auditor wraps `DeliveryLedger`, validates paired
  v3 `(epoch, seq)` stamps per `(receiver, sender)`, enforces lifecycle and
  reconnect-watermark baselines, records loud close/fault causes, checks
  cumulative relay statistics, and decodes both text and bare MessagePack data
  frames. `relay_chaos_e2e` and `scenario_realworld_e2e` now feed every observed
  frame through the auditor and terminate through its combined payload + metric
  conservation assertion. Late joins establish a receiver-local first-frame
  baseline; subsequent frames remain strictly contiguous. Focused cases live in
  the dedicated helper-test binary so they do not multiply across the 25 suites
  that import `websocket_test_helpers`. See
  `progress/session-030-p10b3-conformance-auditor.md`.
  — the always-on checker every subsequent e2e asserts. Wraps (does not
  replace) `DeliveryLedger`. Tracks seq per **(receiver ← sender, epoch)**
  with epoch boundaries at `PlayerJoined`/`PlayerReconnected` (note:
  the ledger's `server_seq` hook at
  `tests/websocket_test_helpers/delivery_ledger.rs:140-161` is
  per-receiver-monotone and would false-fail multi-sender rooms if fed v3
  seq directly — this is why the auditor exists). API sketch:

  ```text
  struct ConformanceAuditor { /* per receiver: DeliveryLedger + SeqTracker + CauseLog */ }
  enum Cause {
      OwnSlowConsumerClose,          // this receiver saw close 4002
      OwnActivityTimeout,            // 4003
      SenderEpochRestart(PlayerId),  // PlayerLeft/PlayerJoined/PlayerReconnected for sender
      UndeliverableFormat { seq_skipped: u64 }, // Error UNSUPPORTED_GAME_DATA_FORMAT
      ReplayTruncated,               // Reconnected{replay: Truncated} => wildcard until
                                     // first post-reconnect frame per sender (re-baseline)
      InjectedFault(String),         // test-authored: RST, SIGKILL, proxy pause
      ServerRestart,
  }
  fn record_wire_frame(receiver, &ServerMessage);  // GameData/controls/Errors/RelayStats
  fn record_close(receiver, code: u16, reason: &str);
  fn record_reconnected(receiver, replay: ReplayStatus);
  fn note_injected_fault(receiver, what: &str);
  fn assert_conformance(&self, metrics, expectations);
      // (1) zero-loss-or-loud (DeliveryLedger), (2) per (receiver←sender,epoch):
      // seq contiguous-from-1, every gap/backwards-jump matched to a Cause
      // recorded BEFORE the post-gap frame, (3) conservation cross-check.
  ```

  Duplicate seq within an epoch is always fatal. The
  `relay_chaos_e2e` and `scenario_realworld_e2e` retrofits landed with the
  auditor in session 030.
- [x] **B4 — time abstraction (core).** Landed (session 017) — see
  [`progress/session-017-p10b4-time-abstraction.md`](progress/session-017-p10b4-time-abstraction.md).
  `src/server/connection_manager.rs` now reads `tokio::time::Instant` (the
  activity reaper `collect_expired_clients` + the heartbeat throttle
  `should_update_last_seen`); production behavior is identical (outside a paused
  runtime it wraps the same monotonic std clock) and every `Instant::now()` here
  already runs inside the tokio runtime, so there is no panic risk. The
  `heartbeat.rs` reaper test that used a real 25 ms `sleep` is now
  `#[tokio::test(start_paused = true)]` + `tokio::time::advance()` (instant,
  no wall-clock dependence), and two new paused-clock tests pin the throttle
  boundary and the unknown-player branch. **Audit result:** `src/rate_limit.rs`
  is already on `tokio::time::Instant` (no change needed); `src/auth/rate_limiter.rs`
  is deliberately left on `std::time::Instant` — it is not a named target and its
  sync `#[test]` unit tests would panic under `tokio::time` (a clean follow-up if
  auth-window paused-clock tests are ever needed). The `chrono::Utc::now`
  reconnection windows / `Clock` seam and the deterministic-simulation
  spikes (shuttle/turmoil, `delivery_concurrency_stress.rs` coordinator races)
  remain follow-ups.

### P10.C — Falsification experiments (Size L) — pre-registered predictions

Rules: reuse `ChaosProxy` (`tests/websocket_test_helpers/chaos_proxy.rs`:
pause/throttle/fragment/RST), `DeliveryLedger`, B1–B3. Zero-flaky policy:
poll-with-ceiling (`poll_until` pattern, `tests/relay_chaos_e2e.rs:173-182`),
no sleeps-as-sync; nightly-scale tests are `#[ignore = "nightly-only (...)"]`
wired into `.github/workflows/verification-nightly.yml`; real-binary suites go
in the nextest `process-spawning` group (`.config/nextest.toml`). Each
experiment records prediction → result in a `progress/` session note; every
red finding becomes either a fix (red-green) or a documented limitation.

| # | Claim under falsification | File (new unless noted) | Pre-registered prediction | Lane |
|---|---|---|---|---|
| H11+H12 ✅ s017 | 16 players can connect and join at defaults | `tests/sixteen_player_admission_e2e.rs` | **Result:** GREEN post-A3 — 16 same-IP clients admit + join one room at the default cap (24); 9th join into a default-cap (8) room is refused RoomFull. Non-vacuity confirmed: forcing the cap to the pre-A3 value 10 fails the admission test. Regression guard landed. | PR, P0 |
| H1+H13 ✅ s017 (no-cascade facet) | One slow consumer costs ≤1 timeout window; control plane stays live during the stall | `tests/slow_consumer_no_cascade_e2e.rs` (new) | **Result: the distinctive DETERMINISTIC claim — NO CASCADE — landed.** 1 flooder + 1 hard-stalled + 3 healthy peers: each healthy peer gets the complete in-order stream + a `PlayerLeft` for the stalled peer ONLY; `slow_consumer_disconnects == 1` EXACTLY (existing tests only used one witness + `>= 1`, never pinning non-spread); conservation holds. 5×+ green incl. under full-suite load; non-vacuity mutation-proven. **Deferred (nightly, brittle timing):** the ~5 s whole-room-freeze DURATION / Pong-RTT spike + join-during-stall. | PR + nightly |
| H9 ✅ s017/s032 (COVERED) | Every continuing-stream seq gap has complete causally prior exact coverage; reconnects re-baseline | `v3_game_data_sequencing_e2e.rs` + `relay_chaos_e2e.rs` + `reconnection_replay_e2e.rs` + conformance/reference-client suites | **Result:** eviction/reconnect, sender incarnation reset, RST/replay, truncated replay, classified omission, lifecycle overtaking, and bounded report rollover are covered. `DeliveryLedger`, per-class conservation, and `ConformanceAuditor` enforce zero silent loss; E5/E6 closed the former truncation phase. | PR + nightly chaos |
| H3 ✅ s017/s028 (COVERED) | Deploy/restart: what 16 clients observe; storm recovery | (existing) `v3_multiprocess_e2e.rs` + `sixteen_player_admission_e2e.rs` | **Result:** restart invalidates old tokens; rejoin-by-code without the original capacity can recreate at cap 8; E3 adds real-process SIGTERM `GoingAway` + close 4000 + no reconnect record. A duplicate restart+capacity composition test remains intentionally omitted. | PR |
| H2 ✅ s044 | 16×30msg/s×1KiB clean relay; find the knee | `tests/sixteen_player_matrix_e2e.rs` (reused exact-ledger harness; no duplicate load test) | **Result:** all 223,200 sweep deliveries across 30/60/120/240/480 msg/s per player were exact, with zero queue backpressure and zero eviction. Exact-head delivery rose from 6.9k/s to 17.0k/s through the 120 target; doubling offered fan-out at 240 produced only 5.0% more completed throughput while p99 jumped from 149 ms to 851 ms, reaching 2.72 s at 480. Writer completion slipped to 2.53/3.33 s before outbound queue pressure appeared, locating this shared-process runner's knee at the client/socket-ingress boundary rather than the server delivery queue. The machine-dependent timing is reported, not a portable pass threshold. | nightly ✅ |
| H8 model ✅ s017/s039; clean empirical ✅ s037; cripple empirical ✅ s038 | 16-player mesh formation fits 600 signals/min | `tests/signal_budget_model.rs` (model, PR ✅) + `tests/webrtc_mesh_budget_e2e.rs` (clean + cripple empirical, nightly ✅) | **Result:** confirmed. The corrected shared control-plane model gives 166 slots/player at N=16, K=10: 165 signals plus one accepted `TransportStatus`, vs `max_signals=600`. Clean exact-head CI formed all 120 edges with exact channel/relay ledgers, zero fallback/pre-release drop/eviction, and **45 signals for every client (720 total; 13.3x signal headroom)**. Session 038's one-crippled-member exact-head run preserved the exact 105-edge healthy submesh, zero-edge crippled member, exactly one held fallback, complete relay floor, and exact signal conservation: **690 total signals (45 per healthy client, 15 crippled), 30.401 s barrier, 30.884 s total**, 24.8 MiB server peak and 24.9–29.1 MiB client peaks. | PR + nightly |
| H4 ✅ s017 (PR-lane already covered — no new test) | Half-open TCP: blind window length; loss visibility after reconnect | (existing) `tests/v3_game_data_sequencing_e2e.rs::evicted_recipient_observes_seq_gap_after_reconnect`; blackhole-pause in `tests/scenario_realworld_e2e.rs` | **Result: REDUNDANT — the distinctive claim is already tested.** `evicted_recipient_observes_seq_gap_after_reconnect` (PR-lane) blackholes a v3 victim (stalled reader), evicts it (poll-to-eviction, conservation asserted), reconnects it, and asserts it OBSERVES the v3 seq gap (`marker_seq == WARMUP+FLOOD+1`, gap>0) — i.e. "the sender's counter never reset ⇒ loss is visible," H4's exact falsification. ChaosProxy-pause half-open is covered in `scenario_realworld_e2e.rs`. Adding `half_open_tcp_e2e.rs` would duplicate these (bloat) — deliberately NOT added. **Deferred (nightly):** blind-window duration is brittle-timing (not asserted per policy); the SIGSTOP/post-SIGCONT multiprocess ledger check. | PR (covered) + nightly |
| H6 ✅ s017/s057 | Reconnect window/claim edge races | `tests/reconnect_window_races_e2e.rs` + `src/server/signaling_tests.rs` | **Result: all three facets CONFIRMED.** Window boundary — inside ⇒ `Reconnected`, after ⇒ `ReconnectionFailed` (time-expiry code), proven to be the handler's check not cleanup deletion via `has_pending_reconnection` true after the sleep with `room_cleanup_interval=3600s`; non-vacuity shown by widening the window. Duplicate-claim — two barrier-released same-token claims on a 2-thread runtime ⇒ exactly one winner (lock-serialized). Reconnect-during-teardown — a `cfg(test)` event gate pauses the real unregister transaction after the record is armed and before the old connection is removed; the replacement receives exact `PlayerAlreadyConnected`, the token remains valid, and the same token returns `Reconnected` after the gate releases teardown. Zero-flaky: explicit synchronization, no timing race or retry loop. This closes the F3 SDK retry rule's server-side evidence. | PR |
| H10 ✅ s046/s047 | Asymmetric bandwidth (≈256kbps victim vs ~90KB/s demand): stable experience or flap loop? | `tests/asymmetric_bandwidth_flap_e2e.rs` | **Result: class contract confirmed after production fix.** Reliable still fails loudly: exact-head s047 closed `4002` after 22.495/21.038 s and both wire-token reconnects succeeded/rotated. Volatile remained connected for the full 60 s with default 10s/5s WebSocket probes enabled and conserved exactly: **9,474 offered = 3,879 delivered + 5,595 causally reported drops** across 51 exact ranges; the final reliable marker arrived and no extra eviction occurred. The healthy peer received the complete stream; max interarrival 980 ms. Root cause was the writer handing megabytes to TCP ahead of later control plus a global sojourn timestamp that let stale lossy data expire fresh reports. Production now requests a configurable 64-KiB socket send buffer (Linux reports about 128 KiB) and partitions writer deadlines: oldest reliable end-to-end, control's own enqueue age, lossy write-progress only. A 32-KiB candidate also passed, but 64 KiB retained more bandwidth-delay-product headroom while staying below the 5-second Pong deadline at 32 KiB/s. H10-R is complete. | PR + nightly ✅ |
| H7 ✅ s017 | `Truncated` + snapshot suffices to rebuild room state | extend `tests/reconnection_replay_e2e.rs` | **Result: prediction REFUTED — no holes.** `ReconnectedPayload` already carries the full snapshot: `current_players` (each with `is_ready`/`is_authority`), an explicit `ready_players` list, AND `current_spectators` — plus `lobby_state`, `max_players`, `supports_authority`. New `truncated_replay_still_carries_full_room_snapshot` proves a ready peer + a live spectator (established before disconnect) both survive in the snapshot of a TRUNCATED reconnect. So "Truncated ⇒ app-level resync from the snapshot" holds with no additive fields; the test guards it against regression. | PR |
| H14 ✅ s041/s047 | Mixed-encoding rooms: `UNSUPPORTED_GAME_DATA_FORMAT` per-message error storm | `tests/mixed_encoding_relay_e2e.rs` | **Result: RED, then GREEN.** JSON-representable MessagePack crosses mixed rooms losslessly. A sustained unconvertible stream reproduced the distinctive failure: with identical throttles, the JSON fallback was evicted after 4,059 per-message report/error pairs while the compact binary recipient survived. The supplemental prose error is limited to one per `(recipient, sender)` per second with a bounded 256-sender table and suppressed-count rollup; exact per-sequence `DeliveryReport` gaps remain unthrottled and authoritative. H10's bounded TCP handoff exposed a second deadline bug in exact-head CI: writer-known unsupported outcomes still inherited oldest-reliable age and closed at 20.86 s. S047 preflights the one current binary fallback, reuses valid decoded data, and gives deterministic unsupported report/advisory writes their bounded progress deadline. Final local CI repro: 5,000 exact reports + 21 advisories completed under 32 KiB/s in 67.88 s, zero eviction, compatible control complete. | PR + nightly ✅ |
| H5 ✅ s034 | Two instances behind an LB: failure catalog (documentation experiment — product is the catalog, not a pass) | `tests/split_brain_two_instances_e2e.rs` | **Result: prediction confirmed and refined.** The same `(game_name, room_code)` silently creates distinct local rooms; A's real token presented to B yields `ReconnectionFailed` / “No disconnection record found”; A signaling B's player yields explicit `SignalTargetNotFound` / “target is not in any room” (not `CrossRoomSignal`, because B's player is wholly absent from A's registry). Nightly two-real-process lane; feeds F1. | nightly ✅ |

- [x] **C-matrix — 16-player grid.** `tests/sixteen_player_matrix_e2e.rs`
  (relay: {JSON, MessagePack} × {clean, jitter-throttle, burst pause/resume}
  × {2, 8, 16}; PR lane = 6 clean cells ≈ 1 min serial; nightly = full grid)
  and `tests/webrtc_mesh_budget_e2e.rs` (native clients via B2;
  {mesh, host} × {clean, 1% loss via `tc netem` on `lo`} × {2, 8} + clean×16;
  the netem step exports `SF_NETEM_ACTIVE=1` and the test **fails loudly** if
  faults were requested but absent — zero-silent-skip). Relay cells run the
  ConformanceAuditor; WebRTC cells use exact plan/graph/signal/channel/status/
  relay ledgers over the native client's JSONL contract. Per-cell pass bars:
  clean → zero evictions, zero
  backpressure, p99 < 250ms (Linux/Windows; exempt on macOS — issue #274);
  fault cells → completeness after fault lift,
  evictions all 4002/4003-attributed, rooms heal. Metrics per cell: ledger,
  latency percentiles (relay cells only), backpressure/eviction counts, RSS.
  **Clean PR lane ✅ (session 036):** all six encoding/size cells run real
  WebSockets at 30 msg/s/player with 1 KiB payloads, use data-lane barriers to
  prove the complete control-priority join lifecycle before measurement, and
  feed JSON text or true MessagePack binary frames through the
  ConformanceAuditor. Each cell requires exact payload/stamp completeness,
  zero default-queue backpressure, zero eviction, and p99 <250 ms (Linux/Windows; exempt on macOS — issue #274) while
  reporting RSS. Default-queue run: 17,880/17,880 deliveries, p99 20–55 ms.
  **Relay fault lane ✅ (session 036):** twelve additional per-client-proxy
  cells cover deterministic jitter + 128 KiB/s downstream throttling and a
  downstream pause held until every sender completes its burst. Every cell
  recovers complete after lift with zero backpressure/eviction; measured p99
  reflects the injected shape (167–515 ms jitter/throttle, 0.98–1.01 s burst
  pause). The ignored
  suite is wired into `verification-nightly.yml`. **Clean WebRTC N=16
  implementation + exact-head CI ✅ (session 037):** 16 native
  webrtc-rs clients must hold simultaneously at a complete 120-edge mesh,
  exchange both exact per-edge data-channel ledgers and the WebSocket relay
  floor, stay under the real 600-signal default per client, and report signal
  p50/p99 plus process RSS. Measured result: every client emitted 45 signals
  (p50/p99 45, total 720), barrier 2.023 s, total 2.513 s, server peak 24.5 MiB,
  clients 28.5–28.9 MiB. **Crippled-ICE implementation (session 038):** one
  crippled member must form zero edges and fall back exactly once while the
  fifteen healthy members retain the exact 105-edge submesh; all sixteen retain
  the full relay floor and exact signal ledger. Exact-head CI passed with 690
  total signals (45 per healthy client, 15 crippled), a 30.401 s barrier, and
  30.884 s total runtime. **Clean topology/size grid ✅ (session 039):** the
  held real-process harness now covers clean mesh and host at N=2/8/16. It
  requires the exact `N(N-1)/2` mesh or `N-1` creator-hosted star (including
  host/leaf offer direction and zero leaf-to-leaf signaling or channel
  traffic), exact signal send/receive conservation, both per-edge channel
  ledgers, the full WebSocket relay floor, all room-wide transport-status
  fan-outs, the shared `Signal` + `TransportStatus` budget, held/final delivery
  conservation, and per-process RSS. **Netem implementation + exact-head CI ✅
  (session 040):** four independently isolated `{mesh, host} x {2, 8}`
  cells install and verify 1% loopback loss through ICE/DTLS/SCTP/channel
  formation, then verify fault removal before releasing exact reliable +
  unreliable channel exchange. A deterministic 100%-loss UDP probe proves the
  qdisc can really drop packets; `SF_NETEM_ACTIVE=1`, passwordless sudo, qdisc
  presence, removal, and per-test shell cleanup all fail loud rather than
  silently substituting a clean run. Exact-head privileged CI passed every
  cell and recorded non-zero loss-phase qdisc drops (mesh N=2: 1; mesh N=8: 35;
  host N=2: 1; host N=8: 8), with the final cleanup confirming no qdisc
  remained.
- [x] **C-partition — CAP suite.** `tests/partition_scenarios_e2e.rs` and
  `tests/webrtc_mesh_budget_e2e.rs`:
  (1) symmetric blackhole mid-room (reaper eviction in
  [ping_timeout, ping_timeout+cleanup_interval], 4003, room heals);
  (2) **asymmetric server→client** (pause ServerToClient only): client pings
  keep flowing, queue fills → **4002 despite a live, pinging client** — the
  CP-over-availability choice made observable; (3) **asymmetric
  client→server** (pause ClientToServer only): 4003 while the client was
  happily receiving → SDK rule "liveness = sending" (F3); (4) restart =
  H3; (5) P2P partial-mesh partition with relay-floor assertion
  (`--cripple-ice` coarse case now; design a per-peer `--drop-ice-from
  <ordinal>` knob for `clients/native` — seam at
  `clients/native/src/client.rs:876` — as the follow-up for true pairwise
  partitions). **Session 042 completed the three real-socket directional
  cells:** symmetric blackhole and client→server blackhole close with exact
  `4003`/missed-Pong attribution; server→client blackhole under reliable relay
  pressure closes with exact `4002` attribution after proving an application
  Ping crossed the unaffected direction. Every cell observes `PlayerLeft`,
  admits a replacement, and relays after recovery. E4 falsified the old
  30–90-second client→server estimate: idle socket partitions are now bounded
  by roughly the probe interval + Pong deadline (10 + 5 seconds by default).
  The measured table and F3 bidirectional-liveness SDK rule are documented.
  H3 already covers restart; session 038 covers the coarse crippled-ICE relay
  floor. **Session 043 completed the last facet:** reciprocal
  `--drop-ice-from <ordinal>` faults remove exactly one N=3 mesh edge without
  suppressing offer/answer/candidate signaling observability. Both healthy
  edges exchange exact reliable/unreliable channel ledgers, every member
  retains the complete relay-floor ledger, all clients correctly report
  overall WebRTC `connected:true` (the status is any-pair), and the exact drop
  ledger proves non-vacuous fault scope.

### P10.D — Formal expansion (Size L) — spec-first, house conventions

Every module: seeded-bug constant with a documented minimal counterexample
(bug=TRUE cfg must fail, checked cfgs pin FALSE), `Done` stutter with deadlock
checking on, action↔code correspondence comments, small exhaustive
`_Small.cfg` (auto-globbed by `scripts/run-tla-model-check.sh` — zero CI
plumbing). Module sketches: Appendix O. Cut deliberately: standalone
ReconnectClaimSafety (claim race is lock-serialized +
`fuzz_reconnect_tokens`-covered; the residual GC/rollback interplay is
absorbed into D1); RateLimiter TLA+ (fixed-window counter behind one lock —
build a perfect-recall proptest twin in
`tests/model_based_state_machines.rs` style instead, and pin the documented
`2N−1` sliding-window burst property); a must-fail MultiInstance module
(inverts the runner contract — replaced by D6).

- [x] **D1 — `RoomLifecycleGC.tla`** — landed with A1 (session 016). Models the
  GC sweep vs activity refresh + the reconnection `protected` guard, with a
  `StaleActivityBug` seeded constant: TRUE reproduces the pre-fix behavior and
  TLC violates BOTH `ActiveRoomNeverReaped` and `ReconnectWindowRespected`
  (verified during dev); FALSE in the two checked cfgs (`_Small`,
  `_WindowBoundary`), both green in the auto-globbed suite.
- [x] **D2 — `ControlPriorityDelivery.tla`** (session 019/020) — spec-first for
  E2's queue split + sojourn eviction, merged BEFORE that implementation;
  absorbs DeliveryLiveness. Composes with `DeliveryContract.tla` (does not
  re-derive the data grace close). Frames by CLASS (data|ctrl) with discrete
  age; the writer is UNFAIR. `ControlAgeBounded` (control drained strictly
  first, never starved behind data) + the `DeliveryEventuallyResolves` liveness
  (`enqueued ~> written ∨ closed` under `WF(Tick, SojournEvict, CloseFinish)`
  ONLY — never writer fairness; the sojourn close is what makes it true) +
  `PerClassConservation` + `CtrlDropsAreLoud` + `StalenessBounded`. Two seeded
  bugs, both pinned FALSE in the checked cfg (215 states, green): `SingleQueueBug`
  (control on the data FIFO) violates `ControlAgeBounded`; `NoSojournEvictionBug`
  violates the liveness. **A TLA+ operator-precedence bug (`x' = a \/ b` parses
  as `(x' = a) \/ b`, leaving `x'` free) was found + fixed during dev and the
  whole `formal/tla/` swept — `RoomLifecycleGC.tla` already parenthesized
  correctly, so D2 was the only instance.** Appendix O.2.
- [x] **D3 — `SenderPacingReaper.tla`** (session 019). The repo's first
  discrete-tick (`now` + `Tick`) model; composes join_all parking + serialized
  receive loop + reaper clock + the **pre-park delay** (`maybe_update_last_seen`
  DB write + `rooms` lock). Time advances only while parked (capped at grace) or
  once while broadcasting (the pre-park delay `d`), so the reaper-visible gap
  peaks at `d + SLOW`. `HealthySenderNeverReaped == ~sndEvicted` is the contract;
  `GapWithinPingDeadline` is the stronger boundary characterization (trips one
  step earlier). The `TimeoutInversionBug` cfg (effective grace = ping, the
  boundary) exhibits the 4003-on-healthy counterexample via the `d = 1` path
  (peak gap `PING + 1`); the two checked cfgs `_Small` (slow 2 < ping 4) and
  `_Boundary` (slow 3 = ping − 1, tightest safe) are green in the auto-globbed
  suite. **Deliverable:** modeling `d`, the model derives `slow >= ping` unsafe —
  exactly the region the A2 check rejects — so the strict `<` is the NECESSARY
  floor (not proven sufficient: `d` is unbounded under lock contention, so a
  thin margin can still invert; operators size the margin). No behavior change;
  the check comment + boundary-test doc + `formal/README.md` carry the framing.
  Reworked after an adversarial formal-methods review flagged a
  necessity-as-sufficiency overstatement in the first draft. Appendix O.3.
- [x] **D4 — `EndToEndGapAccountability.tla`** (flagship) (session 022) —
  composes SequencedRelay + ReconnectReplay + Teardown across a full
  disconnect/reconnect with **two senders** (the single-sender `justified` flag
  abstraction is unsound at ≥2 senders — justification is tracked per
  `(recipient, sender)` pair) and a per-recipient `sockBuf` between writer and
  client observation (written-to-socket ≠ processed-by-client; both wiped at
  eviction, modeling kernel-buffer loss). **The "snapshot heals the dropped
  tail" argument holds**: on reconnect the client REPLACES membership with the
  authoritative snapshot set (upsert, not delta replay) AND re-baselines each
  per-sender `(epoch, seq)` from the `Reconnected.sender_watermarks` — the
  executable proof that **E5's watermarks are necessary**. Three seeded bugs
  pin non-vacuity, each violating exactly its paired invariant:
  `SingleFlagBug` / `NoBaselineResetBug` → `ClientCanClassify`,
  `NoSnapshotReconcileBug` → `MembershipEventuallyHonest` (all verified red).
  Shipped `_Small.cfg` exhaustive (2602 distinct states, complete graph at
  depth 16 — the tiny 1-slot config makes the full graph small, not the
  1–10M originally estimated) + `_Sim.cfg` bounded random simulation, wired by
  a runner addition: cfg basename ending `_Sim` → `tlc -simulate num=20000
  -depth 80` (`num` is per-worker so wall-clock is ~constant across cores;
  20000 walks ~19M sampled states in ~40–65s, right-sized from the original
  200000 which was ~7min and blew the shared 10-minute CI budget). Both green;
  TLC exits non-zero on a violation under `-simulate`, so the sim has teeth.
  Appendix O.4.
- [x] **D5 — `DeliveryClasses.tla`** (session 020/021) — spec-first for E2's
  reliable/latest/volatile classes. Frames by `[class, key, id]` (unique ids),
  unfair writer. `ReliableConservation` + `LatestConservation` +
  `VolatileConservation` (each class only in its legit buckets),
  `AccountedSupersession` (held ALWAYS — ledger write atomic with the coalesce),
  `LatestValueLastWrite` (≤1 queued latest/key, queued rep newest vs
  superseded∪written), `ReportHonest` (out-of-band DeliveryReport never
  overstates), `UniqueIds`, `CoalesceNeverTouchesReliable`. `latest` NEVER parks
  (new-key on a full queue → drop-oldest-volatile or `latDropped`). Four seeded
  bugs (SilentSupersedeBug, CoalesceReliableBug, MisdropLatestBug,
  ReportOverstateBug), each violating its intended invariant; checked cfg green
  (233435 distinct states, LatBudget=3 so the 2-level supersession chain is
  reachable). The proposed per-successor `supersedes_from` scalar was rejected:
  the later `ScalarInPlaceBug` trace proved it unsound across interleaved keys,
  so the implementation uses causally prior exact reports instead. **Two
  adversarial-review rounds: build →
  FIX-FIRST (3 MAJOR faithfulness gaps: reported-vs-scalar + unreachable chain;
  new-key latest parked; no latest/volatile conservation) → fixed → SHIP.**
  Appendix O.5.
- [x] **D6 — split-brain seeded constants (session 018).** Landed — see
  [`progress/session-018-p10d6-split-brain-seeded-constants.md`](progress/session-018-p10d6-split-brain-seeded-constants.md).
  `SplitBrainStampBug` in `SequencedRelay.tla` adds a second instance
  (`SendSplit`) that stamps the same sender's stream from an independent
  `counter2`; TLC violates `GapAccountable` in a 4-action trace (duplicate/
  regressing `seq`, no bracket). `SplitBrainCounterBug` in `ReconnectReplay.tla`
  serves the reconnect from a second instance that join-created the room fresh
  (empty ring, zero watermark, its own `next_sequence`); TLC violates
  `ReplayFaithful` in 3 actions (empty replay drops a retained needed event) and
  `StatusHonest` in 5 (false `complete` over an eviction). _(Refinement vs. the
  original sketch: the reconnect break surfaces first as `ReplayFaithful`, not
  `StatusHonest` — both fire and both are documented; the model is the honest
  ARCH-10 stranded-token shape rather than colliding numeric counters, which
  would make the seq-keyed ghosts ambiguous.)_ Both constants pinned `FALSE` in
  the checked cfgs (state spaces byte-identical to baseline, full auto-globbed
  suite green); each spec header carries its minimal trace. New
  `formal/README.md` "Single-instance theorems (split brain / ARCH-10)" section
  catalogs which invariants are single-instance theorems and states the LB
  room-affinity requirement (feeds F1).
- [x] **D7 — trace-validation pilot (eXtreme-modelling)** (session 045).
  `trace-validation` adds a per-connection JSONL recorder at the exact
  `deliver_or_disconnect` match arms and writer dequeue/write/finalize points.
  The strict generator accepts only the current `v2_legacy_reliable_fifo`
  abstraction, bounds and validates the input, and emits a temporary replay
  bundle. `DeliveryContractTrace.tla` includes the real writer's in-flight
  slot; impossible events make `TNext` unsatisfiable at the offending `i`.
  Existing fixed-seed model-based cases emit the corpus, focused paused-clock
  tests cover parked enqueue/channel-close arms, and a seeded bad first drain
  proves the failure path. The daily verification workflow runs the pilot
  non-gating and uploads evidence. The chaos-suite `DeliveryLedger` refinement
  against `SequencedRelay` remains the explicitly separate second target.
- [x] **D8 — tooling decisions (recorded)** (session 019/020). The recorded
  decisions now live in `formal/README.md` → "Tooling decisions (P10.D8)":
  TLC-first (explicit-state; small finite models, concrete counterexamples);
  discrete-tick integer time (every timed property is a relation between timeout
  constants, not dense time — see `SenderPacingReaper` / `ControlPriorityDelivery`);
  Apalache dev-side only (a future `scripts/run-apalache.sh`, **not CI-gating**
  until it catches something TLC missed); PlusCal rejected (its `pc` breaks the
  action↔code correspondence). The stale-README refresh clause was done in the
  D8-partial commit — the Layout table was a
  module-oriented catalog of all 7 TLA+ specs (it had listed only
  `SignalFishSession`, and "all four configurations" when there are 12), and the
  auto-glob note added. The optional future Apalache helper remains deliberately
  non-gating and is not an unfinished D8 deliverable.

### P10.E — Protocol v3 delivery-reliability additions (Size XL) — v3 is the mutable pre-release version (no separate v4)

One coherent package; full wire shapes in Appendix N. E1–E8 are complete,
each gated by the relevant TLA+ module, wire goldens, and ConformanceAuditor
suites where applicable. **No new negotiation flags: v3 is not
live, so these ARE v3.** The §2.2 gating invariant still governs v2/v3: none of
this is visible below negotiated v3.

- [x] **E1 — `epoch`.** ✅ **DONE (session 023, PR #145 — fully green).** See
  [`progress/session-023-p10e1-incarnation-epoch.md`](progress/session-023-p10e1-incarnation-epoch.md).
  `GameData`/`GameDataBinary` gained `epoch: Option<u32>` beside `seq`;
  `PlayerInfo` (so `RoomJoined.current_players[]` /
  `SpectatorJoined.current_players[]` / `PlayerJoined.player` / `Reconnected`
  snapshots) and `PlayerReconnected` carry each player's current epoch; the bare
  `BinaryGameDataFrame` gained a trailing `epoch` key. All
  stripped per-recipient for pre-v3 ⇒ v2 wire byte-identical (goldens
  unchanged). **Refinement vs. the sketch:** epoch is a single **monotonic
  per-connection** counter (`ClientConnection.game_data_epoch`), NOT a
  `(player, room) → epoch` map — the map would replay a LOWER epoch on a
  same-room leave+rejoin (recipient sees it go backwards) unless it kept
  unbounded per-`(player, room)` state; the monotonic counter upholds the
  data-lane "`(epoch, seq)` increasing per (sender, room)" invariant for free
  (clients baseline relatively). Priority peer lifecycle may overtake a queued
  old-epoch tail, which clients account but suppress after the lifecycle change.
  It increments at the two incarnation
  starts (`assign_client_to_room`, `reassign_connection`), read atomically with
  `seq` via `next_relay_stamp → RelayStamp`, and — the subtle part — **survives
  reconnect**: captured into `DisconnectedPlayer.last_epoch` at disconnect and
  resumed at `last_epoch + 1`, so it stays strictly increasing across the
  sender's absence for a recipient that never left. `saturating_add` (never
  wraps below the `minimum: 1` contract). Review cycle: Bugbot's `missed_events`
  epoch leak (a real v2/v3 regression — replayed control events bypass the
  send-layer strip) fixed red-green; Copilot's overflow nit fixed; a holistic
  adversarial review found zero other defects. Precedent: Aeron image
  sessionId, Kafka producer epoch (KIP-98). Rejected alternative: a
  `SeqReset` control message (reintroduces the cross-queue ordering
  dependency E2 removes).
- [x] **E2 — delivery classes + control-priority queue + DeliveryReport.** ✅
  **DONE (session 032).** Negotiated-v3 JSON `GameData` accepts `reliable`
  (default), keyed `latest`, and `volatile`; raw binary remains reliable. Policy
  is per recipient, so mixed-v2/v3 rooms keep v2 reliable FIFO while v3 peers
  get classified delivery. Reliable waits for bounded data capacity; latest
  supersedes the queued same-`(sender, room, key)` predecessor or, for a new key
  on a full queue, evicts the oldest volatile value or drops the arrival;
  volatile evicts the oldest queued volatile value or drops the arrival. Lossy
  classes never park senders. Each omission atomically queues a causally prior
  exact inclusive range in `DeliveryReport`; one frame holds at most 256 ranges
  and excess non-mergeable ranges roll into bounded control frames. Cumulative
  per-class counters and global labeled metrics obey conservation at quiescence,
  but never authorize a gap. Within an active recipient generation control has
  strict priority; the recipient's own room/spectator transitions are ordering
  barriers. Reliable capacity timeout, oldest outbound/write sojourn expiry, or
  unavailable exact-report capacity fails closed with `4002 slow_consumer`.
  Unsupported-format replacement records its original class and exact range
  before attempting the supplemental error. V2 goldens remain byte-identical.
  See [`progress/session-032-p10e2-delivery-accountability.md`](progress/session-032-p10e2-delivery-accountability.md).
- [x] **E3 — graceful drain.** ✅ **DONE (session 028).** SIGTERM/Ctrl-C now
  starts a drain in `src/main.rs`: new WebSocket upgrades return 503, existing
  room-creating joins are rejected with `SERVER_DRAINING`, negotiated-v3 clients
  receive a best-effort `GoingAway { deadline_ms, retry_after_secs }`, and the
  server then requests `CloseReason::Shutdown` so sockets close with **4000
  `server_shutdown`** under the existing close-frame flush discipline. Added
  `server.drain_grace_secs` (default 30; `0` immediate), joined the cleanup task
  through the same shutdown signal, and kept TLS/non-TLS serve paths on graceful
  shutdown. Drain-close unregister skips reconnect-token arming and discards any
  pre-issued token, matching the deliberate no-session-handoff single-instance
  stance. The shutdown wait tracks real WebSocket handler completion (both
  socket halves through bounded close), not early connection-map unregister, and
  shutdown room cleanup removes membership quietly without `RoomLeft` /
  `PlayerLeft` noise ahead of the 4000 close. Follow-up hardening closed the
  remaining drain races: shutdown close now supersedes earlier lifecycle close
  reasons, normal room/spectator/maintenance traffic is conditionally canceled
  at the enqueue commit point once drain starts, pending reconnect records are
  discarded if drain wins after normal disconnect registration began, and the
  reconnect baseline fetch is serialized with room routing/replay registration.
  Coverage: in-process `GoingAway`+4000+no-reconnect e2e, no-room-traffic
  shutdown unregister test, room-creation lock-order rejection test, conditional
  room/spectator/reconnect drain race tests, real-binary SIGTERM multiprocess
  test, v3 `GoingAway` wire golden/round-trip, AsyncAPI/docs/config guards. See
  [`progress/session-028-p10e3-graceful-drain.md`](progress/session-028-p10e3-graceful-drain.md).
- [x] **E4 — server-initiated WS protocol pings.** ✅ **DONE (session 033).**
  Sent from the socket
  layer below the queues (probe the transport, not the queue):
  `websocket.server_ping_interval_secs` (default 10, 0 = off) +
  `websocket.pong_timeout_secs` (default 5) → close 4003 on missed pong;
  pong receipt feeds the activity reaper (belt stays). Halves GAP-6 to
  ≤ ~15s; free per-connection RTT metric. RFC 6455 pings are auto-answered
  by all compliant stacks incl. browsers — zero client work, no wire change,
  no negotiation. Rejected: app-level JSON ping message (burns queue
  capacity, requires client releases). The implementation uses one outstanding
  nonce per connection, rejects stale/unsolicited Pongs, starts the timeout only
  after the Ping write completes, bypasses both application delivery lanes, and
  exports aggregate RTT histograms plus a missed-Pong counter. Real-socket tests
  cover matching Pong, stale pre-probe Pong, asymmetric client→server blackhole,
  close 4003, metrics, and the `0` disable path. See
  `progress/session-033-p10e4-server-websocket-pings.md`.
- [x] **E5 — `Reconnected` per-sender watermarks.** ✅ **DONE (session
  027).** `Reconnected` payload gained v3-only
  `sender_watermarks: [{player_id, epoch, seq}]` — the authoritative
  re-baseline set for every current room member. Game-data broadcasts allocate
  their relay stamp in the same room-routing read section that snapshots
  recipients; reconnect takes the corresponding write lock, reads each member's
  current relay tail without advancing counters, queues `Reconnected`, and only
  then registers the socket into room routing. This makes the baseline the
  first room frame and prevents a concurrent sender from slipping data ahead of
  it. The field includes `seq: 0` for members that have not relayed in the
  incarnation yet, and is omitted for negotiated-v2 reconnects so the frozen v2
  wire remains byte-identical. The behavioral e2e proves skipped `GameData` is
  not replayed while the watermark reports the sender's tail, and the
  slow-consumer sequencing e2e now asserts the first post-reconnect marker is
  exactly `watermark.seq + 1`.
  Covered by v2/v3 wire goldens, reconnect replay e2e, AsyncAPI, protocol docs,
  and the v3 canonical sample.
- [x] **E6 — accountability rules rewrite.** ✅ **DONE (session 032).** A
  continuing same-epoch hole is valid only when the union of causally prior,
  non-overlapping exact ranges covers every missing sequence. Peer lifecycle
  control may overtake queued old-epoch data: clients retain its accounting
  cursor but suppress stale application payload, require exact announcement of
  every future epoch, and reject old epochs after data advances. Recipient
  room/spectator transitions are generation barriers. Every reconnect replaces
  sender cursors from complete `sender_watermarks` and resets physical-
  connection counters. The ConformanceAuditor and native/browser reference
  clients enforce the same rules. No speculative H14 error coalescer was added.
- [x] **E7 — wire artifacts.** ✅ **DONE (session 032).** Rewrote v3 JSON and
  MessagePack goldens; kept v2 bytes frozen; updated AsyncAPI with exhaustive
  tokens and the 256-range bound; extended protocol/reconnect fuzz scenarios;
  updated canonical v3 samples with runtime-semantic guards; and synchronized
  protocol/config/error/client/operator docs plus CHANGELOG.
- [x] **E8 — future-proofing seam (no implementation).** ✅ **DONE (session
  026).** Added a `transports` capability array to the v3 `ProtocolInfo`
  payload so a future QUIC/WebTransport datagram relay lane can be advertised
  without another negotiation redesign. Current negotiated-v3 payloads advertise
  `["websocket"]`; negotiated-v2 payloads omit the field, preserving the frozen
  v2 `ProtocolInfo` JSON and MessagePack goldens. Covered by additive
  `ProtocolInfoPayload` serialization tests, the end-to-end v2/v3 negotiation
  matrix, v3 sample round-trips, and the AsyncAPI/protocol docs. **Cut list
  (recorded in the ADR, F4, with the
  evidence):** resume-from-seq GameData replay (memory math disqualifies:
  covering the 300s window at 16 senders × 60Hz × 1KiB ≈ 280 MiB/room; a
  1 MiB budget covers ~1.1s; apps must handle `Truncated` anyway —
  end-to-end argument; designated successor = "latest-value replay per key"
  post-E2, gated on a real user need); WebTransport/QUIC relay lane now
  (server crates self-describe non-production in 2026; with self-hosted TURN
  the hard-WebRTC-failure population is ~1–2%; revisit when wtransport drops
  its caveat — QUIC connection migration would then also subsume part of
  the reconnect problem); multi-instance sharding (F1 doctrine instead);
  `SeqReset` message (dominated by E1).

### P10.F — Docs & doctrine (Size S)

- [x] **F1 — "Single-instance deployment contract" page** ✅ **DONE
  (session 034).**
  (`docs/architecture/`): CP posture stated plainly (one home per room;
  losing the home loses the room); LB **room-affinity** requirement
  (reconnects must land on the same instance — token/seq/epoch are
  instance-local); the H5 split-brain failure catalog; drain (E3) interplay
  (LBs stop routing new rooms to draining instances); startup log line
  naming the doctrine. The public page and corrected scaling/deployment/
  architecture surfaces now distinguish shipped in-memory coordination from
  future extension seams. The real-binary H5 catalog pins duplicate logical
  rooms, stranded reconnect state, and explicit cross-instance target-not-found;
  structured startup fields name the single-instance/in-memory/no-handoff
  contract. The in-memory lock remains because production room coordination
  uses it locally; deleting a live local primitive would be incorrect.
- [x] **F2 — operator sizing guidance** ✅ **DONE (session 044).** The scaling
  guide now derives room fan-out and payload-egress floors, publishes H2's
  registered and measured knee tables, and tells operators to reproduce the
  benchmark on their real CPU/TLS/network path. The originally sketched
  `send_queue_capacity × slow_consumer_timeout` "seconds" formula was corrected
  rather than documented: those operands have different units. The actionable
  fail-loud bound is queue fill (`capacity / excess encoded-message rate`)
  **plus** one `slow_consumer_timeout`, with `max_sojourn_ms` as an independent
  bound. The guide also pins the default batching interval as an up-to-16ms
  sparse-message wait (not a universal floor) and retains the measured
  directional partition detection/asymmetry table from session 042.
- [x] **F3 — SDK contract additions** (`docs/guides/building-a-client.md`) —
  **stable-findings subset landed (session 017).** Added two grounded "Common
  pitfalls": (a) rejoin-by-code MUST carry the original `max_players` or a
  vanished room re-creates at the default cap `8` and strands the overflow with
  `ROOM_FULL` (H11/H12 capacity trap); (b) `Reconnect` is windowed (default 300 s
  ⇒ `RECONNECTION_EXPIRED`/`RECONNECTION_TOKEN_INVALID`) and single-winner, and
  a teardown race yields `PLAYER_ALREADY_CONNECTED` with the token NOT consumed ⇒
  retry (H6 + `reconnection_service.rs:247-260`). All error-code names verified;
  doc tests green. **E6 follow-through (session 032):** the guides now define
  exact report-union authorization, lifecycle overtaking/stale suppression,
  room/spectator barriers, and reconnect watermark baselines. **Session 042
  follow-through:** the guide now states the bidirectional liveness rule from
  the CAP experiments: receiving does not prove client→server health,
  application Ping does not prove server→client health, protocol Ping/Pong
  must remain enabled, and `4002`/`4003` terminate the physical connection.
  The client checklist carries both one-way fault cases.
- [x] **F4 — ADR** for the v3 revision (classes + epoch + drain + pings +
  watermarks; the cut list with citations; supersedes the relevant parts of
  `progress/session-015-bulletproofing-followups.md`). ✅ **DONE (session 034):**
  ADR-0006 records the in-place v3 revision, formal/protocol evidence, client
  obligations, and the replay/WebTransport/multi-instance/`SeqReset` cut list.

### P10 sequencing, dependencies, acceptance

| Order | Items | Gate |
|---|---|---|
| 1 | A1–A5 ✅ (s016), B4 ✅ (s017), D1 ✅ (s016 with A1) | red tests exist and fail before each fix; TLC green with bug=FALSE, red with bug=TRUE |
| 2 | B1–B3; C: H11/H12, H1+H13, H9(1–3), H3, H8-model | predictions recorded first; findings triaged red-green or documented |
| 3 | D2, D3 (spec-first, BEFORE any queue-split code); C: H4, H6, H7, C-partition | D3's derived inequality refines A2 |
| 4 | E spec documents (protocol.md/AsyncAPI/ADR draft) + D4, D5 | review checkpoint with owner on final v3 composition |
| 5 | E1–E8 ✅ | each completed item lands with its spec green + conformance suites + rewritten goldens; H9(4) closed by E5/E6; half-open detection bounded by E4 |
| 6 | C: matrix + WebRTC cells + H10/H14/H5; D6, D7; F1–F4 | nightly grid green; doctrine published |

Campaign acceptance bar: (1) BUG-1/BUG-2 fixed with regression tests + TLC
theorems; (2) 16-player matrix green across clean/fault cells with the relay
ConformanceAuditor and WebRTC exact ledgers asserting zero-loss-or-loud + full
gap attribution;
(3) H9 phase-4 green (truncation hole closed by E5/E6); (4) restart test
observes `GoingAway` + 4000 and a drained shutdown; (5) half-open detection
≤ interval+timeout (~15s); (6) every seeded-bug arm of every new TLA+ module
demonstrably fails; (7) v2 goldens byte-identical throughout; (8) docs
(F1–F4) merged; interop suites (native + browser) updated for epoch/classes
and green.

---

## Appendix A — Protocol v3 wire reference (additions only; v2 unchanged)

### Client → server (new / extended)

```text
Authenticate (extended, all new fields optional):
  { "type":"Authenticate", "data":{
      "app_id":"...", "platform":"godot", "game_data_format":"message_pack",
      "protocol_version":3,
      "supported_transports":["relay","direct","webrtc"],
      "supported_topologies":["relay","host","mesh"] } }

Signal (v3 only):
  { "type":"Signal", "data":{
      "to":"<player-uuid>", "generation":"<uuid>", "signal":{ <opaque> } } }

TransportStatus (v3 only, optional):
  { "type":"TransportStatus", "data":{ "transport":"webrtc", "connected":true } }
```

### Server → client (new / extended)

```text
ProtocolInfo (extended):
  adds "protocol_version", "min_protocol_version", "max_protocol_version"

RoomJoined / Reconnected (extended, session 011): add optional "ice_servers"
  (omitted when empty ⇒ v2 bytes unchanged) — the ICE pre-gather list, populated
  only for an eligible v3 recipient (webrtc transport + the game's desired
  topology negotiated, non-relay desired topology, non-finalized room,
  enable_ice_pregather + enable_webrtc on); superseded by SessionPlan ICE

NewPeer (v3 only):
  { "type":"NewPeer", "data":{ "peer_id":"<uuid>", "you_initiate":true } }

Signal (v3 only):
  { "type":"Signal", "data":{
      "from":"<player-uuid>", "generation":"<uuid>", "signal":{ <opaque> } } }

SessionPlan (v3 only; sent alongside the unchanged GameStarting):
  { "type":"SessionPlan", "data":{
      "generation":"<uuid>",
      "topology":"mesh", "transport":"webrtc",
      "host":null,
      "peers":[ {"player_id":"<uuid>","player_name":"P2","is_authority":false,"initiate":true} ],
      "ice_servers":[ {"urls":["stun:...","turn:..."],"username":"...","credential":"..."} ],
      "fallback":"relay" } }
```

`direct_endpoint` is omitted from relay/WebRTC plans; a `host + direct` plan
adds `"direct_endpoint":{"host":"203.0.113.10","port":7777}`.

Opaque `signal` payload convention (matchbox-compatible; client-interpreted):

```text
{ "Offer":"<sdp>" } | { "Answer":"<sdp>" } | { "IceCandidate":"<candidate>" }
```

---

## Appendix B — New / changed Rust types

```rust
// src/protocol/types.rs
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport { Relay, Direct, WebRtc }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Topology { Relay, Host, Mesh }

pub type SessionGeneration = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceServer {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub credential: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPeer {
    pub player_id: PlayerId,
    pub player_name: String,
    pub is_authority: bool,
    pub initiate: bool, // "you send the offer to this peer" (per-recipient)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPlanPayload {
    pub generation: SessionGeneration,
    pub topology: Topology,
    pub transport: Transport,
    #[serde(skip_serializing_if = "Option::is_none")] pub host: Option<PlayerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_endpoint: Option<DirectEndpoint>,
    pub peers: Vec<SessionPeer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub ice_servers: Vec<IceServer>,
    pub fallback: Transport, // always Transport::Relay
}
```

`ClientMessage::Authenticate` gains the three optional fields (B/§2.1);
`ServerMessage` gains `NewPeer`, `Signal`, `SessionPlan(Box<...>)`;
`ProtocolInfoPayload` gains the three version fields.

---

## Appendix C — Config schema additions

```toml
[protocol]
min_protocol_version = 2
max_protocol_version = 3

[session]
default_topology = "relay"        # relay | host | mesh
enable_webrtc = true
enable_direct = true
enable_ice_pregather = true       # ICE on RoomJoined/Reconnected for eligible v3 clients
# game_topology_mappings = { "FastFPS" = "mesh", "BoardGame" = "host" }

[turn]                            # self-hosted only (no third-party cloud)
enabled = false
static_auth_secret = ""           # coturn --static-auth-secret (server-only)
urls = []                         # ["turn:turn.example.com:3478"]
stun_urls = ["stun:stun.l.google.com:19302"]
credential_ttl_secs = 3600
```

---

## Appendix D — Topology/transport selection (pseudocode)

```text
fn choose_session_plan(room, members, cfg) -> SessionPlanPayload:
    transports = ∩(m.supported_transports for m in members) ∪ {Relay}   # Relay always
    desired    = cfg.game_topology_mappings.get(room.game).unwrap_or(cfg.default_topology)

    # `desired` is a ceiling: Mesh may fall through Host+WebRTC and Host+Direct.
    (topology, transport) = first executable rung in:
        Mesh + WebRtc if desired >= Mesh and cfg.enable_webrtc and all support it
        Host + WebRtc if desired >= Host and cfg.enable_webrtc and all support it
        Host + Direct if desired >= Host and cfg.enable_direct and all support it
          and at least one electable host has a validated Direct endpoint
        Relay + Relay

    host = elect_host(room, topology, transport) if topology == Host else None
      # eligible authority → eligible first-join → eligible min-UUID
    direct_endpoint = validated_endpoint(host) if transport == Direct else None
    ice  = build_ice_servers(cfg.turn, members) if transport == WebRtc else []
    # peers[].initiate is filled PER RECIPIENT at send time (Appendix E)
    return SessionPlanPayload{
      generation:new_uuid(), topology, transport, host, direct_endpoint,
      peers:[], ice_servers:ice, fallback:Relay
    }
```

---

## Appendix E — Glare-avoidance (offerer designation)

- **Mesh:** for a recipient `R` and each other peer `P`, set `initiate = (R.id < P.id)`
  (UUID compare). Exactly one of each pair offers. Stateless, no server arbitration.
- **Host:** every non-host client `initiates` to the `host`; the host `initiates` to
  none (it answers all). Clients never signal each other.
- **Late join:** on join into an active WebRTC session, publish a fresh
  authoritative `SessionPlan` to every member. Its per-recipient peer entries
  apply the same mesh/host initiation rules above. `NewPeer` remains in the
  additive schema but is not emitted by the shipped late-join flow.

---

## Appendix F — Ephemeral TURN credentials (coturn REST API)

```text
expiry   = now_unix + credential_ttl_secs
username = "{expiry}:{player_id}"
credential = base64( HMAC_SHA1( static_auth_secret, username ) )
# coturn: turnserver --use-auth-secret --static-auth-secret=<same secret>
```

- `static_auth_secret` lives only on the server; clients receive only the derived,
  expiring username/credential.
- **Self-hosted only (by design):** the server self-mints these credentials
  locally for an operator-run coturn and never contacts a third-party cloud. A
  managed TURN service, if desired, must be wired in out-of-band (e.g. via
  `[session].ice_servers`); there is no built-in `managed` mode. _(A `managed`
  third-party-cloud mode was prototyped as a stub and removed in session 012.)_

---

## Appendix G — Client transport / fallback contract (state machine)

```text
on SessionPlan(plan):
    if plan.transport == Relay:        use GameData over WS         # the floor
    else:
        start P2P (WebRTC/Direct) using plan + ice_servers
        for each peer where initiate: send Offer; else: await Offer → send Answer
        relay all Offer/Answer/IceCandidate via ClientMessage::Signal { to, signal }
        if P2P established within timeout:
            (optionally) stop sending GameData over WS; emit TransportStatus{connected:true}
        else:
            FALL BACK → resume GameData over WS; emit TransportStatus{connected:false}
server: always relays GameData regardless of P2P state (the floor never closes)
```

Data-channel config for game traffic: one **reliable+ordered** channel (commands /
chat / critical events) and one **unreliable+unordered** channel
(`{ ordered:false, maxRetransmits:0 }`) for movement/state. Works browser↔native.

---

## Appendix H — Platform client matrix

| Platform | WebRTC stack | Notes |
|---|---|---|
| Browser | native `RTCDataChannel` | Free; the reason WebRTC is mandatory (no raw UDP; WebTransport is client-server only) |
| Linux/Windows/macOS native | webrtc-rs / libdatachannel / libwebrtc | libdatachannel = lean, broad, browser-interop |
| Mobile (iOS/Android) | libdatachannel / Google libwebrtc | both interop with browsers |
| Steam build | any native stack | Steam networking (GNS/SDR) does **not** interop with browsers; embed WebRTC |
| Godot | built-in + `webrtc-native` (libdatachannel) | covers the whole matrix; easiest |
| Unity | `com.unity.webrtc` | native OK; **WebGL not supported** → custom JS bridge |
| Unreal | Pixel-Streaming WebRTC | P2P-weak; embed libdatachannel outside the PS plugin |

---

## Appendix I — Security requirements (must-haves)

- `wss://` for signaling in production (protects DTLS fingerprints in SDP).
- Authenticate before any room op (existing `app_id` flow); reject unauthenticated.
- Same-room enforcement on every `Signal` hop (P2).
- Per-connection signal rate-limit; cap peers/room, signal payload size; idle-timeout.
- TURN: only ephemeral, server-minted credentials reach clients; rotate the secret.

---

## Appendix J — Scaling (multi-node signaling)

- A pure signaler holds only per-room peer routing — light. Scale by **room
  affinity** (consistent-hash on `room_id`) so a room's peers share a node and
  forwarding is in-process. Use Redis pub/sub only if a room can span nodes. Relay
  and signaling share the same affinity constraint, so no new topology is introduced.

---

## Appendix K — Backward-compatibility test checklist (CI gate)

All items are implemented and CI-enforced (sessions 001–007):

- [x] v2 `Authenticate` (no new fields) ⇒ recorded as v2/`[Relay]`/`[Relay]`
  (`tests/v3_negotiation_e2e.rs`, `tests/protocol_v3_negotiation.rs`).
- [x] v2 client never receives `Signal`, `NewPeer`, or `SessionPlan`
  (`src/server/signaling_tests.rs`, `tests/v3_multipeer_e2e.rs`
  `mixed_v2_v3_n3_relay_floor_no_v3_leakage`, and the TLC `V2Gating` invariant in
  `formal/tla/SignalFishSession.tla`).
- [x] `GameStarting` to a v2 client is byte-identical to the pre-v3 golden snapshot
  (`tests/v2_wire_golden.rs`).
- [x] Mixed v2/v3 room is assigned `relay`; all members behave as v2
  (`tests/v3_session_plan_e2e.rs`, `tests/v3_multipeer_e2e.rs`,
  `src/server/session_policy_tests.rs`, TLC ladder model).
- [x] All v2 golden JSON + MessagePack snapshots unchanged (44 snapshots,
  enforced in CI).
- [x] `legacy-fullmesh` feature still builds and runs unchanged
  (`cargo test --all-features` in CI).

---

## Appendix L — Resolved P0 decisions

1. **Matchbox-compatible signal payloads?** (ADR-0002) — **resolved: yes**
   (keep `signal` opaque, shaped like `PeerSignal`), to unlock `matchbox_socket`
   clients and cut P7 cost.
2. **`/v3/ws` path alias vs in-band only?** — **resolved: both** (alias defaults
   `protocol_version=3`).
3. **Self-mint TURN (adds `sha1`) vs managed-only?** — **resolved: self-mint
   only.** A `managed` (third-party-cloud) mode was prototyped as a stub and then
   removed in session 012 — TURN is self-hosted only, the server contacts no
   external cloud. (Originally recommended supporting both via `[turn].mode`; that
   knob no longer exists.)
4. **Connect P2P at finalize vs progressively during lobby?** — **resolved:
   finalize.** Late joiners receive a refreshed authoritative `SessionPlan`;
   the shipped flow does not emit `NewPeer`.

---

## Appendix M — References (sourced facts behind the plan)

- **TURN need:** ~17.7% of P2P connections are relayed (Hancke, ~10M Chrome calls);
  plan for 15–20%. Conditions: symmetric NAT, CGNAT/mobile, restrictive firewalls.
- **TURN cost:** Cloudflare Realtime TURN ~$0.05/GB, first 1 TB/mo free, free STUN;
  Twilio $0.40–0.80/GB; Metered tiered.
- **Trickle ICE (RFC 8838):** relay individual candidates incrementally, in order,
  exactly once — WebSocket provides this for free.
- **Browser constraint:** browsers cannot do raw UDP; WebTransport is client-server
  only (W3C explainer) ⇒ WebRTC DataChannel is the only true browser P2P transport.
- **matchbox protocol:** server is payload-agnostic; routes `Signal{receiver→sender,
  data}`, sends `IdAssigned`/`NewPeer`/`PeerLeft`; existing peers offer to newcomers;
  native (webrtc-rs) ↔ browser (web-sys) interop supported since 0.5.
- **Interop traps:** Chrome/Safari `.local` mDNS candidates (DataChannel-only apps
  trigger obfuscation); SCTP `a=sctp-port` vs legacy `sctpmap`; DTLS/BUNDLE.
- **Security:** signaling over `wss://` is load-bearing (DTLS fingerprints travel in
  SDP); WebRTC handles DTLS-SRTP/fingerprint verification itself.

---

## Appendix N — Protocol v3 revision wire reference (P10.E; v3 mutable, v2 frozen)

All shapes below are the completed v3 wire (no negotiation flags beyond
`protocol_version: 3`; `tests/v3_reliability_wire_golden.rs` pins them). V3
fields are projected away per pre-v3 recipient; the frozen v2 bytes and reliable
FIFO behavior are unchanged.

```text
client → server  GameData (E2 additions; both optional, default class=reliable)
{ "type": "GameData", "data": {
    "data": <opaque JSON>,
    "class": "reliable" | "latest" | "volatile",   // optional
    "key": <u32>                                   // required iff class=latest
} }
  well-typed illegal pairing => INVALID_DELIVERY_CLASS
  malformed/unknown/null/out-of-range metadata => INVALID_INPUT
  raw WebSocket binary input has no class envelope and is reliable

server → client  JSON GameData (E1 + E2)
{ "type": "GameData", "data": {
    "from_player": "<uuid>",
    "data": <opaque JSON>,
    "seq": <u64>,              // v3; allocated for every accepted send
    "epoch": <u32>,            // v3; monotonic sender incarnation
    "class": "...",            // v3; echoed only when sender supplied it
    "key": <u32>               // v3; present exactly for latest
} }                            // omitted class means reliable

server → client  bare MessagePack BinaryGameDataFrame (always reliable)
{ "from_player": "<uuid>", "encoding": "message_pack", "payload": <bytes>,
  "seq": <u64>, "epoch": <u32> }
  no class/key fields

server → client  DeliveryReport (v3 NEW; event-driven or on RelayStats cadence)
{ "type": "DeliveryReport", "data": {
    "per_class": {
      "reliable": { "delivered": <u64>, "abandoned": <u64>,
                    "unsupported_format": <u64> },
      "latest":   { "delivered": <u64>, "superseded": <u64>,
                    "dropped_full": <u64>, "abandoned": <u64>,
                    "unsupported_format": <u64> },
      "volatile": { "delivered": <u64>, "dropped": <u64>,
                    "abandoned": <u64>, "unsupported_format": <u64> }
    },
    "gaps": [ { "from_player": "<uuid>", "epoch": <u32>,
                "from_seq": <u64>, "to_seq": <u64>,
                "reason": "latest_superseded" | "latest_dropped_full" |
                          "volatile_dropped" | "unsupported_format" } ]
} }
  counters are cumulative per physical connection
  gaps is omitted on counter-only snapshots; when present it has 1..256 ranges
  additional non-mergeable ranges roll into later causally prior reports
  each report freezes its counter frontier at the exact ranges it contains
  RelayStats remains frozen aggregate diagnostics

server → client  GoingAway (E3, v3 NEW; pre-v3 clients get only close 4000)
{ "type": "GoingAway", "data": {
    "deadline_ms": <u64>,           // when the server will close 4000
    "retry_after_secs": <u64|null>  // optional operator hint
} }

server → client  Reconnected additions (E5)
  ...existing payload...,
  "sender_watermarks": [ { "player_id": "<uuid>", "epoch": <u32>, "seq": <u64> } ]
    // authoritative exact coverage for every current room member
    // replace every cursor after every reconnect, regardless of replay status

epoch carriage (E1): RoomJoined.current_players[],
  SpectatorJoined.current_players[], PlayerJoined.player, PlayerReconnected,
  and Reconnected member snapshots each gain "epoch": <u32>.

new ErrorCodes: INVALID_DELIVERY_CLASS (E2), SERVER_DRAINING (E3)
new close-code semantics: 4000 server_shutdown becomes reachable (E3); delivery
  accountability/capacity/sojourn failure closes 4002 (E2)
new config: websocket.send_queue_capacity (1024),
  websocket.control_queue_capacity (128),
  websocket.slow_consumer_timeout_ms (5000),
  websocket.max_sojourn_ms (15000),
  websocket.delivery_stats_interval_secs (0), server.drain_grace_secs (30)

recipient gap/lifecycle rules (E6):
  * within (sender, epoch), the union of causally prior non-overlapping exact
    ranges must cover every missing seq; counters/errors/later reports do not
  * priority peer lifecycle may overtake queued old-epoch data: account that
    tail but suppress its application payload after the lifecycle change
  * future epochs require exact lifecycle announcement; reject old epochs after
    data advances; multiple announcements may be outstanding
  * recipient room/spectator transitions are generation barriers; clear room
    cursors but retain physical-connection counters
  * every reconnect starts a new physical accounting lifetime and replaces all
    sender cursors from sender_watermarks; game data is not replayed
ProtocolInfo (E8): gains "transports": ["websocket"] capability array
  (future QUIC/WebTransport lane advertises here; no other change now).
```

E4 is implemented: server-initiated WebSocket ping/pong timers add no JSON wire
message; their configuration, close semantics, and metrics are documented.

## Appendix O — New TLA+ module sketches (P10.D)

House rules for all: seeded-bug CONSTANT (TRUE-arm cfg must produce a
counterexample; checked cfgs pin FALSE), `Done` self-loop with deadlock
checking on, one action per code function with correspondence comments,
`_Small.cfg` exhaustive. Time = discrete integer `now` + `Tick` action,
timers as absolute-deadline guards (every timed property below is a relation
between timeout constants, not dense time).

```text
O.1  RoomLifecycleGC.tla   (lands with P10.A1; code: src/server/maintenance.rs,
     src/database/mod.rs cleanup_*, src/reconnection.rs claim/sweep,
     src/server/reconnection_service.rs rollback)
CONSTANTS PLAYERS(2), HORIZON(~10), EMPTY_TIMEOUT(2), INACTIVE_TIMEOUT(4),
          RECONNECT_WINDOW(3), StaleActivityBug
VARIABLES now, roomExists, members, lastActivity, emptiedAt,
          pending[p] : {none,pending,claimed,restoredMembership} × expiresAt,
          clientOutcome (ghost)
ACTIONS   Tick / Activity(p) (refreshes lastActivity unless bug) /
          Disconnect(p) (arms pending, sets emptiedAt when room empties) /
          ClaimReconnect(p) (atomic, guarded now<expiresAt) /
          RestoreMembership(p) (GC may interleave BEFORE it — the real gap) /
          CompleteReconnect(p) / RejectReconnect(p) (rollback w/o departure hook) /
          ExpireSweep (claimed records immune) /
          GcEmptySweep, GcInactiveSweep (NO lock action — the 10s join lock
          does not guard GC; model that honestly)
INVARIANTS ActiveRoomNeverReaped; ReconnectWindowRespected
           (pending ∧ now<expiresAt ⇒ roomExists); NoOrphanMember;
           RollbackComplete.  TEMPORAL ClaimResolves (WF restore steps only)
CFGS      _Small; _WindowBoundary (RECONNECT_WINDOW = EMPTY_TIMEOUT tie)

O.2  ControlPriorityDelivery.tla   (spec-first for P10.E2 queue split + sojourn)
   NOTE: this is the original SKETCH. As BUILT (session 019/020, see the D2
   checklist entry) it deviated for the better: NO wall clock (per-frame `age`
   only — a HORIZON would falsely strand a late frame below the bound), frames
   by CLASS not by sender (no Senders/parked-slot), backpressure-as-enablement
   (no ParkedEnqueue/GraceExpired — that stays DeliveryContract's), and
   WF(Tick, SojournEvict, CloseFinish). The sketch below is retained as the
   design intent; the module + this file's D2 entry are the source of truth.
CONSTANTS Senders(2), Data/CtrlBudget, DataCap(2), CtrlCap(1), SOJOURN_BOUND(2),
          HORIZON(8), SingleQueueBug (control routed into data FIFO — must
          violate ControlAgeBounded), NoSojournEvictionBug (must violate
          DeliveryEventuallyResolves)
VARIABLES now, dataQ, ctrlQ (Seq of [sender, enqAt]), connState, closeReason,
          per-class sent/written/dropped counters, parked slot
ACTIONS   Tick / SendData / SendCtrl / ParkedEnqueue / GraceExpired /
          WriterDrainCtrlFirst (UNFAIR) / SojournEvict (WF; head age ≥ bound
          → close "Stale") / CloseFlush / CloseFinish / Done
INVARIANTS PerClassConservation; CtrlDropsAreLoud; ControlAgeBounded
           (ctrl never starves behind data backlog); StalenessBounded
TEMPORAL  DeliveryEventuallyResolves: enqueued ~> (written ∨ closed) under
          WF(Tick, SojournEvict, GraceExpired, CloseFinish) — NEVER WF(writer)

O.3  SenderPacingReaper.tla   (pins BUG-2; derives the A2 inequality)
CONSTANTS PING_TIMEOUT(3), PING_INTERVAL(1), SLOW_TIMEOUT(2), HORIZON(10),
          QCAP(1), TimeoutInversionBug (SLOW_TIMEOUT := PING_TIMEOUT+1 — a
          legal config today; must violate HealthySenderNeverReaped)
VARIABLES now, sndPhase {Reading,Broadcasting,Parked}, sndParkedAt,
          sndLastActivityAt (refreshed only when a frame is PROCESSED),
          sndInbox (client pings arrive each PING_INTERVAL regardless),
          rcpConn, rcpQ (unfair drain), rcpReconnects, sndEvicted (ghost)
ACTIONS   Tick / SenderProcess / BroadcastEnqueue / BroadcastPark /
          ParkResolve / ParkGraceExpire (WF) / RecipientDrain (UNFAIR) /
          RecipientReconnect (budgeted refill loop) / ReaperSweep (WF)
INVARIANT HealthySenderNeverReaped == ~sndEvicted
          + action property ActivityGapBounded (≤ SLOW_TIMEOUT + 2)
DELIVERABLE the exact passing inequality → config cross-field validation

O.4  EndToEndGapAccountability.tla   (flagship composition; validates E5)
CONSTANTS Senders(2 — MANDATORY: single-sender justified-flag is unsound at ≥2),
          SendBudget(3), QCAP(1), SOCKCAP(1), RINGCAP(1), CycleBudget(1),
          ReconnectBudget(1); seeded bugs (all checked FALSE): SingleFlagBug +
          NoBaselineResetBug (must violate ClientCanClassify), NoSnapshotReconcileBug
          (SDK applies only the delta replay — must violate MembershipEventuallyHonest)
VARIABLES server: per-sender counter+epoch, ring (+watermark), members;
          per recipient: conn, queue, sockBuf (written-but-unobserved; wiped
          with queue at Evict — models kernel-buffer loss);
          pending lastSeqSnapshot; client: obs[r][s], justified[r][s],
          clientMembers[r], ghosts accountable/membershipHonest
ACTIONS   SendData / SenderLeave / SenderRejoin (control brackets ring-recorded)
          / WriterDrain (queue→sockBuf) / ClientObserve (gap check per sender;
          ctrl applies membership delta + arms justification) / Evict (wipe
          queue+sockBuf; snapshot ring counter — Seam 1) / ClientReconnect
          (snapshot heal + replay>snapshot + truncated flag + SDK rule:
          reset all baselines) / Done
INVARIANTS ClientCanClassify (per pair); DroppedNeverObserved;
           RingSnapshotSound; at-quiescence MembershipEventuallyHonest
CFGS      _Small (2602 distinct states, exhaustive, complete graph depth 16) +
          _Sim (simulation mode via runner extension: basename *_Sim →
          tlc -simulate num=20000 -depth 80; num is per-worker, ~constant wall
          across cores, ~19M sampled states in ~40–65s)

O.5  DeliveryClasses.tla   (with P10.E2; wire-contract twin of DeliveryReport)
CONSTANTS Keys(2), Rel/Lat/BeBudget, QCAP, SilentSupersedeBug (coalesce
          without accounting — violates AccountedSupersession),
          CoalesceReliableBug (violates ReliableConservation)
VARIABLES queue (Seq[class,key,id,enqAt]), superseded, reported, written,
          droppedWithClose, connState
ACTIONS   SendReliable / SendLatest(k) (in-queue same-key replace, old id →
          superseded) / SendBestEffort (full ⇒ drop-oldest-volatile, counted) /
          WriterDrain / EmitDeliveryReport / Close* / Done
INVARIANTS ReliableConservation (reliable ids: queued ∨ written ∨
           droppedWithClose — never superseded/be-dropped);
           CoalesceNeverTouchesControlOrReliable; LatestValueLastWrite
           (≤1 queued latest per key, id = max non-superseded for key);
           AccountedSupersession (quiescent ⇒ superseded ⊆ reported)

O.6  Trace validation pilot (P10.D7): feature-gated JSONL sink at
     deliver_or_disconnect match arms (SendFast/SendFull/ParkedEnqueue/
     GraceExpired/SendChannelClosed/ParkedChannelClosed) + send-task
     dequeue/write/finalize (WriterStart/WriterDrain, CloseFlushStart/
     CloseFlushDrain/CloseFinish) + queue close (QueueClose) + close
     requests (LifecycleClose). Generator emits `Trace == <<[ev|->..],..>>`;
     DeliveryContractTrace.tla replays with i-indexed TNext; an emitted event
     whose action guard is FALSE deadlocks TLC at step i = pinpointed
     divergence. Drive from the model-based proptest cases (paused clock ⇒
     deterministic). Nightly, non-gating; second target: chaos-suite
     DeliveryLedger traces vs SequencedRelay.
```

## Appendix P — P10 references (sourced facts behind the campaign)

Delivery classes are table stakes: Valve GameNetworkingSockets send flags
(Unreliable/Reliable/NoNagle/NoDelay; 512KB per-connection send buffer with
`k_EResultLimitExceeded` to _that_ sender; ack-vector model from DCCP
RFC 4340 §11.4 / QUIC), EOS `EOS_EPacketReliability`, WebRTC SCTP partial
reliability (RFC 8831 + RFC 3758: `maxRetransmits` xor `maxPacketLifeTime`),
Photon per-event reliable/unreliable over ENet-descended rUDP, QUIC streams
vs datagrams (RFC 9000 / RFC 9221 — datagrams ack-eliciting, never
retransmitted, drop policy explicitly app-level).

Slow-consumer isolation: no surveyed peer couples one connection's latency to
another's queue — Aeron names the strategies (min/max/tagged flow control +
per-subscription tether; "min" is the consistency-over-latency choice),
Colyseus buffers 10 messages for reconnect and drops unreliable, GNS errors
the offending sender only. Latest-value semantics precedent: Photon Fusion
eventual consistency ("changing a Networked Property in quick succession can
result in some changes not being detected" — documented, accountable
supersession) and Photon cached events (keyed current-value for late/rejoin).

Reconnect/resume norms: Photon `ReconnectAndRejoin` + PlayerTTL, **no**
live-event replay (cached events only); Nakama relayed promises processing
order only, matches are in-memory and non-persistable, resync is app logic;
Colyseus can resume only because it owns the state schema (seat reservation +
delta patches). Epoch precedent: Aeron image sessionId; Kafka idempotent
producer epoch (KIP-98); DTLS/QUIC epochs.

Drain-not-handoff is the industry bar: Agones SIGTERM + grace; HTTP/3 GOAWAY;
WebTransport DRAIN_SESSION capsule; Cloudflare Durable Objects disconnect all
WebSockets on every deploy. Server-initiated WS ping is the documented
default liveness posture (RFC 6455 §5.5.2-3 pings are auto-answered by all
compliant stacks including browsers).

WebRTC failure rates (sizing the "TCP-only users" population): callstats.io
~12% session failures, 85% of those NAT/firewall, ~22% of sessions need TURN;
Hancke's re-analysis: 6.7% ICE-failed, ~1.9% hard failures with TURN properly
deployed. The circulating "60-70% corporate TURN relay" figure is
unverifiable — excluded from sizing. WebTransport became Baseline in 2026
(Safari 26.4 joined Chrome/Edge/Firefox; ~85% global) but Rust server crates
(wtransport 0.7.x, h3-webtransport) self-describe as not production-ready —
hence E8 reserves the seam and defers the lane.

Theory: exactly-once delivery is impossible (Two Generals); at-most-once with
loss accounting (v3's posture) is the sound side. End-to-end argument
(Saltzer–Reed–Clark): duplicate suppression/ack/recovery belong at endpoints;
any relay-level reliability feature must remain a performance optimization of
app-level resync — the test the resume-from-seq cut applied. Gaffer on Games
reliability layer: seq + redundant ack bits, "the application composes a new
packet containing lost data if necessary" — philosophically identical to v3
gap accountability. Rollback netcode (GGPO-style) wants unreliable datagrams
plus input redundancy: it belongs on the WebRTC lane, never the TCP floor.
CAP here: a single-instance relay is a total-order broker — no consensus
problem while up; the CP cost is paid at deploys (E3) and instance loss (F1).

---

_Current frontier: deterministic in-repo work through P82 is complete. P53 and P56 remain active
under their unchanged 20-attempt hosted evidence gates;
P7's mobile/Steam matrix cells and P8's operated coturn infrastructure remain
out-of-repo. The phase table and active phase sections are authoritative;
completed session history lives in `progress/session-*.md`._
