# Session 075 — Binary relay envelope allocation

## Scope

Advance the open optimization program in issue
[#207](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/207)
through one falsifiable relay-serialization increment, starting from clean
`main` at `09238c3` after release PR #238 merged.

## Baseline and triage

- P28 was audited as complete and merged; there was no carried staged diff or
  open pull request.
- Issue #207 remained the highest gameplay-impacting open work. Issue #239 is
  the next concrete gameplay-path task after this bounded allocation phase.
- No open dependency pull request was relevant. The visible remote Dependabot
  refs are stale proposals containing version lines already rejected on MSRV
  and duplicate-stack evidence.
- The existing production-seam allocation harness reproduced its recorded
  baseline exactly: direct protocol-v3 MessagePack used 10.001, 11.001, and
  11.001 allocation operations per relay at room sizes 2, 8, and 16; mixed
  MessagePack-source traffic used 18.001, 37.001, and 37.001.

## Failure-first evidence

Before production changed, the checked-in direct-binary ceiling allowed at
most 7,169 allocation operations across 1,024 relays. The room-size-two cell
failed deterministically at 10,241. The final data-driven matrix pins operation,
reallocation, and allocated-byte ceilings for all nine current JSON,
direct-binary, and mixed-format cells rather than guarding one example.

## Implementation

Named MessagePack relay envelopes now allocate their output buffer from the
known opaque payload length plus 128 bytes of fixed headroom for field names,
UUID, encoding, delivery stamps, and container headers. Both the frozen v2
MessagePack envelope and mandatory v3 envelope use the same helper. Raw v2 JSON
and rkyv passthrough remains unchanged.

The same growth class was reviewed for JSON projection. A `serde_json::Value`
does not expose its encoded length, and escaped strings have no safe compact
upper bound without either a second traversal or large systematic
over-allocation. That expansion was rejected rather than trading deterministic
allocation counts for unmeasured CPU or memory cost. Mixed fallback still
benefits because its direct-binary cohorts use the optimized encoder.

## Measured result

Five exact repeats produced identical frame counts, wire bytes, codec work,
SHA-256 output digests, accounting, and queue drainage.

| Scenario | Room 2 | Room 8 | Room 16 |
| --- | ---: | ---: | ---: |
| Direct v3 MessagePack, before | 10.001 | 11.001 | 11.001 |
| Direct v3 MessagePack, after | 5.001 | 6.001 | 6.001 |
| Mixed MessagePack source, before | 18.001 | 37.001 | 37.001 |
| Mixed MessagePack source, after | 18.001 | 28.001 | 28.001 |

The direct-binary encoder performs zero measured output reallocations after the
change. Allocated bytes per relay fell from 2,585–3,825 to 1,581–2,821 across
the direct cells and by 8–9% in the mixed 8-/16-player cells.

The uninstrumented Criterion comparison is recorded as inconclusive rather
than converted into a release claim. Sequential whole-suite runs showed
significant drift in unchanged controls (JSON room 8/16 slowed by 23%/17% and
the mixed room-two negative control slowed by 6%). Target cells moved in both
directions across reruns. P29 therefore relies on deterministic allocation
evidence and exact wire ledgers; machine-sensitive timing remains observational.

## Verification

- Frozen v2 and v3 binary wire golden tests pass for all three encodings.
- The allocation matrix passes all nine checked-in operation, reallocation,
  and allocated-byte ceilings across five exact local repeats. A dedicated
  required `CI / Relay Allocation Ceilings` job now enforces the same harness
  on the pinned Linux toolchain for every push and pull request.
- Exact pre/post Criterion comparisons cover the same production seam at 2,
  8, and 16 players; unchanged-control drift is disclosed above and no runtime
  conclusion is claimed.
- `cargo fmt --check`, strict all-target/all-feature Clippy, the full
  all-feature test suite, `cargo deny`, the 298-test CI policy target, document
  consistency, MSRV consistency, workflow hygiene, tooling parity, LLM policy,
  hook readiness, and both worktree hook suites pass locally.
- Two adversarial passes found and resolved fallible-allocation, measurement,
  byte/reallocation coverage, boundary-test, documentation, and required-check
  contract gaps. The final confirmation pass reported zero remaining diff
  issues. On the first hosted head the new allocation job passed exactly,
  Cursor Bugbot reported no issue, and Copilot identified one error-taxonomy
  gap in fallback writer growth. The follow-up distinguishes capacity overflow
  (`InvalidInput`) from allocator failure (`OutOfMemory`) and pins that contract
  directly. A clean exact-head re-review exposed three suppressed maintenance
  suggestions; the final local head also classifies initial reserve overflow,
  documents the single sample-scoped allocation allowance, and makes the CI
  contract assertion whitespace-insensitive. A subsequent clean exact-head
  review's only suppressed note was also incorporated by retaining the
  allocator's reserve-error text. Final hosted and reviewer evidence remain
  pending publication.

## Follow-up

Issue #207 remains an open measured-optimization umbrella rather than being
declared exhausted. Issue #239 tracks the next bounded milestone: deterministic
TURN-only WebRTC interoperability through pinned local coturn, including an
exact data ledger and the live WebSocket fallback floor.
