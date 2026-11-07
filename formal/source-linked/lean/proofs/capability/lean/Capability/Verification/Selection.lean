import Capability.Verification.SelectionEvaluation

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member

namespace Capability

/-- Protocol richness is independent of enum declaration order. -/
def Rank : Topology → Nat
  | .Relay => 0 | .Host => 1 | .Mesh => 2

/-- An allowed rung needs the ceiling, transport switch, every member and execution readiness. -/
def RungFits (members : List SessionMember) (desired : Topology)
    (direct webrtc : Bool) (rung : Topology × Transport) : Bool :=
  decide (Rank rung.1 ≤ Rank desired) &&
    (match rung.2 with | .Relay => true | .Direct => direct | .WebRtc => webrtc) &&
    (!members.isEmpty && members.all (fun member => MemberSupport member.capabilities rung.1 rung.2)) &&
    (match rung.2 with | .Direct => members.any (DirectReady rung.1) | _ => true)

/-- Actual production callback, for all member lengths, pairs, ceilings and switches. -/
theorem selection_callback_contract (members : Slice SessionMember) (desired : Topology)
    (direct webrtc : Bool) (rung : Topology × Transport) :
    select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool.call_mut
      (desired, direct, webrtc, members) rung = RustM.ok
        (RungFits members.val desired direct webrtc rung, (desired, direct, webrtc, members)) := by
  rcases rung with ⟨topology, transport⟩
  cases topology <;> cases desired <;> cases transport <;>
    simp [select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool.call_mut,
      select_session_pair.closure, topology_rank, transport_enabled,
      member_aggregation_contract, executability_contract,
      RungFits, Rank, Aeneas.Std.bind, Bind.bind]
  all_goals try (split_ifs <;> simp_all)

/-- Independent ADR order; a changed production ladder must fail this contract. -/
def ProtocolLadder : List (Topology × Transport) :=
  [(.Mesh, .WebRtc), (.Host, .WebRtc), (.Host, .Direct)]

/-- Exact first-fit semantics for the actual production selector. -/
theorem selection_contract (members : Slice SessionMember) (desired : Topology)
    (direct webrtc : Bool) :
    select_session_pair members desired direct webrtc = RustM.ok
      ((ProtocolLadder.find? (RungFits members.val desired direct webrtc)).getD (.Relay, .Relay)) := by
  have contract : CallbackContract
      select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool
      (RungFits members.val desired direct webrtc)
      (fun capture => capture = (desired, direct, webrtc, members)) := by
    intro capture rung valid
    subst capture
    exact ⟨_, selection_callback_contract members desired direct webrtc rung, rfl⟩
  have actual := selector_evaluation members desired direct webrtc _ contract
  -- Preserve a semantic goal when a source fault changes the production constants.
  try simpa [UPGRADE_LADDER, RELAY_FLOOR, Array.make, ProtocolLadder] using actual

/-- Equivalent explicit protocol waterfall, with no assumption that a rung is enabled. -/
theorem selection_waterfall_contract (members : Slice SessionMember) (desired : Topology)
    (direct webrtc : Bool) :
    let fits := RungFits members.val desired direct webrtc
    select_session_pair members desired direct webrtc = RustM.ok
      (if fits (.Mesh, .WebRtc) then (.Mesh, .WebRtc)
       else if fits (.Host, .WebRtc) then (.Host, .WebRtc)
       else if fits (.Host, .Direct) then (.Host, .Direct)
       else (.Relay, .Relay)) := by
  rw [selection_contract]
  simp only [ProtocolLadder, List.find?_cons, List.find?_nil]
  split <;> simp_all
  split <;> simp_all
  split <;> simp_all

/-- Relay is selected exactly when every upgrade rung fails its protocol gates. -/
theorem selection_fallback_contract (members : Slice SessionMember) (desired : Topology)
    (direct webrtc : Bool) :
    select_session_pair members desired direct webrtc = RustM.ok (.Relay, .Relay) ↔
      ∀ rung ∈ ProtocolLadder, RungFits members.val desired direct webrtc rung = false := by
  rw [selection_waterfall_contract]
  simp only [ProtocolLadder, List.mem_cons, forall_eq_or_imp]
  repeat' (first | split | simp_all)

/-- Every upgrade passes the complete member/ceiling/enablement/readiness predicate. -/
theorem selection_upgrade_contract (members : Slice SessionMember) (desired : Topology)
    (direct webrtc : Bool) (rung : Topology × Transport)
    (upgrade : rung ≠ (.Relay, .Relay))
    (selected : select_session_pair members desired direct webrtc = RustM.ok rung) :
    rung ∈ ProtocolLadder ∧ RungFits members.val desired direct webrtc rung = true := by
  rw [selection_waterfall_contract] at selected
  simp only [ProtocolLadder, List.mem_cons, List.mem_nil_iff, or_false]
  repeat' (first | split at selected | simp_all)

/-- All rungs, empty input, v2 refusal, ceiling downgrade and missing readiness are reachable. -/
theorem selection_outcomes (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let ready := { member with capabilities := capable, direct_endpoint := .Some endpoint }
    select_session_pair (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Mesh true true = RustM.ok (.Mesh, .WebRtc) ∧
    select_session_pair (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host true true = RustM.ok (.Host, .WebRtc) ∧
    select_session_pair (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host true false = RustM.ok (.Host, .Direct) ∧
    select_session_pair (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Relay true true = RustM.ok (.Relay, .Relay) ∧
    select_session_pair (⟨[], by scalar_tac⟩ : Slice SessionMember) .Mesh true true = RustM.ok (.Relay, .Relay) ∧
    select_session_pair (⟨[{ ready with capabilities := { capable with mesh_topology := false } }], by scalar_tac⟩ : Slice SessionMember)
      .Mesh true true = RustM.ok (.Host, .WebRtc) ∧
    select_session_pair (⟨[{ ready with capabilities := { capable with version := 2#u16 } }], by scalar_tac⟩ : Slice SessionMember)
      .Mesh true true = RustM.ok (.Relay, .Relay) ∧
    select_session_pair (⟨[{ ready with direct_endpoint := .None }], by scalar_tac⟩ : Slice SessionMember)
      .Host true false = RustM.ok (.Relay, .Relay) := by
  simp [selection_contract, ProtocolLadder, RungFits, Rank, MemberSupport, DirectReady, HasEndpoint]

end Capability
