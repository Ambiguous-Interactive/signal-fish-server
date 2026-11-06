import Capability.Verification.Members
import Capability.Verification.Search

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member

namespace Capability

/-- Direct execution needs one member with both axes, v3 and a stored endpoint. -/
def DirectReady (topology : Topology) (member : SessionMember) : Bool :=
  MemberSupport member.capabilities topology .Direct && HasEndpoint member.direct_endpoint

/-- The actual selector callback checks the actual member host predicate. -/
theorem executable_callback_contract (topology : Topology) (transport : Transport) :
    CallbackContract
      rung_is_executable.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool
      (fun member => MemberSupport member.capabilities topology transport &&
        (match transport with | .Direct => HasEndpoint member.direct_endpoint | _ => true))
      (fun capture => capture = (topology, transport)) := by
  intro capture member valid
  subst capture
  refine ⟨(topology, transport), ?_, rfl⟩
  simp [rung_is_executable.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool.call_mut,
    member_host_contract, Aeneas.Std.bind, Bind.bind]
  rfl

private theorem executable_search (members : Slice SessionMember)
    (topology : Topology) (transport : Transport) :
    (do
      let i ← CoreModels.core.slice.Slice.iter members
      let (result, _) ← CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.any
        rung_is_executable.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool
        i (topology, transport)
      pure result) = RustM.ok (members.val.any (fun member =>
        MemberSupport member.capabilities topology transport &&
          (match transport with | .Direct => HasEndpoint member.direct_endpoint | _ => true))) := by
  obtain ⟨remaining, iterates⟩ := any_iterator_contract _ _ _
    (executable_callback_contract topology transport) members.val members.property (topology, transport) rfl
  change CoreModels.core.iter.traits.iterator.iter_any_loop
    (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT SessionMember)
    _ members (topology, transport) = _ at iterates
  simp only [CoreModels.core.slice.Slice.iter, rust_primitives.sequence.seq_from_slice,
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.any,
    CoreModels.core.iter.traits.iterator.Iterator.any.default,
    CoreModels.core.iter.traits.iterator.iter_any, Aeneas.Std.bind, Bind.bind]
  exact congrArg (fun result : RustM (Bool × CoreModels.core.slice.iter.Iter SessionMember) =>
    Bind.bind result (fun pair => RustM.ok pair.1)) iterates

/-- All representable finite slices and every pair, with no legal-pair assumption.
This extra gate does not establish unanimous support or select the elected host. -/
theorem executability_contract (members : Slice SessionMember)
    (topology : Topology) (transport : Transport) :
    rung_is_executable members topology transport = RustM.ok
      (match transport with
       | .Direct => members.val.any (DirectReady topology)
       | _ => true) := by
  cases transport <;>
    simp [rung_is_executable, CoreModels.core.cmp.PartialEq.ne.default,
      Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
      Aeneas.Std.bind, Bind.bind]
  exact executable_search members topology .Direct

/-- A Direct rung cannot pass without an actual eligible member, at any length. -/
theorem direct_executable_witness (members : Slice SessionMember) (topology : Topology) :
    rung_is_executable members topology .Direct = RustM.ok true ↔
      ∃ member ∈ members.val, DirectReady topology member = true := by
  rw [executability_contract]
  simp

/-- Both Direct outcomes, empty input, old versions and a later ready member are reachable. -/
theorem executability_outcomes (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let unready := { member with capabilities := capable, direct_endpoint := .None }
    let ready := { unready with direct_endpoint := .Some endpoint }
    rung_is_executable (⟨[], by scalar_tac⟩ : Slice SessionMember) .Host .Direct = RustM.ok false ∧
    rung_is_executable (⟨[], by scalar_tac⟩ : Slice SessionMember) .Host .WebRtc = RustM.ok true ∧
    rung_is_executable (⟨[unready], by scalar_tac⟩ : Slice SessionMember) .Host .Direct = RustM.ok false ∧
    rung_is_executable (⟨[unready, ready], by scalar_tac⟩ : Slice SessionMember) .Host .Direct = RustM.ok true ∧
    rung_is_executable (⟨[{ ready with capabilities := { capable with version := 2#u16 } }], by scalar_tac⟩ : Slice SessionMember)
      .Host .Direct = RustM.ok false := by
  simp [executability_contract, DirectReady, MemberSupport, HasEndpoint]

end Capability
