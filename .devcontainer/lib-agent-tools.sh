#!/usr/bin/env bash
# Shared agent-tooling helpers for the Signal Fish dev container.
#
# Sourced by .devcontainer/post-create.sh (container create/rebuild) and
# .devcontainer/post-start.sh (every container start/launch). It:
#   1. Routes npm global installs through a user-owned prefix
#      (~/.npm-global) so `npm install -g ...` never needs sudo — the
#      nvm-managed Node under /usr/local/share/nvm is root-owned.
#   2. Installs/refreshes the terminal agent CLIs: OpenAI Codex, OpenCode,
#      and Nanocoder (latest, from npm). A version-check fast path probes the
#      registry and skips the reinstall when the installed version is already
#      current, so post-start stays a no-op cost on every ordinary launch; an
#      unreachable registry keeps whatever is already installed.
#   3. Wires the pinned GitHub MCP server (/usr/local/bin/github-mcp-server,
#      installed by .devcontainer/Dockerfile) into Codex. The other harnesses
#      are configured via committed files (.vscode/mcp.json, .mcp.json,
#      opencode.json).
#
# Contract: every function returns non-zero on failure and never exits, so
# callers can degrade gracefully (`if ! f; then warn; fi`).

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

# Point npm's global prefix at a user-owned directory (env, ~/.npmrc, PATH)
# so global package installs never require root.
configure_user_npm_prefix() {
    local npmrc
    local tmp_npmrc
    local npm_prefix="${NPM_CONFIG_PREFIX:-${HOME:-}/.npm-global}"

    if [[ -z "${HOME:-}" ]]; then
        return 0
    fi

    if ! mkdir -p "$npm_prefix"; then
        echo "[setup] ERROR: could not create npm global prefix directory '$npm_prefix'."
        return 1
    fi

    npmrc="${HOME}/.npmrc"
    if ! tmp_npmrc="$(mktemp)"; then
        echo "[setup] Warning: could not create temp file for npm prefix config."
        return 1
    fi

    # Keep any unrelated ~/.npmrc settings, but pin prefix to the user-owned
    # directory (replacing any stale root-owned prefix from the base image).
    # A missing file is the fresh-container case: write only the prefix line.
    # Any other awk failure must not silently replace the user's settings with
    # an empty or truncated file, so it returns before the rewrite below.
    if [[ -f "$npmrc" ]]; then
        if ! awk -v prefix_line="prefix=${npm_prefix}" '
            /^[[:space:]]*prefix[[:space:]]*=/ {
                if (!written) {
                    print prefix_line
                    written = 1
                }
                next
            }
            { print }
            END { if (!written) print prefix_line }
        ' "$npmrc" >"$tmp_npmrc"; then
            echo "[setup] Warning: could not rewrite $npmrc; leaving it unchanged."
            rm -f "$tmp_npmrc"
            return 1
        fi
    else
        printf 'prefix=%s\n' "$npm_prefix" >"$tmp_npmrc"
    fi

    if ! cat "$tmp_npmrc" >"$npmrc"; then
        echo "[setup] Warning: could not update $npmrc with the user-owned npm prefix."
        rm -f "$tmp_npmrc"
        return 1
    fi
    rm -f "$tmp_npmrc"

    export NPM_CONFIG_PREFIX="$npm_prefix"
    case ":${PATH}:" in
    *":${npm_prefix}/bin:"*) ;;
    *) export PATH="${npm_prefix}/bin:${PATH}" ;;
    esac
}

install_bash_nvm_compatibility_block() {
    local nvm_dir="$1"
    local bashrc="${HOME:-}/.bashrc"
    local tmp_bashrc

    if [[ -z "${HOME:-}" ]]; then
        return 0
    fi

    if ! touch "$bashrc"; then
        echo "[setup] Warning: could not update $bashrc with the npm prefix compatibility block."
        return 0
    fi

    if ! tmp_bashrc="$(mktemp)"; then
        echo "[setup] Warning: could not create temp file for npm prefix compatibility block."
        return 0
    fi

    cat >"$tmp_bashrc" <<EOF
# >>> signal-fish npm prefix + nvm compatibility >>>
# User-owned npm global prefix so \`npm install -g\` never needs sudo.
export NPM_CONFIG_PREFIX="\${NPM_CONFIG_PREFIX:-\$HOME/.npm-global}"
case ":\$PATH:" in
    *":\$NPM_CONFIG_PREFIX/bin:"*) ;;
    *) export PATH="\$NPM_CONFIG_PREFIX/bin:\$PATH" ;;
