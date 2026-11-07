import Capability.Verification.ElectionFixtures
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member
noncomputable section
namespace Capability
open ElectionFixtures
attribute [local irreducible] ElectionOrder.timeKey ElectionOrder.uuidKey lowId highId

private def first (member : SessionMember) : SessionMember := early member
private def second (member : SessionMember) : SessionMember := late member
private def winner (member : SessionMember) : SessionMember := early member
private def candidates (member : SessionMember) : Slice SessionMember := ⟨[first member, second member], by scalar_tac⟩
private def tail (member : SessionMember) : Slice SessionMember := ⟨[second member], by scalar_tac⟩
private def empty : Slice SessionMember := ⟨[], by scalar_tac⟩

private theorem first_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator (.Some highId) (candidates member) .None =
        RustM.ok (.cont ((tail member), .Some ((first member).joined_at, (first member).player_id))) := by
  have times := time_order member
  have reversed_time : ¬ ElectionOrder.timeKey (late member).joined_at < ElectionOrder.timeKey (early member).joined_at := not_lt_of_ge (le_of_lt times)
  have different_time : ElectionOrder.timeKey (late member).joined_at ≠ ElectionOrder.timeKey (early member).joined_at := ne_of_gt times
  simp only [elect_host_loop.body, iterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    candidates, tail, empty, next_cons, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  simp [CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid, uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq,
    CoreModels.core.option.Option.is_none_or,
    elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool.call_once,
    chrono.datetime.DateTime.Insts.CoreCmpPartialOrdDateTime,
    uuid.Uuid.Insts.CoreCmpPartialOrdUuid, pair_key_comparison,
    first, second, winner, early, late, distinct, Ne.symm distinct, times, reversed_time, different_time,
    CoreModels.core.option.Option.Some, CoreModels.core.option.Option.None,
    Aeneas.Std.bind, Bind.bind]

private theorem second_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator (.Some highId) (tail member) (.Some ((first member).joined_at, (first member).player_id)) =
        RustM.ok (.cont (empty, .Some ((winner member).joined_at, (winner member).player_id))) := by
  have times := time_order member
  have reversed_time : ¬ ElectionOrder.timeKey (late member).joined_at < ElectionOrder.timeKey (early member).joined_at := not_lt_of_ge (le_of_lt times)
  have different_time : ElectionOrder.timeKey (late member).joined_at ≠ ElectionOrder.timeKey (early member).joined_at := ne_of_gt times
  simp only [elect_host_loop.body, iterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    candidates, tail, empty, next_cons, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  simp [CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid, uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq,
    CoreModels.core.option.Option.is_none_or,
    elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool.call_once,
    chrono.datetime.DateTime.Insts.CoreCmpPartialOrdDateTime,
    uuid.Uuid.Insts.CoreCmpPartialOrdUuid, pair_key_comparison,
    first, second, winner, early, late, distinct, Ne.symm distinct, times, reversed_time, different_time,
    CoreModels.core.option.Option.Some, CoreModels.core.option.Option.None,
    Aeneas.Std.bind, Bind.bind]

private theorem last_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator (.Some highId) empty (.Some ((winner member).joined_at, (winner member).player_id)) =
        RustM.ok (.done (.Some lowId)) := by
  simp [elect_host_loop.body, iterator, empty, next_empty,
    CoreModels.core.option.Option.map,
    elect_host.closure_1.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidUuid.call_once,
    winner, early, late, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry,
    CoreModels.core.option.Option.Some, CoreModels.core.option.Option.None]

/-- A two-candidate input exposes the production source fault without the rejected contract. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    elect_host iterator (.Some highId)
      (⟨[early member, late member], by scalar_tac⟩ : Slice SessionMember) = RustM.ok (.Some lowId) := by
  change elect_host iterator (.Some highId) (candidates member) = _
  simp only [elect_host, iterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter, Aeneas.Std.bind, Bind.bind]
  unfold elect_host_loop
  rw [loop]
  simp only [Aeneas.Std.uncurry, first_step]
  rw [loop]
  simp only [Aeneas.Std.uncurry, second_step]
  rw [loop]
  simp only [Aeneas.Std.uncurry, last_step]

end Capability
