# Session 083 — Executable Host + Direct Plans

## Scope

Issue #251 exposed a gameplay-path defect: protocol-v3 capability intersection
could select `Host + Direct` without proving that the elected host had supplied
a usable endpoint, and the resulting plan carried no connection target.

## Discovery and prioritization

- No pull requests or dependency-update pull requests were open at session
  start. The previous merged PR's 19 reported workflows were green; the exact
  post-merge `main` SHA could not be independently queried with the available
  workflow interfaces.
- Fresh advisory, license, ban, and source-policy checks passed across all five
  managed Cargo graphs. No dependency intervention displaced gameplay work.
- Among open issues, #251 was the highest-priority concrete gameplay milestone;
  #250 remains the separate client-authentication trust-boundary decision.

## Red-green evidence

The first regression finalized a room whose members advertised Direct but whose
authority supplied no endpoint. Before the production change, the assertion
failed with `Host` selected where the executable ladder required `Relay`; it
passes after endpoint-aware eligibility was added.

## Implementation contract

- `DirectEndpoint` is derived only from a syntactically usable self-declared
  `ConnectionInfo::Direct`: non-empty conservative IP/DNS host, bounded length,
  no surrounding whitespace, no unspecified IP, and a non-zero port.
- Direct host election filters by both negotiated support and endpoint
  eligibility. `SessionPlan.direct_endpoint` is present only for Direct and is
  the validated endpoint of the elected host.
- Membership refresh, reconnect, departure, and failover revalidate the active
  host. A newly ineligible host is replaced deterministically or the room drops
  to the next executable rung; relay downgrades remain sticky.
- The native and browser reference clients explicitly reject Direct execution
  they do not implement, report transport failure, and engage relay fallback.
- Endpoint disclosure retains the existing legacy boundary rather than
  pretending the new v3 field is private: v2/v3 player snapshots and spectator
  snapshots can already contain the self-declared `connection_info`, while the
  v3 plan repeats the elected endpoint. The AsyncAPI schema and public docs now
  describe that boundary explicitly.

## Adversarial review loop

The first independent review found four substantive drift/test gaps: the
AsyncAPI schema omitted `direct_endpoint`; the first privacy text understated
legacy snapshot exposure; client tests covered only error-string formatting;
and authoritative guidance still conflated endpoint-aware host election with
capability-only peer filtering. It also identified that the endpoint validator
sat outside the mutation scope.

All findings were addressed. The schema now has a conditional Direct branch and
serialized-plan consistency test; v2 player/spectator exposure has a regression;
both clients exercise the exact failure status/event sequence, peer-obligation
removal, and continued relay send; project guidance distinguishes
`supports_session` from `can_host`; and endpoint validation moved into the
mutation-scoped validation module. A fresh evaluator found three remaining
comment-level instances of the same predicate/exposure drift; those were fixed,
and the evaluator's final re-review returned `PASS` with no remaining issue.

## Verification

- Focused session-policy tests: 73 passed, including malformed endpoint,
  authority-skip, reconnect, failover, and property-generated membership cases.
- A real-WebSocket test supplies connection information, crosses the
  finalization barrier, and observes the same endpoint-bearing Direct plan at
  both peers.
- Protocol-v3 exact-wire, generated-wire, sample-consistency, and protocol-v2
  golden suites pass.
- Native-client Direct tests, standalone strict clippy, and formatting pass;
  browser unit tests, type checks, Prettier, and production build pass.
- The full local CI driver completed every compilation, unit/integration/e2e,
  workflow, hook, documentation, and advisory lane. Its first default-feature
  test pass recorded two failures only because DNS temporarily could not resolve
  GitHub while cargo-deny refreshed RustSec; after resolution recovered, both
  exact failed cargo-deny integration tests passed unchanged. The all-features
  suite and the driver's later advisory gate were green in that same run.
- `cargo mutants --list` reports 390 mutants. The newly scoped endpoint
  validator contributes 24 and the mutation workflow now covers the inventory
  with 39 complete shards at the unchanged ten-mutant/290-second modeled bound;
  all mutation workflow/script policy tests pass.
- The first hosted mutation run exposed four surviving mutations in the
  validator's combined empty/total-length/whitespace guard. The existing
  overlength examples were also invalid for other reasons, so they did not
  isolate the 253-byte hostname ceiling. Data-driven valid-DNS cases now prove
  253 bytes is accepted while 254 and 255 bytes are rejected; an exact local
  replay of shard 15/39 caught all 10 mutants, including the four survivors.
- Exact-head hosted CI and GitHub reviewer feedback remain before publication
  is complete.
