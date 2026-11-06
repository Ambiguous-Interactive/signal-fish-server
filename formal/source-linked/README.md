# Source-linked verification pilot

Issue [#1464](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464)
asks whether Rust can be translated into theorem provers without maintaining a
second implementation. The source-linked checks use production helpers in
[`src/protocol_core.rs`](../../src/protocol_core.rs) and
[`src/session_capability.rs`](../../src/session_capability.rs).

The standalone Lean crate imports both files with `#[path]`. The Kani crate
imports `protocol_core.rs`. Production version negotiation, capability projection,
support checks, and aggregation call the same helpers. The proof crates do not
change the server's dependencies or public API.

## Contracts and boundaries

| Backend and target | Checked property | Boundary |
| --- | --- | --- |
| Lean negotiation | Explicit requests respect both ceilings and preserve values below the server ceiling; ordered server bounds give the correct `None` default and preserve below-floor requests | All `u16` inputs; configuration validation and handshake refusal remain outside |
| Lean aggregation result | Empty slices return false; every representable finite nonempty slice returns the conjunction of member predicates under the total callback contract below | Callback must terminate successfully and preserve its invariant; the concrete support callback is covered separately below |
| Lean callback semantics | For every representable finite slice and modeled callback, the production result agrees with the ordered callback-step relation, including state-dependent results, errors, and divergence | The model threads private state but discards final state; external effects and arbitrary Rust callbacks remain outside; the concrete support callback is covered below |
| Lean capability projection | Preserve version and all six enum membership bits for every representable capability slice; session support requires version >= 3 and both requested axes | Actual production enums and fields are extracted; connection lookup, default fallback, member getter, and room-state correspondence remain outside |
| Lean concrete capability aggregation | The extracted support callback requires version >= 3 and both requested axes for every member of every representable nonempty slice; empty input returns false | Requires a total, fixed capability getter; production member-field getter, connection lookup, defaults, and room state remain tested boundaries |
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
modules used by Aeneas and hax. All three proof projects share those prepared
dependencies after the runner checks their pins. It avoids fetching the entire
Mathlib cache.

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
The alias backport below resolves that extraction limit. The aggregation proof
adds no server runtime dependency or wire-format change.

## Aggregation mapping and follow-up

The pinned baseline extracts the generic aggregation helper but fails to typecheck
its reference to
`CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all`.
The required Lean job now reruns this source-based capability check:

```bash
python3 formal/source-linked/lean/verify.py --prepare --mutations --probe-capability
```

The runner first tries the unmodified pinned library. It accepts a successful
baseline typecheck. It retries only the known missing-alias failure, using
[`compatibility/IteratorAll.lean`](lean/compatibility/IteratorAll.lean).
That module backports the concrete slice alias from
[hax-lean v0.3.41](https://github.com/cryspen/hax-lean/blob/1d84de68c03bd76457887121853779a52a09e33a/CoreModels/Core/FunsEpilogue.lean#L450).
It delegates to the pinned library's existing `Iterator.all.default` and concrete
slice iterator instance. It adds no loop, axiom, callback model, or replacement
for the extracted production function. Its Apache-2.0 license is retained beside it.

Upstream v0.3.31 and v0.3.32 retain Lean 4.31 but lack this alias. The inspected
v0.3.41 also moves to Lean 4.35 and a different Aeneas revision. The narrow alias
backport leaves all pilot tool and dependency pins unchanged. Its namespace
wrapper and fixed `Type` universe adapt the alias to the pinned model. Remove
it only after a separately reviewed tool update typechecks without the shim.

The runner imports the alias through hax's user-owned assumptions module. It
never edits the extracted function body. It repeats extraction, compares all
generated extraction files byte for byte, and requires a second typecheck.
An unrelated compiler failure or a failed mapped typecheck fails CI. CI does
not require the baseline defect to remain broken.

Each probe retains small source and generated-file snapshots, pin and source
hashes, raw command logs, results, and timings under
`lean/target/verification-logs/capability-probe/`. A baseline failure and its
mapped retry have separate snapshots. Temporary projects and dependency caches
are not needed to inspect the evidence. The snapshot retains the original
private production module imported by the isolated one-function extraction
scenario; it does not introduce a second Rust implementation.

A local ARM64 follow-up run kept the pinned production source and dependencies
unchanged. The full negotiation/mutation/restored/probe command took 157.200
seconds across recorded commands. The baseline capability typecheck failed in
22.894 seconds. The mapped typecheck passed in 12.941 seconds; repeat extraction
took 1.136 seconds and repeat typecheck took 1.736 seconds. These are single
warm-dependency samples, not hosted CI or proof-maintenance estimates.

### Aggregation callback contract

The aggregation theorem checks the extracted helper and the pinned iterator
loop. Its slice domain has no testing bound of eight. It covers every finite
slice representable by the model (`length <= Usize.max`).

The named contracts are `Capability.empty_contract` and
`Capability.all_members_contract`. `Capability.boolean_identity_contract`
specializes the result to boolean members. `Capability.nonempty_outcomes` checks
that both nonempty outcomes are reachable. The aggregation axiom reports contain
only `propext`, `Classical.choice`, and `Quot.sound`; the gate rejects proof holes
and unapproved axioms.

The callback contract permits private state updates. Each callback call must
terminate successfully, return a fixed predicate of the current member, and
preserve a stated state invariant. The initial state must satisfy that invariant.
The empty-slice theorem needs no callback contract because the helper does not
call the callback.

Failing or diverging callbacks and state-dependent truth values fall outside
this total fixed-predicate contract. The operational contract below covers their
modeled results. Neither contract exposes final callback state or external effects.
The capability projection has the separate contract below. Concrete aggregation
callback and production caller/state correspondence remain pending. Keep the bounded Kani checks and adapter tests.

The required runner checks the result and callback-semantics theorems on every run. It
regenerates each scenario twice and rejects different output. Empty-room and
skipped-singleton faults must first extract and typecheck, then fail only the
contract proof. Separate axiom-audited witnesses establish each wrong result.
The restored production source must pass again. Hashed snapshots under
`lean/target/verification-logs/proofs/` retain source, pins, generated definitions,
contracts, raw logs, and timings. The optional typecheck probe remains separate;
its status does not report the required theorem results.

A development run on the ARM64 host used cached dependencies. Its 40 commands
took 199.439 seconds in total, including all four source faults, restored proofs, and the separate extraction
probe. The initial aggregation
extraction/repeat/build/audit took 12.907 seconds; the restored aggregation cycle
took 12.205 seconds. These are single command-time samples, not cold installation,
hosted CI, or whole-server proof costs.

### Stateful callback semantics

[`CallbackSemantics.lean`](lean/proofs/capability/lean/Capability/Verification/CallbackSemantics.lean)
defines `CallbackTrace`, a relation for callback steps. Each step records its
incoming state and member. A true result passes the next state to the next member.
False stops the relation. An error or divergence stops with that modeled result.
Exhaustion returns true for the iterator; the production helper returns false for
empty input.

`Capability.callback_trace_exists` proves that every finite list and modeled
callback has such a specification trace. `Capability.callback_trace_contract`
proves that the extracted production result agrees with its outcome for every
representable finite slice. These theorems require no fixed predicate, invariant,
or successful-termination assumption. They do not execute a second aggregation
implementation. The proof unfolds the pinned iterator loop.

`callback_order_outcomes` uses a callback that checks each member against its
counter, then increments the counter. It accepts `[0, 1]` and rejects `[1, 0]`.
`callback_failure_outcomes` checks errors and divergence after a true prefix,
and checks that false shields both suffixes. The required axiom audit covers
all four new theorems.

The extra-callback source fault invokes the first member before the iterator.
It preserves the mathematical total fixed-predicate result, but changes the
counter callback result. The gate requires generated-module compilation, rejection
of the callback-semantics contract, and an axiom-audited wrong-result witness,
then restores the source.

The pinned iterator model returns the boolean and remaining iterator. It drops
final callback state. The trace is a specification, not an exported observation
of Rust side effects. Callback errors and divergence are values in `RustM`; these
proofs do not establish how an arbitrary concrete Rust callback produces those values.
Allocator failure, process failure, panic recovery, and external state remain
outside this result correspondence. The concrete support callback below has a
separate contract with a total, fixed getter assumption.

### Production capability projection

The projection scenario extracts the actual `Transport`, `Topology`, and
`SessionCapabilities` definitions, their derived enum equality, and the production
`project_capabilities` and `supports_session` functions. Public enum paths remain
re-exported from `protocol::types`. Serde attributes and variants stay unchanged.
The isolated Lean crate uses the server's pinned Serde version. The production
crate gains no dependency.

`Projection.projection_contract` proves that conversion preserves the `u16`
version and all six membership bits for arbitrary representable slices. It permits
empty lists, duplicates, and any ordering. `support_contract` proves that all
versions below three fail, while every other version requires both selected axes.
It covers all nine topology/transport pairs and all capability field assignments.
`projected_support_contract` composes these two extracted functions without input
assumptions. `support_outcomes` checks acceptance, v2 refusal, and a missing
transport at the maximum `u16` version. These are kernel-checked contracts,
not bounded enumeration.

The production `From<&NegotiatedProtocol>` passes its stored vectors as slices to
the projection function. `SessionMember::supports_session` calls the extracted
support predicate. Existing adapter tests now check all nine pairs for every
negotiated subset, including duplicates, reverse order, versions zero through
four, and `u16::MAX`. Tests also check absent/disconnected profiles and member
ordering. Those caller, lookup, and membership relationships are tested; the Lean
composition theorem does not prove the server's entire call path or state.

The required runner regenerates this scenario twice, audits all four contracts,
and checks isolated version-floor and wrong-transport-membership source faults.
Each fault must compile extraction before its contract fails. Separate audited
witnesses establish the wrong values. The restored source must pass. Snapshots
and the final source guard cover both production input modules. The concrete
support callback has the separate aggregation contract below. Connection lookup,
defaults, the actual member getter, final callback state, external effects, room
decisions, and asynchronous correspondence remain unresolved in #1464.

A local ARM64 run with prepared dependencies completed 61 commands in 347.583
seconds. All 15 contracts, seven source faults, audited wrong-result witnesses,
restored proofs, regeneration, and both-source integrity checks passed. The first
projection extraction/repeat/build/audit cycle took 31.501 seconds; its restored
cycle took 11.135 seconds. Accepted checks emitted no warnings. Deliberately
rejected fault proofs can emit tactic diagnostics and warnings.

This expanded local cycle exceeds the original five-minute pilot target. It is
one cached-dependency sample, not a hosted warm-job or installation measurement.
Record hosted evidence separately and reassess cost before expanding more seams.

The reviewed [obligation inventory](obligations.md) tracks the production
seams, current evidence, negative controls, and caller/state/concurrency gaps.
All production source and root Cargo changes now rerun formal CI. Existing
handwritten models and test results remain distinct from source-linked proofs.

### Concrete capability aggregation

Production room selection now calls `session_capability::all_support`. This
helper passes its actual closure to `protocol_core::nonempty_all_support`.
The closure reads capabilities through a getter, then calls `supports_session`
with the requested topology and transport. It adds no intermediate collection.
Lean extracts both helpers and the closure from these production definitions.

`Capability.concrete_callback_contract` checks the extracted closure.
`concrete_aggregation_contract` connects it to the extracted iterator helper.
The contract requires a getter that returns a fixed `SessionCapabilities` value
for each member and terminates successfully. Under this explicit assumption,
the result is true exactly when the slice is nonempty and every member has
version at least three and both requested capability axes. The domain includes
all `u16` versions, all nine axis pairs, and every representable finite slice.
It permits duplicates and any member order.

The actual server getter is `|member| member.capabilities`. Its correspondence
to stored `SessionMember` fields remains tested. The theorem does not prove
connection lookup, profile fallback, construction of those fields, or room-state
transitions. `Fn` alone does not establish the getter contract: a Rust getter can
use interior mutability. No final callback state or external effect is claimed.
`concrete_aggregation_outcomes` checks empty input, duplicate accepting future
versions, a missing transport, and old-version rejection in both member orders.
The wrong-callback-transport fault substitutes Relay for requested Direct.
The runner requires extracted-module compilation, rejection in the concrete
contract file, and a separate audited wrong-result witness, then restored proofs.
A constant-Relay prototype removed a capture and changed the extracted closure
type. That interface failure did not count. The accepted fault preserves the
capture and changes only the Direct branch.

A local run with prepared dependencies passed the initial capability proof,
semantic fault, audited witness, and restored proof across 13 commands. The
mutated extraction built in 24.188 seconds; semantic rejection took 9.461
seconds; the witness took 3.290 seconds. Restored extraction, repeat extraction,
build, and audit took 2.133, 1.985, 2.138, and 7.307 seconds. These are single
local samples; hosted CI checks the full set of scenarios and source faults.

The projection theorem above proves the conversion from negotiated axis lists
separately; the server path from connection state to aggregation is not proved.

## Tool references

- [hax Lean workflow](https://hax.cryspen.com/manual/lean/quick_start/)
- [hax tool and scenario pins](https://hax.cryspen.com/manual/tools/)
- [Kani loop bounds](https://model-checking.github.io/kani/tutorial-loop-unwinding.html)
- [Kani feature limits](https://model-checking.github.io/kani/rust-feature-support.html)
