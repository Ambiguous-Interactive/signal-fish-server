#!/usr/bin/env bash
# CI configuration validator — catches common issues before pushing to CI.
set -euo pipefail

ERRORS=0
WARNINGS=0
CI_WORKFLOW=".github/workflows/ci.yml"

info()    { printf '\033[1;34m[INFO]\033[0m  %s\n' "$1"; }
warn()    { printf '\033[1;33m[WARN]\033[0m  %s\n' "$1"; WARNINGS=$((WARNINGS + 1)); }
error()   { printf '\033[1;31m[ERROR]\033[0m %s\n' "$1"; ERRORS=$((ERRORS + 1)); }
success() { printf '\033[1;32m[OK]\033[0m    %s\n' "$1"; }

# ---------------------------------------------------------------------------
# 1. Check Cargo.lock version compatibility
# ---------------------------------------------------------------------------
info "Checking Cargo.lock version..."
if [[ -f Cargo.lock ]]; then
    LOCK_VERSION=$(grep -m1 '^version' Cargo.lock | sed 's/version = //' | tr -d '"' || true)
    info "Cargo.lock version: ${LOCK_VERSION:-unknown}"
    if [[ "$LOCK_VERSION" == "4" ]]; then
        # Check whether the CI workflow already uses a compatible action version
        if [[ -f "$CI_WORKFLOW" ]] && grep -q 'cargo-deny-action@v[2-9]' "$CI_WORKFLOW" 2>/dev/null; then
            info "Cargo.lock is version 4 (requires Rust 1.78+). CI already uses a compatible cargo-deny-action."
        else
            warn "Cargo.lock is version 4 (requires Rust 1.78+)."
            warn "Ensure CI actions (e.g. cargo-deny-action) ship a compatible Cargo."
            warn "EmbarkStudios/cargo-deny-action@v2 or later is required for lockfile v4."
        fi
    else
        success "Cargo.lock version ${LOCK_VERSION:-unknown} is compatible."
    fi
else
    error "Cargo.lock not found — run 'cargo generate-lockfile' first."
fi

# ---------------------------------------------------------------------------
# 2. Check that deny.toml exists
# ---------------------------------------------------------------------------
info "Checking deny.toml..."
if [[ -f deny.toml ]]; then
    success "deny.toml found."
else
    error "deny.toml not found — cargo-deny will fail in CI."
fi

# ---------------------------------------------------------------------------
# 3. Check CI workflow for outdated cargo-deny-action
# ---------------------------------------------------------------------------
info "Checking ${CI_WORKFLOW} for outdated cargo-deny-action..."
if [[ -f "$CI_WORKFLOW" ]]; then
    if grep -q 'cargo-deny-action@v1' "$CI_WORKFLOW"; then
        warn "${CI_WORKFLOW} uses cargo-deny-action@v1, which does not support Cargo.lock v4."
        warn "Upgrade to EmbarkStudios/cargo-deny-action@v2 or later."
    else
        success "No outdated cargo-deny-action@v1 references found in ${CI_WORKFLOW}."
    fi
else
    warn "${CI_WORKFLOW} not found — skipping cargo-deny-action version check."
fi

# ---------------------------------------------------------------------------
# 4. Run cargo deny check locally
# ---------------------------------------------------------------------------
info "Running 'cargo deny --all-features check'..."
if command -v cargo-deny &>/dev/null || cargo deny --version &>/dev/null; then
    if cargo deny --all-features check; then
        success "cargo deny --all-features check passed."
    else
        error "cargo deny --all-features check failed — fix issues before pushing."
    fi
else
    warn "cargo-deny is not installed locally — skipping local check."
    warn "Install with: cargo install cargo-deny"
fi

# ---------------------------------------------------------------------------
# 5. Docker & smoke-test validation
# ---------------------------------------------------------------------------
info "Checking Docker configuration..."
if [[ -f Dockerfile ]]; then
    success "Dockerfile found."

    if grep -q 'EXPOSE 3536' Dockerfile; then
        success "Dockerfile exposes port 3536."
    else
        error "Dockerfile does not contain 'EXPOSE 3536'."
    fi

    if grep -q 'SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false' Dockerfile; then
        success "Dockerfile sets REQUIRE_METRICS_AUTH=false."
    else
        error "Dockerfile missing ENV SIGNAL_FISH__SECURITY__REQUIRE_METRICS_AUTH=false — server will crash without auth config."
    fi

    if grep -q 'SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false' Dockerfile; then
        success "Dockerfile sets REQUIRE_WEBSOCKET_AUTH=false."
    else
        error "Dockerfile missing ENV SIGNAL_FISH__SECURITY__REQUIRE_WEBSOCKET_AUTH=false — server will crash without auth config."
    fi

    # Verify HEALTHCHECK port matches EXPOSE port
    # NOTE: grep -oP requires GNU grep (not portable to macOS/BSD grep)
    EXPOSE_PORT=$(grep -oP 'EXPOSE \K[0-9]+' Dockerfile | head -1 || true)
    # NOTE: grep -oP requires GNU grep (not portable to macOS/BSD grep)
    HEALTH_PORT=$(grep -oP 'HEALTHCHECK.*localhost:\K[0-9]+' Dockerfile | head -1 || true)
    if [[ -n "$HEALTH_PORT" && -n "$EXPOSE_PORT" ]]; then
        if [[ "$HEALTH_PORT" == "$EXPOSE_PORT" ]]; then
            success "HEALTHCHECK port ($HEALTH_PORT) matches EXPOSE port ($EXPOSE_PORT)."
        else
            error "HEALTHCHECK port ($HEALTH_PORT) does not match EXPOSE port ($EXPOSE_PORT)."
        fi
    elif [[ -z "$HEALTH_PORT" ]]; then
        warn "No HEALTHCHECK directive found in Dockerfile."
    fi
else
    error "Dockerfile not found."
fi

info "Checking CI smoke test configuration..."
if [[ -f "$CI_WORKFLOW" ]]; then
    if grep -q 'Smoke test' "$CI_WORKFLOW"; then
        success "Smoke test step found in ${CI_WORKFLOW}."
        if grep -q 'for i in.*seq' "$CI_WORKFLOW" || grep -q 'retry' "$CI_WORKFLOW"; then
            success "Smoke test uses a retry loop."
        else
            warn "Smoke test may use a bare 'sleep' instead of a retry loop."
        fi
        if grep -q 'docker logs' "$CI_WORKFLOW"; then
            success "Smoke test prints docker logs on failure."
        else
            warn "Smoke test does not print docker logs on failure — add diagnostics."
        fi
    else
        warn "No smoke test step found in ${CI_WORKFLOW}."
    fi
else
    warn "${CI_WORKFLOW} not found — skipping smoke test check."
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
if [[ $ERRORS -gt 0 ]]; then
    error "CI config check finished with ${ERRORS} error(s) and ${WARNINGS} warning(s)."
    exit 1
elif [[ $WARNINGS -gt 0 ]]; then
    warn "CI config check finished with ${WARNINGS} warning(s)."
    exit 0
else
    success "All CI config checks passed."
    exit 0
fi
