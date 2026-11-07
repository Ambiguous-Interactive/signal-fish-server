import Capability.Verification.Members
import Capability.Verification.Search
import Capability.Verification.Election

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member session_admission

noncomputable section
namespace Capability

/-- The saved identity must name a member eligible for the saved session pair. -/
def HostMatch (stored : ActiveSessionPlan) (host : uuid.Uuid) (member : SessionMember) : Bool :=
  decide (member.player_id = host) &&
    (MemberSupport member.capabilities stored.topology stored.transport &&
      (match stored.transport with
       | .Direct => HasEndpoint member.direct_endpoint
       | _ => true))

/-- The extracted closure tests identity and the actual member host predicate. -/
theorem host_match_contract (stored : ActiveSessionPlan) (host : uuid.Uuid) :
    CallbackContract
      (ActiveSessionPlan.host_invalid.closure.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool electionIterator)
      (HostMatch stored host) (fun capture => capture = (host, stored)) := by
  intro capture member valid
  subst capture
  refine ⟨(host, stored), ?_, rfl⟩
  simp [ActiveSessionPlan.host_invalid.closure.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool.call_mut,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq, ActiveSessionPlan.supported_by,
    member_host_contract, HostMatch, Aeneas.Std.bind, Bind.bind]
  try
    split
    · simp_all
      rfl
    · simp_all

private theorem host_search (stored : ActiveSessionPlan) (host : uuid.Uuid)
    (members : Slice SessionMember) :
    ActiveSessionPlan.host_invalid.closure.Insts.CoreOpsFunctionFnOnceTupleUuidBool.call_once
      electionIterator (members, stored) host = RustM.ok (members.val.any (HostMatch stored host)) := by
  obtain ⟨remaining, iterates⟩ := any_iterator_contract _ _ _
    (host_match_contract stored host) members.val members.property (host, stored) rfl
  change CoreModels.core.iter.traits.iterator.iter_any_loop
    (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT SessionMember)
    _ members (host, stored) = _ at iterates
  simp only [ActiveSessionPlan.host_invalid.closure.Insts.CoreOpsFunctionFnOnceTupleUuidBool.call_once,
    electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    CoreModels.core.iter.traits.iterator.Iterator.any,
    CoreModels.core.iter.traits.iterator.Iterator.any.default,
    CoreModels.core.iter.traits.iterator.iter_any, Aeneas.Std.bind, Bind.bind]
  change (do
    let (result, _) ← CoreModels.core.iter.traits.iterator.iter_any_loop
      (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT SessionMember)
      (ActiveSessionPlan.host_invalid.closure.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool electionIterator)
      members (host, stored)
    pure result) = _
  exact congrArg (fun result : RustM (Bool × CoreModels.core.slice.iter.Iter SessionMember) =>
    Bind.bind result (fun pair => RustM.ok pair.1)) iterates

/-- All representable finite member slices, every saved pair and optional identity.
This theorem uses the explicitly trusted UUID equality mapping. -/
theorem host_validity_contract (stored : ActiveSessionPlan) (members : Slice SessionMember) :
    ActiveSessionPlan.host_invalid electionIterator stored members = RustM.ok
      (match stored.topology with
       | .Host => match stored.host with
         | .None => true
         | .Some host => !(members.val.any (HostMatch stored host))
       | _ => false) := by
  cases topology : stored.topology <;> cases host : stored.host <;>
    simp [ActiveSessionPlan.host_invalid, Topology.Insts.CoreCmpPartialEqTopology.eq,
      Topology.read_discriminant, topology, host, CoreModels.core.option.Option.is_some_and,
      host_search, Aeneas.Std.bind, Bind.bind]
  have not_any (xs : List SessionMember) (predicate : SessionMember → Bool) :
      decide (∀ x ∈ xs, predicate x = false) = !xs.any predicate := by
    have equivalent : (∀ x ∈ xs, predicate x = false) ↔ xs.any predicate = false := by
      simpa only [Bool.not_eq_true] using
        (List.any_eq_false (p := predicate) (l := xs)).symm
    simp only [equivalent]
    cases xs.any predicate <;> rfl
  try exact not_any _ _

/-- A valid Host snapshot has an actual matching eligible member; non-Host plans
need no host. No uniqueness or legal-pair assumption hides malformed snapshots. -/
theorem host_validity_witness (stored : ActiveSessionPlan) (members : Slice SessionMember) :
    ActiveSessionPlan.host_invalid electionIterator stored members = RustM.ok false ↔
      stored.topology ≠ .Host ∨ ∃ host, stored.host = .Some host ∧
        ∃ member ∈ members.val, HostMatch stored host member = true := by
  rw [host_validity_contract]
  cases stored.topology <;> cases stored.host <;> simp

/-- Saved-host presence, v2 refusal, Direct endpoint readiness, empty membership,
and host-free non-Host plans have reachable outcomes for arbitrary member metadata. -/
theorem host_validity_outcomes (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let ready := { member with capabilities := capable, direct_endpoint := .None }
    let alone : Slice SessionMember := ⟨[ready], by scalar_tac⟩
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .WebRtc, .Some member.player_id⟩ alone = RustM.ok false ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .WebRtc, .None⟩ alone = RustM.ok true ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .Direct, .Some member.player_id⟩ alone = RustM.ok true ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .Direct, .Some member.player_id⟩
      (⟨[{ ready with direct_endpoint := .Some endpoint }], by scalar_tac⟩ : Slice SessionMember) = RustM.ok false ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .WebRtc, .Some member.player_id⟩
      (⟨[], by scalar_tac⟩ : Slice SessionMember) = RustM.ok true ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .WebRtc, .Some member.player_id⟩
      (⟨[{ ready with capabilities := { capable with version := 2#u16 } }], by scalar_tac⟩ : Slice SessionMember) = RustM.ok true ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Relay, .Direct, .None⟩ alone = RustM.ok false ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Mesh, .Direct, .None⟩ alone = RustM.ok false := by
  simp [host_validity_contract, HostMatch, MemberSupport, HasEndpoint]

end Capability
