# Development

Guide for building, testing, and contributing to Signal Fish Server.

## Prerequisites

- Rust 1.91.0 or later (see `rust-version` in `Cargo.toml`)
- No system libraries required for the default build

### CI Tooling Parity

The devcontainer is expected to include the same core CLI tooling used by CI,
pre-commit support scripts, and local test workflows, including:

- `yq` (YAML code-block validation)
- `taplo` (TOML code-block validation)
- `rg` / ripgrep (search tooling)
- `fd` (modern file discovery)
- Docker CLI support in VS Code devcontainers
- Latest Codex, OpenCode, and Nanocoder terminal clients
- GitHub and Z.AI MCP servers for every supported coding-agent frontend

To keep rebuilds reliable on Windows + WSL + Docker Desktop and avoid known
DNS failures in the feature install path:

- `.devcontainer/devcontainer.json` keeps `docker-outside-of-docker` configured
   with `"moby": false`.
- `.devcontainer/Dockerfile` installs heavy cargo tools via `cargo-binstall`.
- `.devcontainer/post-create.sh` skips `cargo check --all-features` warm-up by
   default for faster startup. Set `SIGNAL_FISH_WARM_CARGO_CHECK=1` to enable it.
- Agent CLI refreshes use a version-check fast path and run behind the editor
  attach point. Set `SIGNAL_FISH_SKIP_AGENT_REFRESH=1` only when working in a
  constrained/offline environment and an installed version is sufficient.

Global npm packages use `/home/vscode/.npm-global`, owned by `vscode`. Run
`npm install --global <package>` directly; do not use `sudo`.
The root and browser-client `node_modules` named volumes are also initialized
for the `vscode` user, so ordinary local `npm install` commands need no `sudo`.

Before opening a fresh clone, create `.env.local` in the repository root.
An empty file is sufficient if you do not use authenticated MCP servers.
For MCP access, add `GITHUB_PERSONAL_ACCESS_TOKEN` and `Z_AI_API_KEY` using
Docker env-file syntax: one unquoted `KEY=value` per line, without `export`.
The file is gitignored and excluded from the Docker build context. CI creates
an empty file before starting its test container.

The devcontainer imports this file for GitHub and other environment consumers.
Z.AI additionally reads the repository `.env.local` **at each MCP startup**, with
file values taking precedence over inherited `Z_AI_API_KEY`. Missing files or
missing keys fall back to the environment; an explicit empty key fails closed.
The launcher accepts dotenv quotes and comments without executing shell code.
For Docker compatibility, keep entries unquoted when using the devcontainer.
Restart Z.AI MCP servers after editing the key; no container rebuild is needed.

All supported frontends use `.devcontainer/zai-mcp.mjs` over stdio. Vision runs
the preinstalled binary; the remote servers use the MCP SDK bundled with the
installed Vision package for HTTP sessions and SSE. There is no startup download.
Claude Code, Nanocoder and OpenCode resolve the launcher from the project root;
VS Code uses its workspace path and Codex gets an absolute path during setup.
Codex setup migrates only repository-marked entries and preserves custom tables.
For an existing container, apply the migration once with:

```bash
python3 .devcontainer/configure-zai-mcp.py "${CODEX_HOME:-$HOME/.codex}/config.toml"
```

Restart the affected MCP servers or agent after migration. Never paste keys
into bug reports or committed config.

```bash
python3 scripts/check-zai-mcp.py --live
```

This defaults to `.env.local`; `--env-file PATH` selects another file.
Without `--live`, it makes no network requests. The live check uses the same
launcher as frontends and checks initialize, initialized notification, and
list-tools without invoking search or vision tools.

The startup RCA identified a credential-loading gap: Docker's env-file import was a creation-time snapshot:
editing `.env.local` could leave GUI agents and terminals with stale or empty
credentials. Native remote configurations relied on those inherited values, and
the old diagnostic stopped before the notification failure reported by Codex.
The inherited key matched `.env.local` during this investigation, so missing
credentials do not explain the reported failure in this session. That key passed
the complete direct HTTP handshake and all four shared launcher checks; the original frontend transport failure was not independently
reproduced. The shared launcher removes both the stale environment dependency
and frontend-specific HTTP handling. A valid key and Z.AI account access remain
required.

Validate parity at any time with:

```bash
bash scripts/check-tooling-parity.sh

```

Run full CI configuration validation (including tooling parity) with:

```bash
bash scripts/validate-ci.sh

```

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

### Validate a Custom Config

