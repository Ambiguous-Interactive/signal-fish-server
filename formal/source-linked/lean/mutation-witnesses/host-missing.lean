import Capability.Verification.Members
import Capability.Verification.Election
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member session_admission
namespace Capability

/-- A Host plan without any saved identity is wrongly marked valid. -/
theorem mutation_returns_wrong_value :
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .WebRtc, .None⟩
      (⟨[], by scalar_tac⟩ : Slice SessionMember) = RustM.ok false := by
  simp [ActiveSessionPlan.host_invalid, Topology.Insts.CoreCmpPartialEqTopology.eq,
    Topology.read_discriminant, CoreModels.core.option.Option.is_some_and,
    Aeneas.Std.bind, Bind.bind]
end Capability
