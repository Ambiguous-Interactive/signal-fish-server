# Rust game-payload encoding study

This standalone benchmark package measures the frozen version-1 gameplay
corpus for [#770](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/770).
It has only development dependencies. The server does not link this package.
The changelog gate treats only `benches/encoding/` as an internal study;
other benchmark paths retain their existing release-note rules.

Run from the repository root:

```bash
cargo nextest run --locked --manifest-path benches/encoding/Cargo.toml --test roundtrip
cargo clippy --locked --manifest-path benches/encoding/Cargo.toml --all-targets -- -D warnings
cargo bench --locked --manifest-path benches/encoding/Cargo.toml --bench allocations
cargo bench --locked --manifest-path benches/encoding/Cargo.toml --bench runtime
```

The runtime command writes Criterion estimates under
`benches/encoding/target/criterion`. Its `-- --test` option runs each path once
without timing. The dedicated hosted workflow checks both benchmarks and the
roundtrip controls. Timings are observations, not CI thresholds.

## Measurement contract

Each run verifies every source byte count and SHA-256 against the
[corpus manifest](../../test-fixtures/encoding/manifest.json).
All codecs start from the same owned Rust model. Each decode produces that
same owned model. The input fixture stays a four-element tuple. Snapshot and
delta fields use the corpus names. Tile tuples retain their original order.
Integer fields use `u32`, except the signed `i32` input axes. Coordinate and
velocity values use `f64`.

The oracle checks all logical values against independently parsed source JSON,
including array order, field names, strings, signed values, and floating-point
bits. Integer and decimal JSON spellings of an equal coordinate represent the
same value. The oracle preserves negative zero. It also checks deterministic
re-encoding. A negative control proves that ordinary equality misses a change
from negative zero to positive zero.

| Codec | Encoding path |
| --- | --- |
| MessagePack named | `rmp_serde::to_vec_named`; map field names |
| MessagePack compact | `rmp_serde::to_vec`; positional struct fields |
| Protobuf | `prost::Message`; tags in `support/model.rs`, packed double lists |
| CBOR (ciborium) | Serde maps and arrays |
| CBOR (cbor4ii) | Serde maps and arrays |
| FlatBuffers | Tables in `schema/game.fbs`; verified readers |
| Cap'n Proto unpacked | `schema/game.capnp`; standard segment framing |
| Cap'n Proto packed | The same schema with standard zero-byte packing |
| Schema varint | Schema field order, unsigned LEB128, ZigZag signed axes |

Schema fields correspond directly to the corpus fields. Each codec uses the
fixture's known shape at decode; no codec pays for a payload-type envelope.
The varint layout prefixes each list and UTF-8 string with its byte or element
count. It stores each `f64` as eight little-endian bytes and each boolean as one
byte. Entity, update, and tile fields follow `support/model.rs` declaration
order. Its decoder rejects truncated, overflowing, and trailing input.

Wire sizes count only the codec payload. They exclude Signal Fish envelopes,
WebSocket framing, and compression. Cap'n Proto sizes include its segment
framing. FlatBuffers sizes include its root offset and tables. Named and compact
MessagePack have different field-name contracts; compare both baselines.

Criterion measures encode and decode separately, with 50 samples, one second
of warmup, and two seconds of measurement per cell. Setup, source parsing,
validation, and output formatting run outside the timed loop. Encode returns a
new owned byte vector. Decode includes verification and conversion to the
common owned model; borrowed-reader traversal is a different workload.
Returned values are dropped in the timed iteration.

The allocation executable installs `stats_alloc` in its own binary. It takes
five samples per operation and refuses a drift between them. Each sample
counts allocations, reallocations, and allocated bytes through construction
of the owned output. Output destruction occurs after the counter snapshot.
Source loading, validation, and reporting do not enter these regions.
The runtime executable uses the normal system allocator.

These are concrete schema and API choices, not optimal implementations of each
format. The corpus is synthetic. Host load can affect timing. Results do not
establish server throughput, cross-language costs, or a new encoding adoption.
Issues #771 and #772 own those separate gates.

## Generated bindings

The checked-in bindings make benchmark builds independent of schema compilers.
`flatc` 25.12.19 generated the FlatBuffers binding. The Cap'n Proto compiler
0.9.2 and `capnpc-rust` 0.27.0 generated the Cap'n Proto binding. `Cargo.lock`
pins the measured Rust implementations.

To regenerate, install those compiler versions and run from the repository
root:

```bash
flatc --rust -o benches/encoding/generated benches/encoding/schema/game.fbs
cargo install capnpc --version 0.27.0 --locked
capnp compile -orust:benches/encoding/generated --src-prefix=benches/encoding/schema benches/encoding/schema/game.capnp
cargo fmt --manifest-path benches/encoding/Cargo.toml
```

Only the FlatBuffers generated module permits unsafe code. The adapters use
its verified root API. The server and handwritten benchmark adapters retain
their existing code policies.

## Recorded baseline

The [2026-10-14 ARM baseline](results/2026-10-14-aarch64/README.md) records all
36 codec/fixture cells and all 72 encode/decode operations. It includes sizes,
Criterion confidence intervals, allocation counts, source fingerprints, and
host limits. Use its source manifest when comparing a later run.
