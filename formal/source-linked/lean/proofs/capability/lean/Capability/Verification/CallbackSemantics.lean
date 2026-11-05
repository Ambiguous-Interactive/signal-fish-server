import Capability.Verification.Callbacks

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core

namespace Capability

/-- Specification steps record each incoming state and member in order.
A false result, failure, or divergence ends the trace. Final state is not
observable in the extracted iterator model. -/
inductive CallbackTrace {T F : Type} (callback : CoreModels.core.ops.function.FnMut F T Bool) :
    List T → F → List (F × T) → RustM Bool → Prop where
  | exhausted (state) : CallbackTrace callback [] state [] (RustM.ok true)
  | rejected {head tail state next}
      (step : callback.call_mut state head = RustM.ok (false, next)) :
      CallbackTrace callback (head :: tail) state [(state, head)] (RustM.ok false)
  | accepted {head tail state next observations outcome}
      (step : callback.call_mut state head = RustM.ok (true, next))
      (rest : CallbackTrace callback tail next observations outcome) :
      CallbackTrace callback (head :: tail) state ((state, head) :: observations) outcome
  | failed {head tail state error}
      (step : callback.call_mut state head = RustM.fail error) :
      CallbackTrace callback (head :: tail) state [(state, head)] (RustM.fail error)
  | diverged {head tail state}
      (step : callback.call_mut state head = RustM.div) :
      CallbackTrace callback (head :: tail) state [(state, head)] RustM.div

/-- Every finite input has a trace, without a totality or fixed-predicate assumption. -/
theorem callback_trace_exists {T F : Type}
    (callback : CoreModels.core.ops.function.FnMut F T Bool) (xs : List T) (state : F) :
    ∃ observations outcome, CallbackTrace callback xs state observations outcome := by
  induction xs generalizing state with
  | nil => exact ⟨[], RustM.ok true, .exhausted state⟩
  | cons head tail ih =>
    cases step : callback.call_mut state head with
    | ok value =>
      obtain ⟨result, next⟩ := value
      cases result with
      | false => exact ⟨_, _, .rejected step⟩
      | true =>
        obtain ⟨observations, outcome, rest⟩ := ih next
        exact ⟨_, _, .accepted step rest⟩
    | fail error => exact ⟨_, _, .failed step⟩
    | div => exact ⟨_, _, .diverged step⟩

private theorem iterator_trace_contract {T F : Type}
    {callback : CoreModels.core.ops.function.FnMut F T Bool} {xs : List T} {state : F}
    {observations : List (F × T)} {outcome : RustM Bool}
    (trace : CallbackTrace callback xs state observations outcome)
    (bound : xs.length ≤ Usize.max) :
    (do let (result, _) ← CoreModels.core.iter.traits.iterator.iter_all_loop
          (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT T)
          callback (⟨xs, bound⟩ : Slice T) state
        RustM.ok result) = outcome := by
  induction trace with
  | exhausted state =>
    unfold CoreModels.core.iter.traits.iterator.iter_all_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
      next_empty, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | rejected step =>
    unfold CoreModels.core.iter.traits.iterator.iter_all_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
      next_cons, step, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | accepted step rest ih =>
    unfold CoreModels.core.iter.traits.iterator.iter_all_loop
    rw [loop]
    simp only [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
      next_cons, step, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    exact ih (by simp only [List.length_cons] at bound; omega)
  | failed step =>
    unfold CoreModels.core.iter.traits.iterator.iter_all_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
      next_cons, step, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | diverged step =>
    unfold CoreModels.core.iter.traits.iterator.iter_all_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
      next_cons, step, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]

/-- The production result agrees with the operational callback trace,
including state-dependent truth values, failure and divergence. Empty input
returns false. This result does not expose an external event trace. -/
theorem callback_trace_contract {T F : Type}
    {callback : CoreModels.core.ops.function.FnMut F T Bool} (members : Slice T) (state : F)
    {observations : List (F × T)} {outcome : RustM Bool}
    (trace : CallbackTrace callback members.val state observations outcome) :
    protocol_core.nonempty_all_support callback members state =
      if members.val.isEmpty then RustM.ok false else outcome := by
  have follows := iterator_trace_contract trace members.property
  unfold protocol_core.nonempty_all_support
  simp only [CoreModels.core.slice.Slice.is_empty, CoreModels.core.slice.Slice.len,
    rust_primitives.slice.slice_length, CoreModels.core.slice.Slice.iter,
    rust_primitives.sequence.seq_from_slice, Aeneas.Std.bind, Bind.bind,
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all,
    CoreModels.core.iter.traits.iterator.Iterator.all.default, CoreModels.core.iter.traits.iterator.iter_all]
  by_cases empty : members.val = []
  · simp [Slice.len, empty, Usize.ofNatCore, UScalar.ofNatCore]
  · have nonzero : Slice.len members ≠ 0#usize := by
      intro eq
      have lengths := congrArg UScalar.val eq
      have : members.val.length = 0 := by simpa [Slice.len] using lengths
      exact empty (List.length_eq_zero_iff.mp this)
    simpa [nonzero, empty, Aeneas.Std.uncurry, Aeneas.Std.bind, Bind.bind] using follows

/-- State threading distinguishes both member order and repeated invocations. -/
theorem callback_order_outcomes :
    protocol_core.nonempty_all_support orderedCounter (⟨[0, 1], by scalar_tac⟩ : Slice Nat) 0 =
      RustM.ok true ∧
    protocol_core.nonempty_all_support orderedCounter (⟨[1, 0], by scalar_tac⟩ : Slice Nat) 0 =
      RustM.ok false := by
  constructor
  · have trace : CallbackTrace orderedCounter [0, 1] 0 [(0, 0), (1, 1)] (RustM.ok true) :=
      .accepted rfl (.accepted rfl (.exhausted 2))
    exact callback_trace_contract _ _ trace
  · have trace : CallbackTrace orderedCounter [1, 0] 0 [(0, 1)] (RustM.ok false) :=
      .rejected rfl
    exact callback_trace_contract _ _ trace

/-- A true prefix propagates the next callback failure or divergence;
a false prefix shields either suffix. -/
theorem callback_failure_outcomes :
    protocol_core.nonempty_all_support partialCallback (⟨[0, 2], by scalar_tac⟩ : Slice Nat) () =
      RustM.fail .panic ∧
    protocol_core.nonempty_all_support partialCallback (⟨[0, 3], by scalar_tac⟩ : Slice Nat) () =
      RustM.div ∧
    protocol_core.nonempty_all_support partialCallback (⟨[1, 2], by scalar_tac⟩ : Slice Nat) () =
      RustM.ok false ∧
    protocol_core.nonempty_all_support partialCallback (⟨[1, 3], by scalar_tac⟩ : Slice Nat) () =
      RustM.ok false := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · have trace : CallbackTrace partialCallback [0, 2] () [((), 0), ((), 2)] (RustM.fail .panic) :=
      .accepted rfl (.failed rfl)
    exact callback_trace_contract _ _ trace
  · have trace : CallbackTrace partialCallback [0, 3] () [((), 0), ((), 3)] RustM.div :=
      .accepted rfl (.diverged rfl)
    exact callback_trace_contract _ _ trace
  · have trace : CallbackTrace partialCallback [1, 2] () [((), 1)] (RustM.ok false) :=
      .rejected rfl
    exact callback_trace_contract _ _ trace
  · have trace : CallbackTrace partialCallback [1, 3] () [((), 1)] (RustM.ok false) :=
      .rejected rfl
    exact callback_trace_contract _ _ trace

end Capability
