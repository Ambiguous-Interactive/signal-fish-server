# Client-language encoding observations — 2026-11-05

The refreshed dependency locks pass all twenty codec/fixture cells, eight Rust
reference wires, and six negative controls. All forty encode/decode operations
retain ten raw samples. The [previous refresh](../2026-10-28-cross-language-aarch64/README.md)
remains unchanged.

Host: shared Linux aarch64 development container. Node.js 24.21.0;
.NET SDK 10.0.402; Godot 4.7.2. CPU quota and memory limits are recorded in
[the host manifest](environment.json). The host was not exclusive. These
samples qualify the current locks; they do not establish a performance change
or server capacity.

| Language | Codec | Fixture | Bytes | Encode median µs | Decode median µs |
| --- | --- | --- | ---: | ---: | ---: |
| typescript | msgpack-all-f64 | input-state | 37 | 2.313 | 0.175 |
| typescript | protobuf | input-state | 10 | 1.651 | 0.278 |
| typescript | msgpack-all-f64 | snapshot-small | 293 | 3.312 | 1.135 |
| typescript | protobuf | snapshot-small | 139 | 5.034 | 0.969 |
| typescript | msgpack-all-f64 | snapshot-large | 1043 | 5.609 | 3.278 |
| typescript | protobuf | snapshot-large | 507 | 11.461 | 3.007 |
| typescript | msgpack-all-f64 | world-delta | 5736 | 24.846 | 16.925 |
| typescript | protobuf | world-delta | 2147 | 33.442 | 20.381 |
| csharp | protobuf | input-state | 10 | 0.037 | 0.063 |
| csharp | protobuf | snapshot-small | 139 | 0.310 | 0.464 |
| csharp | protobuf | snapshot-large | 507 | 1.074 | 1.648 |
| csharp | protobuf | world-delta | 2147 | 4.651 | 7.508 |
| gdscript | msgpack-f32 | input-state | 6 | 4.880 | 3.868 |
| gdscript | schema-varint | input-state | 6 | 2.286 | 2.529 |
| gdscript | msgpack-f32 | snapshot-small | 189 | 40.793 | 25.242 |
| gdscript | schema-varint | snapshot-small | 124 | 6.478 | 9.815 |
| gdscript | msgpack-f32 | snapshot-large | 651 | 135.373 | 86.774 |
| gdscript | schema-varint | snapshot-large | 448 | 18.305 | 30.281 |
| gdscript | msgpack-f32 | world-delta | 3032 | 685.000 | 429.244 |
| gdscript | schema-varint | world-delta | 1755 | 99.946 | 177.465 |

Artifacts: [summary](summary.csv), [raw samples and roundtrips](samples.json),
[Rust wire reference](rust-wire.json), and [host/source manifest](environment.json).
The [method](../../CROSS_LANGUAGE.md) records model boundaries and codec limits.
