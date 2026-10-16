# Refreshed client-language encoding observations — 2026-10-16

The current dependency locks and Godot 4.7.2 pass all twenty codec/fixture
cells, eight Rust reference wires, and six negative controls. All forty
encode/decode operations retain their ten raw samples. The earlier
[baseline](../2026-10-14-cross-language-aarch64/README.md) remains unchanged.

Host: shared Linux aarch64 development container. CPU quota: `max 100000`.
Memory limit: `max`. Node.js 24.21.0; .NET SDK 10.0.402, runtime 10.0.13;
Godot 4.7.2. Other builds ran on the host. No CPU affinity or exclusive host
was applied. These samples qualify the updated codec paths and preserve
observations; they do not establish a performance change or server capacity.

| Language | Codec | Payload | Bytes | Encode median µs | Decode median µs |
| --- | --- | --- | ---: | ---: | ---: |
| typescript | msgpack-all-f64 | input-state | 37 | 7.835 | 0.972 |
| typescript | protobuf | input-state | 10 | 2.502 | 2.562 |
| typescript | msgpack-all-f64 | snapshot-small | 293 | 7.099 | 8.510 |
| typescript | protobuf | snapshot-small | 139 | 14.283 | 6.584 |
| typescript | msgpack-all-f64 | snapshot-large | 1043 | 17.304 | 14.113 |
| typescript | protobuf | snapshot-large | 507 | 28.901 | 17.229 |
| typescript | msgpack-all-f64 | world-delta | 5736 | 76.610 | 80.168 |
| typescript | protobuf | world-delta | 2147 | 71.586 | 72.747 |
| csharp | protobuf | input-state | 10 | 0.153 | 0.392 |
| csharp | protobuf | snapshot-small | 139 | 2.433 | 2.683 |
| csharp | protobuf | snapshot-large | 507 | 5.152 | 6.132 |
| csharp | protobuf | world-delta | 2147 | 19.284 | 23.808 |
| gdscript | msgpack-f32 | input-state | 6 | 21.490 | 13.755 |
| gdscript | schema-varint | input-state | 6 | 10.836 | 8.995 |
| gdscript | msgpack-f32 | snapshot-small | 189 | 166.385 | 98.862 |
| gdscript | schema-varint | snapshot-small | 124 | 45.258 | 75.072 |
| gdscript | msgpack-f32 | snapshot-large | 651 | 1421.237 | 326.947 |
| gdscript | schema-varint | snapshot-large | 448 | 84.289 | 182.415 |
| gdscript | msgpack-f32 | world-delta | 3032 | 3305.848 | 2328.185 |
| gdscript | schema-varint | world-delta | 1755 | 416.445 | 1178.492 |

Artifacts: [summary](summary.csv), [raw samples and roundtrips](samples.json),
[Rust wire reference](rust-wire.json), and [host/source manifest](environment.json).
The [method](../../CROSS_LANGUAGE.md) records model boundaries and codec limits.
