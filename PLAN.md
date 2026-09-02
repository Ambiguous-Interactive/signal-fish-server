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

- #396 — standing correctness/perf sweep. Session 208 closed the
  admission-vs-consumption divergence class: a padded metrics auth token and
  padded TLS paths (admission checked the trimmed view while the runtime
  compared/loaded the raw value), a dead `client_ca_cert_path` under
  `client_auth=none`, and unbounded relay labels echoed into room-state
  metadata are now rejected at admission; `trace_validation.rs` ends its
  projection at the first divergence label so rejected traces carry one
  precise label plus an attributed raw tail (#509). Remaining frontier:
  continue seam sweeps; the parked-state (`senderState`) producers are
  same-thread program-ordered behind their `SendFull` record (verified,
  no race window).
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
  profiles, with exact wire and delivery semantics held constant. The
  2026-09-01 profile found the fan-out core at its floor (0–1 allocation ops
  per relay across room sizes; the classified queue lane at zero) and
  per-relay projection cost proportional to the distinct wire cohorts the
  room's recipient mix requires (4–5 allocs/relay single-encoding; 14–21
  for v2+v3 mixed rooms, with each cohort's frame cached once per relay and
  sibling recipients reusing clones), so no allocation-level target remains at
  these layers without changing wire bytes or delivery semantics.
