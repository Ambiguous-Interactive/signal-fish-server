# Client-language encoding observations — 2026-10-21

The refreshed dependency locks pass all twenty codec/fixture cells, eight Rust
reference wires, and six negative controls. All forty encode/decode operations
retain ten raw samples. The earlier [refresh](../2026-10-16-cross-language-aarch64/README.md)
remains unchanged.

Host: shared Linux aarch64 development container. CPU quota: `max 100000`.
Memory limit: `max`. Node.js 24.21.0; .NET SDK 10.0.402; Godot 4.7.2.
The host was not exclusive. These samples preserve observations for the new
locks. They do not establish a performance change or server capacity.

| Language | Codec | Payload | Bytes | Encode median µs | Decode median µs |
| --- | --- | --- | ---: | ---: | ---: |
| typescript | msgpack-all-f64 | input-state | 37 | 2.231 | 0.159 |
| typescript | protobuf | input-state | 10 | 1.870 | 0.261 |
| typescript | msgpack-all-f64 | snapshot-small | 293 | 3.119 | 1.169 |
| typescript | protobuf | snapshot-small | 139 | 5.022 | 1.360 |
| typescript | msgpack-all-f64 | snapshot-large | 1043 | 6.763 | 4.030 |
| typescript | protobuf | snapshot-large | 507 | 15.770 | 4.095 |
| typescript | msgpack-all-f64 | world-delta | 5736 | 26.434 | 19.793 |
| typescript | protobuf | world-delta | 2147 | 29.888 | 26.038 |
| csharp | protobuf | input-state | 10 | 0.041 | 0.083 |
| csharp | protobuf | snapshot-small | 139 | 0.314 | 0.502 |
| csharp | protobuf | snapshot-large | 507 | 1.029 | 1.654 |
| csharp | protobuf | world-delta | 2147 | 4.542 | 7.456 |
| gdscript | msgpack-f32 | input-state | 6 | 5.788 | 4.288 |
| gdscript | schema-varint | input-state | 6 | 2.552 | 2.823 |
| gdscript | msgpack-f32 | snapshot-small | 189 | 42.606 | 25.962 |
| gdscript | schema-varint | snapshot-small | 124 | 6.517 | 10.158 |
| gdscript | msgpack-f32 | snapshot-large | 651 | 147.799 | 94.754 |
| gdscript | schema-varint | snapshot-large | 448 | 19.965 | 33.431 |
| gdscript | msgpack-f32 | world-delta | 3032 | 750.263 | 444.817 |
| gdscript | schema-varint | world-delta | 1755 | 105.098 | 209.551 |

Artifacts: [summary](summary.csv), [raw samples and roundtrips](samples.json),
[Rust wire reference](rust-wire.json), and [host/source manifest](environment.json).
The [method](../../CROSS_LANGUAGE.md) records model boundaries and codec limits.
