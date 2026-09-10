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
  admission-vs-consumption divergence class (padded metrics auth token, padded
  TLS paths, dead `client_ca_cert_path`, unbounded relay labels — #509);
  session 213 closed the open-mode DoS surface class from #518: a server-wide
  room ceiling (`server.max_rooms` under a server-global cap lock), a
  pre-parse per-connection inbound-message budget closing with new close code
  `4006 inbound_rate_limited`, and an explicitly armed pre-upgrade HTTP
  header-read deadline on both serve paths (hyper's 30 s default was inert
  without a Timer). Session 214 closed the last two #518 items: open-mode
  app UUIDs are now namespaced (no verbatim client-chosen identity) and the
  `/metrics` response is bounded (snapshot byte cap, game-name map entry caps
  incl. history samples). Session 215 closed the #529 credential-echo class
  (v3 snapshots no longer rebroadcast `connection_info` — `relay.token` and
  arbitrary `Custom` JSON — including nested replay events and correlated
  result envelopes) and the #522 restart-to-change-allowlist constraint
  (atomic SIGHUP reload of `security.allowed_apps`, incl. the
  `app_auth_path` registry file). Session 216 killed the nightly
  mutation-testing miss on the finalized-join mixed-path observation guard
  and closed the #526 griefing-forensics item (per-player and per-room
  rejection tallies with throttled info-level attribution). Session 221
  closed two session-218/219 follow-on classes: the `TransferAuthority`
  announcement now sequences through the room event lane (a departure of the
  freshly granted authority could previously leave live members with a
  stale authority view), and authority kick/ban removes only stale durable
  residue — never the target's live membership in another room — with the
  farewell, reconnection credential, and `4007` close gated on a fresh
  post-tombstone route read; the TLS serve stack also gained RFC 8441
  extended CONNECT to match its `h2` ALPN advertisement. Session 222 closed
  the session-221 follow-up wave (#550–#554): admission locks renew their
  leases mid-hold (`LeaseRenewalGuard`, so a stalled storage can no longer
  void the cap guarantee; lost leases are fail-visible via
  `signal_fish_distributed_lock_renewal_failures_total`), both serve stacks
  arm an HTTP/2 keep-alive cycle that reaps parked h2 connections (hyper has
  no h2 header deadline), the whole `/metrics` response carries a 1 MiB byte
  budget with oldest-first history truncation, allowlist reloads prune the
  relay-byte series of revoked app IDs, and the mid-game
  `TransferAuthority` semantics are decided and pinned (role moves, the
  finalize-time transport host does not).   Remaining
  frontier: continue seam sweeps; #539 tracks the coordinated-SDK path for
  the `connected_at` v3 trim (released SDKs 0.8.0–0.12.0 require the field);
  the parked-state (`senderState`) producers are same-thread program-ordered
  behind their `SendFull` record (verified, no race window). The 2026-09-09
  session-223 sweep found no demonstrable in-file defect across the stalest
  seams (authority, messaging, relay_policy, maintenance, shutdown,
  token_binding, outbound_queue, batching, deadline); residual risk
  concentrates in cross-feature interaction seams between the recently
  landed room-access, spectator fan-out, and authority-moderation features —
  sweep there next. The 2026-09-09 session-224 sweep covered exactly those
  seams (three parallel audits + adversarial verification) and closed the
  ban × reconnect-restore divergence class: the restore transaction
  re-checked only the kick tombstone, never the room ban list, and the
  two-hold `BanPlayer` gate sequence plus teardown re-arms left arrival
  orders where a banned player re-seated permanently (#557; restore now
  re-reads the ban under its gate hold and refuses `BANNED`, record intact
  for mid-window unban). Pins: password-perimeter-before-ban ordering on
  both admission paths, ban/password persistence across rotation and
  transfer. The 2026-09-09 session-225 sweep closed the last named frontier:
  the replay/ring-buffer × tombstone preserve/merge seams and the
  #550 lease-renewal paths produced zero new defect classes (tombstone-vs-
  restore ordering, kick-vs-re-key, ban-restore precedence, generation
  isolation, nested replay projection verified safe; two unpinned invariants
  gained pins), while two confirmed #550 lease defects were fixed red-first
  (the app-cap enforcement read ran on an unprotected lease; the room-code
  rotation hold had no renewal). The 2026-09-10 session-227 sweep ran three
  parallel audits over the access-control × reconnection/restore ×
  moderation, spectator-fan-out × authority/replay, and
  admission/lease × reload × metrics-bounds seams with adversarial
  verification, and closed four confirmed classes red-first: a fresh
  same-room rejoin no longer leaves the superseded pending record alive
  (the next disconnect merged it and destroyed the just-issued token);
  `TransferAuthority` now requires the target's live route to be this room
  (a storage-failed residue row could take the role and wedge the authority
  surface — same class as the session-221 kick hardening); the ghost-row
  sweep publishes the absolute correcting `SpectatorDisconnected`; and
  rotation holds the old code's `room_join` lock across the candidate loop
  (a mid-flight old-code joiner could resurrect the dropped code as a
  duplicate room). App-cap lock acquisitions/failures joined the shared
  cap-lock counters. Residual stale-code join-or-create semantics tracked
  by #561; the blocking `fail_operation` refusals under the room gate
  (bounded by `slow_consumer_timeout`, consistent with the leave-path
  pattern) recorded as accepted; a pending player record surviving a
  spectator join into the same room (restore has no current-spectator
  check) noted as a bounded pre-existing seam. Remaining frontier: continue
  seam sweeps into whichever seams new features open.
- #525 — session 217 landed the minimal viable moderation set: authority
  kick (close code `4007 kicked`, no reconnect), authority room-code
  regeneration, and a shipped default spectator cap
  (`server.default_max_spectators`, auto `2× max_players`). Session 218
  completed the access-control tier: `SetRoomAccess` (salted-hash room
  password, checked ahead of every other admission signal, sealable at
  creation), `BanPlayer`/`UnbanPlayer` (room-scoped in-memory ban list,
  TTL = room TTL), and `TransferAuthority` (atomic authority hand-off
  under the room mutation gate, sequenced replay-recorded
  `AuthorityChanged`). Session 219 completed the spectator fan-out
  slimming: v3 connections receive `NewSpectatorJoined` /
  `SpectatorDisconnected` as delta+count events (roster cleared,
  additive `spectator_count`), replayed copies project to the same shape,
  and the frozen v2 bytes are untouched (PR #545).
  Session 220 closed the last thread (#546): the room-namespace
  authority-squat design is resolved — codes are capability-ish and
  first-claim authority is documented v2 behavior, so the shipped defense is
  auto-generated codes plus seal-at-creation, now fail-closed: a
  password-carrying join into an open room is refused with the same
  non-enumerating `PASSWORD_REQUIRED` outcome instead of being seated under
  the squatter's authority; hosted reservation tokens remain tracked by
  #517.
- #378 — CLOSED by the session-217 canonical-gate migration: `Link Check`
  owns offline lychee + internal-link validation, the duplicate
  `Documentation Link Check` job is retired, the strict MkDocs build moved to
  the `Markdown Code Validation` job, and branch protection (no required
  checks, per #513) needed no settings migration.
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
    Remaining levers still need owner input: self-hosted runner labels
    (owner comment excludes DAD-MACHINE and ELI-MACHINE) and the #379
    path-awareness inventory.
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
