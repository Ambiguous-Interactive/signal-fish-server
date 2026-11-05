# Source-linked verification pilot

Issue [#1464](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464)
asks whether Rust can be translated into theorem provers without maintaining a
second implementation. This pilot checks two private production helpers in
[`src/protocol_core.rs`](../../src/protocol_core.rs).

The standalone Lean and Kani crates import that file with `#[path]`. Production
version negotiation and session capability aggregation call the same helpers.
The proof crates do not change the server's dependencies or public API.

## Contracts and boundaries

| Backend and target | Checked property | Boundary |
| --- | --- | --- |
| Lean negotiation | Explicit requests respect both ceilings and preserve values below the server ceiling; ordered server bounds give the correct `None` default and preserve below-floor requests | All `u16` inputs; configuration validation and handshake refusal remain outside |
| Lean aggregation | No accepted proof | Extraction emits a missing iterator alias in the pinned Lean library; generated code does not typecheck |
| Kani negotiation | Both ceilings, exact selection, the `None` default, and preservation of below-floor requests | All `u16` inputs; default/refusal harness assumes ordered server bounds |
| Kani aggregation | A nonempty boolean list passes exactly when every member is true | Length zero through eight, unwind bound ten; no proof for larger lists or the production capability projection |

Lean extraction uses the actual Rust function bodies. Lean proofs must refer to
those generated definitions. Kani harnesses call the imported Rust helpers.
Neither workflow verifies Tokio scheduling, WebSockets, serialization, or the
entire server. Adapter tests exercise production callers, but are not a proof
of whole-program refinement.

The trusted boundary includes the Rust frontend, extractor or verifier, and
models of Rust library behavior. A kernel-checked Lean theorem does not itself
prove the extractor correct. Review the properties and external models when
updating tools. Retain TLA+, Z3, property tests, and trace validation for their
existing scopes.

## Reproduce

Run the pilot tools separately from the stable production build. Python 3.11+
is required by the runners. Use the pinned backend versions; do not substitute
rolling toolchains.

```bash
# Lean extraction and proof checks, including deliberate source faults.
cargo binstall --no-confirm --version 0.4.2 --strategies crate-meta-data cargo-hax
python3 formal/source-linked/lean/verify.py --prepare --mutations

# Kani source checks, including deliberate source faults.
cargo install --locked kani-verifier --version 0.68.0
cargo kani setup
python3 formal/source-linked/kani/verify.py --mutations
```

Lean also needs [elan 4.2.4](https://github.com/leanprover/elan/releases/tag/v4.2.4)
for the host platform, with `~/.elan/bin` on `PATH`. The scenario
`lean-toolchain` files select Lean
4.31.0. `lean/hax.toml` pins Charon `nightly-2026.09.02`, Aeneas
`build-183e4f0`, and hax's Lean models `v0.3.30`. hax downloads its extraction
tools when first used. Kani 0.68.0 supplies its own compiler toolchain.

The `--prepare` step fetches the pinned Lean dependencies and the Mathlib cache
modules used by Aeneas and hax. It avoids fetching the entire Mathlib cache.

The root Rust toolchain remains unchanged. Proof packages are unpublished,
isolated Cargo workspaces. Local proof-tool installation is opt-in; CI uses
the same runner commands.

## Initial source checks

The stable Rust checks cover 196,611 negotiation inputs: `None` and every
`u16` client value for the three supported deployment ranges. Aggregation
checks enumerate all 511 boolean lists through eight members. Separate
production adapter tests cover both member orderings, absent capability axes,
v2 members, v3 members, future version values, and empty rooms.

Temporary lower-clamp, ignored-ceiling, and empty-room mutations each fail the
corresponding checks. The restored source passes. These finite Rust checks are
regression evidence; they are not Lean or Kani proof results.

## Measured Kani result

Kani 0.68.0 passed all three harnesses and all five required reachability
witnesses. The negotiation harnesses cover all `u16` values. The collection
harness covers boolean slices of length zero through eight, with unwind bound
ten. This bound does not prove the collection property for longer rooms.

| Run | Wall time | Result |
| --- | ---: | --- |
| Baseline with installed tools | 4.481 s | Three harnesses passed |
| Warm repeat | 4.230 s | Three harnesses passed |
| Lower-clamp mutation | 0.365 s | Client-ceiling assertion failed |
| Reversed-ceiling mutation | 0.366 s | Server-ceiling assertion failed |
| Empty-room mutation | 3.025 s | Nonempty assertion failed |
| Skipped-member mutation | 3.829 s | Every-member assertion failed |
| Restored baseline | 4.128 s | Three harnesses passed |

Measurements include each runner's verifier command, not installation. They
come from an ARM64 Ubuntu 24.04 container on the development host, not a hosted
CI runner. The Debian 12 host could not run the backend because it needs glibc
2.39. CI selects Ubuntu 24.04. Repeat measurements on the intended deployment
of the proof tools before using them as a capacity estimate.

Cold installation was not measured as a clean run. The host setup required
a separate container after the glibc failure. These numbers measure cached-tool
verification only.

The runner writes raw reports, logs, source hash, and timings to
`target/source-linked/kani/`. It requires exact harness and witness names,
requires all nine named contract assertions to be reachable and proved, rejects
unrelated negative-control failures, and confirms that the production source
and proof lockfile did not change. The formal workflow runs the same command and retains
the evidence. This closes source drift for the selected helper contracts;
the adapter and async boundaries above remain.

### Hosted Kani sample

The first [hosted Kani job](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/42091475014/job/127470333451)
on commit `9d83c78f65da9c575f69c8fbf335440912a94202` passed on Ubuntu 24.04
x86-64 with a backend cache miss. Installation and setup took 17.525 seconds.
The source-check baseline took 3.271 seconds; its warm repeat took 3.921 seconds.
The full baseline, repeat, four mutations, and restored check took 17.481 seconds.
The job took about 55 seconds, including evidence upload and cache storage.
This is one hosted sample, not a latency percentile or capacity guarantee.

## Measured Lean result

hax `0.4.2` extracted the production negotiation helper. Lean `4.31.0` accepted
all three named contracts over `u16` inputs. Their transitive axiom reports
contain only `propext` and `Quot.sound`; no custom axiom or `sorryAx` is accepted.
The two deliberate source faults fail the proof. Separate kernel-checked
counterexamples show that each extracted mutant returns three where the
contract requires two. Restoring the source restores the proof.

| Run | Wall time | Result |
| --- | ---: | --- |
| Warm extraction, build, and axiom audit | 11.318 s | Three contracts accepted |
| Lower-clamp mutation | 22.572 s | Proof rejected; wrong result checked |
| Ignored-ceiling mutation | 22.356 s | Proof rejected; wrong result checked |
| Restored extraction, build, and audit | 11.019 s | Three contracts accepted |
| Full sequence, including version check | 67.341 s | Source and dependency pins unchanged |

These measurements use installed tools and cached dependencies on the ARM64
development host. Lean dependencies were moved from the workspace mount to
local container storage. They are not hosted-runner or clean-install timings.
An initial full Mathlib cache attempt was stopped after 12 minutes 24 seconds.
The runner now requests only the modules used by the pinned dependencies.
The first dependency/proof build after cache preparation took about 166 seconds.

Raw command logs and timings are in
`lean/target/verification-logs/`. CI runs the same runner, retains those logs,
and rejects stale or untracked generated proof files. The runner checks that
source, Cargo lockfile, Lean manifests, and tool pins remain unchanged.

### Hosted Lean sample

The first completed [hosted Lean job](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/42092218396/job/127473309080)
on commit `240484e33808eb2cd06a3e36da509eea069a5290` passed on Ubuntu 24.04
x86-64 with a dependency cache miss. The preparation, proof, mutation, and
restored-check command took 548.94 seconds, including 386.257 seconds for the
first dependency/proof build. The saved cache occupied 2,490,819,057 compressed
bytes (about 2.49 GB).

Within that same run, the restored extraction took 0.917 seconds, its build
1.117 seconds, and its axiom audit 2.520 seconds. These are individual command
times, not a measurement of a complete warm job. The
[retained evidence](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/42092218396/artifacts/13446731301)
records both rejected mutations and the restored proof. This is one cold
sample, not a latency percentile or capacity guarantee.

A subsequent [hosted Lean job](https://github.com/Ambiguous-Interactive/signal-fish-server/actions/runs/42094949991/job/127482419849)
on commit `f57a777f714be69cecabfc4c17631615bdb9c447` passed with a cache hit.
The complete job took 171 seconds, including 70 seconds restoring the cache
and 79.55 seconds running preparation, proofs, mutations, and the restored
check. This is one warm-cache sample, not a latency percentile or capacity
guarantee.

## Decision

Keep the two source-linked CI checks for the stated contracts. The measured
warm checks fit the five-minute pilot target. Keep all existing verification
layers. Do not expand this into a translator project or a whole-server proof.

Kani gives useful bounded aggregation coverage now. Lean establishes that
source extraction and kernel-checked negotiation proofs can run together.
The generic collection experiment also found a concrete tool limitation.
Further Lean collection work needs a separate estimate after that limit is
resolved. No server runtime dependency or wire-format change is required.

## Aggregation limitation and follow-up

hax extracts the generic aggregation helper, but the generated module refers
to a missing constant in hax-lean `v0.3.30`:
`CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all`.
This is a generated-module typecheck failure. It prevents acceptance of even
the proposed empty-list Lean theorem. The pilot records it as a tool limit,
not as a proof result. Reproduce the optional probe after preparing the tools:

```bash
python3 formal/source-linked/lean/verify.py --prepare --probe-capability
```

The required CI job does not depend on preserving this tool failure. The probe
also reports when a later pinned library can typecheck the extraction; that
outcome alone still proves no aggregation theorem.

Fixing that library mapping is the first step. A full arbitrary-length Lean
equivalence would then need an explicit callback contract and a proof over the
iterator loop. The generic callback can mutate state, fail, or diverge in the
extracted semantics. Keep the production adapter tests and the bounded Kani
check while evaluating that additional work.

## Tool references

- [hax Lean workflow](https://hax.cryspen.com/manual/lean/quick_start/)
- [hax tool and scenario pins](https://hax.cryspen.com/manual/tools/)
- [Kani loop bounds](https://model-checking.github.io/kani/tutorial-loop-unwinding.html)
- [Kani feature limits](https://model-checking.github.io/kani/rust-feature-support.html)
