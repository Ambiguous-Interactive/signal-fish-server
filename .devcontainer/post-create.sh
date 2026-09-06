#!/usr/bin/env bash
# Signal Fish Server — Post-create setup for the dev container
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=lib-agent-tools.sh
. "$SCRIPT_DIR/lib-agent-tools.sh"

echo ""
echo "============================================"
echo "  Signal Fish Server — Setting up dev env"
echo "============================================"
echo ""

# Shared agent-tooling helpers (npm prefix, Codex/OpenCode/Nanocoder/Z.AI
# installs, GitHub + Z.AI MCP wiring) live in lib-agent-tools.sh, which powers
# .devcontainer/post-start.sh on every container launch.

prepare_worktree_cache_dirs() {
    local cache_dir
    local uid
    local gid
    local cache_dirs=(
        target
        node_modules
        clients/browser/node_modules
    )

    uid="$(id -u)"
    gid="$(id -g)"

    echo "[setup] Preparing named-volume cache directories..."
    for cache_dir in "${cache_dirs[@]}"; do
        if ! sudo install -d -m 0755 -o "$uid" -g "$gid" "$cache_dir"; then
            echo "[setup] ERROR: could not initialize cache directory '$cache_dir'."
            echo "[setup] Rebuild the dev container so its named volumes are recreated."
            return 1
        fi
    done
    echo "[setup] Named-volume cache directories ready."
}

configure_git_safe_directory() {
    local git_probe
    local repo_dir

    if ! command -v git >/dev/null 2>&1; then
        echo "[setup] Warning: git is unavailable; repository hooks cannot be configured."
        return 0
    fi

    repo_dir="$(pwd -P)"
    if git_probe="$(git -C "$repo_dir" rev-parse --show-toplevel 2>&1)"; then
        return 0
    fi

    if [[ "$git_probe" != *"detected dubious ownership"* ]]; then
        echo "[setup] Warning: workspace is not an accessible Git repository; hooks will be skipped."
        echo "[setup]   $git_probe"
        return 0
    fi

    echo "[setup] Trusting the bind-mounted workspace for container-local Git operations."
    if ! git config --global --get-all safe.directory 2>/dev/null | grep -Fxq -- "$repo_dir"; then
        git config --global --add safe.directory "$repo_dir"
    fi

    if ! git -C "$repo_dir" rev-parse --show-toplevel >/dev/null; then
        echo "[setup] ERROR: Git still rejects the workspace after configuring safe.directory."
        return 1
    fi
}

verify_required_rust_tools() {
    local required_tools=(
        cargo
        cargo-deny
        cargo-tarpaulin
        cargo-watch
        cargo-expand
        cargo-llvm-cov
        cargo-nextest
        cargo-mutants
        cargo-fuzz
        taplo
    )

    echo "[setup] Verifying required Rust tooling..."
    for tool in "${required_tools[@]}"; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "[setup] ERROR: required tool '$tool' is missing from PATH."
            echo "[setup] Rebuild the dev container so the tooling image layer is refreshed."
            return 1
        fi
    done

    # Ensure key binaries execute successfully, not just exist on PATH.
    if ! cargo-deny --version >/dev/null \
        || ! cargo-nextest --version >/dev/null \
        || ! cargo llvm-cov --version >/dev/null \
        || ! cargo mutants --version >/dev/null \
        || ! cargo fuzz --help >/dev/null 2>&1 \
        || ! taplo --version >/dev/null; then
        echo "[setup] ERROR: one or more required Rust tools failed their smoke check."
        return 1
    fi
    echo "[setup] Rust tooling verified."
}

