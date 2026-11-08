import Capability.Verification.FinalizeInputs
import Capability.Verification.ElectionFixtures
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability session_admission
noncomputable section
namespace Capability
open ElectionFixtures
/-- An absent authority is wrongly attached to a Host plan instead of an eligible member. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    let ready := { member with player_id := lowId,
      capabilities := (⟨3#u16, true, true, true, true, true, true⟩ : SessionCapabilities) }
    finalizePlan (.Some highId) (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host true true =
      RustM.ok ⟨.Host, .WebRtc, .Some highId⟩ ∧ highId ≠ lowId := by
  constructor
  · unfold finalizePlan finalize_session_plan
    simp [CoreModels.core.Slice.Insts.CoreConvertAsRefSlice.as_ref,
      selection_contract, ProtocolLadder, RungFits, Rank, MemberSupport,
      Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant,
      CoreModels.core.option.Option.is_some, Aeneas.Std.bind, Bind.bind]
  · exact Ne.symm ElectionFixtures.distinct
end Capability
