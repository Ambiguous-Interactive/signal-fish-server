# Game payload corpus

Frozen inputs for [#769](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/769).
The Rust and cross-language studies in #770 and #771 must use these same values.
These synthetic examples are measurement inputs, not captured game traffic.
They do not establish the best encoding for every game.

`manifest.json` records each file's exact UTF-8 byte count and SHA-256 digest,
including its final LF. Keep the compact JSON files unchanged. The corpus
version changes when a value or source byte changes. Compare codecs against
the same corpus version; do not resize a fixture for one codec.

| Fixture | JSON bytes | Values |
| --- | ---: | --- |
| `input-state` | 17 | One input tick, button mask, and signed horizontal/vertical axes |
| `snapshot-small` | 256 | Two entities and a map name |
| `snapshot-large` | 881 | Eight entities and a map name |
| `world-delta` | 4096 | Three spawns, 54 updates, three removals, and 24 changed tiles |

Input state uses `[tick, buttons, horizontal, vertical]`. Axis values use the
signed range -127 through 127. Snapshots use a tick and an entity list. Each
entity has an integer ID, three position values, three velocity values,
integer health and ammo, and an active flag. The map name identifies the scene.

The world delta gives the current tick and the baseline tick. Spawns use the
snapshot entity shape. Updates carry an ID, position, health, and animation
index. Removals carry entity IDs. Tiles use `[x, y, tile_type]`.

Parse each JSON file as the logical codec input. Preserve array order, field
names, signed values, fractions, booleans, and strings. These values fit exact
integers in all target languages. JSON object key order is not semantic.
Schema-based codecs must document their field mapping and reproduce every
value on decode; do not replace the input with an opaque JSON string. Report
encoded payload bytes separately from Signal Fish envelopes and framing.

The JSON sizes are reference sizes, not required sizes for other codecs. Do
not add padding, codec implementations, or runtime dependencies to this corpus.
The existing relay serialization benchmarks measure a separate production
routing seam and retain their own wire digests.

Run the corpus guard with:

```bash
cargo nextest run --test encoding_corpus_tests
```
