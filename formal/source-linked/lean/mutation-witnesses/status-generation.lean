import Projection.Verification.Profiles
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
noncomputable section
namespace Projection
private def previous : uuid.Uuid := ⟨⟨List.replicate 16 0#u8, by simp⟩⟩
private def current : uuid.Uuid := ⟨⟨List.replicate 16 1#u8, by simp⟩⟩
/-- Distinct concrete UUIDs make the retained report stale; the mutant calls it a duplicate. -/
theorem mutation_returns_wrong_value :
    previous ≠ current ∧
    update_transport_status
      { version := 3#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[], by scalar_tac⟩ }
      current (.Some (previous, .WebRtc, true)) .WebRtc true =
        RustM.ok (.Duplicate, .Some (previous, .WebRtc, true)) := by
  constructor
  · intro same
    have bytes := congrArg (fun id : uuid.Uuid => id.bytes.val.head?) same
    simp [previous, current] at bytes
  · simp [update_transport_status,
      alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
      rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
      rust_primitives.slice.slice_contains,
      contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
      transport_eq,
      Aeneas.Std.bind, Bind.bind]
end Projection
