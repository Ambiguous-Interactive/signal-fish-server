# Session 057 — P10.C H6 reconnect-during-teardown

## Trigger

H6 registered three reconnection race facets, but Session 017 covered only the
window boundary and duplicate-claim cases. The remaining prediction was that a
reconnect arriving after teardown arms its pending record but before the old
connection disappears receives `PlayerAlreadyConnected` without consuming the
token, and succeeds when the client retries after teardown.

## Change

The server's unit-test build now exposes a two-event teardown gate at that exact
boundary. It pauses the real unregister transaction after
`register_disconnection_for_reconnect` completes and before room/connection
removal begins; release builds contain neither the gate nor its branch.

The new handler-level test drives the complete state transition:

- a v3-style token is pre-issued before disconnect;
- unregister arms the same token and stops at the explicit gate;
- the target remains registered while the pending record is present;
- a fresh connection receives exact `PlayerAlreadyConnected`;
- manager validation proves the token remains valid after rejection;
- releasing the gate completes the real teardown; and
- the same fresh connection then redeems that token and receives `Reconnected`
  with the original player and room identities, proving it was not consumed.

There are no sleeps, polling races, retry loops used for synchronization, or
assertions about which task a scheduler happens to run first.

## Verification

Focused verification passed:

- `cargo fmt --all`
- `cargo test --locked --lib reconnect_during_teardown_preserves_token_for_retry -- --nocapture`

The broader cheap local checks, exact-head CI evidence, and automated reviewer
results will be recorded before the session is closed.
