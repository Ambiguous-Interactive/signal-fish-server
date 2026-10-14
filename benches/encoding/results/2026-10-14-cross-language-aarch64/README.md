# Cross-language ARM baseline — 2026-10-14

All four frozen gameplay shapes were measured in all three requested languages.
The run records twenty codec/fixture cells and forty encode/decode operations.
All 400 timed samples are retained. All corpus controls and five independent
negative controls passed. Protobuf and schema-varint match the Rust wire bytes.

Host: Linux aarch64 on the same shared development container as the Rust baseline.
CPU quota: `max 100000`. Memory limit: `max`.
Node.js: 24.21.0. .NET SDK: 10.0.402; runtime: 10.0.13. Godot: 4.5.1.
No CPU affinity or exclusive host was applied. Other host work can affect timings.
No server capacity or language ranking is established by this baseline.

The client-language columns below use sample medians. Rust uses Criterion means
from its earlier run. Sampling, runtimes, and object representations differ.
Use the raw ranges before interpreting small differences. The C# input cell has
wide timing variation. These observations alone do not meet the adoption gate.

| Language | Codec | Payload | Bytes | Encode µs | Decode µs |
| --- | --- | --- | ---: | ---: | ---: |
| Rust | messagepack-named | input-state | 6 | 0.045 | 0.049 |
| Rust | prost | input-state | 10 | 0.049 | 0.039 |
| Rust | schema-varint | input-state | 6 | 0.037 | 0.050 |
| Rust | messagepack-named | snapshot-small | 237 | 0.539 | 0.661 |
| Rust | prost | snapshot-small | 139 | 0.143 | 0.296 |
| Rust | schema-varint | snapshot-small | 124 | 0.157 | 0.207 |
| Rust | messagepack-named | snapshot-large | 843 | 1.361 | 2.090 |
| Rust | prost | snapshot-large | 507 | 0.575 | 1.084 |
| Rust | schema-varint | snapshot-large | 448 | 0.242 | 0.522 |
| Rust | messagepack-named | world-delta | 3752 | 3.931 | 6.830 |
| Rust | prost | world-delta | 2147 | 3.277 | 4.880 |
| Rust | schema-varint | world-delta | 1755 | 0.925 | 2.967 |
| typescript | msgpack-all-f64 | input-state | 37 | 2.277 | 0.236 |
| typescript | protobuf | input-state | 10 | 0.590 | 1.031 |
| typescript | msgpack-all-f64 | snapshot-small | 293 | 4.432 | 1.780 |
| typescript | protobuf | snapshot-small | 139 | 23.330 | 4.750 |
| typescript | msgpack-all-f64 | snapshot-large | 1043 | 9.062 | 6.009 |
| typescript | protobuf | snapshot-large | 507 | 86.391 | 14.260 |
| typescript | msgpack-all-f64 | world-delta | 5736 | 43.045 | 32.286 |
| typescript | protobuf | world-delta | 2147 | 324.245 | 88.276 |
| csharp | protobuf | input-state | 10 | 0.092 | 0.106 |
| csharp | protobuf | snapshot-small | 139 | 0.554 | 0.700 |
| csharp | protobuf | snapshot-large | 507 | 1.440 | 2.417 |
| csharp | protobuf | world-delta | 2147 | 6.616 | 10.745 |
| gdscript | msgpack-f32 | input-state | 6 | 8.178 | 5.812 |
| gdscript | schema-varint | input-state | 6 | 3.124 | 5.686 |
| gdscript | msgpack-f32 | snapshot-small | 189 | 92.335 | 68.364 |
| gdscript | schema-varint | snapshot-small | 124 | 13.982 | 30.973 |
| gdscript | msgpack-f32 | snapshot-large | 651 | 532.614 | 329.915 |
| gdscript | schema-varint | snapshot-large | 448 | 74.887 | 110.845 |
| gdscript | msgpack-f32 | world-delta | 3032 | 2081.443 | 1205.695 |
| gdscript | schema-varint | world-delta | 1755 | 271.582 | 523.888 |

The TypeScript MessagePack mode writes every number as float64 to preserve
negative zero. The GDScript library writes float32 and loses `0.1` precision.
Its smaller sizes apply only to this exactly representable corpus. Neither is
a byte-layout replacement for the Rust named-MessagePack lane.

Artifacts: [summary](summary.csv), [raw samples and roundtrips](samples.json),
[Rust wire reference](rust-wire.json), and [host/source manifest](environment.json).
The [method and reproduction commands](../../CROSS_LANGUAGE.md) describe model
boundaries, numeric controls, generation pins, and library limits.
