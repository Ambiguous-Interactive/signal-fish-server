# PLAN.md — Protocol v3: Cross-Platform P2P + WebRTC (Backward-Compatible)

> Action plan for evolving Signal Fish Server from a WebSocket **relay** into a
> capability-negotiated **signaling + relay** server that supports true
> peer-to-peer (WebRTC) connections across browser, native (Linux/Windows/macOS),
> mobile, and Steam — while keeping every existing v2 client working unchanged.
>
> Status: IN PROGRESS. **All in-repo work through P29 is complete; P30/P31 are
> in publication review.** The M1
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
> assets, #205 broader safety work, #206 distributed-resilience research, #207
> further measured optimization, #213 additional static analyzers, #220
> formal-verification work beyond the bounded exposure theorem. Sessions 062
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
> Owner: TBD. Target protocol version: **v3** (additive over v2); P10 targets
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
(zero warnings), plus a `CHANGELOG.md` entry and `./scripts/check-doc-consistency.sh`.
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
| P16 | Measurement integrity and enforced no-unsafe | S | — | Maintenance | ✅ Done (s062); H14 flake → #212 |
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
| P30 | Spectator lifecycle and room-GC coherence (#241) | S | P10 | Maintenance | 🟡 Review (s076) |
| P31 | Local TURN-only WebRTC interoperability (#239) | S | P4, P7, P8 | M3 | 🟡 Review (s076) |
| P10 | Bulletproofing campaign: falsify → formalize → v3 revision | XL | P9 | v3 | ✅ Done |

---

### P16 — Measurement integrity and enforced no-unsafe (Size S) — ✅ DONE (one pre-existing flake carried to #212)

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

### P30 — Spectator lifecycle and room-GC coherence (Size S) — 🟡 IN REVIEW

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

### P31 — Local TURN-only WebRTC interoperability (Size S) — 🟡 IN REVIEW

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

### P11 — Git-tagged releases + versioned GHCR containers (Size S) — ✅ DONE

**Schedule this before the next crates.io publish or GitHub Release.** A public
release is not complete until its source and container artifacts share one
verifiable version identity.

The current workflows contain both halves of this behavior: `release.yml`
creates an annotated `vX.Y.Z` tag for a manual release, and
`docker-publish.yml` maps a `vX.Y.Z` tag-push event to GHCR tags such as
`:vX.Y.Z`, `:X.Y.Z`, and `:X.Y`. The missing guarantee is orchestration: a tag
pushed with a workflow's `GITHUB_TOKEN` does not start another workflow, so the
manual release path can finish without publishing a versioned container.

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

**Current-release audit (Session 058):** the published `latest` and `0.5.0`
indexes both contain exactly `linux/amd64`, `linux/arm64`, and `linux/arm/v7`.
The canonical verifier also proved that `v0.5.0`, `0.5.0`, and `0.5` share
digest `sha256:af1f3b965f8ec7e7f7678112bd260485d092669d1bba49f7dc0d4eb0849487c8`
and carry tagged source revision
`16ac09b042436b6dbc5cca0b68c462eb2a8ab33f` plus version `0.5.0` on every
runtime manifest. This fresh registry evidence closes issue #122 as completed;
the workflow and Dockerfile policy tests remain the regression guard.

---

### P12 — Fortress Rollback relay interoperability regression (Size S) — ✅ DONE

Fortress Rollback issue 242 reported a Godot single-threaded WASM game becoming
extremely slow when a custom Signal Fish relay adapter fed its polling client.
The client repository owns browser/Godot transport behavior; this repository
owns the complementary release-client-to-current-server acceptance boundary.

- [x] Add a standalone `clients/fortress` crate outside the server package,
  exact-pinning `fortress-rollback = 0.10.0` and the published
  `signal-fish-client = 0.8.0` with its own lockfile and supply-chain policy.
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
  and supply-chain policy. Exact-pin `fortress-rollback = 0.10.0` and
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
  waits, stalls, overflow, malformed frames, unknown senders, decode loss, or
  completion underflow.
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
release must satisfy every healthy gate on both peers and print `HEALTHY`. The
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

### P7 — Reference clients + interop test matrix (Size XL — mostly outside this repo)

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
   `clients/native/` (`signal-fish-reference-native`) on **`webrtc-rs` 0.17
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
  backpressure, p99 < 250ms; fault cells → completeness after fault lift,
  evictions all 4002/4003-attributed, rooms heal. Metrics per cell: ledger,
  latency percentiles (relay cells only), backpressure/eviction counts, RSS.
  **Clean PR lane ✅ (session 036):** all six encoding/size cells run real
  WebSockets at 30 msg/s/player with 1 KiB payloads, use data-lane barriers to
  prove the complete control-priority join lifecycle before measurement, and
  feed JSON text or true MessagePack binary frames through the
  ConformanceAuditor. Each cell requires exact payload/stamp completeness,
  zero default-queue backpressure, zero eviction, and p99 <250 ms while
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
  { "type":"Signal", "data":{ "to":"<player-uuid>", "signal":{ <opaque> } } }

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
  { "type":"Signal", "data":{ "from":"<player-uuid>", "signal":{ <opaque> } } }

SessionPlan (v3 only; sent alongside the unchanged GameStarting):
  { "type":"SessionPlan", "data":{
      "topology":"mesh", "transport":"webrtc",
      "host":null,
      "peers":[ {"player_id":"<uuid>","player_name":"P2","is_authority":false,"initiate":true} ],
      "ice_servers":[ {"urls":["stun:...","turn:..."],"username":"...","credential":"..."} ],
      "fallback":"relay" } }
```

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
    pub topology: Topology,
    pub transport: Transport,
    #[serde(skip_serializing_if = "Option::is_none")] pub host: Option<PlayerId>,
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

    (topology, transport) =
        if desired == Mesh and all(supports(m, Mesh, WebRtc)) and cfg.enable_webrtc:
            (Mesh, WebRtc)
        elif desired == Host and all(supports(m, Host, WebRtc)) and cfg.enable_webrtc:
            (Host, WebRtc)
        elif desired == Host and all(supports(m, Host, Direct)) and cfg.enable_direct:
            (Host, Direct)                 # LAN / routable
        else:
            (Relay, Relay)                 # the floor

    host = elect_host(room) if topology == Host else None      # authority → first-join → min-UUID
    ice  = build_ice_servers(cfg.turn, members) if transport == WebRtc else []
    # peers[].initiate is filled PER RECIPIENT at send time (Appendix E)
    return SessionPlanPayload{ topology, transport, host, peers:[], ice_servers:ice, fallback:Relay }
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

## Appendix L — Open decisions (resolve in P0)

1. **Matchbox-compatible signal payloads?** (ADR-0002) — recommended **yes**
   (keep `signal` opaque, shaped like `PeerSignal`), to unlock `matchbox_socket`
   clients and cut P7 cost.
2. **`/v3/ws` path alias vs in-band only?** — recommended **both** (alias defaults
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

_End of plan. P0–P6 are implemented (see `progress/session-001-*.md` through
`progress/session-006-p6-docs-samples-llm-context.md`) — the M1 server core, the M2
production-P2P server work (ICE/TURN minting, transport status, metrics), and the M3
protocol documentation are complete. Session 007 (`progress/session-007-*.md`) added
mid-session re-planning (host failover, stored-plan consistency, finalization
persistence), the in-repo P7 conformance suites (N≥3 multi-peer + true
multi-process), and a formal verification layer (TLA+/TLC, property suites, parser
hardening). Session 009
(`progress/session-009-p7-native-reference-client.md`, ADR-0004) delivered the P7
native reference client (`clients/native/`, real webrtc-rs stack) and the
native↔native interop matrix cells (mesh, host-star, fallback, late-join, mixed
v2/v3), CI-enforced via `.github/workflows/webrtc-interop.yml`. Session 010
(`progress/session-010-p7-browser-reference-client.md`, ADR-0005) delivered the
P7 browser reference client (`clients/browser/`, real headless-Chromium
`RTCPeerConnection`) and the browser↔native + browser↔browser matrix cells —
including the Chrome `.local` mDNS trap — CI-enforced via
`.github/workflows/browser-interop.yml`, meeting the P7 acceptance bar. Session
011 (`progress/session-011-ice-pregather-and-ice-url-validation.md`) closed the
last in-repo deferred item — the P4 `RoomJoined`/`Reconnected` ICE pre-gather
refinement (capability-gated `ice_servers` at join/reconnect time, v2 wire
byte-identical, single-mint invariant, `enable_ice_pregather` kill switch) —
plus ICE URL scheme validation and duplicate-URL warnings across the session
and turn config blocks. Session 012
(`progress/session-012-self-hosted-turn-only.md`) made TURN self-hosted only —
the never-implemented `managed` third-party-cloud mode and its config surface
(`turn.mode`, `turn.managed_*`, `TurnMode`) were removed, so the server mints
coturn credentials locally and never contacts an external cloud. Session 013
(`progress/session-013-p8-security-review-hardening.md`) closed the last in-repo
**P8** item, the security review: a multi-agent adversarial audit of the v3
signaling surface fixed three hardening findings red-green — the
`TransportStatus`→`PeerTransportStatus` fan-out is now bounded on the existing
control-plane rate-limit budget; zero-valued background-task interval configs are
rejected at startup (instead of silently panicking their tasks) with
defense-in-depth use-site clamps; and all secret comparison is consolidated into a
single constant-time helper (fixing the reconnection-token and metrics-token
compares) — with the TURN-credential and session-policy surfaces independently
confirmed clean. **All in-repo v3-era PLAN work (P0–P10) is complete.** The
remaining v3 forward specification is out-of-repo: the mobile/Steam cells of
the **P7** interop matrix and the rest of **P8** (operating self-hosted coturn
infrastructure). The 2026-07 **P10** bulletproofing campaign is complete; its
verified-bug fixes, falsification experiments, formal expansion, and protocol
v3 revision are recorded in §P10 and Appendices N–P.
**P11 is complete: its workflow gate has landed, and Session 053 published and
verified the historical `v0.4.0` GHCR aliases without moving the preserved
`sha-50b28a9` digest. Session 049 added the
completed P12 Fortress Rollback native relay interoperability regression.
Session 050 implemented P13's exact-release single-threaded Godot WASM/browser
gate and recorded the released 0.8.0 client's enforced `BUSTED` result. Session
051 completed issue 155's two-phase release automation; Session 052 tracked the
upstream fix while it awaited release. Session 056 advanced the fixture to the
published 0.9.0 client and Godot adapter: the exact Emscripten/two-Chromium cell
is now `HEALTHY`, its one-admission negative control remains expected-`BUSTED`,
and P13 is complete for Chromium. Session 057 closes H6's final registered
reconnect-during-teardown facet with deterministic server-side retry evidence.
Session 058 re-verifies P11 against the current `latest` and `0.5.0` GHCR
indexes and closes the stale ARM64 image issue #122.**_
