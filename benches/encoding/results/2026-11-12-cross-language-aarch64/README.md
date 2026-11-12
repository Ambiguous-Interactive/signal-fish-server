# Encoding order controls — 2026-11-12

Existing [#1524](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1524)
requires codec order control before relative-speed claims. These fresh records
use the same corpus and dependency locks as the unchanged
[November 9 record](../2026-11-09-cross-language-aarch64/README.md).

TypeScript and GDScript alternate codec order in every measured sample.
Seeds 1524 and 1525 reverse each fixture's starting order and warmup order.
Each codec runs first five times and second five times per operation and seed.
Each operation keeps its one-second warmup, calibrated iteration count, ten raw
samples, executed order, and checksum. Exact values, float bits, wire agreement,
and the original six negative controls remain required. Nine additional negative
controls reject missing or inconsistent order and warmup evidence.

Both twenty-cell records pass all eight Rust reference wires and all fifteen
negative controls. Independent reconciliation also confirms that the three raw
harness streams equal each saved aggregate and that every paired cohort has
five samples. No attempt failed. No Rust build or test workload ran during these
measurements. The development host was shared; other workload was not excluded.

Host: Linux aarch64; Node.js 24.21.0, .NET SDK 10.0.402, Godot 4.7.2.
Each seed's environment file records the actual runtime versions, host limits,
corpus identity, and every harness source and dependency-lock hash.

## Observed order cohorts

The table gives the range of second-position median divided by first-position
median across all four fixtures and both seeds. Values below one mean a lower
observed time in the second position. These are within-codec observations;
they do not compare codec speeds or establish a causal cache or thermal effect.
The [full cohort table](order-cohorts.csv) retains all 64 encode/decode comparisons.

| Language | Codec | Operation | Second / first median range |
| --- | --- | --- | ---: |
| typescript | msgpack-all-f64 | encode | 0.910–1.012 |
| typescript | msgpack-all-f64 | decode | 0.962–1.020 |
| typescript | protobuf | encode | 0.888–1.276 |
| typescript | protobuf | decode | 0.963–1.011 |
| gdscript | msgpack-f32 | encode | 0.972–1.043 |
| gdscript | msgpack-f32 | decode | 0.978–1.046 |
| gdscript | schema-varint | encode | 0.985–1.102 |
| gdscript | schema-varint | decode | 0.966–1.038 |

The range shows position-dependent variation on this host. Ten samples per
operation and a shared host do not identify its cause or establish a stable
relative-speed ranking. Keep both positions and both starting orders in future
comparative decisions. Do not interpret these records as a speed improvement,
network measurement, or server-capacity result.

## C# baseline

C# measures only protobuf, so there is no competing codec order to alternate.
It records the explicit `single-codec` method, null seed, and position zero for
all samples. Its encode and decode series remain separate. Warmup durations,
counts, raw timings, exact values and float bits are retained in both runs.
This checks the baseline; it cannot establish an order effect or a ranking
against another C# codec or a different runtime.

## Artifacts

- Seed 1524: [raw samples](seed-1524/samples.json), [summary](seed-1524/summary.csv),
  [environment and sources](seed-1524/environment.json), [Rust wires](seed-1524/rust-wire.json).
- Seed 1525: [raw samples](seed-1525/samples.json), [summary](seed-1525/summary.csv),
  [environment and sources](seed-1525/environment.json), [Rust wires](seed-1525/rust-wire.json).

Each seed directory also retains the original TypeScript, C#, and Godot JSONL
streams and process diagnostics. The hosted workflow verifies both complete
records against the current source hashes and summary values. Historical
records remain unchanged. See the [method](../../CROSS_LANGUAGE.md) for limits.
