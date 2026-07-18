# Session 054 — release preparation and heartbeat liveness

## Scope

Repair the two independent failure classes exposed by generated release PR
#182, make both classes deterministic under local and CI validation, and carry
the remediation through an exact-head green pull request and reviewer closure.

## Baseline evidence

PR #182 was generated from `main` for version 0.4.1 and changes only the six
canonical release files. Its MSRV, Linux/macOS/Windows Nextest, coverage, and
AddressSanitizer jobs all fail the same documentation-consistency test because
the generated `[0.4.1]` comparison starts at `v0.4.0`, while the repository has
no corresponding dated changelog section. The dedicated documentation job
passes only because it checks out full history and tags; normal shallow
checkouts do not. The checker therefore has topology-dependent behavior.

The Real-World Scenario Profiles job fails independently in the H10 reliable
asymmetric-bandwidth scenario. Under sustained application backpressure, the
receive task awaits message delivery and cannot promptly read the peer's Pong;
the separate heartbeat task then closes the otherwise-active connection with
code 4003. The same failure is reproducible on `main`, so the generated release
diff did not introduce it.

## Approved invariants

- Reconstruct missing 0.1.1, 0.3.0, and 0.4.0 changelog releases from immutable
  tag snapshots and leave only post-v0.4.0 changes under Unreleased.
- Make changelog comparison validation depend only on repository files, never
  on whether tags happened to be fetched.
- Make release preparation reject an invalid baseline before mutating files.
- Probe only otherwise-idle WebSocket connections. Any valid inbound frame
  proves liveness immediately; stale or mismatched Pongs never satisfy a probe.
- Preserve bounded, sequential application processing and the existing timeout
  defaults rather than masking the race with a larger timeout.
- Add deterministic regression tests, observability, and durable LLM guidance.

## Red/green log

### Release history and preparation

Red evidence:

- A fixture whose `[0.2.0]` link skipped the missing `0.1.1` section passed when
  a local `v0.1.1` tag existed and failed without it.
- A fixture with Cargo version `1.2.3` and latest dated changelog release `1.1.0`
  was mutated successfully instead of failing preflight.

Green implementation:

- Reconstructed `0.1.1` (2026-02-23), `0.3.0` (2026-06-20), and `0.4.0`
  (2026-07-12) from annotated tag history. A one-time top-level Markdown bullet
  inventory proved conservation of all 132 existing blocks: 12 remain
  Unreleased, 43 belong to 0.4.0, and 77 belong to 0.3.0. The sole additional
  0.1.1 retrospective note is supported by its three-commit release range.
- The checker now validates exactly one first-position Unreleased section,
  strict unique descending semver headings, real calendar dates, one contiguous
  adjacent comparison chain, and a direct oldest-release link using files only.
- Release preparation now rejects Cargo/changelog drift, a missing/lightweight/
  non-ancestor baseline tag, an existing target tag/section, and lockfile drift
  before editing the six output files. It validates both the baseline and result.
- Data-driven checker cases, topology parity, byte-identical preflight failure
  assertions, and an integrated prepare-then-real-checker fixture are green.

Targeted evidence:

- `cargo test --test release_prepare_tests -- --nocapture`: 7 passed.
- `cargo test --test doc_consistency_script_tests -- --nocapture`: 5 passed.
- `cargo test --test doc_consistency_policy_tests -- --nocapture`: 10 passed.
- `bash scripts/check-doc-consistency.sh --skip-changelog-gate`: passed.
