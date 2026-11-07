import Capability.Verification.ElectionFixtures
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member
noncomputable section
namespace Capability
open ElectionFixtures
attribute [local irreducible] ElectionOrder.timeKey ElectionOrder.uuidKey lowId highId

private def first (member : SessionMember) : SessionMember := early member
private def second (member : SessionMember) : SessionMember := late member
private def winner (member : SessionMember) : SessionMember := late member
private def candidates (member : SessionMember) : Slice SessionMember := ⟨[first member, second member], by scalar_tac⟩
private def tail (member : SessionMember) : Slice SessionMember := ⟨[second member], by scalar_tac⟩
private def empty : Slice SessionMember := ⟨[], by scalar_tac⟩

private theorem option_eq_none (id : uuid.Uuid) :
    CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq
      uuid.Uuid.Insts.CoreCmpPartialEqUuid (.Some id) .None = RustM.ok false := rfl

private theorem optional_some {F T : Type} (callback : CoreModels.core.ops.function.FnOnce F T Bool)
    (value : T) (state : F) :
    CoreModels.core.option.Option.is_none_or callback (.Some value) state = callback.call_once state value := rfl

private theorem comparison_result (member : SessionMember) :
    (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iterator).call_once
      (second member) ((first member).joined_at, (first member).player_id) = RustM.ok true := by
  unfold elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool
  unfold elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool.call_once
  simp only [chrono.datetime.DateTime.Insts.CoreCmpPartialOrdDateTime,
    uuid.Uuid.Insts.CoreCmpPartialOrdUuid]
  rw [pair_key_comparison]
  simp only [first, second, time_order, true_or, decide_true]

private theorem advance_step
    (iter : CoreModels.core.iter.traits.collect.IntoIterator
      (Slice SessionMember) SessionMember (Slice SessionMember))
    (members remaining : Slice SessionMember) (head : SessionMember)
    (current : CoreModels.core.option.Option (chrono.datetime.DateTime chrono.offset.utc.Utc chrono.offset.utc.Utc × uuid.Uuid))
    (replace : Bool)
    (next : iter.iteratorIteratorInst.next members = RustM.ok (.Some head, remaining))
    (comparison : CoreModels.core.option.Option.is_none_or
      (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iter) current head = RustM.ok replace) :
    elect_host_loop.body iter .None members current =
      RustM.ok (if replace then .cont (remaining, .Some (head.joined_at, head.player_id))
        else .cont (remaining, current)) := by
  unfold elect_host_loop.body
  rw [next]
  simp only [Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry, option_eq_none, Bool.false_eq_true, ↓reduceIte]
  rw [comparison]
  cases replace <;> rfl

private theorem first_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator .None (candidates member) .None =
        RustM.ok (.cont ((tail member), .Some ((first member).joined_at, (first member).player_id))) := by
  apply advance_step iterator (candidates member) (tail member) (first member) .None true
  · exact next_cons _ _ _
  · rfl

private theorem second_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator .None (tail member) (.Some ((first member).joined_at, (first member).player_id)) =
        RustM.ok (.cont (empty, .Some ((winner member).joined_at, (winner member).player_id))) := by
  have comparison : CoreModels.core.option.Option.is_none_or
      (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iterator)
      (.Some ((first member).joined_at, (first member).player_id)) (second member) = RustM.ok true := by
    rw [optional_some, comparison_result]
  have step := advance_step iterator (tail member) empty (second member)
    (.Some ((first member).joined_at, (first member).player_id)) true (next_cons _ _ _) comparison
  simpa only [Bool.true_eq, ↓reduceIte, second, winner] using step

private theorem last_step (member : SessionMember) :
    elect_host_loop.body (T1 := Slice SessionMember) (Clause0_IntoIter := Slice SessionMember)
      iterator .None empty (.Some ((winner member).joined_at, (winner member).player_id)) =
        RustM.ok (.done (.Some highId)) := by
  simp [elect_host_loop.body, empty, next_empty,
    CoreModels.core.option.Option.map,
    elect_host.closure_1.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidUuid.call_once,
    winner, late, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry,
    CoreModels.core.option.Option.Some, CoreModels.core.option.Option.None]

private theorem election_result (member : SessionMember) :
    elect_host iterator .None (candidates member) = RustM.ok (.Some highId) := by
  rw [election_start iterator .None (candidates member) (candidates member) (by rfl)]
  unfold elect_host_loop
  apply three_steps _ _
    ((tail member), .Some ((first member).joined_at, (first member).player_id))
    (empty, .Some ((winner member).joined_at, (winner member).player_id))
  · exact first_step member
  · exact second_step member
  · exact last_step member

/-- A two-candidate input exposes the production source fault without the rejected contract. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    elect_host iterator .None
      (⟨[early member, late member], by scalar_tac⟩ : Slice SessionMember) = RustM.ok (.Some highId) := by
  simpa only [candidates, first, second] using election_result member

end Capability
