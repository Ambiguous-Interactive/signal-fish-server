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
- Remaining: confirm allocation 20 (due ~2026-08-25 05:45 UTC) preserves the
  audited pattern; 19/20 per OS are already collected and audited in the issue
  (all manifests complete/eligible, toolchain pinned, zero backpressure,
  exact-complete deliveries). The lane decision is committed in
  `docs/development.md` (macOS correctness-only; Linux/Windows keep the
  existing broad-job ceiling; no new isolated PR job); close #274 once the
  full cohort stands.

Acceptance: 100 requested observations per matrix cell are reviewable for each
operating system, semantic failures are not censored, and the resulting lane
placement matches the observed execution context.

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

### P97 — Public WebSocket upgrade deployment and first eligible probe (#367)

- Configure the repository variable `SIGNAL_FISH_PUBLIC_WS_URL` with the
  operator-owned public WebSocket endpoint.
- Deploy the correlated image and record its platform-authoritative immutable
  image digest, source revision, and rollout-complete UTC in the operator-owned
  deployment record.
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

## Unscheduled open-issue frontier

These items remain live but are not active phases. Re-rank them whenever new
correctness evidence appears.

- #220 — extend formal models only around a named, evidence-backed correctness
  seam not already covered by the retained TLA+/TLC and Z3 suites.
- #213 — add analyzers only when they prevent a demonstrated defect class with
  acceptable signal and maintenance cost.
- #396 — continue the correctness and performance sweep through named,
  gameplay-facing seams; require a deterministic counterexample or current
  profile before changing production behavior.
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
