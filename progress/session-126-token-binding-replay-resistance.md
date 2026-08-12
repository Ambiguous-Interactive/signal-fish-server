# Session 126 — Token-binding replay resistance and lean fail-closed CI

## Scope and prioritization

The session began from clean `main` at `f3f4a28`. P53/#274 and P56/#290 remain
evidence-only phases whose pre-registered 20-attempt scheduled cohorts were at
six eligible attempts per cell; no in-repository change can honestly complete
them. There was no open pull request or dependency update to incorporate. The
highest actionable correctness and security gap was therefore #347, explicitly
carried from P84. The session also folded in measured, semantics-preserving CI
work from #345 and repaired a newly identified required-status graph defect.

## Replay-resistant token binding

Token binding v2 sends a random 32-byte server challenge as the first
application message after upgrade. Both endpoints derive a 32-byte connection
key using HKDF-SHA-256 with the decoded 16-byte WebSocket handshake key as input
keying material, the challenge as salt, and a protocol-specific info label.
Every proof carries version 2, the `server_nonce_hkdf_sha256` scheme, and an
exact sequence. One locked frontier covers JSON and binary messages, so a valid
proof advances only after signature verification and cannot be replayed,
reordered, skipped, or moved to another connection.

JSON proofs cover RFC 8785 canonical bytes after removing the top-level proof;
accepted numbers are restricted to portable safe-range integer syntax.
Binary proofs cover the exact inner legacy MessagePack payload in a versioned,
named-field MessagePack outer envelope. Distinct NUL-delimited JSON and binary
domains prevent cross-format substitution. Raw binary frames are unauthorized
on every token-bound connection, not only fingerprint mode. The old
client-key-only scheme remains deserializable solely to produce a precise
migration error and cannot be enabled. Non-participating clients remain
unchanged while token binding is optional.

The documentation defines byte-for-byte HKDF, canonical JSON, HMAC, sequence,
fingerprint, and binary-envelope contracts. Separately calculated complete JSON
and MessagePack golden vectors are pinned in tests. Unit coverage also proves
portable safe-integer acceptance, recursive rejection of negative zero,
fractions, exponents, and overflow, replay and cross-connection rejection,
fingerprint enforcement, and signed binary parsing. Real TLS coverage proves
signed JSON and binary traffic through the production listener and rejection of
proof-less frames.

## Certificate-bound reconnect credentials

The rustls-verified leaf-certificate fingerprint now persists on the live
connection and is captured when a reconnect token is pre-issued at join. The
identity survives disconnect registration, reconnect reassignment, and rollback;
a duplicate same-room registration preserves the original issuance identity
instead of stripping or replacing it.

The reconnect manager compares the provided and recorded identity under the
same replay-state write lock used for bearer, player, room, expiry, and claim
checks. Equal-length identity values use the existing constant-time comparison.
A missing or different certificate returns the stable invalid-token result
before a claim is installed, so it neither consumes nor temporarily reserves
the valid credential. Real mTLS tests prove that certificate B with a valid v2
proof cannot use A's stolen token and that A can immediately use the same token
afterward. Rotation policy is explicit: outstanding A tokens require A; B uses
the normal join flow to receive a new B-bound token.

## CI correctness, speed, and cost

All required jobs in `ci.yml` depended on the non-required `Quick Check`. A
quick-gate failure could therefore skip every required downstream context,
which GitHub can report without a blocking failure. Each required dependent job
now runs after prerequisite failure but not workflow cancellation, checks the
prerequisite result in its first setup-free
step, and fails closed before any expensive setup when that result is not
`success`. Windows matrix jobs retain explicit Bash shells for the guard.

The latest cold `Unused Dependencies` main run spent 42 seconds compiling
cargo-machete and 369 seconds compiling cargo-udeps within a 526-second job.
Both exact versions now come from checksum-verified release binaries through the
repository's already pinned installer action, retaining the analyzer commands,
nightly executable probe, and cargo-udeps informational policy. The same change
removes a measured 113-second Taplo source build from documentation validation.
The cold-path evidence supports up to 411 seconds (about 78%, roughly seven
rounded hosted minutes) saved in unused-dependency analysis plus 113 seconds in
documentation validation; warm savings require hosted measurement. Issue #345
remains open for the administrator-only required-check audit, legacy status
alias deletion, and hosted before/after measurement.

## Verification and publication

The complete mandatory local suite is green: formatting, zero-warning all-target
all-feature Clippy, the full locked all-feature test suite, cargo-deny, CI and
MSRV consistency, tooling parity, workflow hygiene, documentation policy, LLM
policy, hook readiness, and worktree pre-commit/pre-push checks all pass. The
focused acceptance evidence includes 20 token-binding units, nine real TLS/mTLS
tests, 18 AsyncAPI consistency tests, 41 configuration tests, 46 frozen-v2 wire
goldens, 11 v3 wire properties, and the public-API privacy compile test. Three
independent adversarial review tracks closed with zero remaining findings.

PR [#349](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/349)
is fully green at exact implementation head
`78e14901a6da85e443a17755d98a663064479add`: all 19 indexed PR workflows
settled without a failure (18 succeeded and the inapplicable Dependabot
auto-merge run skipped). One Rustdoc job initially failed in `actions/checkout`
because the hosted runner had no usable CA file; its targeted rerun passed
without a source change. The first hosted heads found three integration gaps
that local focused gates had not exposed: both standalone dependency graphs
needed the new HKDF edge, a JSON-fenced documentation fragment was not a
complete JSON value, and Panic Policy required the sequence frontier increment
to use explicit checked arithmetic. Each was fixed at its source; the lockfile
guard now compares dependency lists as well as versions, and the exact hosted
Panic Policy job is green.

Bugbot's one review finding identified that a configured reserved v1/v3
subprotocol could relabel the v2 challenge/proof contract. Startup validation
and negotiation now independently reject that mismatch while preserving custom
non-reserved aliases and disabled legacy configuration; the thread is resolved.
The three independent adversarial review tracks report zero remaining findings.
Hosted cost evidence also validates the analyzer optimization: installing both
prebuilt analyzers took 0.93 seconds and the complete `Unused Dependencies` job
took about 110 seconds, versus the audited 526-second cold main run (about 416
seconds saved on this head). The previously red main Pages deployment also
passed its targeted rerun, confirming its self-signed-certificate failure was
runner/network state rather than repository behavior.
