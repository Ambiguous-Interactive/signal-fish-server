import Capability.Verification.Members
import Capability.Verification.Election
import Capability.Verification.Search
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member session_admission
noncomputable section
namespace Capability

/-- A capable bystander with different UUID bytes wrongly validates the saved host. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    let saved : uuid.Uuid := ⟨⟨List.replicate 16 0#u8, by simp⟩⟩
    let bystander : uuid.Uuid := ⟨⟨List.replicate 16 1#u8, by simp⟩⟩
    saved ≠ bystander ∧
    ActiveSessionPlan.host_invalid electionIterator ⟨.Host, .WebRtc, .Some saved⟩
      (⟨[{ member with player_id := bystander, capabilities := ⟨3#u16, true, true, true, true, true, true⟩ }], by scalar_tac⟩ : Slice SessionMember) =
      RustM.ok false := by
  let saved : uuid.Uuid := ⟨⟨List.replicate 16 0#u8, by simp⟩⟩
  let bystander : uuid.Uuid := ⟨⟨List.replicate 16 1#u8, by simp⟩⟩
  constructor
  · intro equal
    have bytes := congrArg (fun id : uuid.Uuid => id.bytes.val.head?) equal
    simp at bytes
  · let candidate := { member with player_id := bystander, capabilities := ⟨3#u16, true, true, true, true, true, true⟩ }
    let stored : ActiveSessionPlan := ⟨.Host, .WebRtc, .Some saved⟩
    have callback : CallbackContract
        (ActiveSessionPlan.host_invalid.closure.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool electionIterator)
        (fun candidate => MemberSupport candidate.capabilities .Host .WebRtc) (fun capture => capture = (saved, stored)) := by
      intro capture candidate valid
      subst capture
      refine ⟨(saved, stored), ?_, rfl⟩
      by_cases same : candidate.player_id = saved <;>
        simp [ActiveSessionPlan.host_invalid.closure.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool.call_mut,
          uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq, ActiveSessionPlan.supported_by,
          member_host_contract, stored, same, Aeneas.Std.bind, Bind.bind]
    obtain ⟨remaining, iterates⟩ := any_iterator_contract _ _ _ callback
      [candidate] (by scalar_tac) (saved, stored) rfl
    change @CoreModels.core.iter.traits.iterator.iter_any_loop (Slice SessionMember)
      (ActiveSessionPlan.host_invalid.closure.closure (Slice SessionMember) (Slice SessionMember)) SessionMember
      (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT SessionMember)
      (ActiveSessionPlan.host_invalid.closure.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool electionIterator)
      (⟨[candidate], by scalar_tac⟩ : Slice SessionMember) (saved, stored) = _ at iterates
    have returned := congrArg (fun result : RustM (Bool × Slice SessionMember) =>
      Bind.bind (Bind.bind result (fun pair => RustM.ok pair.1))
        (fun found => RustM.ok (!found))) iterates
    simpa [ActiveSessionPlan.host_invalid, Topology.Insts.CoreCmpPartialEqTopology.eq,
        Topology.read_discriminant, CoreModels.core.option.Option.is_some_and,
        ActiveSessionPlan.host_invalid.closure.Insts.CoreOpsFunctionFnOnceTupleUuidBool.call_once,
        electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
        CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter,
        CoreModels.core.iter.traits.iterator.Iterator.any,
        CoreModels.core.iter.traits.iterator.Iterator.any.default, CoreModels.core.iter.traits.iterator.iter_any,
        candidate, stored, saved, bystander, MemberSupport, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry] using returned
end Capability
