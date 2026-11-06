import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_admission
namespace Projection
/-- The extracted production fault gives the wrong admission result on this input. -/
theorem mutation_returns_wrong_value :
    joiner_supports_sticky_plan
      { version := 3#u16, transports := ⟨[Transport.Relay], by scalar_tac⟩,
        topologies := ⟨[Topology.Relay, .Mesh], by scalar_tac⟩ }
      { topology := .Mesh, transport := .WebRtc, host := .None } = RustM.ok true := by
  simp [joiner_supports_sticky_plan,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains, rust_primitives.slice.slice_contains_go,
    Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant,
    Aeneas.Std.bind, Bind.bind]
end Projection
