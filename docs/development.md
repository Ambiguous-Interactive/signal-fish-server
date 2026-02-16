# Development

Guide for building, testing, and contributing to Signal Fish Server.

## Prerequisites

- Rust 1.88.0 or later (see `rust-version` in `Cargo.toml`)
- No system libraries required for the default build

## Building

### Debug Build

```bash
cargo build
```

### Release Build

```bash
cargo build --release
```

Optimized and stripped for production.

### With Optional Features

```bash
# TLS support
cargo build --features tls

# Legacy full-mesh mode
cargo build --features legacy-fullmesh

# All features
cargo build --all-features
```

## Running

### Development

```bash
cargo run
```

### With Custom Config

```bash
# Using -c flag (not implemented - config.json is loaded by default)
# The server automatically looks for config.json in the working directory
cargo run
```

Note: The `-c` flag shown in some examples is not currently implemented. The server automatically loads `config.json` from the working directory if it exists.

### Validate Config

```bash
cargo run -- --validate-config
```

### Print Resolved Config

```bash
cargo run -- --print-config
```

## Testing

### Run All Tests

```bash
cargo test
```

### Test with All Features

```bash
cargo test --all-features
```

### Run Specific Test

```bash
cargo test test_room_creation
```

### Test with Output

```bash
cargo test -- --nocapture
```

### Integration Tests

```bash
cargo test --test integration_tests
```

### E2E Tests

```bash
cargo test --test e2e_tests
```

## Linting

### Format Check

```bash
cargo fmt --check
```

### Apply Formatting

```bash
cargo fmt
```

### Clippy (Default)

```bash
cargo clippy --all-targets -- -D warnings
```

### Clippy (All Features)

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

### Markdown Linting

Check markdown files for formatting issues, missing language identifiers, and inconsistencies:

```bash
./scripts/check-markdown.sh
```

Auto-fix markdown issues where possible:

```bash
./scripts/check-markdown.sh fix
```

Common markdown linting rules enforced by CI:

- **MD040**: All code blocks must have language identifiers (e.g., ` ```bash ` not just ` ``` `)
- **MD060**: Tables must have consistent alignment
- **MD013**: Lines should not exceed 120 characters (except tables)
- **MD044**: Proper capitalization of technical terms (JavaScript, GitHub, WebSocket, etc.)

See `.markdownlint.json` for the complete rule configuration.

### Spell Checking

Check for typos in code and documentation:

```bash
typos
```

Technical terms that are commonly flagged as typos are configured in `.typos.toml`. If a legitimate
technical term is flagged, add it to the `[default.extend-words]` section.

## Benchmarks

```bash
cargo bench
```

View results in `target/criterion/report/index.html`.

## Code Coverage

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html --output-dir coverage
```

Open `coverage/index.html` to view results.

## Docker Development

### Build Image

```bash
docker build -t signal-fish-server .
```

### Build with Cache

```bash
docker build -t signal-fish-server --cache-from ghcr.io/ambiguousinteractive/signal-fish-server:latest .
```

### Run Image

```bash
docker run -p 3536:3536 signal-fish-server
```

### With Custom Config

```bash
docker run -p 3536:3536 -v ./config.json:/app/config.json:ro signal-fish-server
```

## Project Structure

```text
signal-fish-server/
├── src/
│   ├── main.rs                  # Binary entry point
│   ├── lib.rs                   # Library crate root
│   ├── server.rs                # EnhancedGameServer core
│   ├── auth/                    # Authentication
│   ├── config/                  # Configuration
│   ├── coordination/            # Room coordination
│   ├── database/                # Database trait + impl
│   ├── protocol/                # Message types
│   ├── security/                # TLS and crypto
│   ├── server/                  # Room service logic
│   └── websocket/               # WebSocket handlers
├── tests/                       # Integration tests
├── benches/                     # Benchmarks
├── config.example.json          # Example config
├── Cargo.toml
└── Dockerfile
```

## Adding a New Feature

1. **Write tests first**

```bash
# Add test in tests/integration_tests.rs
cargo test test_new_feature -- --nocapture
```

2. **Implement the feature**

```bash
# Make changes in src/
cargo build
```

3. **Run full test suite**

```bash
cargo test --all-features
```

4. **Lint and format**

```bash
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
```

5. **Update documentation**

- Add doc comments to public APIs
- Update CHANGELOG.md
- Update README.md if user-facing

## Debug Logging

Set log level:

```bash
RUST_LOG=debug cargo run
```

Trace level (very verbose):

```bash
RUST_LOG=trace cargo run
```

Module-specific logging:

```bash
RUST_LOG=signal_fish_server::websocket=debug cargo run
```

## Profiling

### CPU Profiling

```bash
cargo install flamegraph
cargo flamegraph --bench benchmark_name
```

### Memory Profiling

```bash
cargo install cargo-valgrind
cargo valgrind --bin signal-fish-server
```

## Common Development Tasks

### Add a Protocol Message

1. Add enum variant to `ClientMessage` or `ServerMessage` in `src/protocol/messages.rs`
2. Implement serialization/deserialization
3. Add handler in `src/server.rs` or `src/server/` submodule
4. Add tests in `tests/integration_tests.rs`
5. Update protocol documentation in `docs/protocol.md`

### Add a Configuration Option

1. Add field to appropriate config struct in `src/config/`
2. Add default value in `src/config/defaults.rs`
3. Add validation in `src/config/validation.rs`
4. Update `config.example.json`
5. Add tests for default, custom, and invalid values

### Add a New Endpoint

1. Add route in `src/websocket/routes.rs`
2. Implement handler function
3. Add tests in `tests/e2e_tests.rs`
4. Update endpoint documentation

## Testing Strategy

### Unit Tests

Place in the same file as the code:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_code_generation() {
        let code = generate_room_code(6);
        assert_eq!(code.len(), 6);
    }
}
```

