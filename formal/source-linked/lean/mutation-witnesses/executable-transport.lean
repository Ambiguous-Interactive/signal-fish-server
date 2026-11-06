import Capability.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability
/-- The inverted gate wrongly accepts Direct with no members or endpoints. -/
theorem mutation_returns_wrong_value :
    rung_is_executable (⟨[], by scalar_tac⟩ : Slice SessionMember) .Host .Direct = RustM.ok true := by
  simp [rung_is_executable, Transport.Insts.CoreCmpPartialEqTransport.eq,
    Transport.read_discriminant, Aeneas.Std.bind, Bind.bind]
end Capability
