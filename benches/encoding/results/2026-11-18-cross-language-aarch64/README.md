# Encoding dependency qualification — 2026-11-18

The standalone study uses Cap'n Proto 0.28.0 and bindings generated
with `capnpc-rust` 0.28.1. The prior binding failed to compile with
0.28.0 because it used capability-table types that the new version removed.
The corpus, schemas, language harnesses, and order method stay fixed.

Each seed passed 20 language cells, eight Rust reference wires, and
15 negative controls. The Rust wire exports match the November 16
exports byte for byte. The independent verifier reconciles 64 order
comparisons with the three raw language streams and passes six
negative controls. Five Rust fixture tests cover all frozen values,
floating-point bits, and malformed input.

Linux ARM64 ran Node.js 24.21.0, .NET SDK 10.0.402, Godot 4.7.2, and
Rust 1.94.0. Server validation ran on the same host during collection.
Use these samples for source and wire qualification. The timings do
not establish a speed change or server capacity.

- Seed 1524 [samples](seed-1524/samples.json), [summary](seed-1524/summary.csv),
  [source manifest](seed-1524/environment.json), and [Rust wires](seed-1524/rust-wire.json).
- Seed 1525 [samples](seed-1525/samples.json), [summary](seed-1525/summary.csv),
  [source manifest](seed-1525/environment.json), and [Rust wires](seed-1525/rust-wire.json).
- [Order comparisons](order-cohorts.csv).
