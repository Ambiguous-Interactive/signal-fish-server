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

private theorem option_identity (id : uuid.Uuid) (authority : CoreModels.core.option.Option uuid.Uuid) :
    CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq
      uuid.Uuid.Insts.CoreCmpPartialEqUuid (.Some id) authority =
      RustM.ok (decide (CoreModels.core.option.Option.Some id = authority)) := by
  cases authority <;> simp [CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid, uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq]

private theorem optional_some {F T : Type} (callback : CoreModels.core.ops.function.FnOnce F T Bool)
    (value : T) (state : F) :
    CoreModels.core.option.Option.is_none_or callback (.Some value) state = callback.call_once state value := rfl

private theorem comparison_result (member : SessionMember) :
    (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iterator).call_once
      (second member) ((first member).joined_at, (first member).player_id) = RustM.ok false := by
  unfold elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool
  unfold elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool.call_once
  simp only [chrono.datetime.DateTime.Insts.CoreCmpPartialOrdDateTime,
    uuid.Uuid.Insts.CoreCmpPartialOrdUuid]
  rw [pair_key_comparison]
  have times := time_order member
  have reversed_time : ¬ ElectionOrder.timeKey (late member).joined_at < ElectionOrder.timeKey (early member).joined_at := not_lt_of_ge (le_of_lt times)
  have different_time : ElectionOrder.timeKey (late member).joined_at ≠ ElectionOrder.timeKey (early member).joined_at := ne_of_gt times
  simp only [first, second, reversed_time, different_time, false_and, false_or, decide_false]

private theorem advance_step
    (iter : CoreModels.core.iter.traits.collect.IntoIterator
      (Slice SessionMember) SessionMember (Slice SessionMember))
    (authority : CoreModels.core.option.Option uuid.Uuid)
    (members remaining : Slice SessionMember) (head : SessionMember)
    (current : CoreModels.core.option.Option (chrono.datetime.DateTime chrono.offset.utc.Utc chrono.offset.utc.Utc × uuid.Uuid))
    (replace : Bool)
    (next : iter.iteratorIteratorInst.next members = RustM.ok (.Some head, remaining))
    (comparison : CoreModels.core.option.Option.is_none_or
      (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iter) current head = RustM.ok replace) :
    elect_host_loop.body iter authority members current =
      RustM.ok (if replace then .cont (remaining, .Some (head.joined_at, head.player_id))
        else .cont (remaining, current)) := by
  unfold elect_host_loop.body
  rw [next]
  simp only [Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry, option_identity, ite_self]
  rw [comparison]
  cases replace <;> rfl

private theorem first_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator (.Some highId) (candidates member) .None =
        RustM.ok (.cont ((tail member), .Some ((first member).joined_at, (first member).player_id))) := by
  apply advance_step iterator (.Some highId) (candidates member) (tail member) (first member) .None true
  · exact next_cons _ _ _
  · rfl

private theorem second_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator (.Some highId) (tail member) (.Some ((first member).joined_at, (first member).player_id)) =
        RustM.ok (.cont (empty, .Some ((winner member).joined_at, (winner member).player_id))) := by
  have comparison : CoreModels.core.option.Option.is_none_or
      (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iterator)
      (.Some ((first member).joined_at, (first member).player_id)) (second member) = RustM.ok false := by
    rw [optional_some, comparison_result]
  have step := advance_step iterator (.Some highId) (tail member) empty (second member)
    (.Some ((first member).joined_at, (first member).player_id)) false (next_cons _ _ _) comparison
  simpa only [Bool.false_eq_true, ↓reduceIte, first, winner] using step

private theorem last_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator (.Some highId) empty (.Some ((winner member).joined_at, (winner member).player_id)) =
        RustM.ok (.done (.Some lowId)) := by
  simp [elect_host_loop.body, empty, next_empty,
    CoreModels.core.option.Option.map,
    elect_host.closure_1.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidUuid.call_once,
    winner, early, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry,
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
  simp only [first_step]
  rw [loop]
  simp only [second_step]
  rw [loop]
  simp only [last_step]

end Capability
