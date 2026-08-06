# Config and Wire-Format Drift

Use this when changing config docs, config examples, serde enum tokens, or
binary game-data transport.

## Config Tokens

- Config examples and docs must use canonical serde tokens, not Rust enum
  variant names.
- Required string config values that represent credentials, paths, URL payloads,
  or protocol tokens must reject whitespace-only values with `trim().is_empty()`;
  use indexed errors for list entries so operators can find the exact bad field.
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
  `security.cors_origins` and JSON arrays/maps such as `allowed_apps`.
- Comma-list shorthand must stay type-scoped to simple list fields.

## Binary Game Data

- `ServerMessage::GameDataBinary` is an in-memory broadcast carrier.
- The negotiated-v3 binary WebSocket frame is the private
  `websocket::sending` MessagePack metadata envelope for every opaque payload
  encoding. V2 keeps its historical MessagePack map or raw JSON/rkyv
  passthrough bytes.
- Do not re-export the binary frame encoder or frame struct as public API.
- `ProtocolInfo.game_data_formats` comes from
  `ProtocolConfig::supported_game_data_formats()`: `json` is always advertised
  and `message_pack` is advertised only when enabled. `rkyv` is a reserved /
  internal enum token and must not be documented as a negotiated or advertised
  client game-data format unless runtime support and negotiation are added in
  the same change.
