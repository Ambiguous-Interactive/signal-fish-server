# Client-language encoding observations — 2026-10-28

The refreshed dependency locks pass all twenty codec/fixture cells, eight Rust
reference wires, and six negative controls. All forty encode/decode operations
retain ten raw samples. The [previous refresh](../2026-10-21-cross-language-aarch64/README.md)
remains unchanged.

Host: shared Linux aarch64 development container. Node.js 24.21.0;
.NET SDK 10.0.402; Godot 4.7.2. CPU quota and memory limits are recorded in
[the host manifest](environment.json). The host was not exclusive. These
samples qualify the current locks; they do not establish a performance change
or server capacity. The Rust dependency update includes Cap’n Proto 0.27.3.

| Language | Codec | Fixture | Bytes | Encode median µs | Decode median µs |
| --- | --- | --- | ---: | ---: | ---: |
| typescript | msgpack-all-f64 | input-state | 37 | 2.196 | 0.232 |
| typescript | protobuf | input-state | 10 | 1.499 | 0.390 |
| typescript | msgpack-all-f64 | snapshot-small | 293 | 4.062 | 1.544 |
| typescript | protobuf | snapshot-small | 139 | 5.278 | 1.538 |
| typescript | msgpack-all-f64 | snapshot-large | 1043 | 7.421 | 4.206 |
| typescript | protobuf | snapshot-large | 507 | 15.981 | 3.900 |
| typescript | msgpack-all-f64 | world-delta | 5736 | 31.693 | 18.262 |
| typescript | protobuf | world-delta | 2147 | 27.570 | 24.884 |
| csharp | protobuf | input-state | 10 | 0.041 | 0.064 |
| csharp | protobuf | snapshot-small | 139 | 0.307 | 0.478 |
| csharp | protobuf | snapshot-large | 507 | 1.110 | 1.485 |
| csharp | protobuf | world-delta | 2147 | 4.569 | 7.418 |
| gdscript | msgpack-f32 | input-state | 6 | 5.630 | 4.701 |
| gdscript | schema-varint | input-state | 6 | 2.840 | 3.144 |
| gdscript | msgpack-f32 | snapshot-small | 189 | 49.764 | 29.424 |
| gdscript | schema-varint | snapshot-small | 124 | 7.595 | 11.828 |
| gdscript | msgpack-f32 | snapshot-large | 651 | 168.325 | 109.134 |
| gdscript | schema-varint | snapshot-large | 448 | 22.476 | 37.239 |
| gdscript | msgpack-f32 | world-delta | 3032 | 822.564 | 509.942 |
| gdscript | schema-varint | world-delta | 1755 | 115.703 | 227.788 |

Artifacts: [summary](summary.csv), [raw samples and roundtrips](samples.json),
[Rust wire reference](rust-wire.json), and [host/source manifest](environment.json).
The [method](../../CROSS_LANGUAGE.md) records model boundaries and codec limits.