make_project_scripts_executable() {
    local chmod_log
    local first_script

    if [[ ! -d "scripts" ]]; then
        return 0
    fi

    first_script="$(find scripts -type f -name '*.sh' -print -quit)"
    if [[ -z "$first_script" ]]; then
        return 0
    fi

    # Probe an existing bind-mounted file. A newly created file can accept
    # chmod on Docker Desktop even when existing Windows-hosted files cannot.
    if ! chmod +x "$first_script" 2>/dev/null; then
        echo "[setup] Warning: workspace mount does not allow chmod; skipping scripts/**/*.sh executable-bit normalization."
        echo "[setup] This is common on Windows bind mounts. Scripts can still be run with: bash scripts/<name>.sh"
        return 0
    fi

    if ! chmod_log="$(mktemp)"; then
        echo "[setup] Warning: could not create a chmod diagnostic log; skipping executable-bit normalization."
        return 1
    fi
    if find scripts -type f -name '*.sh' -exec chmod +x {} + 2>"$chmod_log"; then
        rm -f "$chmod_log"
        echo "[setup] Made scripts/**/*.sh executable."
        return 0
    fi

    echo "[setup] Warning: could not mark every script executable; continuing."
    sed -n '1,10p' "$chmod_log" | sed 's/^/[setup]   /'
    rm -f "$chmod_log"
}

if ! prepare_worktree_cache_dirs; then
    echo "[setup] Warning: named-volume ownership setup failed; continuing."
fi

if ! configure_git_safe_directory; then
    echo "[setup] Warning: Git safe-directory setup failed; continuing."
fi

# Config is local and fast, so make every MCP available before registry work.
if ! configure_codex_mcp_servers; then
    echo "[setup] Warning: Codex MCP configuration failed; continuing setup."
fi

if ! refresh_agent_npm_tools; then
    echo "[setup] Warning: one or more agent npm tools failed to refresh; continuing setup."
    echo "[setup] Retry later with: bash .devcontainer/post-start.sh"
fi

if ! verify_required_rust_tools; then
    echo "[setup] Warning: required Rust tooling verification failed; continuing so the container remains usable."
fi

# Pre-download all dependencies
echo "[setup] Fetching cargo dependencies..."
if run_with_retries 3 5 cargo fetch; then
    echo "[setup] Dependencies fetched."
else
    echo "[setup] Warning: cargo fetch failed after retries; continuing."
    echo "[setup] You can retry manually with: cargo fetch"
fi

if is_truthy "${SIGNAL_FISH_WARM_CARGO_CHECK:-0}"; then
    echo "[setup] Pre-building (cargo check --all-features)..."
    cargo check --all-features 2>&1 || echo "[setup] Warning: cargo check failed, continuing..."
    echo "[setup] Build cache warmed."
else
    echo "[setup] Skipping cargo check warm-up (set SIGNAL_FISH_WARM_CARGO_CHECK=1 to enable)."
fi

if ! make_project_scripts_executable; then
    echo "[setup] Warning: script executable-bit normalization failed; continuing."
fi

# Install git hooks if the script exists
if [ -f "scripts/enable-hooks.sh" ]; then
    if bash scripts/enable-hooks.sh --quiet; then
        echo "[setup] Git hooks configured."
    else
        echo "[setup] Warning: Git hook setup failed; continuing."
    fi
fi

echo ""
echo "============================================"
echo "  Signal Fish Server — Ready!"
echo "============================================"
echo ""
echo "  Useful commands:"
echo ""
echo "    cargo build                         Build the server"
echo "    cargo run                           Run the server (port 3536)"
echo "    cargo test --all-features           Run all tests"
echo "    cargo nextest run --all-features    Run tests with nextest"
echo "    cargo clippy --all-targets --all-features"
echo "                                        Lint with clippy"
echo "    cargo fmt                           Format code"
echo "    cargo deny check                    Check dependencies"
echo "    cargo llvm-cov --all-features --html"
echo "                                        Generate coverage report"
echo "    cargo bench                         Run benchmarks"
echo "    codex                               Start Codex CLI; sign in if prompted"
echo "    opencode                            Start OpenCode"
echo "    nanocoder                           Start Nanocoder"
echo ""
echo "  Agent CLIs and the Z.AI Vision MCP refresh on every container start;"
echo "  rerun manually with: bash .devcontainer/post-start.sh"
echo ""
echo "  GitHub and Z.AI MCPs are wired into every harness (Codex, Claude Code,"
echo "  Copilot, VS Code, OpenCode, Nanocoder). Set these in .env.local before"
echo "  opening the container: GITHUB_PERSONAL_ACCESS_TOKEN and Z_AI_API_KEY."
echo ""
echo "  Full check (mandatory before commit):"
echo "    cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features"
echo ""
echo "  VS Code tasks are available via Terminal > Run Task"
echo ""
