# Source-linked verification pilot

Issue [#1464](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1464)
asks whether Rust can be translated into theorem provers without maintaining a
second implementation. The source-linked checks use production helpers in
[`src/protocol_core.rs`](../../src/protocol_core.rs),
[`src/session_capability.rs`](../../src/session_capability.rs),
[`src/session_admission.rs`](../../src/session_admission.rs), and
[`src/session_member.rs`](../../src/session_member.rs).

The standalone Lean crate imports all four files with `#[path]`. The Kani crate
imports `protocol_core.rs`. Production version negotiation, capability projection,
support checks, aggregation, and sticky admission call the same helpers. The proof crates do not
change the server's dependencies or public API.

## Contracts and boundaries

| Backend and target | Checked property | Boundary |
| --- | --- | --- |
| Lean negotiation | Explicit requests respect both ceilings and preserve values below the server ceiling; ordered server bounds give the correct `None` default and preserve below-floor requests | All `u16` inputs; configuration validation and handshake refusal remain outside |
| Lean aggregation result | Empty slices return false; every representable finite nonempty slice returns the conjunction of member predicates under the total callback contract below | Callback must terminate successfully and preserve its invariant; the concrete support callback is covered separately below |
| Lean callback semantics | For every representable finite slice and modeled callback, the production result agrees with the ordered callback-step relation, including state-dependent results, errors, and divergence | The model threads private state but discards final state; external effects and arbitrary Rust callbacks remain outside; the concrete support callback is covered below |
| Lean capability projection | Preserve version and all six enum membership bits for every representable capability slice; session support requires version >= 3 and both requested axes | Actual production enums and fields are extracted; connection lookup and room-state correspondence remain outside; actual member access has the contract below; lookup-result/default resolution has the separate contract below |
| Lean concrete capability aggregation | The extracted support callback requires version >= 3 and both requested axes for every member of every representable nonempty slice; empty input returns false | Requires a total, fixed capability getter; the actual production member-field getter is proved below; connection lookup and room state remain tested boundaries |
| Lean production profiles | Actual `From<&NegotiatedProtocol>` reads the stored version and both vectors; the live resolver uses the fixed v2 floor; both actual defaults agree after projection and reject all session pairs | All representable stored profiles; actual server constant extracted; map selection, locking, member construction, and async correspondence remain outside |
| Lean profile resolution | Present lookup results preserve all fields; absent results use the supplied floor and exact relay-only fields; projection and support compose through resolution | Every `u16` version/floor and arbitrary representable capability slices; map lookup and room state remain outside; the fixed floor and actual member getter have separate contracts |
| Lean stored-host validity | A Host plan is invalid exactly when its saved identity has no eligible matching current member; non-Host plans need no host | Every representable finite member slice and all stored pairs; UUID byte equality is a trusted external mapping; election, plan updates and publication remain outside |
| Lean membership repair | Preserve a usable saved plan; otherwise replace only its host with the eligible election result or remove it and publish relay; exact stored-state update and replan flag; finite modeled termination | Every representable finite member slice, all stored pairs, profiles and authorities; pinned slice enumeration and shared-reference clone; generation, snapshots, endpoint validity, commits and delivery remain outside |
| Lean sticky admission | Admit exactly protocol v3+ profiles containing both stored axes; agree with production profile support; ignore stored host presence and identity | Every `u16`, representable vector, and stored pair, including illegal pairs; UUID is an external type interpretation; map lookup and room admission/publication remain outside |
| Lean session pair selection | Select the first permitted, unanimously supported and executable ADR rung; otherwise Relay/Relay | Every representable finite member slice, all capability fields, ceilings and switches; configuration lookup, endpoint validation, election, state and publication remain outside |
| Lean host election | A seated authority wins; otherwise elect the least timestamp/UUID key; only input identities can win; empty input alone returns no host; finite modeled iteration terminates and permutations preserve the winner | Extracted generic production loop instantiated with the pinned slice iterator; Chrono/UUID field interpretations and tuple comparison are trusted mappings; the eligible-filter contracts below cover the shared filter; caller arguments and state remain tested/pending |
| Lean eligible host election | Filter by the actual capability and Direct endpoint predicate before authority preference; otherwise select the minimum eligible key; return no host exactly when no member qualifies; terminate for finite modeled input | Extracted production helper and closure with pinned slice and Filter models; caller arguments, snapshots, endpoint validation and async correspondence remain tested/pending |
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

Keep the two source-linked CI checks for the stated contracts. The original
pilot's warm checks fit its five-minute target. Later extensions add contracts
and source faults; the pilot timing does not estimate the expanded gate. Record
each full-cycle measurement in issue #1464. Keep all existing verification layers.
Do not expand this into a translator project or claim a whole-server proof.

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
That module backports the concrete slice aliases from
[hax-lean v0.3.41](https://github.com/cryspen/hax-lean/blob/1d84de68c03bd76457887121853779a52a09e33a/CoreModels/Core/FunsEpilogue.lean#L450).
They delegate to the pinned library's existing iterator defaults and concrete
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

The probe changes only its temporary scenario's include list to select the
original generic helper. It checks that every other parsed setting stays the same.
It uses generated external declarations. The required capability scenario still
extracts all concrete member and host functions with their reviewed mappings.

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
The capability projection and concrete aggregation callback have separate contracts
below. The concrete callback requires a total, fixed capability getter. Production
member-getter correspondence is proved below; caller/state correspondence remains pending. Keep the bounded Kani
checks and adapter tests.

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
and the final source guard cover all production input modules. The concrete
support callback has the separate aggregation contract below. Connection lookup,
final callback state, external effects, room decisions,
and asynchronous correspondence remain unresolved in #1464. Lookup-result/default
resolution has the separate source contract below.

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

The actual server getter is `|member| member.capabilities`. The member contracts
below prove its correspondence to stored `SessionMember` fields. The theorem does not prove
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

### Profile lookup-result and default resolution

The live capability lookup adapters and `SessionCapabilities::default` call
`resolve_connection_capabilities`. This fixed-floor function calls
`session_capability::resolve_capabilities` with the extracted server constant.
The generic resolver consumes the existing
copied lookup result. It adds no allocation or profile clone and preserves the
current runtime behavior. The proof scenario imports this actual production
function; it does not copy the default implementation into the proof crate.

[`Defaults.lean`](lean/proofs/projection/lean/Projection/Verification/Defaults.lean)
checks six contracts. `known_profile_contract` preserves every field of any
present capability value, regardless of its version or the supplied floor.
`absent_profile_contract` checks all seven fallback fields for every `u16`
floor. The fallback has only Relay transport and Relay topology.
`resolved_projection_contract` connects production projection, `Some`, and
resolution for arbitrary representable axis slices, including empty lists,
duplicates, and all orders. `resolved_support_contract` checks the composed
support decision for every version, floor, and requested axis pair.

`absent_support_contract` states the exact missing-profile result: only
Relay/Relay can pass, and it passes exactly when the supplied floor is at least
three. The theorem does not assume a floor of two. `resolution_outcomes` checks
a known future-version profile with duplicates and rejection of every pair for
an absent profile at floor two, plus reachable Relay/Relay acceptance at floor
three. The fixed-floor production resolver supplies `SERVER_MIN_PROTOCOL_VERSION`. The
production-profile contracts below check that constant and both default bodies.
The adapter test also asserts the exact default fields.

The required gate audits all six contracts. Two additional isolated source
faults promote the absent-profile version to three or replace a known profile's
version by the supplied floor. Each must extract and compile before the
resolution contract fails. A separate audited kernel witness checks each wrong
result. The runner then restores the production source, proves the contracts,
and checks identical regeneration and source/pin integrity.

This closes the pure lookup-result/default decision. It does not prove the
connection manager's map lookup, locking, or selected profile identity,
membership snapshot or asynchronous correspondence. The actual member-field
getter has the separate contract below.
Those obligations remain open. Kani retains its previous negotiation and
zero-to-eight-member aggregation bounds; no new Kani claim is made.

### Production negotiated profiles and fixed defaults

[`Profiles.lean`](lean/proofs/projection/lean/Projection/Verification/Profiles.lean)
checks nine additional contracts. The production `NegotiatedProtocol` type,
both `Default` implementations, and `From<&NegotiatedProtocol>` now live in the
shared source. The former module paths re-export the same types. The public
`config::SERVER_MIN_PROTOCOL_VERSION` path re-exports the extracted constant.
No runtime dependency, allocation, wire format, or public API changes.

A proof-crate invocation calls the actual production `From` implementation.
This makes extraction follow the trait body; it does not restate that body.
`profile_fields_contract` checks that the adapter selects this profile's version
and both complete vectors. `profile_support_contract` composes it with the
support theorem for every `u16` version, representable vector, and axis pair.
It permits empty lists, duplicates, and every order. `profile_outcomes` checks
future-version acceptance, v2 rejection, and missing-axis rejection through
that actual adapter.

`compact_default_contract` and `negotiated_default_contract` check both actual
default bodies against the literal v2 relay contract. They extract the server
constant rather than accepting a floor as a theorem input.
`defaults_agree_contract` checks equality after the actual production adapter.
`default_rejects_contract` rejects every session pair at the v2 floor.
`connection_resolution_contract` checks both branches of the actual fixed-floor
resolver for every compact profile, including all capability bits and versions.
`connection_profile_support_contract` composes the actual From adapter, fixed
resolver, and support predicate. Neither function accepts a caller-supplied
floor. The generic resolver's present-profile branch remains in production use.

The vector-default claim uses the pinned library allocation model. Actual
allocator failure and process failure remain excluded.

Three new isolated production-source faults change the server constant to
three, overwrite the profile version with the floor, or discard the stored
transport vector. The runner first compiles each extracted module. It requires
a semantic rejection in `Profiles.lean`, then checks and audits a separate
concrete wrong-result witness. It restores and rechecks all projection proofs.
All nine theorem names are required by the axiom audit and CI.

Both live lookup callers and the compact Default use the checked fixed-floor
resolver. Its present and absent branches are both checked.
The map's selected identity, synchronization, `Option` callback wiring,
member-field construction, room state, and asynchronous correspondence
remain tested or pending. The actual `From` body and fixed default bodies are
proved; this is not a proof of the whole lookup operation or session selector.
Kani's existing eight-member bound is unchanged.

The two negotiation faults also retain their isolated source and proof trees
before cleanup. Failed extraction, proof, or witness attempts keep a failed
status. A retained snapshot alone does not establish a successful fault control.

### Sticky-session admission

The actual stored `ActiveSessionPlan` and `joiner_supports_sticky_plan` now live
in [`session_admission.rs`](../../src/session_admission.rs). The server re-exports
their former paths. The predicate body, UUID host field, runtime dependencies,
allocations, locking, and observable behavior stay unchanged. Rust, .NET, and
Godot clients need no changes.

[`Admission.lean`](lean/proofs/projection/lean/Projection/Verification/Admission.lean)
checks four contracts. `admission_contract` states the exact admission result:
version at least three and membership of both stored axes. It covers every
`u16`, every representable vector, empty lists, duplicates, all orders, all nine
stored pairs, and arbitrary host values. It assumes neither a legal stored pair
nor a relay floor in the profile. `admission_support_contract` proves agreement
with the actual production From/support path. `admission_host_independent`
checks that host presence and identity cannot change admission. Ordinary Direct
clients need no endpoint. `admission_outcomes` checks reachable Direct acceptance,
v2 rejection, and rejection for either missing axis at the maximum version.

Four compiled production-source faults lower the version floor, drop either
axis check, or require a stored host. Each must compile before `Admission.lean`
rejects it. A separate kernel-checked witness establishes its wrong result.
The axis and host faults use relay-containing v3 mesh profiles, as production
negotiation permits. The version fault deliberately supplies a v2 profile with
upgrade bits. It checks the version guard on an inconsistent internal snapshot;
it does not prove handshake normalization. The runner audits every contract and
witness, retains fault evidence, and restores the proofs. Independent Rust subset cases check all six representative
versions, all 64 axis subsets, all nine pairs, and three host values. Existing
adapter and finalized-join tests remain caller evidence.

The pinned extractor emits `uuid.Uuid` without a Lean declaration, even when
Charon includes its concrete definition. The assumptions module supplies a
16-byte structure matching the locked UUID dependency. This is a trusted external
type interpretation, with no UUID operations or added axioms. It does not prove
UUID layout, equality, or generation. Admission never reads UUID contents;
future UUID operations need supported models and cannot silently use invented
methods. Projection snapshots retain the Charon LLBC, including the concrete
UUID type, in addition to source, generated Lean, contracts, pins and raw logs.

The connection adapter fails closed when its map row is absent. Its map lookup,
identity and lock relationship remain tested. The finalized-room join gate,
stored-plan consistency, transaction commit, reconnect policy, and publication
remain open state/caller obligations. A correct pure admission decision does not
prove that every caller selects the correct room or profile. No whole-room or
asynchronous proof is claimed. Kani keeps its eight-member bound.

### Actual production members and host eligibility

[`Members.lean`](lean/proofs/capability/lean/Capability/Verification/Members.lean)
checks six contracts over the actual `SessionMember` and `DirectEndpoint` types
in [`session_member.rs`](../../src/session_member.rs). Former server and public
protocol paths re-export the same types. Constructors and endpoint validation
stay in their existing modules. No runtime, wire, allocation or API change is
intended; Rust, .NET and Godot clients need no changes.

`member_support_contract` checks the actual method's capability field access.
`member_getter_contract` proves the actual aggregation closure satisfies the
fixed getter contract without assumptions about a user-supplied callback.
`member_aggregation_contract` applies the existing arbitrary-length theorem to
every representable finite slice of actual members. It includes empty input,
all versions, every axis assignment, duplicates and all orders. Membership
construction, map identity and synchronization remain outside this theorem.

`member_host_contract` states the exact actual `can_host` result: v3 and both
requested axes, plus a stored endpoint only for Direct. It proves presence,
not endpoint validation, network reachability or legal-pair selection.
`member_host_inputs` proves independence from identity, name, authority and
join time, and from endpoint contents when presence agrees.
`member_host_outcomes` checks future-version acceptance, old-version rejection,
Direct endpoint presence/absence, WebRTC without a Direct endpoint, empty-room
rejection and ordinary Direct membership without a host endpoint. These checks
apply for every metadata value and endpoint value, with no input assumptions.

The required gate audits all six contracts and tests four isolated production
faults: an overwritten getter version, an overwritten member-adapter version,
a missing Direct endpoint check, and an unwanted WebRTC endpoint requirement.
Each must compile extraction, reject a member contract and pass an independently
kernel-checked wrong-result witness. Restored proofs, identical regeneration,
source/pin hashes and raw command timings remain required. The WebRTC endpoint
fault retains the original transport comparison and adds an endpoint requirement;
removing that comparison dropped an extracted equality definition and was not
accepted as a semantic fault control. Capability snapshots
now retain Charon LLBC as well as the external type interpretations.

The proof crate pins Chrono 0.4.45 without clocks. The backend leaves UUID and
Chrono type references undeclared. The assumptions module supplies the existing
16-byte UUID interpretation and a timestamp interpretation containing packed
date, seconds, fraction and offset. The timestamp interpretation permits all
scalar values, including invalid dates; it overapproximates the locked Chrono
representation. The member support and eligibility predicates never inspect
either metadata type. Timestamp interpretation adds no operations or axioms.
Host validity below adds an explicit UUID equality mapping. Neither mapping
proves Chrono, UUID, timestamp ordering or host election.

### Stored-host validity

[`HostValidity.lean`](lean/proofs/capability/lean/Capability/Verification/HostValidity.lean)
checks the actual `ActiveSessionPlan::host_invalid` method, moved unchanged to
[`session_admission.rs`](../../src/session_admission.rs). The live membership
resolver still calls this method. Only its pure decision is proved.

`any_iterator_contract` proves the pinned search loop for total callbacks.
`host_match_contract` connects the extracted search closure to exact saved-ID
matching and the actual member host predicate. `host_validity_contract` covers
every representable finite member slice, optional saved identity, version,
capability assignment, endpoint and stored pair. A Host plan is invalid when
no matching eligible member exists. Relay and Mesh need no host. No uniqueness
or legal-pair assumption excludes malformed snapshots. `host_validity_witness`
requires an actual matching eligible member for a valid Host snapshot.
`host_validity_outcomes` checks absent identities, absent members, old versions,
Direct endpoint readiness, WebRTC without an endpoint and both non-Host cases.

The original probe failed on UUID equality and the concrete iterator `any`
alias. The reviewed compatibility file now also backports `any` from the same
upstream revision, lines 455–458. It delegates to the pinned iterator model;
the proof unfolds that model's loop. No search implementation is copied.

UUID equality is an explicit new trusted mapping in the capability assumptions
module: equality of the whole sixteen-byte UUID value. The locked UUID 1.29.0
source derives `PartialEq` on its sole `[u8; 16]` field (`src/lib.rs:445,466`).
The mapping adds no custom axiom. This is not a source proof of UUID equality.
The pinned hax invocation uses Aeneas `-core-models-lib`, which omits nonlocal
definitions even when Charon includes their bodies. A direct pinned Aeneas
probe without that mode emits UUID equality, but changes standard-library
models and leaves other operations unresolved. It is not an accepted proof.
The production predicate theorem therefore depends on the stated library mapping,
as well as the pinned Rust frontend, extractor and library models.

Four isolated source faults drop identity matching, drop host eligibility,
invert the topology gate, or accept an absent saved host. Each must compile,
reject only a host contract, and pass an axiom-audited wrong-result witness.
Identity and eligibility faults preserve the original calls and closure captures;
they force the boolean gate to accept. Capture/type failures do not count.
The wrong-ID witness proves its two UUID byte values differ. The restored
contracts and deterministic regeneration remain required. Rust tests independently
cover versions, all capability subsets, both member orders and endpoint presence;
a capable bystander cannot validate a missing or incapable saved host.

UUID implementation correspondence, map identity/locking, member construction,
host election, re-election, relay reset, transaction commit, publication and
asynchronous correspondence remain open in P10/P12. These predicates do not
establish the state transitions. No runtime behavior, wire format, public API,
allocation or runtime dependency changes; Rust, .NET and Godot clients are
unaffected for that reason. Kani keeps its eight-member bound and unwind ten.

### Selector executability

The actual `rung_is_executable` body now lives unchanged in
[`session_member.rs`](../../src/session_member.rs). The live selector imports it;
no independent implementation replaces production behavior.
[`Executability.lean`](lean/proofs/capability/lean/Capability/Verification/Executability.lean)
checks four required contracts. `executable_callback_contract` connects the actual
search closure to the proved member host predicate. `executability_contract`
checks every representable finite member slice and every topology/transport pair.
Direct needs at least one v3 member with both requested axes and a stored endpoint.
Other transports pass this extra gate, including empty input.
`direct_executable_witness` requires a concrete eligible member whenever Direct
passes. `executability_outcomes` checks empty input, missing endpoints, old versions,
future versions, and a ready member after an unready member.

The contract makes no legal-pair or membership-uniqueness assumption. It checks
endpoint presence, not endpoint validation or reachability. The extra gate alone
does not establish unanimous support: the selector separately calls `all_support`.
The next section adds the pure ladder, ceiling and transport-switch contracts.
Configuration lookup, host election, input construction, state commits and
asynchronous correspondence remain open in P09–P12.
The existing member and library-model boundaries still apply. No new trusted
mapping or axiom is introduced. Kani keeps its eight-member bound and unwind ten.

Three isolated source faults invert the Direct bypass, permit an endpoint without
capabilities, or conjoin the bypass with the member search. Each must compile,
fail only an executability contract and pass a separate axiom-audited wrong-result
witness. Required proofs are restored and regenerated twice. Source/pin guards,
hashes, raw logs and timings use the existing evidence pipeline. An independent
Rust matrix covers all capability subsets, representative versions, all pairs,
endpoint presence, both member positions, duplicates and empty input.

This is an internal source move with added verification. It changes no runtime
behavior, wire format, public API, allocation or runtime dependency. Rust, .NET
and Godot clients need no changes for that reason. The exact source blobs are
recorded in `.github/internal-refactor.json`; hashes identify the reviewed diff,
not a proof of semantic equivalence.

### Session pair selection

The live `choose_session_plan` now calls the unchanged iterator decision in
[`session_member::select_session_pair`](../../src/session_member.rs).
It passes the resolved topology ceiling and the two transport switches.
The ladder, floor and rank move with this private decision. The caller still
creates the generation and owns the members. Host election has its separate
source-linked boundary below.

[`Selection.lean`](lean/proofs/capability/lean/Capability/Verification/Selection.lean)
specifies the ADR-0001 order independently: Mesh/WebRTC, Host/WebRTC, Host/Direct,
then Relay/Relay. Its `RungFits` predicate requires the topology ceiling, the
matching transport switch, nonempty unanimous v3+ support for both axes, and
Direct execution readiness. The predicate uses the proved member semantics.
It does not derive the required order from the production ladder constant.

Eight additional audited contracts cover the pinned array search loop, its lift
through the production selector, the actual callback, first-fit selection, the
explicit waterfall, exact fallback, accepted-upgrade gates and reachable outcomes.
The search proof terminates for finite arrays under a total fixed-predicate
callback contract. The actual callback discharges that contract without an
external getter assumption. All representable member slices, capability fields,
versions, ceilings and switch values are covered. Identity uniqueness is not
required. Empty rooms and legacy members cannot enable an upgrade.

Eight isolated production-source faults change each priority, require an exact
ceiling, ignore the ceiling, read the wrong transport switch, omit unanimity,
omit execution readiness, or change the relay floor. Each extracted module must
compile before the owning selection contract rejects it with an unsolved goal.
Compiler errors, tool errors and failures in other modules do not count. A
separate kernel-checked witness proves each wrong result without importing the
rejected selection contract. Witnesses reuse the proved generic iterator lift
with a concrete actual callback; they do not substitute a selection implementation.
The standard runner restores proofs, regenerates twice, audits axioms, and retains
source hashes, generated files, raw logs and timings. The independent Rust oracle
checks 82,944 capability/version/endpoint/membership/ceiling/switch combinations.

P09's pure pair decision is proved. Configuration map lookup and argument mapping,
member construction and endpoint validation, elected-host choice, state commits,
publication and asynchronous correspondence remain tested or pending. An endpoint
in these proofs is present in the input; its address validity and reachability
are not proved. UUID, timestamp and other metadata do not enter the pair decision.
The existing external member types remain. This proof also relies on the pinned
array `IntoIterator`, `IntoIter::next`, `Iterator::find`, option fallback and
sequence models. The search theorem unfolds those definitions; their refinement
to the Rust standard library remains a trusted extraction boundary.
Kani retains its eight-member aggregation bound and unwind ten; it does not
establish these new selector contracts.

This internal source move preserves behavior, APIs, wire formats, allocation
strategy and runtime dependencies. Rust clients are unaffected because session
pairs and defaults are unchanged. .NET clients are unaffected for the same reason.
Godot clients are unaffected for the same reason. No client migration is required.
The exact source diff is declared in `.github/internal-refactor.json`; the blob
hashes identify the reviewed change and do not prove semantic equivalence.

### Host election

The shared production helper calls
[`session_member::elect_host`](../../src/session_member.rs).
The loop still prefers the explicit authority among its input candidates.
Otherwise it keeps the least `(joined_at, player_id)` key. It stores that copied
key instead of a borrowed member. It adds no allocation and keeps the same
iterator, early return and strict comparison. The shared eligible-host helper
below owns the callers' eligibility filter; authority cannot bypass it.

[`Election.lean`](lean/proofs/capability/lean/Capability/Verification/Election.lean)
checks seven contracts over the extracted generic loop with the pinned slice
iterator: empty input iff no host, authority preference, input membership,
minimum timestamp/UUID when authority is absent, successful finite termination,
empty/singleton outcomes, and permutation stability. The minimum and authority
properties specify the required protocol behavior independently of the scan
used in the loop-correspondence lemma. Duplicate identities, proxy authority
flags, names, capabilities and endpoints need no extra assumptions. Eligibility
is a caller obligation; this loop selects among candidates it receives.

The initial pinned extraction rejected a candidate reference held across the
loop (`There should be no bottoms in the value`). Copying the two key fields
clears that failure. Instantiating the extraction with a Rust slice-iterator
wrapper also pulls in a higher-ranked `Iterator::find` lifetime unsupported by
the pinned backend. The required extraction therefore keeps the actual generic
function. The Lean contracts instantiate its iterator dictionary with the pinned
slice model. This is not an extracted proof of the production `Filter` adapters.
The eligible-host contracts below add Filter enumeration correspondence. Caller
arguments, input snapshots and state correspondence remain tested or pending in
P10. Neither a failed extraction nor a typecheck is
counted as a protocol theorem.

The new external comparison mappings are explicit trusted boundaries:

- UUID 1.29.0 derives comparison on its fixed sixteen-byte field.
- Chrono 0.4.45 `DateTime::partial_cmp` compares its `NaiveDateTime`. Its derived
  order reads signed packed date, unsigned seconds, then unsigned fraction.
- The pinned standard tuple comparison checks the first field, then the second
  on equality. The TimeZone interpretation supplies only the associated type
  needed by the UTC comparison; it models no timezone conversion operations.

[`ElectionOrder.lean`](lean/proofs/capability/lean/Capability/Verification/ElectionOrder.lean)
proves four interpretation lemmas. Numeric timestamp keys preserve field order
and equality for all modeled scalar values. Numeric UUID keys preserve byte
order and exact identity for every sixteen-byte array. These lemmas check the
encodings, not the Rust dependency layouts or implementations. Timestamp inputs
overapproximate valid Chrono values; construction and calendar validity are not
claimed. The extractor, standard iterator model and dependency interpretations
remain trusted even when every axiom audit passes.

Three isolated source faults ignore authority, reverse date priority, and drop
the UUID tie-break. Each extracted module must compile, fail only its election
contract with an unsolved goal, and have a separate kernel-checked wrong-result
witness. The witnesses use two candidate records with the same supplied date;
changing their seconds or UUID gives the required ordering. They do not import
the rejected election contract. Tool failures and signature errors do not count.
The runner restores the source, checks identical regeneration and approved axioms,
and retains sources, pins, generated output, raw command logs and timings.

The independent Rust sorting oracle checks 110,592 forward/reversed elections.
Further cases cover every UUID byte, timestamp extremes, negative years,
year/leap-day boundaries and leap-second fractions. Existing caller tests cover
filtered authority and capability cases. These are adapter evidence, not formal
caller/state refinement. Kani retains its separate eight-member aggregation
bound and does not prove election.

The change is internal. Rust, .NET and Godot clients are unaffected because
runtime behavior, public APIs and wire formats are unchanged. The exact source
diff is declared in `.github/internal-refactor.json`. Issue #1464 remains open
for the caller, endpoint, state and concurrency obligations.

## Tool references

- [hax Lean workflow](https://hax.cryspen.com/manual/lean/quick_start/)
- [hax tool and scenario pins](https://hax.cryspen.com/manual/tools/)
- [Kani loop bounds](https://model-checking.github.io/kani/tutorial-loop-unwinding.html)
- [Kani feature limits](https://model-checking.github.io/kani/rust-feature-support.html)

### Eligible host election

Both finalize-time selection and membership repair now call the same private
[`elect_eligible_host`](../../src/session_member.rs) helper. It filters candidates
with the actual `SessionMember::can_host` method before calling `elect_host`.
An authority with an old protocol, a missing capability axis, or a missing
Direct endpoint cannot bypass this gate. An eligible seated authority wins;
otherwise the least eligible timestamp/UUID key wins.

[`EligibleElection.lean`](lean/proofs/capability/lean/Capability/Verification/EligibleElection.lean)
checks the extracted callback, the pinned Filter iterator and the actual shared
helper for every representable finite member slice. The callback contract uses
the independent capability and endpoint predicates proved for `can_host`. The
whole-helper contract connects filtered enumeration to the checked election loop.
Separate contracts specify eligible authority preference, eligible membership,
minimum eligible fallback, no host exactly when no member qualifies, and successful
finite termination. `eligible_outcomes` checks accepting and rejecting singleton
snapshots with arbitrary member metadata and endpoint contents. The input domain
includes all `u16` versions, capability
subsets, endpoint presence, metadata, optional authority and empty slices.

The concrete slice wrapper still hits the pinned backend's higher-ranked
`Iterator::find` lifetime limit. A generic wrapper with a borrowed closure also
fails in Aeneas `Interp.ml`. The production helper accepts an `IntoIterator` and
uses a `move` closure over the copied axes; this extracts without changing tool
pins. One local dispatch alias exposes the pinned library's existing
`Iterator.filter.default` under the generated `Iterator.filter` method name.
It adds no predicate, loop, axiom or iterator semantics. The existing reviewed
compatibility file carries this alias; the gate compares its installed bytes
with the canonical file. The proofs instantiate that extracted generic helper
with the pinned slice iterator and Filter model. They do not replace the
production predicate or election body.

The caller argument selection, snapshot construction, endpoint validation,
locking, generation updates, publication and async interleavings remain tested
or pending. An endpoint's presence proves neither its validity nor reachability.
Chrono/UUID comparison retains the trusted interpretations described above.
No runtime dependency, allocation, public API, wire format or client behavior
changes. Rust, .NET and Godot clients need no migration because both callers
apply the same eligibility predicate and election order as before.

Three isolated source faults bypass all eligibility, drop Direct endpoint
readiness, or return the authority before checking eligibility. Every control
must extract and typecheck, then fail only the eligible-election semantic
contract. Each has an independently audited theorem of the wrong result over
the actual extracted helper. The runner rejects compiler/tool errors as fault
evidence, restores the source and rechecks the capability proofs. The required
CI run retains source hashes, pins, generated output, logs, audits and timings.

Before tool setup, `--mutations` checks that every declared source anchor occurs
exactly once. The same definitions drive this preflight and the fault controls.
Missing or ambiguous anchors fail before dependency preparation and proof work.

## Membership session transition

The production `session_admission::resolve_membership_plan` now owns the decision
previously in `membership_session_decision`. Its output contains the plan to
publish, the optional sticky-state update, and the replan flag. The server
adapter passes the same stored plan, authority and shared member slice. It adds
one new generation and retains the original member vector as before. Runtime
behavior, wire formats and public APIs are unchanged. Rust, .NET and Godot
clients therefore need no changes.

Eight required contracts in `Membership.lean` check the extracted production
function. The specification preserves any valid stored plan, including a
non-Host plan. An invalid Host plan uses the already-verified eligible election
on its stored pair. A replacement changes only the host and writes the exact
plan selected for publication. No eligible replacement removes sticky state,
publishes Relay/Relay without a host, and does not count a host replan. No stored
plan also publishes relay but leaves state untouched. Reachability cases cover
a valid incumbent, repair, and Direct endpoint refusal. The totality contract
covers every finite slice representable by the pinned model.

The contracts compose the existing saved-host validity, capability, Filter,
authority and timestamp/UUID election results. They do not assume a legal stored
pair, unique identities, a nonempty room, or a capable authority. UUID equality,
UUID/Chrono ordering, extraction and pinned standard-library models retain their
existing trust boundaries.

The first concrete-slice extraction reproduced the pinned Aeneas failure on
`core::slice::Iter::find`: a higher-ranked lifetime relates to a free lifetime.
The accepted production function uses generic enumeration with a clone for the
validity pass. Production supplies `&[SessionMember]`; cloning that reference
preserves the snapshot and order without cloning the members. Required Lean
contracts instantiate the same pinned slice iterator as the election proofs and
the library's existing shared-reference Clone model. They do not prove arbitrary
custom iterators or custom Clone implementations correct. `host_invalid` shares
this generic enumeration seam; its existing four contracts and source-fault
witnesses now use the pinned iterator explicitly.

A local dispatch alias exposes the pinned `Iterator.any.default`, as the
existing alias does for `Iterator.filter.default`. It adds no loop, predicate,
axiom or replacement for extracted Rust. Tool and dependency pins are unchanged.

Four independent source faults invert validity, change the repaired transport,
drop the state-removal update, or suppress the replan flag. Each must compile,
fail only the membership semantic contracts, and pass a separate audited
kernel-checked wrong-result witness. CI retains baseline, faults, restored
proofs, generated definitions, source/pin hashes and command timings. The
independent Rust oracle compares 737,280 production outcomes against sorted
eligible-member sets and a sticky-state table. Existing production-adapter tests
check endpoint failover and keeping or replacing the saved host.

These checks establish the pure transition over the supplied snapshot.
Snapshot acquisition, current connection/profile identity, endpoint syntax and
reachability, generation freshness, transaction commit, publication, cancellation
and concurrent membership changes remain tested or pending. They are separate
P12 correspondence obligations; this stage does not close issue #1464.
