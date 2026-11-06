import Capability.Verification.ConcreteCallbacks

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member

namespace Capability

/-- The actual member method reads this member's stored capabilities. -/
theorem member_support_contract (member : SessionMember)
    (topology : Topology) (transport : Transport) :
    SessionMember.supports_session member topology transport =
      RustM.ok (MemberSupport member.capabilities topology transport) := by
  exact supports_value member.capabilities topology transport

/-- The actual production closure meets the getter contract without assumptions. -/
theorem member_getter_contract :
    GetterContract
      session_member.all_support.closure.Insts.CoreOpsFunctionFnTupleSharedSessionMemberSessionCapabilities
      () SessionMember.capabilities := by
  intro member
  rfl

/-- Every representable member slice, including empty input, with no abstract getter boundary. -/
theorem member_aggregation_contract (members : Slice SessionMember)
    (topology : Topology) (transport : Transport) :
    session_member.all_support members topology transport = RustM.ok
      (!members.val.isEmpty && members.val.all
        (fun member => MemberSupport member.capabilities topology transport)) := by
  exact concrete_aggregation_contract _ () SessionMember.capabilities
    member_getter_contract members topology transport

/-- Presence of the stored endpoint, independent of its contents. -/
def HasEndpoint (endpoint : core.option.Option DirectEndpoint) : Bool :=
  match endpoint with
  | .None => false
  | .Some _ => true

/-- Actual host eligibility requires both axes and v3; only Direct requires an endpoint. -/
theorem member_host_contract (member : SessionMember)
    (topology : Topology) (transport : Transport) :
    SessionMember.can_host member topology transport = RustM.ok
      (MemberSupport member.capabilities topology transport &&
        (match transport with
         | .Direct => HasEndpoint member.direct_endpoint
         | _ => true)) := by
  simp only [SessionMember.can_host, member_support_contract, bind_tc_ok]
  cases transport <;> cases member.direct_endpoint <;>
    simp [CoreModels.core.cmp.PartialEq.ne.default,
      Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
      CoreModels.core.option.Option.is_some, HasEndpoint, Aeneas.Std.bind, Bind.bind]
  all_goals (try split) <;> (try simp_all)

/-- Identity, name, authority and join time do not determine host eligibility. -/
theorem member_host_inputs (left right : SessionMember)
    (capabilities : left.capabilities = right.capabilities)
    (endpoint : HasEndpoint left.direct_endpoint = HasEndpoint right.direct_endpoint)
    (topology : Topology) (transport : Transport) :
    SessionMember.can_host left topology transport =
      SessionMember.can_host right topology transport := by
  simp only [member_host_contract, capabilities]
  cases transport <;> simp [endpoint]

/-- Every metadata value permits these reachable accepting and refusing snapshots. -/
theorem member_host_outcomes (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    SessionMember.can_host { member with capabilities := capable, direct_endpoint := .None }
      .Host .WebRtc = RustM.ok true ∧
    SessionMember.can_host { member with capabilities := capable, direct_endpoint := .None }
      .Host .Direct = RustM.ok false ∧
    SessionMember.can_host { member with capabilities := capable, direct_endpoint := .Some endpoint }
      .Host .Direct = RustM.ok true ∧
    SessionMember.can_host { member with
        capabilities := { capable with version := 2#u16 }, direct_endpoint := .Some endpoint }
      .Host .Direct = RustM.ok false ∧
    session_member.all_support (⟨[], by scalar_tac⟩ : Slice SessionMember)
      .Host .WebRtc = RustM.ok false ∧
    session_member.all_support
      (⟨[{ member with capabilities := capable, direct_endpoint := .None }], by scalar_tac⟩ : Slice SessionMember)
      .Host .Direct = RustM.ok true := by
  simp [member_host_contract, member_aggregation_contract, MemberSupport, HasEndpoint]

end Capability
