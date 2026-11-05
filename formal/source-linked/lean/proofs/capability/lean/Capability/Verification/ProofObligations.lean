import Capability.Verification.Callbacks

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core

namespace Capability

/-- A callback can update its private state, but must terminate successfully,
return the fixed member predicate, and preserve its state invariant.
Failure, divergence, and state-dependent truth values do not satisfy this contract. -/
def CallbackContract {T F : Type} (callback : CoreModels.core.ops.function.FnMut F T Bool)
    (predicate : T → Bool) (invariant : F → Prop) : Prop :=
  ∀ state member, invariant state → ∃ next,
    callback.call_mut state member = RustM.ok (predicate member, next) ∧ invariant next

private theorem next_empty {T : Type} (members : Slice T) (empty : members.val = []) :
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next members =
      RustM.ok (.None, members) := by
  simp [CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next,
    rust_primitives.sequence.seq_len, Slice.len, empty, Usize.ofNatCore, UScalar.ofNatCore]
  rfl

private theorem next_cons {T : Type} (head : T) (tail : List T)
    (bound : (head :: tail).length ≤ Usize.max) :
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next
      (⟨head :: tail, bound⟩ : Slice T) =
      RustM.ok (.Some head, (⟨tail, by simp only [List.length_cons] at bound; omega⟩ : Slice T)) := by
  simp [CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next,
    rust_primitives.sequence.seq_len, rust_primitives.sequence.seq_remove, Slice.len]
  rfl

private theorem iterator_contract {T F : Type}
    (callback : CoreModels.core.ops.function.FnMut F T Bool) (predicate : T → Bool)
    (invariant : F → Prop) (contract : CallbackContract callback predicate invariant)
    (xs : List T) (bound : xs.length ≤ Usize.max) (state : F) (valid : invariant state) :
    ∃ remaining, CoreModels.core.iter.traits.iterator.iter_all_loop
      (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT T)
      callback (⟨xs, bound⟩ : Slice T) state = RustM.ok (xs.all predicate, remaining) := by
  induction xs generalizing state with
  | nil =>
    refine ⟨⟨[], bound⟩, ?_⟩
    unfold CoreModels.core.iter.traits.iterator.iter_all_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
      next_empty, List.all_nil, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | cons head tail ih =>
    obtain ⟨next, returns, valid_next⟩ := contract state head valid
    have tail_bound : tail.length ≤ Usize.max := by simp only [List.length_cons] at bound; omega
    obtain ⟨remaining, recursive⟩ := ih tail_bound next valid_next
    cases result : predicate head with
    | false =>
      refine ⟨⟨tail, tail_bound⟩, ?_⟩
      unfold CoreModels.core.iter.traits.iterator.iter_all_loop
      rw [loop]
      simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
          next_cons, returns, result, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    | true =>
      refine ⟨remaining, ?_⟩
      unfold CoreModels.core.iter.traits.iterator.iter_all_loop
      rw [loop]
      simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
          next_cons, returns, result, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
      exact recursive

/-- Empty slices return false without invoking even a failing or divergent callback. -/
theorem empty_contract {T F : Type}
    (callback : CoreModels.core.ops.function.FnMut F T Bool) (state : F)
    (members : Slice T) (empty : members.val = []) :
    protocol_core.nonempty_all_support callback members state = RustM.ok false := by
  simp [protocol_core.nonempty_all_support, CoreModels.core.slice.Slice.is_empty,
    CoreModels.core.slice.Slice.len, rust_primitives.slice.slice_length, Slice.len, empty,
    Usize.ofNatCore, UScalar.ofNatCore]

/-- Every representable finite slice is accepted exactly when it is nonempty
and all members satisfy the fixed predicate, under the total callback contract. -/
theorem all_members_contract {T F : Type}
    (callback : CoreModels.core.ops.function.FnMut F T Bool) (predicate : T → Bool)
    (invariant : F → Prop) (contract : CallbackContract callback predicate invariant)
    (members : Slice T) (state : F) (valid : invariant state) :
    protocol_core.nonempty_all_support callback members state =
      RustM.ok (!members.val.isEmpty && members.val.all predicate) := by
  obtain ⟨remaining, iterates⟩ := iterator_contract callback predicate invariant contract
    members.val members.property state valid
  change CoreModels.core.iter.traits.iterator.iter_all_loop
    (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT T)
    callback members state = RustM.ok (members.val.all predicate, remaining) at iterates
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
    simp only [nonzero, decide_false, Bool.false_eq_true, ↓reduceIte]
    rw [iterates]
    simp [Aeneas.Std.uncurry, empty]

theorem boolean_identity_contract (members : Slice Bool) :
    protocol_core.nonempty_all_support booleanIdentity members () =
      RustM.ok (!members.val.isEmpty && members.val.all id) := by
  apply all_members_contract booleanIdentity id (fun _ => True)
  · intro state member _
    exact ⟨state, rfl, trivial⟩
  · trivial

/-- Both outcomes of the nonempty theorem are reachable. -/
theorem nonempty_outcomes :
    protocol_core.nonempty_all_support booleanIdentity (⟨[true], by scalar_tac⟩ : Slice Bool) () =
      RustM.ok true ∧
    protocol_core.nonempty_all_support booleanIdentity (⟨[false], by scalar_tac⟩ : Slice Bool) () =
      RustM.ok false := by
  constructor <;> rw [boolean_identity_contract] <;> rfl

end Capability
