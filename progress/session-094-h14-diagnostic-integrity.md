# Session 094 — H14 diagnostic integrity (P51)

## Scope and prioritization

Remote triage found no open pull requests or dependency updates. Main was at
`127fee1` (PR #280) with no failing check at the start of the session. Nine
issues remained open. The authentication trust boundary (#250) has the highest
raw gameplay/security impact, but it requires an owner decision about the
credential and compatibility contract. The resilience, formal-methods,
optimization, safety, and analyzer issues (#206, #220, #207, #205, #213) are
broad campaigns without one bounded current defect; #204 is off the gameplay
path; and #274's remaining work requires a hosted-runner distribution before a
timing-policy decision.

Issue #281 was therefore the highest-impact item that could be completed
without inventing product requirements. It records a diagnostic gap in the H14
mixed-encoding accountability experiment after one intermittent hosted run
evicted the compatible control and caused the fallback reader's `PlayerLeft`
panic to erase both recipients' terminal evidence.

## Failure-first evidence

Before P51, each reader task panicked independently on timeout, EOF, socket
error, eviction, unexpected messages, and conformance assertions. The parent
then used `JoinHandle::expect`, so the first reader panic happened before the
combined counters were printed. Proxy destination-write counters were absent,
as were retained pump terminations and control errors. A RED run consequently
could not distinguish server behavior from a hosted VM stall or an
under-delivering fault injector.

The deterministic RED-path regression constructs a completed fallback beside
a compatible `4002 slow_consumer` close and proves that one output preserves
both recipient frontiers, both actual proxy rates, pre/post-teardown pump
causes, control errors, backpressure, and eviction counters.

## Implementation

- Both readers return structured terminal observations with exact semantic and
  wire counters instead of panicking. Unexpected errors and lifecycle events
  remain failures after the combined evidence is emitted.
- Raw text/binary/close frames are retained and replayed through the existing
  conformance auditor only after both readers finish. This preserves all
  protocol assertions without allowing one reader's assertion panic or poisoned
  mutex to erase its sibling's state.
- Destination-write bytes and elapsed time share one measurement origin and
  are captured independently when each reader completes, before the equal
  32 KiB/s throttles are lifted. Returned streams are then closed and proxy
  termination registration is awaited, so diagnostics include both the
  pre-teardown state and the retained terminal pump cause instead of racing an
  empty registry.
- The fixed 5,000-message burst, production queue capacity, five-second
  full-queue timeout, 15-second maximum sojourn, 90-second experiment deadline,
  and every accountability/amplification/non-vacuity oracle are unchanged.

## Red-green evidence

The focused RED diagnostic regression passes and proves the combined output
contains both recipients and both proxies. The unchanged ignored H14 selector
passes locally in about 12.2 seconds with 5,000/5,000 compatible deliveries,
5,000/5,000 fallback-accounted sequences, roughly 2,218 fallback payload bytes
against 389,618 compatible payload bytes (0.01x), one non-vacuous backpressure
event, and zero evictions. Both proxies retained a deterministic
client-to-server EOF termination after reader teardown.

## Validation and review

The first independent adversarial review found six issues in the initial
implementation: auditor assertions could still panic in the readers; three
semantic oracles had weakened into diagnostics; bytes were sampled after
unthrottling; pump termination registration was racy; the RED path lacked a
focused regression; and PLAN/progress tracking was missing. Every finding was
addressed before the full validation gauntlet and publication. Follow-up review
found the two proxy rates used different elapsed-time origins; the final code
uses one origin while preserving each reader's independent terminal frontier.
The final adversarial pass reported zero findings.

The authoritative local validation completed with:

- `cargo fmt --all -- --check`;
- strict all-target, all-feature Clippy with warnings denied;
- `cargo test --locked --all-features --quiet`;
- the exact ignored H14 selector with `--nocapture`;
- `cargo deny --all-features check`;
- the CI configuration, documentation consistency, MSRV consistency, workflow
  hygiene, LLM file-size, and example-extraction policy scripts;
- the focused documentation-policy and 305-test CI-configuration suites; and
- hook readiness plus the worktree pre-commit and pre-push preflights.

All commands exited successfully.

## Hosted validation and review

PR #282's implementation head `133ca79` reached a terminal state with 40 check
runs: 37 successes, two intentional policy skips, and one failure from the
Copilot reviewer's account quota. The successful checks include the complete
CI platform matrix, MSRV, coverage, Miri, AddressSanitizer, audits, and the
Verification Nightly workflow. Nightly's real-world scenario job passed the
exact mixed-encoding amplification experiment with the unchanged H14 workload.

Cursor Bugbot reviewed `133ca79` and reported zero issues; GitHub exposed no
inline review threads. Copilot was explicitly requested through its supported
bot reviewer identity and returned only the repository account's quota-limit
message. No independent human reviewer was available: there is no CODEOWNERS
file, the connected repository identity is the PR author, and GitHub rejected
the explicit human request because authors cannot review their own pull
requests.
