# Protocol Quick Reference and Common Scenarios

## Protocol Quick Reference

### v2 Client Messages (JSON/MessagePack)

Canonical sample: [v2-client-messages.jsonl](code-samples/protocol/v2-client-messages.jsonl)

### v2 Server Messages

Canonical sample: [v2-server-messages.jsonl](code-samples/protocol/v2-server-messages.jsonl)

### v3 Client Messages (additive over v2)

Canonical sample: [v3-client-messages.jsonl](code-samples/protocol/v3-client-messages.jsonl)

### v3 Server Messages (additive over v2)

Canonical sample: [v3-server-messages.jsonl](code-samples/protocol/v3-server-messages.jsonl)

## Active external clients

The active client implementations live in sibling repositories:

- [Rust client](https://github.com/Ambiguous-Interactive/signal-fish-client-rust)
  (local path `../signal-fish-client-rust`).
- [.NET client](https://github.com/Ambiguous-Interactive/signal-fish-client-dotnet)
  (local path `../signal-fish-client-dotnet`).
- [Godot client](https://github.com/Ambiguous-Interactive/signal-fish-client-godot)
  (local path `../signal-fish-client-godot`).

These clients are separate from the reference clients under `clients/`.
For each API, protocol, or observable behavior change, assess all three.
Include additive messages, error codes, defaults, ordering, reconnect rules,
capability negotiation, and encoding changes. Inspect the sibling checkout
when available; otherwise use the connected GitHub app.

Before merging the server PR, create or update a tracking issue in each
affected client repository. Search for existing issues first. Include the
server issue/PR link, old and new behavior, compatibility and migration needs,
and client acceptance tests. Link each client issue from the server PR and
add a backlink to the server issue. Record a reason for each unaffected client.
If impact cannot be verified, open an assessment issue; do not assume parity.
Reference-client tests do not prove external-client compatibility. Keep client
issues open until the client changes and their validation are complete.

## Common Scenarios

### Adding a New Protocol Message

1. Define in `src/protocol/messages.rs` -> handler in `src/server.rs`
   or `src/server/` submodule -> serialization tests -> e2e tests
2. Follow [Mandatory Workflow and Checklists](skills/mandatory-workflow/SKILL.md) for validation.
3. Assess the three [active external clients](#active-external-clients).
   Create or update issues in each affected sibling repository and link them
   from the server PR before merging. This also applies to behavior changes
   that keep the message schema unchanged.

### Adding a Configuration Option

1. Add the field to the appropriate struct in `src/config/`
2. Add a default value in `src/config/defaults.rs`
3. Add validation in `src/config/validation.rs` if needed
4. Update `config.example.json` with the new option and a comment
5. Add tests for default value, custom value, and invalid value cases

### Performance Debugging

```bash
RUST_LOG=signal_fish_server=trace cargo run   # Trace logging
cargo bench                                    # Benchmarks
```

## Resources

[Matchbox](https://github.com/johanhelsing/matchbox) |
[Tokio](https://tokio.rs/) | [Axum](https://docs.rs/axum/latest/axum/)
