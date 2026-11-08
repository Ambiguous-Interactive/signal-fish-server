import Projection.Verification.Profiles
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
noncomputable section
namespace Projection
/-- The mutant treats a changed connected flag as a duplicate. -/
theorem mutation_returns_wrong_value (generation : uuid.Uuid) :
    update_transport_status
      { version := 3#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[], by scalar_tac⟩ }
      generation (.Some (generation, .WebRtc, false)) .WebRtc true =
        RustM.ok (.Duplicate, .Some (generation, .WebRtc, false)) := by
  simp [update_transport_status,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains,
    contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq, transport_eq,
    Aeneas.Std.bind, Bind.bind]
end Projection
