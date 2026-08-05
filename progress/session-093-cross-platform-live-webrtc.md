# Session 093 — Cross-platform live native WebRTC (P50)

## Scope and prioritization

Remote triage found no open pull requests and nine open issues. The previous
session's PR #279 was fully green across all 16 triggered workflows. Among the
remaining gameplay-path items, issue #274 still appeared to carry a rare
`PlayerAlreadyConnected` reconnect failure, while issue #275 recorded that the
native client had no live WebRTC proof outside Linux.

The reconnect item was historical bookkeeping, not a current defect. GitHub
Actions run 30804575762 checked out merge commit `0cfb7ab` (P38 over
`2900b53`) at 10:13 UTC on 2026-08-03. Its test dropped the reporter, waited for
`PlayerLeft`, and immediately reconnected. `PlayerLeft` commits before the old
connection is physically removed, so the production guard correctly returned
`PlayerAlreadyConnected`. Commit `263be4e`, merged later that day at 19:15 UTC,
added the explicit `server.disconnect_client(...).await` teardown join now in
the test. That exact selector then passed ten consecutive runs in session 082;
it passes on current `main`, and lifecycle serialization proves the join cannot
return with the old entry still registered. The later #274 comment had compared
the old failure against post-fix source and therefore missed the chronology.

With the semantic reconnect item already resolved, #275's cross-platform live
transport gap was the highest-impact concrete work remaining.

## Failure-first evidence

Before this session, `.github/workflows/webrtc-interop.yml`'s
`native-platforms` matrix ran on Windows and macOS but stopped after build,
link, and unit tests. Its own comments and `clients/native/README.md` explicitly
said live transport was proved on Linux only. A platform-specific fault in
interface enumeration, UDP binding, ICE, DTLS, SCTP, child-process teardown, or
path handling could therefore pass every non-Linux check.

The existing IPv6 cell already proved that a two-member room is the smallest
complete live transport, but its family restriction is a separate Linux-hosted
claim. Running the full N=3/fault suite on every platform would add cost and
timing surface beyond the missing evidence. The new cell isolates the exact
gap: default platform networking, one planned pair, and both data channels.

## Implementation

- `two_peer_mesh_exchanges_over_live_webrtc` spawns the real server and two real
  native clients, requires a mesh/WebRTC plan, one direct host/host selected
  pair per endpoint with concrete unicast addresses, and exact sends/receives
  on both reliable and unreliable SCTP channels.
- The existing IPv6 cell now shares the same two-client runner and layers only
  its family-specific candidate assertions on top. This removes duplicated
  process orchestration while keeping the claims separate.
- The Windows/macOS matrix now caches and builds both package graphs, builds the
  root server binary, resolves the Windows `.exe` suffix, passes the exact
  binary path to the standalone test crate, and runs only the new live selector.
  Linux still runs the full seven-scenario suite, including that selector.
- The CI policy test parses the workflow and pins both platform legs, the root
  build, failure propagation, Windows suffix, server handoff, selector, and the
  behavioral acceptance markers in the Rust scenario.
- The native README and existing unreleased changelog entry now state the exact
  evidence boundary: live native WebRTC on all desktop platforms; the larger
  topology, fault, TURN, browser, and IPv6 matrices remain Linux-specific.

## Red-green evidence

The prior workflow and README are the direct red state: no non-Linux job built
the server or executed any integration selector, and the platform table marked
both live cells unproved. The updated policy test fails if the live step, exact
selector, server handoff, or either platform leg is removed or made non-fatal.

Locally, the focused policy test and the new real-process selector pass. The
selector establishes and exchanges over a direct host/host pair in about two
seconds; it does not substitute a mocked transport or a compile-only check.

## Validation and review

The local gauntlet is green:

- root `cargo fmt --check`, strict all-target/all-feature clippy, and
  `cargo test --locked --all-features`;
- native-client formatting, strict all-target clippy, the complete seven-cell
  `scripts/run-webrtc-interop.sh` suite, and focused reruns of both two-peer
  selectors after review remediation;
- root and standalone-client `cargo deny --all-features check`;
- all 305 CI policy tests (304 passed, one intentionally ignored), the document
  consistency policy/script tests, CI configuration, workflow hygiene, MSRV,
  tooling parity, Markdown, LLM-file, hook-readiness, pre-commit worktree, and
  pre-push worktree checks.

The first independent adversarial review found five issues in the session's
own change: the live workflow command was pinned only by substrings and could
be hollowed out with `--ignored`; refactoring had discarded post-drain client
stderr; several scenario labels and runner-shape comments were stale; old
cold-cache timings no longer described the enlarged job; and P50's PLAN phase
was inconsistent. A second agent implemented every warranted correction. The
final independent adversarial pass reviewed the complete remediated diff and
reported zero issues.

PR #280's implementation head `4852482` completed every substantive hosted
workflow successfully. WebRTC Interop run 31031080742 passed the Linux full
suite plus `Native Client Build + Live WebRTC` on both `windows-latest` and
`macos-latest`; each non-Linux leg built the real server and client and passed
the exact two-peer live selector. CI run 31031078683 passed all 16 jobs,
including Windows and macOS nextest. Advanced Safety run 31031077648, TURN-only
run 31031078869, Browser Interop run 31031077968, and the remaining policy,
documentation, dependency, link, and spelling workflows all passed.

Verification Nightly run 31031078068 initially exposed one unrelated H14
control failure: the equally throttled compatible MessagePack recipient was
evicted as a slow consumer, and the JSON fallback then panicked on that peer's
`PlayerLeft`. That is the inverse of issue #212's historical amplification
signature, which evicted only the fallback. The unchanged exact test passed
locally under one CPU and under added contention in about 12.2 seconds with
5,000/5,000 sequences accounted, 2,218 fallback bytes against 389,618
compatible bytes (0.01x), and zero evictions. The failed-job retry then passed
the H14 step on the identical SHA after the identical precursor suites, and the
nightly run finished green. No production or workload change was justified.
Issue #281 records the missing symmetric terminal/proxy-throughput diagnostics
needed to classify a future recurrence without weakening the burst, throttle,
or production deadlines.

Cursor Bugbot reviewed exact implementation head `4852482` and reported zero
issues. GitHub Copilot was explicitly requested but could not review because
the requesting account had exhausted its quota; the PR author is the connected
GitHub identity, so no independent human reviewer was available to request.
The independent local adversarial loop nevertheless reviewed the complete
remediated diff to zero findings, and a separate four-agent H14 audit converged
on preserving the strong experiment unchanged. P50 is complete.
