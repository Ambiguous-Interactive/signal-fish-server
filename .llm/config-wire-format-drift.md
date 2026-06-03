# Config and Wire-Format Drift

Use this when changing config docs, config examples, serde enum tokens, or
binary game-data transport.

## Config Tokens

- Config examples and docs must use canonical serde tokens, not Rust enum
  variant names.
- `logging.format` values are `json` and `text`.
- `security.transport.tls.client_auth` values are `none`, `optional`, and
  `require`.
- Token binding scheme values use `sec_websocket_key_sha256`.
- `metrics.dashboard_cache_history_fields` values are `active_rooms`,
  `rooms_by_game`, `player_percentiles`, `game_percentiles`,
  `active_connections`, and `rooms_created`.
- Keep `docs/configuration.md` aligned with `Config::default()` and the
  `SIGNAL_FISH__...` environment override form.

## Env Vars

- Field overrides use the `SIGNAL_FISH__` prefix with double underscores between
  path segments.
- Single-underscore names are reserved for special controls such as
  `SIGNAL_FISH_CONFIG_JSON`, not config fields.
- Env override values are parsed as JSON before any legacy shorthand. Do not add
  generic comma splitting: it corrupts string fields such as
  `security.cors_origins` and JSON arrays/maps such as `authorized_apps`.
- Comma-list shorthand must stay type-scoped to simple list fields.

## Binary Game Data

- `ServerMessage::GameDataBinary` is an in-memory broadcast carrier.
- The negotiated binary WebSocket frame is the private `websocket::sending` bare
  MessagePack frame.
- Do not re-export the binary frame encoder or frame struct as public API.
