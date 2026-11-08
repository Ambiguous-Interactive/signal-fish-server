import Capability.Verification.Selection
import Capability.Verification.EligibleElection

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_admission

noncomputable section
namespace Capability

/-- Production supplies one shared slice to both passes. The pinned AsRef model
returns that slice and the pinned iterator enumerates its members in order. -/
abbrev finalizeAsRef := CoreModels.core.Slice.Insts.CoreConvertAsRefSlice SessionMember
abbrev finalizePlan := finalize_session_plan finalizeAsRef electionIterator

end Capability
