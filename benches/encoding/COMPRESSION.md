# Relay compression decision

Reject permessage-deflate adoption on the current server stack for
[#773](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/773).
The measured payload savings merit a future supported-transport experiment.
They do not justify a new WebSocket implementation in this session.
The server keeps its current wire behavior and dependencies.

## Current transport boundary

The root lockfile pins Axum 0.8.9 and its Tungstenite 0.29.0 transport.
That transport has no permessage-deflate support, as its
[versioned README](https://github.com/snapview/tungstenite-rs/blob/v0.29.0/README.md)
states. Its `WebSocketConfig` has no compression setting, and its
[frame parser](https://github.com/snapview/tungstenite-rs/blob/v0.29.0/src/protocol/mod.rs)
rejects nonzero reserved bits. Adding an upgrade response header cannot add
the required framing, decompression, or negotiated connection state.
Runtime adoption therefore requires a transport change, followed by real
upgrade, mixed-client, fragmentation, and connection-lifecycle qualification.
This study does not implement or claim that qualification.

## Reproduce

Run from the repository root:

```bash
cargo bench --locked --bench relay_compression_frames --features allocation-tracking -- target/relay-compression-corpus
python3 scripts/measure-relay-compression.py --self-test
python3 scripts/measure-relay-compression.py target/relay-compression-corpus --output target/relay-compression.json
```

The exporter also accepts `--test` in place of the directory. It checks every
captured production frame against the existing exact-byte ledger without
writing a corpus. This is a benchmark tool, not a server configuration option.

## Measurement contract

The exporter captures the existing production relay fixture after fan-out and
socket materialization. It preserves all six scenarios, 2/8/16-player rooms,
1,024 sends, recipient identities, and accounted unsupported-format omissions.
It checks the existing frozen SHA-256 wire digests. The Python reader separately
checks corpus digests, counts, lengths, opcodes, and wire digests.

The isolated prototype uses Python's zlib raw DEFLATE with a 15-bit window,
levels 1 and 6, and one context per recipient. It compares context takeover
with a fresh context per message. It removes and restores each sync-flush tail
as specified in [RFC 7692](https://www.rfc-editor.org/rfc/rfc7692).
Every measured variant must restore exact bytes, order, recipients, and opcodes.
Controls reject changed corpus bytes and ledgers, and prove that takeover needs
its own connection history. A fresh connection starts a fresh context.

Byte totals include the unmasked server frame header for one unfragmented
message. They exclude TCP, TLS, and upgrade bytes. CPU samples use process CPU
time, nine repeats after warm-up, and include context creation, transformation,
and returned Python containers. Decode samples exclude equality checks.
The identity-loop samples retain the Python iteration baseline.
These are added transform costs. They are not Rust writer or server throughput
measurements. The prototype adds no server runtime dependency.

## Observations

The retained ARM64 Linux run uses Python 3.11 and zlib 1.2.13.
The full raw samples, source identity, exporter identity, and corpus digests
are in [the result file](results/2026-11-12-aarch64-compression/measurement.json).
Source hashes describe the measurement-time checkout. The exporter binary hash
and frozen wire digests identify the measured frames independently.
The following table uses level 1 and 16-player rooms. CPU is median compression
time per recipient frame in microseconds.

| Lane | Saved without takeover | CPU | Saved with takeover | CPU |
| --- | ---: | ---: | ---: | ---: |
| V3 JSON text | 84.47% | 6.73 | 97.46% | 3.15 |
| V3 MessagePack binary | 85.22% | 6.90 | 97.43% | 3.51 |

The same identity loop costs 0.10 microseconds per frame. Takeover decompression
costs 1.10 microseconds for JSON and 1.33 for MessagePack.

The production fixture repeats an 896-byte string and stable gameplay fields.
Its high savings do not represent arbitrary game payloads. Separate deterministic
6-, 96-, and 1,024-byte opaque controls measure a different boundary: distinct
bytes on each message. The result file keeps those controls separate from
production-derived frames. They prevent a claim that every payload shrinks.

| Distinct opaque bytes per frame | Size increase with takeover | CPU |
| --- | ---: | ---: |
| 6 | 27.88% | 2.30 |
| 96 | 6.12% | 8.48 |
| 1,024 | 0.58% | 44.96 |

## Decision limits

This completes a measured reject decision for the current stack. It does not
reject DEFLATE for every deployment or establish live extension negotiation.
Revisit adoption when a supported transport exposes the extension. Keep it
opt-in and measure realistic payload entropy, per-connection memory, mixed
compressed/plain peers, CPU, and end-to-end latency before shipping it.
