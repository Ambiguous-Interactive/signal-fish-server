#!/usr/bin/env bash
# Signal Fish Server — Post-create setup for the dev container
set -euo pipefail

echo ""
echo "============================================"
echo "  Signal Fish Server — Setting up dev env"
echo "============================================"
echo ""

# Install or update OpenAI Codex CLI from npm on each container create.
# This keeps the CLI current even when Docker reuses cached image layers.
run_with_retries() {
    local max_attempts="$1"
    local initial_delay_seconds="$2"
    shift 2

    local attempt=1
    local delay_seconds="$initial_delay_seconds"
    while true; do
        if "$@"; then
            return 0
        fi

        if ((attempt >= max_attempts)); then
            echo "[setup] ERROR: Command failed after $max_attempts attempts: $*"
            return 1
        fi

        echo "[setup] Command failed (attempt $attempt/$max_attempts); retrying in ${delay_seconds}s..."
        sleep "$delay_seconds"
        attempt=$((attempt + 1))
        if ((delay_seconds < 30)); then
            delay_seconds=$((delay_seconds * 2))
            if ((delay_seconds > 30)); then
                delay_seconds=30
            fi
        fi
    done
}

is_truthy() {
    case "${1:-}" in
    1 | true | TRUE | yes | YES | on | ON)
        return 0
        ;;
    *)
        return 1
        ;;
    esac
}

remove_user_npm_prefix_config() {
    local user_npmrc="${HOME:-}/.npmrc"
    local tmp_npmrc

    if [[ -z "${HOME:-}" || ! -f "$user_npmrc" ]]; then
        return 0
    fi

    if ! grep -Eq '^[[:space:]]*prefix[[:space:]]*=' "$user_npmrc"; then
        return 0
    fi

    echo "[setup] Removing user npm prefix from $user_npmrc so nvm can manage global npm installs."
    tmp_npmrc="$(mktemp)"
    awk '!/^[[:space:]]*prefix[[:space:]]*=/' "$user_npmrc" >"$tmp_npmrc"
    cat "$tmp_npmrc" >"$user_npmrc"
    rm -f "$tmp_npmrc"
}

install_bash_nvm_compatibility_block() {
    local nvm_dir="$1"
    local bashrc="${HOME:-}/.bashrc"
    local tmp_bashrc

    if [[ -z "${HOME:-}" ]]; then
        return 0
    fi

    if ! touch "$bashrc"; then
        echo "[setup] Warning: could not update $bashrc with nvm compatibility block."
        return 0
    fi

    if ! tmp_bashrc="$(mktemp)"; then
        echo "[setup] Warning: could not create temp file for nvm compatibility block."
        return 0
    fi

    cat >"$tmp_bashrc" <<EOF
# >>> signal-fish nvm compatibility >>>
# Keep nvm-managed Node available without npm prefix overrides.
unset NPM_CONFIG_PREFIX
unset npm_config_prefix
unset PREFIX
unset prefix
export NVM_DIR="\${NVM_DIR:-$nvm_dir}"
if [ -d "\$NVM_DIR/current/bin" ]; then
    case ":\$PATH:" in
        *":\$NVM_DIR/current/bin:"*) ;;
        *) export PATH="\$NVM_DIR/current/bin:\$PATH" ;;
    esac
fi
# <<< signal-fish nvm compatibility <<<

EOF

    if ! awk '
        $0 == "# >>> signal-fish nvm compatibility >>>" { skip = 1; next }
        $0 == "# <<< signal-fish nvm compatibility <<<" { skip = 0; next }
        $0 == "# >>> signal-fish nvm PATH fallback >>>" { skip = 1; next }
        $0 == "# <<< signal-fish nvm PATH fallback <<<" { skip = 0; next }
        !skip { print }
    ' "$bashrc" >>"$tmp_bashrc"; then
        echo "[setup] Warning: could not prepare nvm compatibility block."
        rm -f "$tmp_bashrc"
        return 0
    fi

    if ! cat "$tmp_bashrc" >"$bashrc"; then
        echo "[setup] Warning: could not write nvm compatibility block to $bashrc."
        rm -f "$tmp_bashrc"
        return 0
    fi

    rm -f "$tmp_bashrc"
}

