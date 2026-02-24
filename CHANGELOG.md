# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-02-24

### Changed

- Clarified protocol and guide documentation for player readying:
  `PlayerReady` is the canonical lobby-ready message, has no payload, and
  toggles ready/unready state per send.
- Updated MSRV from `1.87.0` to `1.88.0` and synchronized related configuration/documentation files.
- Updated production and development dependencies to latest compatible stable releases (verified 2026-02-15).
- Standardized dependency version requirements to minor-version form (for example, `1.0`) to allow safe patch updates.

### Added

- Added Architecture Decision Record (ADR) documentation scaffolding under `docs/adr/`.
- Added ADR index integration in `docs/README.md` and `docs/architecture.md`.

## [0.1.0] - 2026-02-15

### Added

- Initial release of Signal Fish Server.
- Core WebSocket signaling server with in-memory state.
- Room creation, joining, and leaving with room codes.
- Lobby state machine (`waiting` -> `lobby` -> `finalized`).
- Player ready-state and authority management.
- Spectator mode and reconnection with token-based event replay.
- In-memory rate limiting and Prometheus-compatible metrics endpoint.
- JSON config file + environment variable configuration.
- Docker image support.
- Optional TLS/mTLS support via `rustls` (`tls` feature).
- Optional legacy full-mesh mode (`legacy-fullmesh` feature).

[Unreleased]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Ambiguous-Interactive/signal-fish-server/releases/tag/v0.1.0
