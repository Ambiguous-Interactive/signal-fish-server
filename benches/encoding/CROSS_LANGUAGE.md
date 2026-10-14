# Client-language gameplay encoding study

This study closes the measurements requested in
[#771](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/771).
TypeScript, C#, and GDScript use the same four frozen inputs as the
[Rust matrix](README.md). No server or client runtime links these harnesses.

## Run

Install Node.js 24, .NET SDK 10.0.402, Godot 4.5.1, and the repository's Rust
toolchain. Restore the locked TypeScript graph once:

```bash
npm ci --ignore-scripts --prefix benches/encoding/typescript
python3 benches/encoding/cross_language.py --check
python3 benches/encoding/cross_language.py --output /tmp/encoding-results
python3 benches/encoding/cross_language.py --verify-recorded /tmp/encoding-results
```

Use `--dotnet /path/to/dotnet` and `--godot /path/to/godot` when needed.
The runner builds C# in Release mode. Godot runs headless with stdout silenced.
The existing library still formats its diagnostic strings. That cost remains
inside its decode measurement. TypeScript runs through `tsx` with Node.js V8.

The hosted workflow runs the complete correctness controls. Timings are recorded
observations, not thresholds. Each benchmark keeps its result observable with a
checksum. It warms each operation for one second, calibrates a fixed iteration
count for about 100 ms, then records ten samples at that count. Parsing source
JSON, preparing the typed model, verifying results, and writing output happen
outside timing. Encode allocates bytes. Decode allocates the codec's owned model.
Protobuf-to-corpus projection happens outside timing. Garbage collection and
codec validation remain inside timing. These are client microbenchmarks, not
server throughput or network latency measurements.

## Contracts and controls

The runner checks source lengths and hashes against the corpus manifest. Each
language checks logical values and coordinate/velocity float64 bits, including
negative zero. The independent Python oracle checks all twenty cells and their
samples. Recorded-source hashes and CSV summaries must agree with the raw
samples. Its five negative controls reject changed values, negative-zero loss,
missing cells, duplicate cells, and changed protobuf bytes.

The checked-in protobuf schema matches Rust `prost` field tags, integer types,
packed repeated doubles, and tile fields. C# and TypeScript protobuf output and
GDScript schema-varint output must equal the Rust export byte for byte for all
four shapes. Each language decodes those same bytes in its roundtrip control.
Godot's JSON parser produces double values for integer fields; the harness
normalizes known integer fields before timing. Coordinates retain double values.

[StreamPeer](https://docs.godotengine.org/en/4.5/classes/class_streampeer.html)
has no `put_uvarint` or `get_uvarint` methods. The local GDScript adapter implements
those operations through `put_u8` and `get_u8`. Its layout matches the Rust
schema-varint adapter, including ZigZag axes and little-endian double coordinates.
Integer limits, empty lists, Unicode, and non-float32 coordinate values have
separate controls. The GDScript adapter is measurement code, not a general parser.

## Codec limits

| Language | Codec | Contract and limit |
| --- | --- | --- |
| TypeScript | `@msgpack/msgpack` 3.1.3 | Named fields; `forceIntegerToFloat` preserves negative zero but writes every number as float64 |
| TypeScript | `@bufbuild/protobuf` 2.12.1 | Generated schema; packed repeated doubles |
| C# | `Google.Protobuf` 3.33.0 | Generated schema; packed repeated doubles |
| GDScript | Godot4MessagePack | Named fields; integers normalized; floats encoded as float32 |
| GDScript | Schema varint | Schema field order; unsigned LEB128; ZigZag axes; float64 coordinates |

The TypeScript default MessagePack mode turns negative zero into integer zero.
A negative control proves that loss. The measured float64 mode preserves all
corpus values but pays for float64 encoding of integer fields too. It is not the
Rust named-MessagePack integer layout.

The GDScript library preserves every coordinate in this frozen corpus because
these values are exactly representable as float32. Its separate `0.1` control
proves that it loses precision outside that set. A small payload size from that
library does not qualify it for the original float64 schema or adoption.

`godot/messagepack.gd` and its license are copies with only whitespace normalized from
[Godot4MessagePack](https://github.com/luiherch/Godot4MessagePack), revision
`3804906acd98cd91101215c223ff10c23daf6991`. Its source hash is in the measurement
manifest. No upstream fix or production dependency is introduced.

`protoc` 3.21.12 generated C# `Game.cs`.
`@bufbuild/protoc-gen-es` 2.12.1 generated TypeScript `game_pb.ts`.
Regenerate from the repository root:

```bash
protoc -I benches/encoding/schema --csharp_out=benches/encoding/csharp benches/encoding/schema/game.proto
protoc -I benches/encoding/schema \
  --plugin=protoc-gen-es=benches/encoding/typescript/node_modules/.bin/protoc-gen-es \
  --es_out=benches/encoding/typescript --es_opt=target=ts benches/encoding/schema/game.proto
```

The [ARM baseline](results/2026-10-14-cross-language-aarch64/README.md) records
all encode/decode samples, wire bytes, runtime versions, host limits, and source
hashes beside the Rust baseline. Different runtime and model costs limit direct
language comparisons. The Rust run uses Criterion's different sampling method.
No result here decides the separate adoption gate in #772.
