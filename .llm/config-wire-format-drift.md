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

## Coupled Defaults

- When one input's validity depends on another input's default (a window that
  must stay below a bound, a count that must fit a capacity), define the
  defaults in ONE constructor that both the enum-string parser and the
  per-field env overrides use. Duplicated literal defaults drift and produce
  configs that parse fine but are refused at run time.
- A runnable default combination is a contract: the all-defaults config must
  be exercised by a test (parse it and run the validation), or a coupled
  refusal will silently kill the default path (seen: a burst window of
  300 ms against a 250 ms bound made `CHURN=reconnect-burst` with no other
  overrides impossible to start).

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
  `ProtocolConfig::supported_game_data_formats()`: `json` is always advertised,
  `message_pack` is advertised when enabled, and the opaque `rkyv`/`protobuf`
  tokens (#627) are advertised only behind `enable_rkyv_game_data` /
  `enable_protobuf_game_data` (default off keeps the pre-#627 advertisement
  byte-identical). The server never decodes opaque encodings: cross-format
  delivery reports `unsupported_format` instead of a JSON conversion.