### Integration Tests

Place in `tests/` directory:

```rust
#[tokio::test]
async fn test_create_and_join_room() {
    let server = create_test_server().await;
    // Test multi-step flows
}
```

### E2E Tests

Test full WebSocket flows:

```rust
#[tokio::test]
async fn test_websocket_connection() {
    let addr = spawn_test_server().await;
    let ws = connect_websocket(&addr).await;
    // Test complete session
}
```

## MSRV and Toolchain Management

### Minimum Supported Rust Version (MSRV)

The project MSRV is defined in `Cargo.toml` (`rust-version = "1.88.0"`). This is the oldest
Rust compiler version guaranteed to build the project.

### Verifying MSRV Consistency

Before committing changes that update the MSRV, verify all configuration files are consistent:

```bash
./scripts/check-msrv-consistency.sh
```

This script validates that the following files all use the same Rust version:

- `Cargo.toml` (source of truth)
- `rust-toolchain.toml` (developer toolchain)
- `clippy.toml` (MSRV-aware lints)
- `Dockerfile` (production build environment)

### Updating MSRV

When a dependency requires a newer Rust version, follow the MSRV update checklist:

1. **Update all configuration files**:
   - `Cargo.toml`: `rust-version = "1.XX.0"`
   - `rust-toolchain.toml`: `channel = "1.XX.0"`
   - `clippy.toml`: `msrv = "1.XX.0"`
   - `Dockerfile`: `FROM rust:1.XX-bookworm`

2. **Verify consistency**:

   ```bash
   ./scripts/check-msrv-consistency.sh
   ```

3. **Test with new MSRV**:

   ```bash
   cargo clean
   cargo check --locked --all-targets
   cargo test --locked --all-features
   ```

4. **Update documentation**:
   - Update this file's Prerequisites section
   - Update `CHANGELOG.md`
   - Document reason for MSRV bump in commit message

See [`.llm/skills/msrv-and-toolchain-management.md`](../.llm/skills/msrv-and-toolchain-management.md)
for comprehensive MSRV management guidance.

## Continuous Integration

The project uses GitHub Actions for CI. All PRs must pass:

- `cargo fmt --check` - Code formatting
- `cargo clippy --all-targets --all-features -- -D warnings` - Rust linting
- `cargo test --all-features` - All tests
- `cargo build --release` - Release build
- **MSRV verification** - Validates MSRV consistency and builds with exact MSRV
- **Markdown linting** - Validates markdown files for formatting and best practices
- **Spell checking** - Checks for typos in code and documentation
- **YAML validation** - Validates workflow files and configuration
- **Actionlint** - Validates GitHub Actions workflow syntax

### Running All CI Checks Locally

Before pushing, run all CI checks locally:

```bash
# Format check
cargo fmt --check

# Clippy
cargo clippy --all-targets --all-features -- -D warnings

# Tests
cargo test --all-features

# Markdown linting
./scripts/check-markdown.sh

# Spell checking (install with: cargo install typos-cli)
typos

# MSRV consistency
./scripts/check-msrv-consistency.sh
```

Or use the pre-commit hook to run checks automatically:

```bash
./scripts/enable-hooks.sh
```

## Release Process

1. Update version in `Cargo.toml`
2. Update `CHANGELOG.md`
3. Run full test suite: `cargo test --all-features`
4. Build release: `cargo build --release`
5. Tag release: `git tag v0.x.x`
6. Push: `git push origin v0.x.x`

## Next Steps

- [Library Usage](library-usage.md) - Embedding the server
- [Architecture](architecture.md) - System design
