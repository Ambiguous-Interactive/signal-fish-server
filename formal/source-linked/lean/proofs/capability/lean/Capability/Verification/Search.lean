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
