# Session 110 — Native interop and browser-origin integrity

## Scope and prioritization

Remote triage found no open or draft pull request and no dependency update to
incorporate. P53/#274 and P56/#290 remain correctly gated on their unchanged
20-attempt hosted cohorts, both at 3/20; their deterministic fixes are already
merged, so this session did not reset or weaken either oracle.

Current `main` was not green. WebRTC Interop run `31282596854` failed on macOS
after both native data channels opened and exact reliable/unreliable traffic
crossed them: one client emitted no selected-candidate-pair event. This was the
third recurrence of issue #301's evidence race, so #301 was reopened with the
new production-seam evidence. A separate safety audit then reproduced a
browser-origin defect: with only `https://allowed.example` configured, valid
WebSocket upgrade requests carrying `Origin: https://evil.example` received
HTTP 101 and registered clients on both enhanced protocol paths. Issue #319
tracks that newly confirmed security gap.

## Failure-first evidence and root causes

The native reference client took one WebRTC statistics snapshot as soon as
both SCTP channels opened. Inspection of the pinned webrtc-rs 0.20 event loop
showed that data-channel readiness can become observable before the driver
drains the earlier selected-candidate-pair event into its statistics
accumulator. The transport and gameplay path were healthy; the independent
evidence snapshot was eventually consistent.

The server parsed `security.cors_origins` only into `tower_http::CorsLayer`.
HTTP CORS response headers do not authorize a WebSocket handshake, and neither
the shared handler nor either production router checked the upgrade request's
`Origin`. Invalid configured values could also degrade to permissive CORS.

## Implementation

The native client now observes the exact selected-pair statistics
postcondition for at most one second, retaining the immediate snapshot as its
fast path and the existing exact type/address oracle as the acceptance result.
Deterministic tests prove a delayed third snapshot succeeds and a permanently
missing observation stops at the evidence deadline.

`OriginPolicy` now strictly parses one wildcard, an explicitly configured
opaque `null` origin, or a comma-separated set of canonical serialized HTTP(S)
origins. The same parsed value builds HTTP CORS behavior and
authorizes `/v2/ws` plus `/v3/ws` before connection registration. Disallowed
browser origins receive HTTP 403, ambiguous duplicate headers are rejected,
and malformed configuration fails validation instead of becoming permissive.
The public router helpers install the required policy on every enhanced route;
a raw handler without that extension denies browser origins. Origin-less native
clients remain intentionally compatible because an Origin header is a browser
cross-site control, not non-browser authentication. The development wildcard
retains its documented open behavior.

## Regression boundary

Real loopback WebSocket upgrades cover explicit allow, explicit deny, absent
Origin, wildcard, and both enhanced protocol paths. The HTTP health route also
proves the same exact allowlist emits CORS response headers only for an allowed
origin. Configuration cases reject blank values and origin URIs with paths.
Every v3 integration fixture now uses the policy-bearing route constructor, so
tests cannot silently reintroduce an ungoverned production-shaped alias.

The complete Linux native reference-client workflow passed: formatting,
zero-warning Clippy, 94 unit tests, and all eight live multi-process interop
scenarios. Those scenarios exercised exact selected-pair evidence, reliable and
unreliable data channels, IPv6, host-star and mesh plans, late replanning,
partial ICE failure with relay fallback, and the mixed v2/v3 relay floor.

## Documentation and compatibility

README/configuration tables, the feature guide, the deployment checklist,
source comments, and `[Unreleased]` now state that `security.cors_origins`
governs both HTTP and browser WebSocket origins. They also state the exact
native-client exception and avoid presenting this policy as authentication.
P68 records the bounded work without changing P53 or P56's hosted evidence
thresholds.

## Adversarial review and final validation

Three failure-and-review rounds closed every reported boundary. The first
review required policy extraction at the raw handler boundary, fail-closed
library constructors, strict observation-future cancellation, standalone-route
coverage, and canonical browser serialization. The second caught Tokio's
inner-first timeout polling at the exact deadline, noncanonical port/IP forms,
and detached test-server lifecycle. The final pass found one undocumented
`Origin: null` branch; its explicit-policy regression and opaque-origin warning
were added, after which the reviewer reported zero actionable findings.

The exact final tree passed root and native formatting; warnings-denied Clippy
for every target and feature; `cargo test --locked --all-features` (770 library
tests plus every integration target); `cargo deny --all-features check`; CI,
MSRV, documentation, workflow-hygiene, LLM-policy, tooling-parity, Markdown,
README-badge, source-hygiene, and hook-readiness gates; and the real native
WebRTC workflow with all 94 unit tests and all eight multi-process scenarios.
The pre-commit worktree preflight also passed; its existing 1000 ms advisory
remains tracked separately by issue #318 and is unrelated to this change.

PR #320 reached ready-for-review at implementation head
`a24ddaab6d844bbab203bdea19cf0a054524b404`. All 15 applicable hosted
workflows succeeded: the core CI matrix, WebRTC and browser interop, TURN-only,
Fortress native and WASM interop, verification-nightly fault profiles, formal
verification, fuzzing, AddressSanitizer/Miri, unused-dependency, documentation,
link, spelling, and Markdown gates. The Dependabot-only workflow skipped as
designed. The first WASM attempt exposed one legitimate fixture integration
gap—the Chromium page's concrete loopback origin was not configured—and the
harness now derives that exact allowlist entry from its allocated HTTP port;
the pinned Godot/Emscripten/Chromium rerun passed.

The final GitHub audit found no review threads or PR comments. Cursor Bugbot
reviewed exact head `a24ddaa` without an actionable finding; Copilot's review
requests reported only the repository owner's exhausted quota. The repeated
independent adversarial loop ended with zero findings. The PR is mergeable and
non-draft; P68 remains open in PLAN.md only for merge, while P53 and P56 retain
their unchanged 3/20 hosted evidence cohorts.