```bash
# The server automatically loads config.json from the working directory.
# -c is the short form of --validate-config: validate and exit without serving.
cargo run -- -c

```

To serve after validation succeeds, run `cargo run` without `-c`. Configuration
file selection is automatic; `-c` does not accept a path argument.

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
- **MD046**: Use fenced code blocks consistently
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

Measure steady-state heap traffic in the protocol-v3 relay fan-out and
classified outbound queue:

```bash

cargo bench --locked --bench relay_allocations --features allocation-tracking

```

The allocation harness uses a warmed current-thread runtime, prebuilt shared
payload, and warmed classified queues to isolate coordinator routing, fan-out,
and enqueue costs. Recipient snapshots and any actually-backpressured wait set
are rebuilt inside each measured call; healthy recipients must resolve through
the synchronous queue fast path, and the benchmark fails above four allocation
operations per relay plus one fixed operation for the complete 4,096-relay
sample. The harness excludes the inbound handler's stamp and
message construction and its outer builder allocation. It repeats each sample
five times and fails if the samples drift or if the attempt, enqueue, and
receiver-drain ledgers do not prove every expected delivery. Its allocator uses
sequentially consistent counters, so its output is an allocation baseline, not
a latency benchmark; use the ordinary Criterion benchmarks for timing.

Measure the complete ingress-to-wire projection across frozen-v2 raw binary,
protocol-v3 JSON and MessagePack, and mixed-format recipient cohorts:

```bash

cargo bench --locked --bench relay_serialization_allocations --features allocation-tracking

```

This required CI benchmark checks five identical samples per 2-, 8-, and
16-player cell. It proves exact wire digests, codec-operation counts, delivery
ledgers, queue drainage, and allocation-operation, reallocation, and allocated-
byte ceilings. When projection work repeats within a relay's recipient set, it
uses one 472-byte lazy frame cache; single-recipient, non-repeating, and
frozen-v2 raw-passthrough relays do not allocate it.

The ignored real-WebSocket saturation diagnostic retains the exact 16-player
rates used for release-profile comparisons without adding them to default CI:

```bash

cargo test --release --locked --all-features \
  --test sixteen_player_matrix_e2e \
  sixteen_player_relay_saturation_diagnostic_preserves_exact_delivery \
  -- --exact --ignored --nocapture --test-threads=1

```

It runs 960 and 1,920 messages per second per sender, validates the complete
delivery ledger, and prints throughput, latency, backpressure, and RSS
observations. Compare revisions on the same idle machine in alternating order;
the output is diagnostic rather than a portable capacity threshold.
When the base revision predates this named test, apply only the harness diff to
its clean worktree before building so both binaries execute identical inputs:

```bash

base_worktree=/path/to/clean/base-worktree
git diff 875057d -- tests/sixteen_player_matrix_e2e.rs |
  git -C "$base_worktree" apply -

```

Do not interpret a zero-test filter result from the unmodified base as a
comparison run.

Hosted-runner timing policy is measured separately by the
`Relay Timing Observations` workflow. Its daily/manual matrix runs five complete
all-feature clean-grid repetitions in a dedicated process on Linux, Windows,
and macOS, keeps every delivery, conformance, zero-backpressure, and
zero-eviction oracle, but deliberately does not gate the observed wall clock.
Each job retains raw output, one JSONL row per completed cell, and an explicit
attempt manifest for 30 days, the repository's configured maximum, including
RED-run artifacts. At the nominal daily cadence, that covers the 20-allocation
cohort plus a 10-day audit margin. Because missed or delayed schedules can
stretch the cohort, maintainers audit it incrementally and download an attempt
before its artifact expires when collection will exceed 30 days. Records
identify the event, attempt, commit, exact Rust toolchain, workload schema,
runner image, and completion state.

Issue #274's platform decision uses the first 20 consecutive scheduled,
first-attempt allocations per operating system after `relay-clean-v1` lands
(100 requested observations per cell). Manual, pull-request, and rerun samples
are diagnostic only. The GitHub workflow-run ledger is the denominator: a RED,
cancelled, missing-artifact, incomplete, different-toolchain, or different
workload-version attempt invalidates that enablement cohort instead of being
dropped in favor of a later green run. A replacement cohort requires a
documented cause and a new workload version before collection restarts.

The dedicated-process observations may justify only an equivalently isolated,
all-feature PR timing job; they cannot set a threshold for the concurrently
loaded broad Nextest job. The current 250 ms ceiling is a candidate for that
isolated job only if every eligible observation is below it and the largest
retains at least 2x headroom (at most 125 ms). Otherwise the platform stays
correctness-only. A new ceiling must not be invented from one outlier or one
shared-runner sample.

