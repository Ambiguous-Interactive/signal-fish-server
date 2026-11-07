import Capability.Verification.ProofObligations

open CoreModels Aeneas Aeneas.Std

namespace Capability

/-- The pinned search loop returns the finite-list existential under a total callback contract. -/
theorem any_iterator_contract {T F : Type}
    (callback : CoreModels.core.ops.function.FnMut F T Bool) (predicate : T → Bool)
    (invariant : F → Prop) (contract : CallbackContract callback predicate invariant)
    (xs : List T) (bound : xs.length ≤ Usize.max) (state : F) (valid : invariant state) :
    ∃ remaining, CoreModels.core.iter.traits.iterator.iter_any_loop
      (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT T)
      callback (⟨xs, bound⟩ : Slice T) state = RustM.ok (xs.any predicate, remaining) := by
  induction xs generalizing state with
  | nil =>
    refine ⟨⟨[], bound⟩, ?_⟩
    unfold CoreModels.core.iter.traits.iterator.iter_any_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_any_loop.body,
      next_empty, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | cons head tail ih =>
    obtain ⟨next, returns, valid_next⟩ := contract state head valid
    have tail_bound : tail.length ≤ Usize.max := by simp only [List.length_cons] at bound; omega
    obtain ⟨remaining, recursive⟩ := ih tail_bound next valid_next
    cases result : predicate head with
    | true =>
      refine ⟨⟨tail, tail_bound⟩, ?_⟩
      unfold CoreModels.core.iter.traits.iterator.iter_any_loop
      rw [loop]
      simp [CoreModels.core.iter.traits.iterator.iter_any_loop.body,
        next_cons, returns, result, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    | false =>
      refine ⟨remaining, ?_⟩
      unfold CoreModels.core.iter.traits.iterator.iter_any_loop
      rw [loop]
      simp [CoreModels.core.iter.traits.iterator.iter_any_loop.body,
        next_cons, returns, result, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
      exact recursive

end Capability

namespace Capability

private theorem array_next_empty {T : Type} (n : Usize) (xs : Slice T) (empty : xs.val = []) :
    CoreModels.core.array.iter.IntoIter.Insts.CoreIterTraitsIteratorIterator.next (N := n) xs =
      RustM.ok (.None, xs) := by
  simp [CoreModels.core.array.iter.IntoIter.Insts.CoreIterTraitsIteratorIterator.next,
    CoreModels.rust_primitives.sequence.seq_len, Slice.len, empty, Usize.ofNatCore, UScalar.ofNatCore]
  rfl

private theorem array_next_cons {T : Type} (n : Usize) (head : T) (tail : List T)
    (bound : (head :: tail).length ≤ Usize.max) :
    CoreModels.core.array.iter.IntoIter.Insts.CoreIterTraitsIteratorIterator.next (N := n)
      (⟨head :: tail, bound⟩ : Slice T) =
      RustM.ok (.Some head, (⟨tail, by simp only [List.length_cons] at bound; omega⟩ : Slice T)) := by
  simp [CoreModels.core.array.iter.IntoIter.Insts.CoreIterTraitsIteratorIterator.next,
    CoreModels.rust_primitives.sequence.seq_len, CoreModels.rust_primitives.sequence.seq_remove, Slice.len]
  rfl

def RustOption {T : Type} : Option T → CoreModels.core.option.Option T
  | none => .None | some value => .Some value

/-- The pinned array search returns the first successful predicate and terminates. -/
theorem array_find_contract {T F : Type} (n : Usize)
    (callback : CoreModels.core.ops.function.FnMut F T Bool) (predicate : T → Bool)
    (invariant : F → Prop) (contract : CallbackContract callback predicate invariant)
    (xs : List T) (bound : xs.length ≤ Usize.max) (state : F) (valid : invariant state) :
    ∃ remaining, CoreModels.core.iter.traits.iterator.iter_find_loop
      (CoreModels.core.array.iter.IntoIter.Insts.CoreIterTraitsIteratorIterator T n)
      callback (⟨xs, bound⟩ : Slice T) state = RustM.ok (RustOption (xs.find? predicate), remaining) := by
  induction xs generalizing state with
  | nil =>
    refine ⟨⟨[], bound⟩, ?_⟩
    unfold CoreModels.core.iter.traits.iterator.iter_find_loop
    rw [loop]
    simp [CoreModels.core.iter.traits.iterator.iter_find_loop.body,
      array_next_empty, RustOption, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | cons head tail ih =>
    obtain ⟨next, returns, valid_next⟩ := contract state head valid
    have tail_bound : tail.length ≤ Usize.max := by simp only [List.length_cons] at bound; omega
    obtain ⟨remaining, recursive⟩ := ih tail_bound next valid_next
    cases result : predicate head with
    | true =>
      refine ⟨⟨tail, tail_bound⟩, ?_⟩
      unfold CoreModels.core.iter.traits.iterator.iter_find_loop
      rw [loop]
      simp [CoreModels.core.iter.traits.iterator.iter_find_loop.body,
        array_next_cons, returns, result, RustOption, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    | false =>
      refine ⟨remaining, ?_⟩
      unfold CoreModels.core.iter.traits.iterator.iter_find_loop
      rw [loop]
      simp [CoreModels.core.iter.traits.iterator.iter_find_loop.body,
        array_next_cons, returns, result, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
      exact recursive


end Capability
