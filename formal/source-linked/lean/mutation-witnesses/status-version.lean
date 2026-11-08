import Projection.Verification.Profiles
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
noncomputable section
namespace Projection
/-- The mutant accepts a v2 report and stores its current-generation state. -/
theorem mutation_returns_wrong_value (generation : uuid.Uuid) :
    update_transport_status
      { version := 2#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[], by scalar_tac⟩ }
      generation .None .WebRtc true =
        RustM.ok (.Changed, .Some (generation, .WebRtc, true)) := by
  simp [update_transport_status,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains,
    contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    Aeneas.Std.bind, Bind.bind]
end Projection
