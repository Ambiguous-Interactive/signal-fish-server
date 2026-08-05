# Session 096 — Room-code collision retries (P54)

## Scope and prioritization

Remote triage found nine open issues, no open pull requests, and no open
Dependabot updates. The exact `main` head `0103712` had all 17 push workflows
pass on their first attempts. Issue #250 remains the highest raw security concern but
requires an owner decision about the credential and compatibility contract.
P53/#274 is collecting its pre-registered hosted timing cohort. Issue #284 is
therefore the highest-impact bounded gameplay reliability item.

## Failure-first evidence

Automatic creation charged the correct rate-limit operation while `room_code`
was `None`, then generated one `String` before entering coordinated admission.
That erased create-only intent: if the candidate already existed, the ordinary
existing-room branch attempted to join it. The first deterministic regression
scripted `TAKEN1` followed by `FRESH1`; before the fix it entered `TAKEN1`'s join
path and failed its player-name check instead of creating `FRESH1`.

The database's atomic uniqueness rejection was also only an `anyhow` string,
and the existing room-code collision metric had no production caller.

## Implementation

- Automatic requests retain create-only intent through each candidate's
  process-local room-code lock. An occupied lookup or atomic database uniqueness
  loss returns one typed internal collision classification.
- Only that collision is retried, for at most eight candidates. Each attempt
  releases its room-code, application-cap, and game-cap locks in the established
  reverse order before the next candidate. The client-level creation/join rate
  budgets remain charged once before the loop.
- `GameDatabase::create_room_classified` provides the typed result while its
  default implementation preserves source compatibility for database adapters;
  the shipped in-memory backend reports exact collision identity. Automatic
  creation also confirms an occupied candidate after an untyped legacy-adapter
  error, preserving retries during cross-process uniqueness races while
  adopting a creator-owned ambiguous commit so it cannot orphan a second room.
- Collision count, logical retry operations, recovery, and exhaustion are
  observable through dedicated metrics and code-free tracing diagnostics;
  recovered collisions do not permanently degrade service health or distort
  the generic infrastructure-retry success rate.
- Deterministic tests cover collision then success, eight-collision exhaustion,
  explicit-code semantics, concurrent creators, application ownership/quota
  integrity, typed storage classification, legacy untyped collision recovery,
  and ambiguous-commit adoption.

## Operational contract

With `s` random suffix characters, one game's namespace contains `32^s` codes.
At `r` occupied codes, a candidate collides with probability `r / 32^s`; eight
uniform candidates all collide with probability `(r / 32^s)^8`. Documentation
now recommends keeping expected occupancy below 1% of the suffix space and
shows that the common two-character prefix plus default six-character total
length leaves 1,048,576 suffixes for the default 1,000-room cap.

## Dependency and CI audit

The repository-wide audit found two JavaScript graphs that Dependabot and the
scheduled security job did not cover. The root markdown tooling graph reported
four advisories (two high, two moderate) through `markdownlint-cli2` 0.18.1,
and the browser reference client's direct `esbuild` 0.27.7 pin reported one low
advisory. The session updates those exact pins to 0.23.2 and 0.28.1,
respectively, adds daily grouped Dependabot entries for both locked graphs, and
runs `npm audit` for both graphs in the existing scheduled audit job. Structural
tests derive the required coverage from tracked `package-lock.json` files so a
future graph cannot silently fall outside automation or scanning. Markdown CI
now installs Node 22 before the updated linter, matching its declared engine.

One latest scheduled Mutation Testing run on `main` was red only because the
final artifact upload timed out with `ETIMEDOUT`; mutation execution itself
passed, later pull-request mutation runs passed, and all workflows attached to
the exact current `main` push passed. This is recorded as hosted artifact
transport evidence, not attributed to a code failure or described as a wholly
green scheduled-workflow history.

## Validation and review

The failure-first regression is green. Focused room-code, generated-creation,
concurrency, typed-database, and authenticated ownership/quota tests pass. The
full local gauntlet is also green: formatting, strict all-target/all-feature
Clippy, `cargo test --locked --all-features`, all 309 CI configuration tests,
documentation and workflow policy checks, cargo-deny, both npm audits, browser
tests/type checks/formatting, Markdown, and hook preflights. The dependency
configuration guards were observed red before the YAML changes and green after
them. Three adversarial passes found and then cleared legacy-adapter ambiguous
commit handling, permanent health degradation after a recovered collision, and
shared retry-metric denominator drift. Publication, exact-head hosted CI, and
the hosted reviewer feedback loop remain to be completed on the final head.
