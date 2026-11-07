import Capability.Verification.SelectionEvaluation
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability

/-- The source fault returns this pair where the ADR waterfall requires another pair. -/
theorem mutation_returns_wrong_value (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let ready := { member with capabilities := capable, direct_endpoint := .Some endpoint }
    let members : Slice SessionMember := ⟨[{ ready with capabilities := { capable with mesh_topology := false } }], by scalar_tac⟩
    select_session_pair members .Mesh true true = RustM.ok (.Relay, .Relay) := by
  let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
  let ready := { member with capabilities := capable, direct_endpoint := .Some endpoint }
  let members : Slice SessionMember := ⟨[{ ready with capabilities := { capable with mesh_topology := false } }], by scalar_tac⟩
  change select_session_pair members .Mesh true true = RustM.ok (.Relay, .Relay)
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
        executability_contract, MemberSupport, predicate, members, ready, capable,
        Aeneas.Std.bind, Bind.bind]
  have result := selector_evaluation members .Mesh true true predicate contract
  simpa [UPGRADE_LADDER, RELAY_FLOOR, Array.make, predicate] using result

end Capability
