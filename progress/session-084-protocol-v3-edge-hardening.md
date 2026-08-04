# Session 084 — Protocol-v3 edge and specification hardening

## Scope and prioritization

Issue #257 requested a deep v3 sweep across relay/P2P/ICE/TURN races,
capability negotiation, and the wire contract. No pull request or dependency
update was open at session start. The current `main` commit completed all 19
hosted workflows successfully, so the session stayed on the highest-impact
open gameplay/protocol issue rather than inheriting CI repair work.

Three independent audits covered server lifecycle state, wire/spec drift, and
native/browser WebRTC interoperability. They found three production/client
defects and three specification/sample defects. Offer/answer/candidate
serialization, room topology selection, and the relay floor otherwise remained
aligned. Coordinated retained-link replacement for refreshed ICE/TURN requires
a wire-level generation barrier and is tracked separately rather than being
approximated with unsafe endpoint-local state: issue #258 specifies the
bidirectional ordering proof, logical-event semantics, and interop regressions.

## Red-green evidence and fixes

### Reconnect claim versus room garbage collection

The first regression claimed a reconnect record, advanced its original
disconnect age beyond the configured window, and observed that the room fell
out of `rooms_with_active_reconnections`. That failed before the fix. Active
claims now protect their room independently of the original window, so an
in-flight restore cannot race maintenance into deleting the room it is about
to rejoin.

### Protocol-floor negotiation

A deployment configured for `[3, 3]` previously turned an explicit client
maximum of 2 into negotiated v3. The config-level regression failed with 3
where the client's truthful maximum was 2. Negotiation now caps only downward;
authentication rejects a result below the deployment floor with the append-only
`UNSUPPORTED_PROTOCOL_VERSION` code. Real-WebSocket regressions cover both an
authenticated explicit-v2 request on `/v3/ws` and an auth-disabled `/v2/ws`
endpoint default against a v3-only deployment.

### Required data-channel loss

Both reference clients observed peer-connection failure but ignored
data-channel close/error events. A required reliable or unreliable channel
could therefore disappear while the peer connection remained `connected`,
leaving the pair and its reported transport state falsely healthy. The browser
regression first failed because no close callback was registered.

Both engines now forward close/error as a generation-scoped terminal channel
event. Their orchestrators remove only the unusable link (not its authoritative
plan obligation), clear exchange state, report the changed false transport
state exactly once, and retain the WebSocket relay floor. Stale callbacks from
replaced links and duplicate close/error callbacks are rejected by the existing
generation fence.

## Wire and specification closure

- `ConnectionInfo` is now an exact five-branch union matching every internally
  tagged Rust variant, rather than a discriminator with arbitrary properties.
- `SessionPlan` now admits only the four executable topology/transport pairs:
  relay/relay, host/direct, host/webrtc, and mesh/webrtc. Each branch constrains
  host, direct endpoint, peer, and ICE fields appropriately.
- `AuthorityChanged.authority_player` and `AuthorityResponse.reason` are
  required-but-nullable, matching JSON and named-MessagePack null goldens.
- Canonical v3 `RoomJoined` and `Reconnected` snapshots pair every `epoch` with
  `seq`, and the sample ready set matches `is_ready`.
- Sample and real-WebSocket tests enforce paired v3 baselines and byte-absent
  v2 projection.

## Documentation and compatibility

Public protocol, authentication, versioning, ADR, error-code, AsyncAPI, and
changelog documentation now state that servers never raise a client's declared
maximum and explain the structured rejection. `UNSUPPORTED_PROTOCOL_VERSION`
was appended to preserve every existing rkyv discriminant; the frozen v2 error
tokens remain unchanged.

## Adversarial review and verification

Independent server, client, and wire/spec adversarial reviews all reached PASS.
The client review rejected an attempted endpoint-local retained-pair rebuild
because it lacked a bidirectional generation barrier; that experiment was
fully reverted, the real late-join regression returned green, and the required
wire contract was captured in follow-up issue #258.

The final worktree passed the complete local matrix:

- `scripts/run-local-ci.sh` (22/22 gates, including default/all-feature tests,
  Clippy, MSRV, workflow/actionlint, advisory, documentation, and policy tests)
- `cargo deny --all-features check`
- `scripts/check-ci-config.sh` and `cargo test --locked --test ci_config_tests`
  (301 passed, one intentionally ignored)
- native all-feature Clippy plus `scripts/run-webrtc-interop.sh` (65 unit tests
  and all five real native WebRTC cells)
- `scripts/run-browser-interop.sh` (all nine Chromium/native interop cells)
- `scripts/run-turn-interop.sh` (TURN-only positive and mismatched-secret
  fallback controls)

Exact-head hosted CI completed on
`27642721b2264b57b68a1cdb286c4d5cd5f2d785`: all 18 applicable workflows
succeeded and the Dependabot-only workflow was intentionally skipped. Cursor
Bugbot reviewed that exact head with no findings or inline threads. Copilot was
requested after each push but returned its account-quota notice. PR #259 merged
to `main` as `f2311b8f6c205a8c29b667157e75d236fa5b2f1b`, completing P41.
