import Capability.Verification.Members
import Capability.Verification.Search
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability
/-- A stored endpoint cannot make a v2 member a Direct host. -/
theorem mutation_returns_wrong_value (member : SessionMember) (endpoint : DirectEndpoint) :
    let candidate := { member with capabilities := ⟨2#u16, true, true, true, true, true, true⟩,
      direct_endpoint := .Some endpoint }
    rung_is_executable (⟨[candidate], by scalar_tac⟩ : Slice SessionMember) .Host .Direct = RustM.ok true := by
  let candidate := { member with capabilities := ⟨2#u16, true, true, true, true, true, true⟩,
    direct_endpoint := .Some endpoint }
  let predicate := fun member : SessionMember =>
    (MemberSupport member.capabilities .Host .Direct && HasEndpoint member.direct_endpoint) ||
      HasEndpoint member.direct_endpoint
  have callback : CallbackContract
      rung_is_executable.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool
      predicate (fun capture => capture = (.Host, .Direct)) := by
    intro capture member valid
    subst capture
    refine ⟨(.Host, .Direct), ?_, rfl⟩
    simp [rung_is_executable.closure.Insts.CoreOpsFunctionFnMutTupleSharedSessionMemberBool.call_mut,
      member_host_contract, CoreModels.core.option.Option.is_some, predicate, HasEndpoint,
      Aeneas.Std.bind, Bind.bind]
    try split <;> simp_all
  obtain ⟨remaining, iterates⟩ := any_iterator_contract _ _ _ callback
    [candidate] (by scalar_tac) (.Host, .Direct) rfl
  have returned := congrArg (fun result : RustM (Bool × CoreModels.core.slice.iter.Iter SessionMember) =>
    Bind.bind result (fun pair => RustM.ok pair.1)) iterates
  simpa [rung_is_executable, CoreModels.core.cmp.PartialEq.ne.default,
    Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
    CoreModels.core.slice.Slice.iter, rust_primitives.sequence.seq_from_slice,
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.any,
    CoreModels.core.iter.traits.iterator.Iterator.any.default, CoreModels.core.iter.traits.iterator.iter_any,
    candidate, predicate, MemberSupport, HasEndpoint, Aeneas.Std.bind, Bind.bind] using returned
end Capability
