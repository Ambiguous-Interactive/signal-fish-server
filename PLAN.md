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
   frontier: continue seam sweeps; #539's staged path is tracked in the
   frontier note below (owner-unblocked 2026-09-11, SDK half in review);
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
   cap-lock counters. The 2026-09-10 session-229 sweep covered the
   remaining spectator × moderation seams (ban/kick target classification,
   spectator cap, transfer × spectator, kick/ban with spectators present,
   rotation × live spectators, unban × spectator) and closed one confirmed
   class red-first: a moderation eviction of a pending-record holder no
   longer closes the holder's live spectator session in another room (the
   route read that gates the farewell and the `4007` close saw only seated
   routes). Three documented contracts gained pins (spectator-target
   kick/ban refusal, service-level `TOO_MANY_SPECTATORS` refusal,
   stale-rotated-code join-or-create semantics), and #561 was resolved as
   documented semantics (a stale code is an unknown code; the tombstone
   registry was rejected as a namespace-wide design change needing owner
   input). The 2026-09-10 session-230 sweep closed the #566
   rotation × transfer gap as documented semantics; the coordinated-SDK
   decision it shared with #539 was made 2026-09-11 (see the #539 staging
   note below): the docs (authority,
   rooms-and-lobbies, protocol reference, AsyncAPI descriptions) now state
   that after a rotation the role hand-over needs the new code shared out of
   band first, and a red-verified delivery pin
   (`rotation_then_transfer_delivers_no_room_code_to_the_successor`) freezes
   the wire contract.    Remaining frontier: continue
   seam sweeps into whichever seams new features open. #539 is unblocked
   and staged (2026-09-11 owner decision): the client half is
   signal-fish-client-rust#257 (`serde(default)` tolerant parse, public
   type unchanged); after its release, bump `clients/fortress` (=0.8.0)
   and `clients/fortress-wasm` (=0.9.0), then re-land the v3
   `connected_at` trim (the #538 first-cut design). The 2026-09-10
   session-228 sweep closed the spectator-join seam red-first (a same-room
   spectator join now discards the unclaimed pending record — the
   pre-spectator token could previously re-seat the player after the
   spectator session ended) and adjudicated the seal-restore seam (pinned:
   `SetRoomAccess` gates fresh admissions only; a pre-seal record restores
   without a password — resumption, not admission; bans refuse restores).
   Also verified safe: rotation × spectator code resolution (spectator
   joins have no creation branch, so the #561 resurrection class does not
   apply to them; mid-swap misdirection re-reads by room id and cannot
   strand), TURN/relay issuance × rotation/reload/restore (pure, fail-closed
   mint; restore folds fresh plan/ICE repair; TURN config is not
   SIGHUP-reloadable), metrics bounds × spectator counters (none exist).
   Recorded as accepted: zero-member record-protected rooms count toward
   `server.max_rooms` for the reconnect window (intentional protection ×
   ceiling; availability-only, bounded by window × code space); the room
   event lane stalls behind one slow recipient up to `slow_consumer_timeout`
   per send with an unbounded job queue behind it (per-client rate budgets
   bound admission); a tightened per-app `max_relay_bytes` override reaches
   live connections only on reconnect (same revocation-is-restart contract).
   The 2026-09-11 session-232/233 sweep closed the error-reply amplification
   class: every polite per-frame reply (error refusals incl. retryable
   handshake refusals and the unsupported-format warning, join/spectator/
   reconnection refusals, authority denials and internal-error replies,
   room-operation and moderation failure envelopes, the application-Ping
   `Pong`) now charges the per-connection `max_inbound_error_replies`
   budget; exhaustion fires metric + farewell + `4006` exactly once and the
   gate follows the physical socket across reconnect identity swaps. The
   room event lane stall remains recorded-as-accepted above (per-client
   admission budgets bound it). The 2026-09-11 session-234 sweep ran three
   parallel audits over the error-reply budget's new cross-feature seams
   (budget × admission/entry, budget × moderation/authority/spectator,
   budget × reconnect/restore/replay/slow-lane) with adversarial
   verification and closed with zero new defect classes: every reply path
   charges before sending (or documents the one-reply grace), exhaustion
   side effects fire exactly once (one-shot decided inside the charge
   critical section; close pins are first-reason-wins), no charged-withhold
   leaves admission state mutated (rollback precedes every post-mutation
   charged refusal), the 4006 close lands through a stalled lane (watch
   signal + 1 s bounded finalize), and the reconnect swap/rollback both
   carry the charged gate. Two unpinned invariants gained pins
   (rollback-arm gate carry; 4007-kick × 4006-exhaustion first-pin
   arbitration in both orders, one-shot + metric asserted), and the
   first-pinned-reason-wins close-code attribution contract plus the
   never-charged recipient-side format advisory are now documented
   (protocol.md close codes, `CloseReason::InboundRateLimited`, CHANGELOG).
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
    scenario-profiles cron leg.
    Remaining levers still
    need owner input: self-hosted runner labels (owner comment excludes
    DAD-MACHINE and ELI-MACHINE), the #379 path-awareness inventory, and the
    per-PR interop-quartet cohort question (#568: browser/fortress/
    fortress-wasm/turn measured at ~180 Linux-billed minutes over
    2026-09-08..10); a cargo-deny single-container consolidation is blocked
    by the pinned action's one-manifest-per-boot input and the fortress-wasm
    1.94 toolchain pin.
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
