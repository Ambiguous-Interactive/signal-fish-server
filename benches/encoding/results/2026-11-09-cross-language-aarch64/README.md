# Client-language encoding observations — 2026-11-09

The TypeScript protobuf 2.17.0 dependency locks pass all twenty codec/fixture
cells, eight Rust reference wires, and six negative controls. All forty
encode/decode operations retain ten raw samples. The
[previous refresh](../2026-11-05-cross-language-aarch64/README.md) remains unchanged.

Host: shared Linux aarch64 development container. Node.js 24.21.0;
.NET SDK 10.0.402; Godot 4.7.2. CPU quota and memory limits are recorded in
[the host manifest](environment.json). The host was not exclusive. These
samples qualify the current locks. They do not establish a performance change
or server capacity.

| Language | Codec | Fixture | Bytes | Encode median µs | Decode median µs |
| --- | --- | --- | ---: | ---: | ---: |
| typescript | msgpack-all-f64 | input-state | 37 | 2.165 | 0.143 |
| typescript | protobuf | input-state | 10 | 1.571 | 0.230 |
| typescript | msgpack-all-f64 | snapshot-small | 293 | 3.721 | 0.993 |
| typescript | protobuf | snapshot-small | 139 | 5.346 | 0.842 |
| typescript | msgpack-all-f64 | snapshot-large | 1043 | 4.958 | 2.878 |
| typescript | protobuf | snapshot-large | 507 | 12.426 | 2.522 |
| typescript | msgpack-all-f64 | world-delta | 5736 | 22.767 | 14.193 |
| typescript | protobuf | world-delta | 2147 | 30.534 | 17.149 |
| csharp | protobuf | input-state | 10 | 0.031 | 0.067 |
| csharp | protobuf | snapshot-small | 139 | 0.260 | 0.403 |
| csharp | protobuf | snapshot-large | 507 | 0.895 | 1.406 |
| csharp | protobuf | world-delta | 2147 | 4.075 | 6.507 |
| gdscript | msgpack-f32 | input-state | 6 | 4.446 | 3.983 |
| gdscript | schema-varint | input-state | 6 | 2.092 | 2.294 |
| gdscript | msgpack-f32 | snapshot-small | 189 | 38.097 | 23.375 |
| gdscript | schema-varint | snapshot-small | 124 | 5.836 | 8.599 |
| gdscript | msgpack-f32 | snapshot-large | 651 | 125.599 | 77.536 |
| gdscript | schema-varint | snapshot-large | 448 | 16.938 | 27.856 |
| gdscript | msgpack-f32 | world-delta | 3032 | 631.148 | 375.606 |
| gdscript | schema-varint | world-delta | 1755 | 89.091 | 160.407 |

Artifacts: [summary](summary.csv), [raw samples and roundtrips](samples.json),
[Rust wire reference](rust-wire.json), and [host/source manifest](environment.json).
The [method](../../CROSS_LANGUAGE.md) records model boundaries and codec limits.
