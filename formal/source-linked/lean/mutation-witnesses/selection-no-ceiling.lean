import Capability.Verification.SelectionEvaluation
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability

/-- The source fault returns this pair where the ADR waterfall requires another pair. -/
theorem mutation_returns_wrong_value (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let ready := { member with capabilities := capable, direct_endpoint := .Some endpoint }
    let members : Slice SessionMember := ⟨[ready], by scalar_tac⟩
    select_session_pair members .Relay true true = RustM.ok (.Mesh, .WebRtc) := by
  let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
  let ready := { member with capabilities := capable, direct_endpoint := .Some endpoint }
  let members : Slice SessionMember := ⟨[ready], by scalar_tac⟩
  change select_session_pair members .Relay true true = RustM.ok (.Mesh, .WebRtc)
  let predicate : (Topology × Transport) → Bool := fun _ => true
  have contract : CallbackContract
      select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool predicate
      (fun capture => capture = (.Relay, true, true, members)) := by
    intro capture rung valid
    subst capture
    rcases rung with ⟨topology, transport⟩
    refine ⟨_, ?_, rfl⟩
    cases topology <;> cases transport <;>
      simp [select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool.call_mut,
        select_session_pair.closure, topology_rank, transport_enabled, member_aggregation_contract,
        executability_contract, MemberSupport, DirectReady, HasEndpoint, predicate, members, ready, capable,
        Aeneas.Std.bind, Bind.bind]
  have result := selector_evaluation members .Relay true true predicate contract
  simpa [UPGRADE_LADDER, RELAY_FLOOR, Array.make, predicate] using result

end Capability
