import Capability.Verification.MembershipInputs
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability session_admission
noncomputable section
namespace Capability
/-- Kernel-checked wrong state for the isolated production-source fault. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    let ready := { member with capabilities := (⟨3#u16, true, true, true, true, true, true⟩ : SessionCapabilities) }
    resolveMembership (.Some ⟨.Host, .WebRtc, .Some member.player_id⟩) .None
      (⟨[ready], by scalar_tac⟩ : Slice SessionMember) = RustM.ok ⟨⟨.Host, .WebRtc, .Some member.player_id⟩, .Some (.Some ⟨.Host, .WebRtc, .Some member.player_id⟩), true⟩ := by
  simp [resolveMembership, resolve_membership_plan, membershipClone,
    CoreModels.core.Shared0T.Insts.CoreCloneClone, CoreModels.core.Shared0T.Insts.CoreCloneClone.clone,
    host_validity_contract, eligible_election_contract, election_evaluation,
    HostMatch, MemberSupport, Eligible, electionScan, earlierCandidate,
    memberCandidate, RustOption, Aeneas.Std.bind, Bind.bind]
end Capability
