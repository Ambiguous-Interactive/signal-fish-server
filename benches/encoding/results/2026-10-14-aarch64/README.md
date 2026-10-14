# ARM codec baseline, 2026-10-14

Corpus version 1. Rust 1.91.0. Shared Linux aarch64 devcontainer, 12 visible
logical CPUs, no CPU quota or memory limit. The process was not pinned.
These observations do not establish deployed server capacity.

[Method and commands](../../README.md), [source and host evidence](environment.json),
[full allocation data](allocations.csv), [timing and size data](summary.csv),
and [original Criterion estimates](criterion-estimates.json).

The source fingerprint covers the benchmark code, generated bindings, schemas,
manifest, and lockfile. Each allocation operation count includes allocations
and reallocations. The table shows Criterion mean time in nanoseconds.
The CSV retains each mean's 95 percent confidence interval.

Packed Cap'n Proto uses fewer bytes than compact MessagePack for the three
larger fixtures. It uses more bytes for input state. These concrete layouts
therefore do not prove a wire-size win across the whole corpus. Cross-language
costs and implementation availability remain separate gates in #771 and #772.

## input-state

| Codec | Bytes | Encode ns | Decode ns | Encode ops | Decode ops |
| --- | ---: | ---: | ---: | ---: | ---: |
| messagepack-named | 6 | 45.4 | 48.7 | 1 | 0 |
| messagepack-compact | 6 | 45.3 | 53.1 | 1 | 0 |
| prost | 10 | 48.8 | 39.1 | 1 | 0 |
| ciborium | 8 | 73.7 | 257.1 | 1 | 0 |
| cbor4ii | 8 | 30.9 | 63.8 | 1 | 0 |
| flatbuffers | 36 | 223.9 | 41.1 | 7 | 0 |
| capnp-unpacked | 32 | 176.3 | 51.9 | 3 | 1 |
| capnp-packed | 13 | 291.5 | 110.9 | 4 | 2 |
| schema-varint | 6 | 37.3 | 49.7 | 1 | 0 |

## snapshot-small

| Codec | Bytes | Encode ns | Decode ns | Encode ops | Decode ops |
| --- | ---: | ---: | ---: | ---: | ---: |
| messagepack-named | 237 | 539.2 | 660.7 | 6 | 6 |
| messagepack-compact | 139 | 434.1 | 499.8 | 6 | 6 |
| prost | 139 | 143.1 | 296.5 | 1 | 6 |
| ciborium | 170 | 701.7 | 1568.5 | 6 | 6 |
| cbor4ii | 242 | 259.8 | 434.4 | 6 | 6 |
| flatbuffers | 272 | 523.7 | 410.9 | 12 | 6 |
| capnp-unpacked | 224 | 233.1 | 316.7 | 3 | 7 |
| capnp-packed | 93 | 565.1 | 555.6 | 6 | 8 |
| schema-varint | 124 | 156.9 | 207.0 | 5 | 6 |

## snapshot-large

| Codec | Bytes | Encode ns | Decode ns | Encode ops | Decode ops |
| --- | ---: | ---: | ---: | ---: | ---: |
| messagepack-named | 843 | 1360.8 | 2089.7 | 8 | 18 |
| messagepack-compact | 505 | 908.7 | 1515.7 | 7 | 18 |
| prost | 507 | 575.4 | 1083.8 | 1 | 19 |
| ciborium | 572 | 1869.7 | 4795.3 | 8 | 18 |
| cbor4ii | 860 | 778.0 | 1295.8 | 8 | 18 |
| flatbuffers | 824 | 791.7 | 1115.0 | 13 | 18 |
| capnp-unpacked | 704 | 320.2 | 731.1 | 3 | 19 |
| capnp-packed | 280 | 827.7 | 1469.2 | 7 | 20 |
| schema-varint | 448 | 241.6 | 522.4 | 7 | 18 |

## world-delta

| Codec | Bytes | Encode ns | Decode ns | Encode ops | Decode ops |
| --- | ---: | ---: | ---: | ---: | ---: |
| messagepack-named | 3752 | 3931.1 | 6830.1 | 10 | 64 |
| messagepack-compact | 2021 | 2598.3 | 5955.6 | 9 | 64 |
| prost | 2147 | 3277.3 | 4879.7 | 1 | 71 |
| ciborium | 2769 | 7634.9 | 23531.1 | 10 | 64 |
| cbor4ii | 3849 | 2670.0 | 6864.9 | 10 | 64 |
| flatbuffers | 3504 | 3509.1 | 6005.4 | 18 | 64 |
| capnp-unpacked | 3312 | 875.7 | 3811.1 | 3 | 65 |
| capnp-packed | 1218 | 2790.2 | 7479.7 | 9 | 66 |
| schema-varint | 1755 | 925.4 | 2967.3 | 9 | 64 |
