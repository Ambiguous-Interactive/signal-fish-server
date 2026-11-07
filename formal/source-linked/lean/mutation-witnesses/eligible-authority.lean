import Capability.Verification.ElectionFixtures
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
noncomputable section
namespace Capability
open ElectionFixtures

/-- An ineligible authority is wrongly elected before the production eligibility filter. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    elect_eligible_host iterator (.Some member.player_id)
      (⟨[{ member with
        capabilities := ⟨2#u16, false, false, false, false, false, false⟩,
        direct_endpoint := .None }], by scalar_tac⟩ : Slice SessionMember) .Host .Direct =
      RustM.ok (.Some member.player_id) := by rfl

end Capability