load_node_toolchain() {
    local nvm_dir="${NVM_DIR:-/usr/local/share/nvm}"

    unset NPM_CONFIG_PREFIX
    unset npm_config_prefix
    unset PREFIX
    unset prefix
    remove_user_npm_prefix_config

    if [[ -s "$nvm_dir/nvm.sh" ]]; then
        # shellcheck disable=SC1091
        . "$nvm_dir/nvm.sh"

        if command -v nvm >/dev/null 2>&1; then
            nvm use --silent default >/dev/null 2>&1 \
                || nvm use --silent --lts >/dev/null 2>&1 \
                || true
        fi
    fi

    if [[ -d "$nvm_dir/current/bin" ]]; then
        export PATH="$nvm_dir/current/bin:$PATH"
    fi

    install_bash_nvm_compatibility_block "$nvm_dir"

    if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
        echo "[setup] ERROR: Node.js and npm are required to install OpenAI Codex CLI."
        echo "[setup] Rebuild the dev container so the Node devcontainer feature is applied."
        exit 1
    fi
}

install_codex_cli() {
    local codex_npm_spec="${CODEX_NPM_SPEC:-@openai/codex@latest}"

    load_node_toolchain

    echo "[setup] Installing OpenAI Codex CLI from npm: $codex_npm_spec"
    run_with_retries 5 3 npm install --global --include=optional "$codex_npm_spec"

    if ! command -v codex >/dev/null 2>&1; then
        echo "[setup] ERROR: Codex CLI install completed, but 'codex' is not on PATH."
        echo "[setup] npm global prefix: $(npm prefix --global)"
        exit 1
    fi

    echo "[setup] Codex CLI version:"
    codex --version
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
            exit 1
        fi
    done

    # Ensure key binaries execute successfully, not just exist on PATH.
    cargo-deny --version >/dev/null
    cargo-nextest --version >/dev/null
    cargo llvm-cov --version >/dev/null
    cargo mutants --version >/dev/null
    cargo fuzz --help >/dev/null 2>&1
    taplo --version >/dev/null
    echo "[setup] Rust tooling verified."
}

make_project_scripts_executable() {
    local chmod_log
    local chmod_probe
    local first_script

    if [[ ! -d "scripts" ]]; then
        return 0
    fi

    first_script="$(find scripts -type f -name '*.sh' -print -quit)"
    if [[ -z "$first_script" ]]; then
        return 0
    fi

    chmod_probe="$(mktemp scripts/.chmod-probe.XXXXXX 2>/dev/null || true)"
    if [[ -z "$chmod_probe" ]]; then
        echo "[setup] Warning: could not create chmod probe in scripts/. Skipping executable-bit normalization."
        return 0
    fi

    if ! chmod +x "$chmod_probe" 2>/dev/null; then
        rm -f "$chmod_probe"
        echo "[setup] Warning: workspace mount does not allow chmod; skipping scripts/**/*.sh executable-bit normalization."
        echo "[setup] This is common on Windows bind mounts. Scripts can still be run with: bash scripts/<name>.sh"
        return 0
    fi
    rm -f "$chmod_probe"

    chmod_log="$(mktemp)"
    if find scripts -type f -name '*.sh' -exec chmod +x {} + 2>"$chmod_log"; then
        rm -f "$chmod_log"
        echo "[setup] Made scripts/**/*.sh executable."
        return 0
    fi

    echo "[setup] Warning: could not mark every script executable; continuing."
    sed -n '1,10p' "$chmod_log" | sed 's/^/[setup]   /'
    rm -f "$chmod_log"
}

if ! install_codex_cli; then
    echo "[setup] Warning: Codex CLI installation failed after retries; continuing setup."
    echo "[setup] You can retry later with: npm install --global --include=optional ${CODEX_NPM_SPEC:-@openai/codex@latest}"
fi
verify_required_rust_tools

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

make_project_scripts_executable

# Install git hooks if the script exists
if [ -f "scripts/enable-hooks.sh" ]; then
    bash scripts/enable-hooks.sh --quiet
    echo "[setup] Git hooks configured."
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
echo ""
echo "  Full check (mandatory before commit):"
echo "    cargo fmt && cargo clippy --all-targets --all-features && cargo test --all-features"
echo ""
echo "  VS Code tasks are available via Terminal > Run Task"
echo ""
