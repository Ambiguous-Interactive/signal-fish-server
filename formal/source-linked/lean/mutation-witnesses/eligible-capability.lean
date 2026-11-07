import Capability.Verification.ElectionFixtures
import Capability.Verification.ConcreteCallbacks
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
noncomputable section
namespace Capability
open ElectionFixtures

private def rejected (member : SessionMember) : SessionMember :=
  { member with
    capabilities := ⟨2#u16, false, false, false, false, false, false⟩,
    direct_endpoint := .None }

/-- An old, unsupported singleton is wrongly elected by the extracted production wrapper. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    elect_eligible_host iterator (.Some member.player_id)
      (⟨[rejected member], by scalar_tac⟩ : Slice SessionMember) .Host .Direct =
      RustM.ok (.Some member.player_id) := by
  simp only [elect_eligible_host, iterator,
    CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter,
    CoreModels.core.iter.traits.iterator.Iterator.filter.default,
    CoreModels.core.iter.adapters.filter.Filter.new,
    Aeneas.Std.bind, Bind.bind]
  unfold elect_host
  simp only [CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter,
    Aeneas.Std.bind, Bind.bind]
  unfold elect_host_loop
  rw [loop]
  simp only [elect_host_loop.body,
    CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next,
    CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop]
  rw [loop]
  simp [CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop.body,
    next_cons,
    elect_eligible_host.closure.Insts.CoreOpsFunctionFnMutTupleSharedSharedSessionMemberBool.call_mut,
    SessionMember.can_host, SessionMember.supports_session, supports_value, MemberSupport,
    CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid, uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq,
    rejected, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]

end Capability
