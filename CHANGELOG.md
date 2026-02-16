# Changelog

## Unreleased

### Added

- Architecture Decision Records (ADRs) documentation structure
  - Created docs/adr/ directory with comprehensive index
  - Added ADR-001: Reconnection Protocol documenting WebSocket reconnection design
  - Integrated ADR references into main documentation navigation (docs/README.md, docs/architecture.md)

### Changed

- **CI: Updated nightly toolchain for cargo-udeps from nightly-2025-02-21 to nightly-2026-01-15**
  - Updated nightly version used by cargo-udeps from 360-day-old nightly-2025-02-21 to recent nightly-2026-01-15
  - Added comprehensive documentation explaining why nightly is needed for cargo-udeps
  - Documented update criteria, policy, and trade-offs between pinned vs rolling nightly
  - Added "Nightly-Only CI Tools" section to `.llm/skills/msrv-and-toolchain-management.md`
  - Clarified that nightly is ONLY for CI analysis tools, never for production builds
  - This does not affect production code, which continues to use stable MSRV (1.88.0) from Cargo.toml

- **MSRV Update: Rust 1.87.0 → 1.88.0**
  - Minimum supported Rust version updated from 1.87.0 to 1.88.0
  - Updated in `Cargo.toml` (Rust-version field) and `rust-toolchain.toml`
  - Required to support rand 0.10 dependency update
  - All documentation and configuration files updated to reflect new MSRV

- Updated all production and development dependencies to latest stable versions (verified against crates.io as of 2026-02-15)
  - **Critical dependency updates:**
    - `rand`: 0.9 → 0.10 (latest stable random number generation)
    - `getrandom`: 0.3 → 0.4 (latest stable system randomness source)
    - `reqwest`: 0.12 → 0.13 (dev-dependency, latest stable HTTP client)
    - `matchbox_signaling`: 0.13.0 → 0.14.0 (optional dependency, latest stable)
  - **Version specification standardization:**
    - Standardized all dependency version specifications to use minor versions only (e.g., "1.0" instead of "1.0.228")
    - This allows automatic patch updates while maintaining compatibility
    - Applied consistently across all 50+ dependencies in both [dependencies] and [dev-dependencies] sections
  - **Quality assurance:**
    - All 224 tests passing with updated dependencies
    - Zero clippy warnings with `clippy --all-targets --all-features -- -D warnings`
    - No security advisories detected
    - Cargo.lock updated to reflect latest compatible versions
  - **Dependency duplicate reduction:**
    - Updated getrandom to 0.4, reducing version conflicts in dependency tree
    - Remaining duplicates are all transitive dependencies from third-party crates:
      - `base64`: v0.21.7 (via hdrhistogram) + v0.22.1 (direct) — cannot eliminate without hdrhistogram update
      - `getrandom`: v0.2.17, v0.3.4, v0.4.1 — v0.4.1 is our direct dependency; v0.2/v0.3 from legacy crypto crates
      - `rand`: v0.9.2 (via proptest/test deps) + v0.10.0 (direct) — v0.10.0 is our direct dependency
      - `rand_core`: v0.6.4, v0.9.5, v0.10.0 — matches respective rand versions
      - `cpufeatures`: v0.2.17, v0.3.0 — from different crypto crate generations
      - `hashbrown`: v0.14.5, v0.16.1 — v0.16.1 used by lru and rkyv; v0.14.5 from dashmap
    - These duplicates have minimal impact (total ~50KB) and cannot be eliminated without upstream updates

### Technical Notes

- No breaking API changes detected in dependency updates
- All existing functionality verified through comprehensive test suite
- Performance benchmarks remain consistent
- Docker image builds successfully with updated dependencies
- Updated `src/protocol/room_codes.rs` to use rand 0.10 API (`rand::rng()` and `RngExt` trait)

## 0.1.0 — Initial Release

- Core WebSocket signaling server with in-memory state
- Room creation, joining, leaving with room codes
- Lobby state machine (waiting -> countdown -> playing)
- Player ready-state and authority management
- Spectator mode
- Reconnection with token-based event replay
- In-memory rate limiting
- Prometheus-compatible metrics endpoint
- JSON config file + environment variable configuration
- Docker image support
- Optional TLS/mTLS via rustls (feature: tls)
- Optional legacy full-mesh mode (feature: legacy-fullmesh)