esac
export NVM_DIR="\${NVM_DIR:-$nvm_dir}"
if [ -d "\$NVM_DIR/current/bin" ]; then
    case ":\$PATH:" in
        *":\$NVM_DIR/current/bin:"*) ;;
        *) export PATH="\$NVM_DIR/current/bin:\$PATH" ;;
    esac
fi
# <<< signal-fish npm prefix + nvm compatibility <<<

EOF

    if ! awk '
        $0 == "# >>> signal-fish npm prefix + nvm compatibility >>>" { skip = 1; next }
        $0 == "# <<< signal-fish npm prefix + nvm compatibility <<<" { skip = 0; next }
        $0 == "# >>> signal-fish nvm compatibility >>>" { skip = 1; next }
        $0 == "# <<< signal-fish nvm compatibility <<<" { skip = 0; next }
        $0 == "# >>> signal-fish nvm PATH fallback >>>" { skip = 1; next }
        $0 == "# <<< signal-fish nvm PATH fallback <<<" { skip = 0; next }
        !skip { print }
    ' "$bashrc" >>"$tmp_bashrc"; then
        echo "[setup] Warning: could not prepare npm prefix compatibility block."
        rm -f "$tmp_bashrc"
        return 0
    fi

    if ! cat "$tmp_bashrc" >"$bashrc"; then
        echo "[setup] Warning: could not write npm prefix compatibility block to $bashrc."
        rm -f "$tmp_bashrc"
        return 0
    fi

    rm -f "$tmp_bashrc"
}

load_node_toolchain() {
    local nvm_dir="${NVM_DIR:-/usr/local/share/nvm}"

    configure_user_npm_prefix || true

    # Fast path: lifecycle shells usually resolve node/npm from the container
    # environment already; skip sourcing nvm.sh (slow) unless needed.
    if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
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
    fi

    install_bash_nvm_compatibility_block "$nvm_dir"

    if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
        echo "[setup] ERROR: Node.js and npm are required to install the agent CLIs (Codex, OpenCode, Nanocoder)."
        echo "[setup] Rebuild the dev container so the Node devcontainer feature is applied."
        return 1
    fi
}

# Strip an optional tag/version suffix from an npm spec: "@openai/codex@latest"
# -> "@openai/codex", "opencode-ai@latest" -> "opencode-ai", a bare package name
# (scoped or not) passes through unchanged.
npm_spec_package_name() {
    local spec="$1"
    local pkg="${spec%@*}"

    # A bare scoped package ("@openai/codex") strips to "" at its leading '@'.
    if [[ -z "$pkg" ]]; then
        pkg="$spec"
    fi
    printf '%s\n' "$pkg"
}

# Latest version the registry reports for <spec> (empty on any failure, e.g.
# when offline or a custom spec tag does not resolve).
npm_registry_latest_version() {
    local spec="$1"
    npm view "$spec" version 2>/dev/null | tr -d '\r' | tail -n 1
}

# Version of <pkg> installed under the current global prefix (empty if absent).
# Parsed from `npm ls --json` rather than `<binary> --version` so the check does
# not depend on any CLI's version output format.
npm_global_installed_version() {
    local pkg="$1"
    # The package name is appended AFTER the -e script, so node exposes it as
    # process.argv[1] (argv[0] is the node executable; argv[1] is only the
    # literal "[eval]" when no script argument follows).
    npm ls --global --json --depth=0 2>/dev/null | node -e '
        let raw = "";
        process.stdin.setEncoding("utf8");
        process.stdin.on("data", (chunk) => { raw += chunk; });
        process.stdin.on("end", () => {
            try {
                const deps = JSON.parse(raw).dependencies || {};
                const entry = deps[process.argv[1]];
                process.stdout.write(
                    entry && typeof entry.version === "string" ? entry.version : ""
                );
            } catch {
                process.stdout.write("");
            }
        });
    ' "$pkg" 2>/dev/null
}

