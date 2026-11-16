# Encoding dependency qualification — 2026-11-16

Existing [PR #1653](https://github.com/Ambiguous-Interactive/signal-fish-server/pull/1653)
updates the standalone Rust reference graph from prost/prost-derive 0.14.4 to
0.14.6. Existing [#1111](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1111)
records this qualification. The server does not link this study package.

## Experiment and result

The original hosted cross-language job refused the changed lockfile because
the November 12 records fingerprinted 0.14.4. A matched local control changes
only the lockfile: both old records pass with the baseline lock and reject
the candidate lock. Every other recorded source hash matches. The rejection
is a valid provenance guard, not evidence of a codec disagreement.

These fresh records use the candidate lock and the unchanged frozen corpus,
harnesses, schemas, generated bindings, and order method. Seeds 1524 and 1525
reverse the starting and warmup order. Each paired codec runs first five times
and second five times per operation. Each operation retains its one-second
warmup, calibrated iteration count, ten raw samples, positions, and checksum.
C# retains its separate single-codec method and null seed.

Both twenty-cell records pass all eight Rust reference wires and all fifteen
value/order negative controls. The Rust exports also equal the November 12
exports byte for byte. The independent cohort verifier reconciles all three
raw process streams with each aggregate, recomputes 64 paired comparisons,
and passes six changed/missing-cohort controls. Saved sources and CSV summaries
match the raw records. No collection attempt failed or was dropped.

Host: Linux aarch64; Node.js 24.21.0, .NET SDK 10.0.402, Godot 4.7.2 and
Rust 1.94.0. Rust builds and corpus tests completed before collection; the
runner exports Rust wires after the timed language processes. No concurrent
Rust build or test workload ran during measurements. The host remained
shared; other workload was not excluded. Each environment file records actual
versions, host limits, corpus identity and all measured source/lock hashes.

The workflow now reconciles these current-source records. The source guard,
measurement method, all assertions and negative controls remain unchanged.
The [November 12 records](../2026-11-12-cross-language-aarch64/README.md)
remain unchanged evidence for their original lockfile.

## Observed order cohorts

The table gives second-position median divided by first-position median,
ranged across four fixtures and both seeds. Values below one mean a lower
observed time in the second position. The [full table](order-cohorts.csv)
retains all 64 comparisons. These are within-codec observations; they do not
rank codecs or establish a causal cache, runtime or thermal effect.

| Language | Codec | Operation | Second / first median range |
| --- | --- | --- | ---: |
| typescript | msgpack-all-f64 | encode | 0.909–1.204 |
| typescript | msgpack-all-f64 | decode | 0.599–1.016 |
| typescript | protobuf | encode | 0.891–1.181 |
| typescript | protobuf | decode | 0.970–1.074 |
| gdscript | msgpack-f32 | encode | 0.994–1.046 |
| gdscript | msgpack-f32 | decode | 0.969–1.028 |
| gdscript | schema-varint | encode | 0.972–1.032 |
| gdscript | schema-varint | decode | 0.935–1.049 |

Keep all positions and both starting orders in later comparisons. Shared-host
variation and ten samples do not establish a stable ranking. This refresh
qualifies the dependency and current records; it claims no speed improvement,
server-capacity result or encoding adoption.

## Artifacts

- Seed 1524: [samples](seed-1524/samples.json), [summary](seed-1524/summary.csv),
  [environment and sources](seed-1524/environment.json), [Rust wires](seed-1524/rust-wire.json).
- Seed 1525: [samples](seed-1525/samples.json), [summary](seed-1525/summary.csv),
  [environment and sources](seed-1525/environment.json), [Rust wires](seed-1525/rust-wire.json).

Each directory also retains the original TypeScript, C# and Godot JSONL streams
and process diagnostics. See the [method](../../CROSS_LANGUAGE.md) and
[independent verifier](../../verify_order_cohorts.py).
