import Capability.Verification.SelectionEvaluation
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability

/-- The source fault returns this pair where the ADR waterfall requires another pair. -/
theorem mutation_returns_wrong_value :
    select_session_pair (⟨[], by scalar_tac⟩ : Slice SessionMember) .Mesh true true =
      RustM.ok (.Host, .Direct) := by
  let members : Slice SessionMember := ⟨[], by scalar_tac⟩
  change select_session_pair members .Mesh true true = RustM.ok (.Host, .Direct)
  let predicate : (Topology × Transport) → Bool := fun _ => false
  have contract : CallbackContract
      select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool predicate
      (fun capture => capture = (.Mesh, true, true, members)) := by
    intro capture rung valid
    subst capture
    rcases rung with ⟨topology, transport⟩
    refine ⟨_, ?_, rfl⟩
    cases topology <;> cases transport <;>
      simp [select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool.call_mut,
        select_session_pair.closure, topology_rank, transport_enabled, member_aggregation_contract,
        executability_contract, predicate, members,
        Aeneas.Std.bind, Bind.bind]
  have result := selector_evaluation members .Mesh true true predicate contract
  simpa [UPGRADE_LADDER, RELAY_FLOOR, Array.make, predicate] using result

end Capability