# install_npm_global_cli <binary> <npm-spec> [max_attempts]
#
# Installs or upgrades <binary> from npm, but only when the installed version
# differs from what the registry reports: the version-check fast path keeps a
# post-start refresh to a couple of registry probes instead of three full
# reinstalls on every container launch. An unreachable registry never blocks
# startup — an installed CLI is kept as-is, and an absent CLI skips the
# otherwise-doomed install attempts (every launch would otherwise pay the full
# retry/backoff cost for an install that cannot reach the registry).
install_npm_global_cli() {
    local binary="$1"
    local spec="$2"
    local max_attempts="${3:-5}"

    load_node_toolchain || return 1

    local pkg
    local installed
    local latest
    pkg="$(npm_spec_package_name "$spec")"
    installed="$(npm_global_installed_version "$pkg")"
    latest="$(npm_registry_latest_version "$spec")"

    if [[ -z "$latest" ]]; then
        if [[ -n "$installed" ]]; then
            echo "[setup] Registry unreachable; keeping installed ${binary} (${installed})."
        else
            echo "[setup] Registry unreachable and ${binary} is not installed; skipping install (rerun post-create when online)."
        fi
        return 0
    fi

    if [[ -n "$latest" && "$latest" = "$installed" ]]; then
        echo "[setup] ${binary} is current (v${installed}); skipping reinstall."
        return 0
    fi

    echo "[setup] Installing ${binary} CLI from npm: ${spec} (${installed:-absent} -> ${latest:-latest})"
    if ! run_with_retries "$max_attempts" 3 npm install --global --include=optional "$spec"; then
        return 1
    fi

    if ! command -v "$binary" >/dev/null 2>&1; then
        echo "[setup] ERROR: ${binary} install completed, but '${binary}' is not on PATH."
        echo "[setup] npm global prefix: $(npm prefix --global 2>/dev/null || echo unknown)"
        return 1
    fi

    echo "[setup] ${binary} CLI version:"
    "$binary" --version 2>/dev/null || true
}

install_codex_cli() {
    install_npm_global_cli "codex" "${CODEX_NPM_SPEC:-@openai/codex@latest}"
}

install_opencode_cli() {
    install_npm_global_cli "opencode" "${OPENCODE_NPM_SPEC:-opencode-ai@latest}"
}

install_nanocoder_cli() {
    install_npm_global_cli "nanocoder" "${NANOCODER_NPM_SPEC:-@nanocollective/nanocoder@latest}"
}

# Point Codex at the pinned GitHub MCP server. Idempotent: a marker-delimited
# block is appended to ~/.codex/config.toml exactly once, and any pre-existing
# user-managed [mcp_servers.github] table is left untouched. The server
# inherits GITHUB_PERSONAL_ACCESS_TOKEN from the container environment.
configure_codex_github_mcp() {
    local codex_dir="${CODEX_HOME:-${HOME:-}/.codex}"
    local config_toml
    local tmp_toml

    if [[ ! -x /usr/local/bin/github-mcp-server ]]; then
        echo "[setup] Warning: /usr/local/bin/github-mcp-server not found; skipping Codex GitHub MCP config."
        echo "[setup] Rebuild the dev container to install the pinned GitHub MCP server."
        return 1
    fi

    if [[ -z "${HOME:-}" ]]; then
        return 0
    fi

    if ! mkdir -p "$codex_dir"; then
        echo "[setup] Warning: could not create $codex_dir."
        return 1
    fi

    config_toml="$codex_dir/config.toml"
    if ! touch "$config_toml"; then
        echo "[setup] Warning: could not write $config_toml."
        return 1
    fi

    if grep -Fq '# >>> signal-fish github mcp >>>' "$config_toml"; then
        return 0
    fi

    if grep -Eq '^[[:space:]]*\[mcp_servers\.github\]' "$config_toml"; then
        echo "[setup] Existing [mcp_servers.github] found in $config_toml; leaving it untouched."
        return 0
    fi

    if ! tmp_toml="$(mktemp)"; then
        echo "[setup] Warning: could not create temp file for Codex MCP config."
        return 1
    fi

    {
        cat "$config_toml"
        # TOML requires a newline before a new table header.
        if [ -n "$(tail -c 1 "$config_toml" 2>/dev/null)" ]; then
            printf '\n'
        fi
        cat <<'EOF'
# >>> signal-fish github mcp >>>
# GITHUB_PERSONAL_ACCESS_TOKEN is inherited from the container environment
# (see remoteEnv in .devcontainer/devcontainer.json).
[mcp_servers.github]
command = "/usr/local/bin/github-mcp-server"
args = ["stdio"]
# <<< signal-fish github mcp <<<
EOF
    } >"$tmp_toml"

    if ! cat "$tmp_toml" >"$config_toml"; then
        echo "[setup] Warning: could not write Codex MCP config to $config_toml."
        rm -f "$tmp_toml"
        return 1
    fi
    rm -f "$tmp_toml"

    echo "[setup] Codex GitHub MCP server configured in $config_toml."
}
