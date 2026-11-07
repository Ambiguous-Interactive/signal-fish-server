import Capability.Verification.MembershipInputs
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability session_admission
noncomputable section
namespace Capability
/-- An unrecoverable host wrongly leaves the stale stored plan untouched. -/
theorem mutation_returns_wrong_value :
    resolveMembership (.Some ⟨.Host, .Direct, .None⟩) .None
      (⟨[], by scalar_tac⟩ : Slice SessionMember) =
      RustM.ok ⟨⟨.Relay, .Relay, .None⟩, .None, false⟩ := by
  simp [resolveMembership, resolve_membership_plan, membershipClone,
    CoreModels.core.Shared0T.Insts.CoreCloneClone, CoreModels.core.Shared0T.Insts.CoreCloneClone.clone,
    host_validity_contract, eligible_election_contract, election_evaluation,
    MemberSupport, HasEndpoint, Eligible, electionScan,
    RustOption, Aeneas.Std.bind, Bind.bind]
end Capability
