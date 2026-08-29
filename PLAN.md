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

### Reviewer capacity for a literal all-green pull request (#377)

- Restore the external Copilot reviewer quota, or disable/reconfigure that
  administrative integration without weakening repository-owned CI or human
  review policy.
- Exercise the integration on the exact pull-request head after the owner-side
  change, retaining any substantive feedback and its resolution.

Acceptance: the exact pull-request head or a dedicated test pull request has a
terminal successful review result, or explicitly reports neutral/skipped
unavailability after owner reconfiguration, with no failed required check.
Repository-owned green checks alone do not satisfy this administrative
requirement.

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

- #220 — extend formal models only around a named, evidence-backed correctness
  seam not already covered by the retained TLA+/TLC and Z3 suites.
- #213 — add analyzers only when they prevent a demonstrated defect class with
  acceptable signal and maintenance cost.
- #396 — continue the correctness and performance sweep through named,
  gameplay-facing seams; require a deterministic counterexample or current
  profile before changing production behavior. Prior sessions swept the
  rate limiter internals, the `websocket/sending.rs` v2-projection corridor,
  shutdown/drain choreography (#454, fully resolved), the spectator seam
  (join/detach/prune/retry panic-repair parity), and the reconnect seam
  (session-186: drain-admission gates, claim-guard invariant). Session-187
  swept the heartbeat/reaper + connection-lifecycle seam and the
  auth/identity + coordination/outbound-queue seam. Session-188 swept the
  room-lifecycle/ownership seam and the protocol parse/validate seam.
  Session-189 resolved the residuals: duplicate/too-late Authenticate now
  answers a coded refusal (#468, fixed), the all-liveness-disabled
  configuration warns at startup (#465, fixed), the open-policy application
  UUID divergence is documented (#464, closed as documented), the
  ChannelClosed reservation metric gap was refuted (#470, counted once at
  reservation), the token-binding plaintext warning was already landed
  (#462, closed with evidence), and #463's TransportStatus fan-out gate-hold
  was pinned as a deterministic counterexample (#472). Session-190 decided
  and implemented the #463 treatment (#473): the fan-out releases the room
  mutation gate and the sender lifecycle gate before delivery (snapshot-only
  gate hold), and the socket writer fail-closed-suppresses every documented
  v3-only server-to-client variant on a pre-v3 queue, eliminating the
  reconnect-identity-swap race class; the pinned counterexample was rewritten
  to the improved contract with a deterministic eviction-ordering regression
  oracle. Deliberate gate-holds documented in #463 (signaling plan-before-
  signal, session-policy departure snapshot stability) remain by design.
   Session-191 swept the delivery-ledger/DeliveryReport, room-join baseline,
   and room/maintenance-cleanup seams: the first fixed the tail-parked causal
   DeliveryReport eviction class (queue-age deadline measured the server's own
   parking, not recipient progress) and fail-closed the two pending-omission
   write paths that bypassed the v3-only writer arm; the join-baseline and
   cleanup seams audited sound (records only). Session-192 swept the
   authority/relay-policy seam (no dangling authority on any mutation path,
   deterministic total repair, topology/signal-relay predicate agreement),
   the room-identity/state-machine seam (bounded collision retry with
   dual-lock atomicity, exactly two validated atomic LobbyState writers, no
   half-applied observations), and the upgrade-admission seam (no
   panic/hang/unbounded-memory, bounded kernel-keyed rejection throttle,
   fail-closed ordering) — all sound under adversarial re-verification; the
   recorded foot-guns (`update_room_authority` `Some(non-member)` grant
   contract, `toggle_player_ready` coordinator-parity divergence,
   rejection-log eviction discarding suppressed counts) are now documented at
   their sites. Next session: name new seams from fresh evidence.
- #423 / #424 — choose Miri phase 2 only through the recorded owner decision:
  split the job, accept the measured single-lane duration, or move it to the
  weekly schedule. Preserve full native coverage and retain exact-head hosted
  runner-time evidence for whichever allocation/latency tradeoff is selected.
- #318 — use representative cross-platform measurements to choose the next
  hook-latency reduction without weakening fail-closed checks.
- #378 — consolidate duplicate hosted link validation only with an atomic
  branch-protection migration and equivalent-or-broader coverage evidence.
- #379 — make verification-nightly pull-request fan-out path-aware only after
  an owner exports the required-check/ruleset inventory and a historical
  changed-file replay proves net allocation and runner-time savings. On the
  2026-08-15 replay, classification reduced retained-trigger workers from 469
  to 454, but 67 classifier jobs raised total allocations to 521; broadening
  fail-closed triggers raised them to 569. Do not add per-lane status runners;
  prefer a required classifier with server-side job skips and retain hosted
  before/after evidence.
- #207 — pursue the next optimization only from current allocation and latency
  profiles, with exact wire and delivery semantics held constant.
