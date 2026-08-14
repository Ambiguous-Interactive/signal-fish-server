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

## Active hosted evidence

### P53 — Hosted relay timing evidence (#274)

- Evaluate the first 20 consecutive eligible scheduled attempts per operating
  system, counting red, cancelled, missing, and incomplete attempts under the
  rules in `docs/development.md`.
- Stratify timing claims by exact source commit if the cohort spans multiple
  implementations.
- Decide from the completed distribution whether each platform receives a
  pull-request timing gate or correctness-only placement, and commit that lane
  decision before closing #274.

Acceptance: 100 requested observations per matrix cell are reviewable for each
operating system, semantic failures are not censored, and the resulting lane
placement matches the observed execution context.

### P56 — Compatible-control hosted eviction closure (#290)

- Complete the unchanged `h14-capacity-v1` cohort through 20 eligible scheduled
  first attempts, retaining red, cancelled, missing, and incomplete outcomes.
- Correlate the deterministic queue-arbitration fix with the full hosted
  distribution rather than throughput alone, then close or refine #290.

Acceptance: all 20 first attempts preserve exact fallback accounting, complete
compatible delivery, 0.01x-class amplification, and zero unintended eviction.
A counterexample must be retained, converted into a deterministic
repository-local reproduction, and used to refine #290, but does not satisfy
this acceptance criterion.

## External acceptance

### P97 — Public WebSocket upgrade deployment and first eligible probe (#367)

Deployment of the correlated server image is tracked in
[signal-fish-cloud#595](https://github.com/Ambiguous-Interactive/signal-fish-cloud/issues/595).

- Deploy the correlated image and record the platform-authoritative immutable
  digest, source revision, and rollout-complete UTC in signal-fish-cloud#595.
- Classify the first schedule-triggered public probe with
  `github.run_attempt == 1` whose start time follows that rollout boundary,
  regardless of outcome or which headers survive the proxy.
- Use its application and proxy correlation IDs to close #367 or narrow the
  failing boundary with retained evidence.

Acceptance: the first eligible post-rollout scheduled probe passes, admitting
every simultaneous upgrade with distinct application correlation IDs and
conserved outcomes. A failure must refine #367 with correlated evidence but
does not satisfy this acceptance criterion.

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

## Next repository correctness work

### P101 — Stalled-join and process-pause delivery evidence (#374)

- Under a deterministically gate-held stalled recipient, admit a fresh client
  before eviction and prove its exact `RoomJoined` baseline plus every healthy
  incumbent's `PlayerJoined`, authoritative membership, conservation, and no
  unrelated eviction.
- Add an ignored nightly real-binary SIGSTOP/SIGCONT scenario that accepts only
  exact post-resume delivery or causally prior exact gap accounting, rejects
  sequence/lifecycle regression, and finishes with healthy reconnect or
  teardown.
- Register semantic negative controls and avoid elapsed-duration or Pong-RTT
  thresholds; unsupported platforms must skip explicitly.

Acceptance: both scenarios are wired into the appropriate nightly lane with
retained diagnostics, deterministic synchronization, non-vacuous oracles, and
no weakening of the existing PR-lane suites.

## Unscheduled open-issue frontier

These items remain live but are not active phases. Re-rank them whenever new
correctness evidence appears.

- #205 — scope the remaining broader safety work against guarantees not already
  covered by unsafe-code prohibition, warnings-as-errors, Miri, and sanitizers.
- #213 — add analyzers only when they prevent a demonstrated defect class with
  acceptable signal and maintenance cost.
- #318 — use representative cross-platform measurements to choose the next
  hook-latency reduction without weakening fail-closed checks.
- #204 — obtain and integrate the remaining design assets while preserving
  protocol and client compatibility.
- #207 — pursue the next optimization only from current allocation and latency
  profiles, with exact wire and delivery semantics held constant.
