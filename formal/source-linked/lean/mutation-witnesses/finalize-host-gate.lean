import Capability.Verification.FinalizeInputs
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability session_admission
noncomputable section
namespace Capability
/-- Kernel-checked wrong result for this isolated production-source fault. -/
theorem mutation_returns_wrong_value (member : SessionMember) (endpoint : DirectEndpoint) :
    let ready := { member with capabilities := (⟨3#u16, true, true, true, true, true, true⟩ : SessionCapabilities),
      direct_endpoint := .Some endpoint }
    finalizePlan .None (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host true true =
      RustM.ok ⟨.Host, .WebRtc, .None⟩ := by
  unfold finalizePlan finalize_session_plan
  simp [CoreModels.core.Slice.Insts.CoreConvertAsRefSlice.as_ref,
    selection_contract, ProtocolLadder, RungFits, Rank, MemberSupport, DirectReady,
    HasEndpoint, Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant,
    Aeneas.Std.bind, Bind.bind]
end Capability
