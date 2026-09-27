# Signal Fish Server Plan

This file is a forward-only work queue. It contains only future, incomplete, or
actively collecting work. When an item is fully complete, remove it here;
completion evidence belongs in source, tests, durable documentation, GitHub,
and the ignored `progress/` session notes rather than being duplicated in this
plan.

## Goal and ordering

Advance the server toward production-ready cross-platform signaling and relay
operation. Prioritize observed gameplay correctness first, then usability and
operability, then measured performance. Prefer the smallest change that closes
a demonstrated failure class, with red-first tests and evidence proportionate
to risk.

## Execution rules

- Keep one session's work in one pull request; do not stack pull requests.
- Time-box each session to roughly one hour of active work (see GOAL.md).
  Scope the session to one green PR; carry the remainder forward here.
- Start production fixes with a deterministic failing test and sweep adjacent
  paths for the same failure class.
- Run the mandatory local Rust sequence and repository gauntlet before
  publication. The exact pull-request head must finish with all applicable
  hosted checks green and no unresolved substantive review feedback.
- Do not infer hosted acceptance from reruns or hand-picked survivors. Count
  every eligible schedule-triggered first attempt according to the cohort's
  pre-registered rules.
- Open a focused GitHub issue for newly discovered work that cannot be closed
  completely in the current change.

## External acceptance

### P7 — Mobile and Steam interoperability

- Run the documented v3 interoperability matrix with maintained out-of-repo
  mobile and Steam builds, including live signaling, reliable and unreliable
  data channels, relay fallback, and reconnect behavior.
- Record exact client revisions, platform/WebRTC stacks, and evidence for each
  supported platform cell.

Acceptance: mobile and Steam rows have reproducible green cross-stack evidence;
documentation clearly distinguishes demonstrated support from integration
guidance until then.

### P8 — Operated self-hosted TURN

- Provision and operate the documented self-hosted coturn deployment with TLS,
  ephemeral credentials, rotation, monitoring, and capacity evidence.
- Validate relay-only browser/native and external-platform sessions against the
  operated service without weakening credential or candidate-path assertions.

Acceptance: a maintained environment demonstrates reproducible relay-only
sessions and operational secret rotation. Multi-node room-spanning fan-out is
outside this plan's architecture scope.

## Unscheduled open-issue frontier

These items remain live but are not active phases. Re-rank them whenever new
correctness evidence appears.

- #636 — research: optimization campaign (owner 2026-09-26): maximize
  rooms + relays per ARM node; owner follow-up 2026-09-26: SIMDJson and,
  in general, more hot-loop CPU perf. Session 267 posted the criterion
  runtime baseline (`relay_serialization_runtime`, release profile): the
  hot loop runs 0.7-1.4 us per relay send across all cohorts (1M+ per
  second per core), the mixed-cohort multiplier shows on CPU too (21.5 ms
  vs 14.9 ms per 15,360-delivery sample at room 16), and the v3 JSON text
  cohort pays ~58% serialization premium over MessagePack at room 2.
  Candidate targets in cost order: cohort-count reduction, JSON text
  serialization (a simd-json-class change is an owner decision: new
  dependency with internal `unsafe` against the no-unsafe crate policy —
  the policy forbids `unsafe` in this crate, not in dependencies, but the
  review bar is higher). Any change must keep exact wire and delivery
  semantics (#207 rule) and carry a red-first measurement. The #207
  allocation profile still bounds the relay core.
- #512 — hosted CI: the session-239 audit found every per-event workflow
  path-narrowed, cache-warmed, and cohort-consolidated. Remaining levers
  need owner input: self-hosted runner labels; the interop quartet stays
  per-PR per #568; a cargo-deny single-container consolidation is blocked
  by the pinned action's one-manifest-per-boot input and the fortress-wasm
  1.94 toolchain pin. Local loop: sessions 262-266 landed the cheap levers;
  session 267 measured the remaining floor dead ends (mold: no gain, link
  is 1.2 s of ~10.5 s; dev-loop resolution: 0.3 s; nightly `-Zthreads`:
  slower than stable) and shrank the last 1 s test classifier window
  (full `--lib` wall 4.19 s -> 3.44 s). The remaining floor is rustc
  crate-size work; the structural option (crate split) is parked in #642
  pending an owner decision.
- #207 — pursue the next optimization only from current allocation and latency
  profiles, with exact wire and delivery semantics held constant. The
  2026-09-01 profile found the fan-out core at its floor (0–1 allocation ops
  per relay across room sizes; the classified queue lane at zero) and
  per-relay projection cost proportional to the distinct wire cohorts the
  room's recipient mix requires (4–5 allocs/relay single-encoding; 14–21
  for v2+v3 mixed rooms, with each cohort's frame cached once per relay and
  sibling recipients reusing clones), so no allocation-level target remains at
  these layers without changing wire bytes or delivery semantics. The
  session-212 budget gate reads app policy through a lock-guarded projection
  of `Copy` fields, keeping the charge path allocation-free.
- #396 — CLOSED 2026-09-12 (standing correctness/perf sweep, closed with the
  session-237 enforcement-seam sweep). The sweep practice continues
  opportunistically wherever new features open seams; per-session closure
  evidence lives in the closed issue, session notes, and merged PRs.
- #525 — CLOSED (minimal moderation set, access-control tier, and spectator
  fan-out slimming landed across sessions 217-220; the #546 squat design
  resolved in session 220). Follow-on credential work is tracked under #517.
- #378 — CLOSED (canonical Link Check gate, session 217).
- #517 — the credential story is ratified (owner decisions 2026-09-11:
  no shared secret, 5-minute TTL, self-hosting must keep public-`app_id`
  mode, `connect_token` field name) and this repo's half is implemented:
  `Authenticate` carries an optional `connect_token`
  (`sfct_v1.<b64url(payload)>.<b64url(sig)>`, Ed25519), verified after the
  allowlist resolves in the order encoding → signature → expiry →
  300 s + 60 s skew TTL ceiling → app-id binding, all failures reported as
  `CONNECT_TOKEN_INVALID` on a retryable, budget-charged refusal. Key config
  is `security.connect_token.public_key`/`public_key_path` (public material,
  file folded at load, fail-closed), SIGHUP-reloadable alongside the
  allowlist. Absent field is byte-identical to today; presented token
  without a configured key is refused fail closed. Session 236 implemented
  the #574 enforcement knob (owner sign-off 2026-09-12): layered
  `security.connect_token.required` global default + per-app
  `require_connect_token` override, missing-token refusals report the
  distinct `CONNECT_TOKEN_REQUIRED` (retryable, budget-charged, 4006 on
  exhaustion), posture reloads with the key, and a required entry with no
  key is dead config rejected at startup/SIGHUP. Enforcement in open mode
   also closes the legacy skip-`Authenticate` path (pre-auth frames refused
   `MISSING_APP_ID`, silence hits `4001 auth_timeout`). Cloud-side edge
   enforcement (option 1) and token minting are the control plane's work,
   filed 2026-09-14 as signal-fish-cloud#782 (owner-directed hand-off);
   remaining frontier: SDK mint/attach halves (tracked in the SDK repos).
   Verified safe (session-236 sweep): enforcement × reconnect identity swap
   (handshake guards block re-entry), enforcement × allowlist reload races
   (fail-closed in both swap orders).
