# Gameplay encoding adoption decision

Decision for [#772](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/772):
do not add a new game-data format from this study. Keep the existing catalog.
The completed measurements do not qualify any candidate under the requested gate.
This decision applies to the frozen corpus and measured implementations.
It does not rule out a future codec or a different implementation.

The gate requires a wire-size saving of at least 20 percent or a client CPU
gain of at least 2x across the whole corpus. It also requires maintained
implementations in Rust, GDScript, C#, and web, with no new server runtime
dependencies. A larger-fixture gain alone cannot qualify a candidate.

## Whole-corpus counterexample

The [Rust baseline](results/2026-10-14-aarch64/README.md) measures the same
owned gameplay model for every codec. Its `input-state` fixture is one of
the four required shapes. Named MessagePack uses 6 bytes, encodes in
45.45 ns, and decodes in 48.74 ns. Compact MessagePack also uses 6 bytes.

| Candidate | Input bytes | Encode gain | Decode gain | Encode plus decode gain |
| --- | ---: | ---: | ---: | ---: |
| Protobuf (`prost`) | 10 | 0.93x | 1.25x | 1.07x |
| CBOR (`ciborium`) | 8 | 0.62x | 0.19x | 0.28x |
| CBOR (`cbor4ii`) | 8 | 1.47x | 0.76x | 0.99x |
| FlatBuffers | 36 | 0.20x | 1.19x | 0.36x |
| Cap'n Proto, unpacked | 32 | 0.26x | 0.94x | 0.41x |
| Cap'n Proto, packed | 13 | 0.16x | 0.44x | 0.23x |
| Schema varint | 6 | 1.22x | 0.98x | 1.08x |

Gain is the named-MessagePack mean divided by the candidate mean.
The last column divides the sums of encode and decode means. It is a
derived comparison, not a separate timed operation. Values below 1x
are slower. These values come from the retained
[CSV](results/2026-10-14-aarch64/summary.csv), rounded to two decimal places.
No candidate saves bytes in this cell. No candidate reaches 2x on either
operation or their sum. The retained confidence intervals also remain below
2x when the comparison uses the upper baseline and lower candidate limits.
Thus no measured candidate establishes the requested whole-corpus win for
the measured Rust implementation. Small differences on a shared host do not establish rankings.

## Other client boundaries

The [client-language study](CROSS_LANGUAGE.md) retains twenty cells and
four hundred samples. Its recorded-output verifier passes for the checked-in
data. Protobuf reduces TypeScript wire size in all four measured shapes,
but that baseline writes every number as float64 to preserve negative zero.
It is not the Rust MessagePack layout. Protobuf encode and decode are slower
than that TypeScript baseline on all three larger shapes. C# has no measured
MessagePack comparison, so its protobuf times do not establish a CPU gain.

The GDScript schema-varint adapter is measurement code, not a maintained
general codec. Its MessagePack baseline loses float64 precision outside the
exactly representable frozen inputs. Its timing gains do not establish a
drop-in implementation with the required numeric contract. The study does
not supply maintained implementations in every required language for each
candidate. The prior catalog assessment in
[#679](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/679)
also records the GDScript availability gaps.

## Result

Close this adoption decision with no new format. Do not change v2/v3 tokens,
capabilities, mixed-room enforcement, defaults, or server dependencies.
The independent catalog task
[#678](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/678)
and optional compression experiment
[#773](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/773)
remain separate. New evidence must satisfy the same gate before reopening
an adoption proposal.
