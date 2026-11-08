import Projection.Verification.CapabilityNegotiation
open CoreModels Aeneas
open Aeneas.Std hiding namespace core alloc
open signal_fish_proof_core session_capability
namespace Projection
/-- The changed production rule yields these axes, contrary to the negotiated contract. -/
theorem mutation_returns_wrong_value :
    (do let result ← negotiate_capabilities 3#u16 (.Some ⟨[], by scalar_tac⟩) (.Some ⟨[.Mesh], by scalar_tac⟩)
        RustM.ok (result.1.val, result.2.val)) = RustM.ok ([.Relay], [.Mesh]) := by
  have one : (1 : Nat) ≤ Usize.max := by scalar_tac
  simp [one, negotiate_capabilities,
    core.option.Option.unwrap_or_else,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, core.slice.Slice.contains,
    rust_primitives.slice.slice_contains,
    contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    contains_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq,
    alloc.vec.Vec.push, rust_primitives.sequence.seq_push,
    dedup_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    dedup_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq,
    firstOccurrences, Aeneas.Std.bind, Bind.bind]
end Projection
