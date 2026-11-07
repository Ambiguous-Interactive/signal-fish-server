import Capability.Verification.HostValidity
import Capability.Verification.EligibleElection

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_admission

noncomputable section
namespace Capability

/-- The pinned shared-reference clone copies the reference, not the members. -/
abbrev membershipClone := CoreModels.core.Shared0T.Insts.CoreCloneClone (Slice SessionMember)

abbrev resolveMembership := resolve_membership_plan electionIterator membershipClone


end Capability
