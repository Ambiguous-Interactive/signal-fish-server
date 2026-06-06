# Protocol Quick Reference and Common Scenarios

## Protocol Quick Reference

### v2 Client Messages (JSON/MessagePack)

Canonical sample: [v2-client-messages.jsonl](code-samples/protocol/v2-client-messages.jsonl)

### v2 Server Messages

Canonical sample: [v2-server-messages.jsonl](code-samples/protocol/v2-server-messages.jsonl)

## Common Scenarios

### Adding a New Protocol Message

1. Define in `src/protocol/messages.rs` -> handler in `src/server.rs`
   or `src/server/` submodule -> serialization tests -> e2e tests
2. Follow [Mandatory Workflow and Checklists](skills/mandatory-workflow.md) for validation.

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
