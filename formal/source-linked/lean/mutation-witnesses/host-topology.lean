import Capability.Verification.Members
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member session_admission
namespace Capability

/-- A non-Host plan is wrongly marked invalid. -/
theorem mutation_returns_wrong_value :
    ActiveSessionPlan.host_invalid ⟨.Relay, .Relay, .None⟩
      (⟨[], by scalar_tac⟩ : Slice SessionMember) = RustM.ok true := by
  simp [ActiveSessionPlan.host_invalid, CoreModels.core.cmp.PartialEq.ne.default,
    Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant,
    CoreModels.core.option.Option.is_some_and, Aeneas.Std.bind, Bind.bind]
end Capability
