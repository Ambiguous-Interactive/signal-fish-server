# Session 060 — P14 dependency hygiene

## Trigger

Dependabot PR #179 was the repository's only live maintenance work, but its
base was ten commits behind `main` and its `tokio-tungstenite` 0.29→0.30 bump
would create two WebSocket stacks. Axum 0.8.9 still resolves
`tokio-tungstenite` and `tungstenite` 0.29, while 0.30 would be used only by
the direct test client.

## Change

- Refreshed Tokio 1.52.3→1.52.4, UUID 1.23.4→1.24.0, Clap 4.6.1→4.6.2,
  Rustls 0.23.41→0.23.42, Regex 1.13.0→1.13.1, Syn 2.0.118→2.0.119,
  Trybuild 1.0.117→1.0.118, and Saphyr 0.0.9→0.0.11.
- Removed `tokio-tungstenite` from normal dependencies because production code
  uses Axum's WebSocket surface; the direct raw client is test-only and remains
  a dev dependency.
- Deliberately retained `tokio-tungstenite` 0.29 so the server and tests share
  one Tungstenite implementation. The 0.30 upgrade remains deferred until the
  server framework can move in lockstep.
- The refreshed resolver also removed the otherwise-unused `windows-sys` 0.60
  family; Quinn's compatible Windows target now shares the existing 0.52
  family instead of retaining a third Windows bindings generation.
- The required full local supply-chain audit exposed a same-class tooling bug:
  `check-advisories.sh` placed cargo-deny's global `--all-features` option after
  the `check` subcommand, which cargo-deny 0.20.2 rejects. The invocation is now
  ordered correctly and the CI policy suite rejects the invalid form.

## Verification

- `cargo check --locked --all-features`
- targeted Clippy for `ci_config_tests` with warnings denied
- data-driven Saphyr validator behavior plus every live workflow parsed
- exact server-Ping/Pong WebSocket regression
- cargo metadata MSRV audit: all eight refreshed crates declare Rust 1.85 or
  lower against the project's Rust 1.89 floor
- `cargo tree`: one `tokio-tungstenite`/`tungstenite` version, with the root's
  direct dependency classified `dev`
- `scripts/check-advisories.sh --full`: no RustSec advisories; bans, licenses,
  and sources all pass
- ShellCheck, Bash syntax, CI config, documentation consistency, and workflow
  hygiene

## CI follow-up

The first exact-head WebRTC Interop run failed at its fast lockfile precheck,
before compilation: removing the root package's normal `tokio-tungstenite`
dependency changed the path package metadata recorded in
`clients/native/Cargo.lock`. Regenerating that fixture lock removed the stale
edge. A repository-wide `cargo metadata --locked --no-deps` sweep then passed
for the root, native, Fortress native, Fortress WASM, and fuzz manifests.

The implementation head `93971e637a63db1e63ec0afac98593cbe078a7c1`
then passed all 12 applicable pull-request workflows after an isolated retry;
the only non-success was the intentional Dependabot auto-merge skip. The first
Verification Nightly attempt had exposed a 1%-loss WebRTC N=8 mesh miss: 27 of
28 peer links formed, one SCTP INIT/ACK exchange did not recover, and the test
failed loudly at its 360-second deadline.

The documentation-only head reproduced that same failure with a different
client. That recurrence proved the earlier retry was not sufficient evidence:
ICE remains `connected` when webrtc-rs exhausts the SCTP INIT/ACK handshake, so
the reference client receives no terminal peer-connection state and never
rebuilds the wedged link. The follow-up closes that class with one bounded,
coordinated pair rebuild before the P2P deadline. A `PairRetry` marker crosses
the server's ordered opaque signaling relay before the fresh Offer; both sides
discard the old engine generation and retain the server-authored glare role.
Retry attempts are generation-deduplicated and budgeted, connected peers do
not emit duplicate logical pair events, and deliberate crippled/pair-partition
negative controls explicitly disable retries. The extension is opt-in because
non-reference peers do not negotiate `PairRetry`; the homogeneous netem matrix
enables one attempt without changing general matchbox/browser interop.

Cursor Bugbot then caught an asymmetric-open bookkeeping gap in the first
retry implementation: the receiver could close a live generation while the
logical `connected_pairs` set still counted it. Current connectivity and the
one-per-obligation public pair event are now separate. A rebuild clears current
connectivity and exchange state; the fresh generation restores it without a
duplicate `p2p_pair_connected` event. Genuine terminal/removal paths clear
both sets, and a focused state-transition regression covers all three edges.

The next exact-head Bugbot pass found the remaining two edges of that same
rebuild transition: a live-to-retrying pair could leave the aggregate status
stale and disappear from exchange obligations long enough to satisfy the exit
criteria, while creating the replacement generation restarted the P2P timeout.
The retry gap is now explicit state that blocks success until the replacement
connects or the original window expires. Rebuilding immediately re-resolves a
previously reported aggregate status, and retry generations cannot arm or
extend either P2P timer. Focused regressions pin both the reconnect transition
and the initial-versus-retry deadline policy. The netem oracle accepts only
deduplicated real transport transitions and still requires the final expected
state; scenarios without coordinated retry retain their exact one-report rule.

A delayed Bugbot result on that head identified two further generation edges.
The channel oracle now permits one additional open/exchange generation only
for each observed retry marker on that exact peer while retaining the required
final generation and exact event schema. Separately, a genuinely new initial
pairing refreshes its full establishment/retry window; only
`PairGeneration::Retry` preserves the existing deadline. The timer regression
covers both policies so preventing retry extension cannot shorten a later
`NewPeer` or authoritative-plan addition.

The otherwise-green exact head then exposed the two equivalent Windows
surfaces of a connection-reset race in the directional-partition oracle. The
test already allowed raw `Io(ConnectionReset)` after observing the
authoritative server cause metric, but Tungstenite can wrap that same reset as
`Protocol(ResetWithoutClosingHandshake)` before surfacing the forwarded close
frame. The shared classifier now recognizes both forms, rejects unrelated I/O
errors, and has a focused data-driven regression; the semantic server-cause
and healed-room proofs remain mandatory.

Local follow-up verification passed 59 native-client unit tests, native
Clippy with warnings denied, both root/native formatting checks, and the root
WebRTC matrix compile. The healthy native N=3 mesh scenario and the intentional
crippled-ICE relay-fallback negative control both pass as real multi-process
runs. The matrix's signal oracle still requires every sent signal to arrive
exactly once and every Offer to have exactly one Answer, including retry
generations. Cursor Bugbot found no issues on the pre-fix heads; Copilot was
explicitly requested after each push but reported quota exhaustion. PR:
<https://github.com/Ambiguous-Interactive/signal-fish-server/pull/191>.
