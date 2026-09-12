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

- #396 — CLOSED 2026-09-12 (standing correctness/perf sweep, closed with the
  session-237 enforcement-seam sweep). The sweep practice continues
  opportunistically wherever new features open seams; per-session closure
  evidence lives in the closed issue, session notes, and merged PRs.
- #539 — still blocked on the human-run crates.io release of
  `signal-fish-client` (latest remains 0.12.0 as of 2026-09-12; the tolerant
  client half is merged in `signal-fish-client-rust#257`, awaiting release).
  After release: bump the `clients/fortress` and `clients/fortress-wasm`
  pins, prove the Fortress interop run, then re-land the v3 `connected_at`
  trim (the #538 first-cut design) with the AsyncAPI `V3PlayerInfo`/
  spectator schema split in the same change.
- #525 — CLOSED (minimal moderation set, access-control tier, and spectator
  fan-out slimming landed across sessions 217-220; the #546 squat design
  resolved in session 220). Follow-on credential work is tracked under #517.
- #378 — CLOSED (canonical Link Check gate, session 217).
- #512 — session 220 moved the Windows lint/nextest lanes into the #513
   daily cron cohort (measured: the Windows pair averaged ~40 of ~92 billed
   minutes per CI run, 43%; the cron gains one Windows pair per day, paid
   back by a single CI-triggering event). Session 221 removed the remaining
   per-event full-suite duplication: the instrumented coverage gate and the
   MSRV full-suite run joined the noon cron (~20 fewer ubuntu minutes per
   CI-workflow event, ~13 events/day measured; MSRV compilation still
   verifies per event). Session 223 moved the last two heavy duplicative
   cohorts off per-event triggers: the ci-safety Miri/ASan lanes (~38 Linux
   minutes per eligible change, weekly cron → daily 02:00 UTC) and the
   webrtc-interop native-platforms matrix (~28 billed minutes per run, macOS
   10x + Windows 2x, new daily 05:00 UTC cron); both keep manual dispatch.
   Session 224 (#557) removed the post-merge
   push-to-main wave from the 16 validation workflows (measured: five
   merge waves at ~40 wall minutes each, ~200 Linux-billed minutes/day,
   ~27% of Linux spend; squash content is identical to the PR run, and
   the noon cron re-proves main daily) and moved the relay-timing
   native-platforms legs (macOS 10x + Windows 2x) into the
   schedule/dispatch cohort; per-event validation coverage is unchanged.
   Session 225 consolidated job granularity (measured pool ~115 Linux
    billed minutes/day): verification-nightly's four short lanes share one
    runner setup, the cargo-audit/npm/SBOM steps joined the `deny`
    supply-chain job, and the doc-test lanes joined `Rustdoc Validation`.
    Session 226 closed the #558 owner-input-free levers: the
    `panic-policy` job is now an ubuntu-gated step of `lint`, the
    `relay-allocations` job is now an ubuntu-gated step of `nextest`, and
    the `z3` job is now a step of the formal-verification `tlc` job
    (measured before: 2.8 + 2.2 billed minutes per CI event plus two
    runner setups; guard constants/tests migrated atomically, retired
    check names documented in the naming-contract header).
    Session 228 closed the release-preflight deadlock and the last
    validation push wave: the issue-#557 push-trigger removal had left
    `check-release-preflight.sh` requiring an `event=push` run of "CI" at
    the release commit — unsatisfiable, so every future release would have
    failed closed; the preflight now also accepts the merged release pull
    request's own `pull_request` run at the squash head (single-parent
    check enforced; strict commit→PR mapping), and doc-validation dropped
    its push trigger entirely (~50–90 Linux-billed minutes/day at the
    session-224 merge rate; docs-deploy still strict-builds main docs).
    Two dead PR path filters stopped allocating suites that never consumed
    the change: verification-nightly no longer fires on the four
    sequenced-relay trace inputs (formal-verification owns their per-PR
    gating) and browser-interop narrows `clients/**` to
    `clients/browser/**` + `clients/native/**`. The 2026-09-10 session-230
    wave moved the last owner-input-free per-PR compile leg onto the cron
    cohort (the non-gating nightly `cargo-udeps` analysis now runs daily at
    07:00 UTC with `cargo-machete` staying per-PR), dropped the never-read
    per-job dependency cache from the `deny` supply-chain job, and
    de-duplicated the lint job's cross-OS `cargo fmt` re-check behind the
    quick-check gate.
    The 2026-09-11 session-231 wave retired the standalone
    `h14-pr.yml` gate: the nightly-only H14 amplification selector now runs
    as an ubuntu-only step of the per-PR `nextest` job (exact #558
    one-runner-setup pattern; the selector's test binary was already part of
    that job's compiled suite graph, so the saved runner allocation
    duplicated only setup and build). Measured: h14-pr averaged 1.1–2.2
    ubuntu-billed minutes per `src/**` pull request (~24 billed minutes over
    the 2026-09-08..10 window) and the step adds the ~12 s experiment
    itself. Hosted H14 attempt-evidence artifacts stay on the daily
    scenario-profiles cron leg. Session 238 merged verification-nightly's
    standalone `starved-runtime` job into `multiprocess-delivery` (both
    suites build the same server workspace plus `clients/native` graphs, so
    the second job re-paid one runner setup and a duplicate two-workspace
    compile on every schedule and pull-request event; each lane keeps its
    own step and the consolidated job summary keeps per-suite sections).
    Remaining levers still
    need owner input: self-hosted runner labels and the #379 path-awareness
    inventory (the per-PR interop-quartet cohort question was decided
    2026-09-11: status quo — all four interop lanes stay per-PR, #568);
    a cargo-deny single-container consolidation is blocked
    by the pinned action's one-manifest-per-boot input and the fortress-wasm
    1.94 toolchain pin.
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
  enforcement (option 1) and token minting are the control plane's work.
  Verified safe (session-236 sweep): enforcement × reconnect identity swap
  (handshake guards block re-entry), enforcement × allowlist reload races
  (fail-closed in both swap orders). Remaining frontier: SDK mint/attach
  halves (tracked in the SDK repos).
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
  these layers without changing wire bytes or delivery semantics. The
  session-212 budget gate reads app policy through a lock-guarded projection
  of `Copy` fields, keeping the charge path allocation-free.
