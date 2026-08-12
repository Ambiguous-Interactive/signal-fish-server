# Session 130 — CI validation and scheduling integrity

## Scope and prioritization

The session began from clean `main` at merged P88 commit `426b3d3`. There were
no open pull requests or dependency updates. P53/#274 and P56/#290 remain
hosted-evidence cohorts at 7/20 eligible first attempts; their implementation
work is complete and no local change can manufacture the remaining evidence.

The audit also reproduced a false-red when multiple independent Cargo processes
shared `target/`: a default-feature build replaced the unhashed
`CARGO_BIN_EXE_signal-fish-server` proxy while all-feature mTLS tests ran. The
ordinary sequential default-to-all-feature order was then tested directly and
correctly replaced the proxy with a TLS-capable binary before the mTLS suite.
An initial lazy-copy mitigation was removed after adversarial review showed it
could not establish immutable feature identity before first access. Concurrent
Cargo invocations therefore remain responsible for using isolated target
directories; this session does not claim to solve that external coordination
race.

## CI correctness and cost

The workflow audit closed three independent policy gaps:

- local `--fix` mode no longer appends `|| true` to either Clippy invocation, so
  an unfixable lint cannot be reported as an all-green local run;
- actionlint, documentation validation, and LLM policy now trigger when their
  consumed config/tooling files change, including the shared Lychee policy,
  with release-preflight parity updated;
- cancellable workflows use event-scoped PR numbers and stable push refs. This
  avoids same-named fork-branch collisions and fixes the former unique-run-ID
  push fallback that defeated stale-run cancellation. Docker publication now
  has an explicit non-cancellable serialization group instead of relying on a
  comment that the old text-based guard mistook for real configuration.

Full mutation testing is restored to its stated weekly/manual role. Successful
PR run `31519240825` allocated one baseline plus 40 shards and consumed 206.30
raw / 228 per-job-rounded Linux minutes. The full scope, inventory guard,
negative-oracle strength, weekly schedule, and manual dispatch remain intact;
only the contradictory PR trigger and now-dead fork gates were removed.

## Changelog classification

These changes affect internal CI validation and scheduling only. They do not
change the server binary, public API, protocol, configuration, runtime behavior,
or release artifacts, so no changelog entry is warranted.

## Verification

Strict all-target Clippy, the complete all-feature test suite, cargo-deny,
workflow hygiene, Actionlint, Markdown, documentation, tooling parity, MSRV,
LLM policy, and cross-platform hook gates pass. The first adversarial review's
five findings and the second review's pending-publication replacement finding
were incorporated; the third pass reported zero findings. The exact-head hosted
rollup remains before P89 is complete.
