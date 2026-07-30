# Session 069 — Consistency and durability contract

**Branch:** `agent/session-069-consistency-contract`
**Base:** `51600fd` (PR #228, session 068)

## Objective

Close the concrete CAP/distributed-resilience contract gap in issues #206/#210.
Add one bounded quantitative formal-verification increment toward the
open-ended issue #220, and incorporate the two dependency updates opened
during the session without carrying their generated failures forward.

## Starting evidence

- No open or draft pull request existed at the initial snapshot.
- `main` exactly matched the merge of PR #228, whose source revision passed
  every required workflow.
- The shipped server and its models demonstrated split-home failures and
  bounded control replay, but no single document defined commit,
  acknowledgement, connection-loss, and process-loss semantics per operation.
- The reconnect documentation had no exact gameplay-loss cut or
  configuration-shaped quantitative ceiling.
- Dependabot subsequently opened PRs #229 and #230. PR #229 bypassed root
  manifest holds through the fuzz package's path dependency. PR #230 selected
  `webrtc` 0.17.2 while leaving `webrtc-sctp` 0.17.1, which failed to compile
  because the mixed family disagreed on `Config.mtu`.

## Implementation

- Add the consistency and durability contract and ADR-0008, defining one
  active home process per room, local-memory-only commits, operation-specific
  acknowledgement evidence, reconnect behavior, and fail-loud room rebuild
  boundaries.
- Correct stale claims that custom `GameDatabase` storage makes live rooms
  restartable or that reconnect recovers missed gameplay payloads.
- Add `ReconnectLossBound.tla` with exhaustive normal-window and zero-window
  configurations. It proves the additional exposure caused by a disconnect:
  the old queue tail plus every dequeued-but-client-unobserved pipeline stage
  plus gameplay accepted while absent. Already-accounted delivery-class
  omissions are outside the theorem.
- Require every standalone Dependabot Cargo job that path-depends on the server
  to inherit all root version holds. The live YAML/manifest inventory covers
  both native and fuzz jobs without coupling scheduled audit coverage to the
  presence of an `ignore` block.
- Add the native lock to the scheduled RustSec audit and enforce root/native
  banned-crate and source-policy parity while permitting only a narrower
  graph-specific native license set.
- Refresh the complete native-client dependency graph so all compatible
  webrtc-rs crates move together to 0.17.2 instead of retaining the
  uncompilable partial selection from PR #230.

## Quantitative evidence

- The checked ceiling is
  `QCAP + PCAP + BURST + RATE * WINDOW`. `BURST` is available immediately and
  steady admissions require elapsed rate quanta, the discrete counterpart of
  the enforced arrival curve `A(T) <= B + ceil(R*T)`.
- Both positive TLC configurations pass. The `WINDOW = 0` shape can spend its
  burst but cannot admit steady-rate traffic.
- The CI-pinned expected-failure configuration enables
  `IgnorePostQueuePipelineBug`; the runner passes only for the exact reachable
  `ReconnectExposureBounded` violation (`7 > 6`), proving the complete
  post-queue term is necessary.
- The full TLA+ runner and every Z3 proof set pass with the new model included.

## Dependency evidence

- Rust 1.89 locked compilation succeeds for the fully refreshed native client.
- The supported WebRTC interop runner builds the real server and exercises the
  native client with the required server-binary environment.
- Root, native, and fuzz cargo-deny policy passes. The native license check no
  longer emits unmatched `OpenSSL`/legacy `ring` policy warnings.
- Live policy tests require scheduled cargo-audit coverage for every
  Dependabot-managed Cargo graph.

## Changelog classification

The new behavioral contract and corrected durability claims are user-visible
documentation, so the Unreleased `Changed` section records them. The same
section's existing dependency-reproducibility and compatible-refresh entries
now record cross-job hold inheritance, complete scheduled advisory coverage,
and the coherent webrtc-rs family refresh.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- `cargo test --locked --all-features`
- Root, native, and fuzz `cargo deny --all-features check`
- Native Rust 1.89 locked all-target compilation
- Native reference-client runner: 60 unit tests and all five real
  multi-process WebRTC scenarios
- Full TLA+ runner, including the exact-diagnostic expected failure, and every
  Z3 proof set
- Model-based state machines, reconnect replay/window races, and the ignored
  two-process split-brain catalog
- CI configuration, Dependabot, MSRV, documentation consistency, workflow
  hygiene, Markdown, tooling-parity, LLM policy, and hook readiness/preflight
  checks

The CI configuration checker retains the known Docker `HEALTHCHECK` parsing
warning tracked by issue #226; it exits successfully and all other checks pass.

## Adversarial review

The first independent contract/model audit rejected the original total-loss
claim: it excluded already-accounted latest/volatile omissions, treated an
average rate as a burst-free arrival bound, omitted the server batcher and
active write, and conflated enqueue with socket-write evidence. The corrected
theorem is additional disconnect exposure, uses an explicit burst arrival
curve, covers every stage through client observation, separates all commit and
delivery boundaries, and pins its seeded bug as an exact expected TLC failure.
The final re-review reported zero findings.

The dependency audit found incomplete cross-job hold inheritance, missing
scheduled native RustSec coverage, and copied deny-policy drift. A separate
fix/re-review loop generalized the live inventories, covered all managed
graphs, and removed unmatched native policy. Its final two re-reviews reported
zero findings. Changelog classification and wording review pass.

## Publication and hosted evidence

- Ready-for-review PR
  [#231](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/231)
  targets `main`, is mergeable, and contains the contract, proof, dependency
  fixes, and this session record.
- Implementation head `98a578e1a15024f6488fd674d28b047e3a424017`
  passed every applicable hosted workflow: CI, Advanced Safety, Verification
  Nightly, Formal Verification, WebRTC Interop, Browser Interop, Documentation
  Validation, Link Check, Workflow Hygiene, Unused Dependencies, Spellcheck,
  Markdownlint, YAML Lint, and ActionLint. The Dependabot auto-merge workflow
  skipped as intended for a human-authored branch.
- The first hosted Link Check exposed deterministic `403` responses from two
  DOI resolver URLs. Commit `98a578e` retains the DOI identifiers as citation
  text but validates the papers through the authors' primary hosted copies;
  the exact workflow then passed.
- Bugbot reviewed implementation head `98a578e` and reported zero findings.
  Copilot review was explicitly requested after both implementation pushes,
  but the service returned its requester-quota-exhausted notice both times and
  supplied no actionable review. GitHub reports no inline review threads.
- Dependabot PRs #229 and #230 were diagnosed, superseded by the coherent fixes
  in #231, and closed with root-cause comments. PR #231 closes concrete issue
  #210 on merge. Broad research issues #206 and #220 remain open because this
  session defines the current boundary and adds one bounded proof rather than
  adopting distributed ownership or exhausting future formal work. Existing
  issue #226 continues to track the unrelated Docker `HEALTHCHECK` parser
  warning.

This publication record is a documentation-only follow-up to the fully green
implementation head. Its push must pass the same exact-head hosted gates and a
fresh Bugbot/Copilot trigger before handoff.
