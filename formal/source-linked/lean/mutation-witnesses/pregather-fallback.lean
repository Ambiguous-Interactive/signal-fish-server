import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_admission
namespace Projection
/-- This source fault refuses ICE for a runnable Host/WebRTC fallback. -/
theorem mutation_returns_wrong_value :
    ice_pregather_eligible true true .Mesh false
      { version := 3#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[Topology.Host], by scalar_tac⟩ } = RustM.ok false := by
  simp [ice_pregather_eligible, CoreModels.core.cmp.PartialEq.ne.default,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains, rust_primitives.slice.slice_contains_go,
    Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
    Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant,
    Aeneas.Std.bind, Bind.bind]
end Projection
