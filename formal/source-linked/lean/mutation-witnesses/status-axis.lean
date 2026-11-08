import Projection.Verification.Profiles
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
noncomputable section
namespace Projection
/-- The mutant accepts an unnegotiated transport and stores its report. -/
theorem mutation_returns_wrong_value (generation : uuid.Uuid) :
    update_transport_status
      { version := 3#u16, transports := ⟨[], by scalar_tac⟩,
        topologies := ⟨[], by scalar_tac⟩ }
      generation .None .WebRtc true =
        RustM.ok (.Changed, .Some (generation, .WebRtc, true)) := by
  simp [update_transport_status, Aeneas.Std.bind, Bind.bind]
end Projection
