# Session 097 — Public app-ID trust boundary

## Outcome

- Advanced P55 against issue #250 by defining the shipped WebSocket boundary as
  public app-ID allowlisting and accounting, not hostile-client authentication.
- Integrated Dependabot PR #287's browser toolchain updates into the aggregate
  branch and repaired its Prettier/changelog failures.
- Left P53 collecting hosted timing evidence: 0 of 20 eligible scheduled
  attempts per OS exist, so its evidence gate cannot honestly be closed yet.

## Contract

- Canonical operator fields are `security.enforce_app_id_allowlist` and
  `security.allowed_apps`; canonical Rust types are `AppRegistrationEntry`,
  `AppIdAllowlist`, and `AppContext`.
- Legacy JSON/file/env names remain accepted. Deprecated `app_secret` input is
  discarded before merging and is never retained, logged, serialized, or
  printed.
- JSON sources merge in their documented low-to-high order. A source that mixes
  canonical and legacy names installs an enforced empty allowlist, preventing a
  lower-priority open config from taking effect. Canonical field env overrides
  retain final precedence.
- Duplicate public IDs are rejected instead of silently selecting the last
  quota/rate-limit entry.
- Frozen v2/v3 `Authenticate`, `Authenticated`, `AuthenticationError`, and
  timeout identifiers remain unchanged.

## Evidence

The initial RED checks reproduced missing canonical config/Rust fields, and the
incorporated dependency PR reproduced Browser Interop formatting plus doc-gate
failures. The completed implementation proves:

- canonical and legacy config round trips, precedence, conflict fail-closure,
  environment ordering, duplicate rejection, and absence of retained secrets;
- concurrent reuse of the same public ID and rejection of unknown IDs;
- real-WebSocket binding of app context: app A creates a room, app B receives
  non-enumerating seated and spectator denials, and a second app-A connection
  may join;
- existing seated/spectator/reconnect/ready-state admission matrices continue
  to use connection-bound context; and
- v2/v3 golden/property fixtures remain byte-compatible.

## Adversarial loop

The first independent reviews found reversed config precedence, fail-open alias
fallback, silent duplicate-ID overwrite, stale operator authentication claims,
nonfunctional deployment examples, stale config-reference fixtures, and the
advanced main dependency commit. Those findings were implemented before the
follow-up review. The final independent re-scan reported no behavioral,
dependency, compatibility, or terminology blockers.

## Validation and publication

- Exact rebased-head `cargo fmt --check`, warning-denied all-target/all-feature
  Clippy, and locked all-feature Rust test suite: pass.
- Focused app-ID, loader, config-reference, Docker-policy, frozen-v2 golden,
  exact close-code, browser format, browser typecheck, and browser test suites:
  pass.
- `cargo deny --all-features check`: pass with the repository's accepted
  duplicate/unmatched-license warnings.
- Documentation consistency, Markdown, 993 internal links, workflow hygiene,
  LLM policy, hook readiness/preflights, and hook-policy suites: pass.
- Aggregate PR and hosted exact-head CI/review: tracked by the session PR after
  publication.
