# Session 085 — Wire-fenced retained WebRTC pair generations

## Scope and prioritization

Session 084's implementation and exact-head hosted review are complete in
merged PR #259; this session first reconciled its stale local tracking. No pull
request or dependency update was open, and the latest reviewed head passed all
18 applicable workflows. Issue #258 is the highest gameplay-impacting open
item: an authoritative WebRTC plan refresh cannot safely apply renewed ICE/TURN
credentials while either endpoint retains an older physical peer connection,
because the current wire carries no pair generation.

P42 introduces a server-authored generation for authoritative WebRTC plans and
all signaling frames, then makes both reference clients rebuild retained links
and reject stale wire signals while keeping logical connectivity events and the
WebSocket relay floor stable.

## Red-green evidence and fixes

The protocol now requires a server-authored `generation` UUID on every
`SessionPlan` and both directions of `Signal`. One authoritative publication
shares a generation across recipients; every later finalization, late join,
reconnect, or host-failover publication receives a fresh one. The signaling
server remains topology-agnostic plumbing and forwards that value unchanged.

Both reference clients now treat a changed generation as a physical-connection
barrier: clear pending signaling, rebuild retained WebRTC pairs with the new ICE
list, preserve logical pair/exchange observations, and reject signals from old,
unknown, or pre-plan generations. The same sweep corrected inverted buffering
logic that retained signals from peers outside the plan and added 32-per-peer /
128-total defensive bounds for the legitimate plan-before-engine race.
Duplicate publications of the same generation target only genuinely added
peers, so an endpoint with a missing local engine link cannot unilaterally
replace a connection that its healthy peer retains. Changed generations target
every retained peer regardless of asymmetric local health; both endpoint-state
directions have native and browser regression coverage.

The AsyncAPI contract, canonical JSONL samples, changelog, protocol reference,
integration guides, scenarios, architecture pages, and formal-model abstraction
boundary now describe the exact generation contract. Spec guards require the
field and reference type in every legal plan and signal branch. JSON and
MessagePack round trips, stale-generation decisions, bounded buffers, shared /
fresh publication generations, and exact relay preservation have direct tests.

The native late-join interoperability cell initially exposed a stale oracle:
incumbents now deliberately report `true -> false -> true` while rebuilding a
retained pair, so the joiner observes the post-join `false/true` fan-outs. The
test now asserts those exact sequences and the same logical pair/exchange counts
instead of expecting no post-join status.

## Validation evidence

- `scripts/run-webrtc-interop.sh`: green, including real N=3 late-join retained
  pair replacement, host star, degraded ICE fallback, and mixed-v2 floor.
- `scripts/run-browser-interop.sh`: green across all nine Chromium/native and
  browser/browser cells.
- `scripts/run-turn-interop.sh`: green for the pinned TURN-only positive and
  mismatched-secret WebSocket-fallback control.
- Final `scripts/run-local-ci.sh`: 22/22 checks green; default and all-feature
  suites, Clippy, Markdown/docs, hooks, workflow policy, advisories, and doc
  consistency all pass after the complete patch.
- Focused protocol/spec/sample/replan/signaling suites and both reference-client
  unit suites pass.
- Two adversarial review passes completed. The first caught and drove the
  same-generation asymmetric-link correction plus active-document shorthand
  repairs; the second reported zero actionable findings and independently
  passed strict Rust/browser/docs validation.
- Cursor's first hosted pass caught reversed outer/inner map cardinalities in
  the hand-built MessagePack depth fixture. The corrected fixture now proves
  both the debug rejection boundary and the release-profile 1021-level valid
  acceptance boundary instead of failing early on an invalid envelope.

## Follow-ups recorded

- #260 resets or re-keys `TransportStatus` deduplication across room membership
  generations.
- #261 makes the remaining versioned AsyncAPI accountability envelopes exact,
  disjoint v2/v3 wire unions.

PR #262 is published. Exact-head hosted review and CI completion are pending
the fixture correction follow-up push.