The `relay-clean-v1` cohort decides issue #274's lane placement:

- **macOS stays correctness-only**, permanently. Its dedicated-process
  distribution contains observations above the 125 ms headroom ceiling (worst
  p99 ≈ 165 ms) while every correctness oracle stayed green — direct evidence
  that hosted-macOS wall clock tracks shared-tenancy scheduling, not relay
  behavior.
- **Linux and Windows keep their existing 250 ms p99 wall-clock ceiling** in
  the broad Nextest matrix, unchanged (the gate excludes only macOS). The
  cohort validates keeping it: worst dedicated-process observations were
  ≈ 12 ms (Linux) and ≈ 24 ms (Windows) with zero backpressure, at least 5x
  inside the 125 ms candidacy bar.
- **No new dedicated isolated PR timing job is added for any platform**: no
  platform needs more gating than it already has, correctness oracles run
  everywhere, and per-PR allocation budget is governed by issue #379's
  evidence rules.

Issue #274's comment history retains the full per-allocation audit
(manifests, toolchain/workload pins, and the per-OS distributions, including
the final cohort allocation).

P56's H14 validation uses the existing `scenario-profiles` job in
`Verification Nightly`, preserving the same profile-CI runner and precursor
context as the post-fix baseline. The exact
`unsupported_message_pack_fallback_does_not_flap_weaker_recipient` selector
runs after unrelated scenario or relay-matrix failures when checkout, the Rust
toolchain, and Nextest setup succeeded. Its raw log and versioned attempt
manifest are uploaded immediately afterward for the repository's 30-day
maximum, before a later experiment can fail or consume the job timeout.
That retention covers the 20-allocation cohort plus a 10-day margin only at the
nominal daily cadence; maintainers audit incrementally and download attempts
before expiry if schedule gaps stretch collection beyond 30 days.

The fixed cohort is `h14-capacity-v1`: the first 20 consecutive scheduled,
first-attempt allocations after the P56 production fix. Eligibility depends
only on the scheduled event and first run attempt, never on test outcome,
artifact presence, or completeness. A RED, cancelled, skipped, missing, or
incomplete attempt therefore breaks acceptance instead of creating a sliding
green-only window; reruns and manual/PR runs are diagnostic. Scheduled run
`31070254464` was manually audited before manifests existed and remains the
first eligible observation because this evidence-only instrumentation changes
neither production code, workload, runner context, nor test oracle. A cohort
restart requires a documented causal fix or workload change and a new contract
version.

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

docker build -t signal-fish-server --cache-from ghcr.io/ambiguous-interactive/signal-fish-server:latest .

```

### Run Image

The image ships the secure compiled defaults (metrics auth and the app-ID
allowlist on). For a local trial, opt into the open development posture
explicitly:

```bash

docker run --rm -p 3536:3536 \
  -e SIGNAL_FISH__SECURITY__ENFORCE_APP_ID_ALLOWLIST=false \
  -e SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false \
  signal-fish-server

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
│   ├── auth/                    # Public app-ID allowlist
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

The project MSRV is defined in `Cargo.toml` (`rust-version = "1.91.0"`). This is the oldest
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

See [MSRV Management](https://github.com/Ambiguous-Interactive/signal-fish-server/blob/main/.llm/skills/msrv-management/SKILL.md)
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

# Fast hook-equivalent policy checks over unstaged work
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-commit.ps1 -Worktree
pwsh -NoLogo -NoProfile -NonInteractive -File scripts/hooks/pre-push.ps1 -Worktree

# Canonical local preflight, including hook readiness, LLM policy checks, markdown,
# workflow hygiene, docs/changelog consistency, and policy test suites.
./scripts/run-local-ci.sh

```

Enable git hooks as a final last-resort guard:

```bash

./scripts/enable-hooks.sh

```

## Release Process

Use the two reviewed workflows in the [release runbook](releasing.md):

1. Dispatch **Prepare Release**, review its generated version/changelog pull
   request, and merge only after required checks pass.
2. Dispatch **Release - Publish Crate** from the default branch. It selects the
   reviewed version-introduction commit and creates the immutable annotated tag,
   crate, GitHub Release, binaries, SBOM, and versioned container artifacts.
3. Run the independent public-artifact verification from the release runbook.

Do not edit release metadata or create and push version tags by hand during the
normal release path.

## Next Steps

- [Library Usage](library-usage.md) - Embedding the server
- [Architecture](architecture.md) - System design
