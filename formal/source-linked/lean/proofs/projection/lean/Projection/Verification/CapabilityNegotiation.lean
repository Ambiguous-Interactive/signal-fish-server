import Projection.Verification.ProofObligations

open CoreModels Aeneas
open Aeneas.Std hiding namespace core alloc
open signal_fish_proof_core session_capability

namespace Projection

/-- Keep the first occurrence: the accumulator contains exactly the already emitted prefix. -/
def firstOccurrences {T : Type} [DecidableEq T] : List T → List T → List T
  | [], seen => seen
  | x :: xs, seen => firstOccurrences xs (if x ∈ seen then seen else seen ++ [x])

private theorem first_occurrences_length {T : Type} [DecidableEq T]
    (xs seen : List T) : (firstOccurrences xs seen).length ≤ xs.length + seen.length := by
  induction xs generalizing seen with
  | nil => simp [firstOccurrences]
  | cons x xs ih =>
    simp only [firstOccurrences]
    split
    · have h := ih seen
      simp only [List.length_cons]; omega
    · have h := ih (seen ++ [x])
      simp only [List.length_append, List.length_singleton] at h
      simp only [List.length_cons]; omega

private theorem drain_next_nil {T : Type} (items : alloc.vec.Vec T) (empty : items.val = []) :
    alloc.vec.drain.Drain.Insts.CoreIterTraitsIteratorIterator.next (A := alloc.alloc.Global)
      (items, core.marker.PhantomData.mk) = RustM.ok (.None, (items, core.marker.PhantomData.mk)) := by
  simp [alloc.vec.drain.Drain.Insts.CoreIterTraitsIteratorIterator.next,
    rust_primitives.sequence.seq_len, Slice.len, empty, Usize.ofNatCore, UScalar.ofNatCore]
  rfl

private theorem drain_next_cons {T : Type} (x : T) (xs : List T)
    (bound : (x :: xs).length ≤ Usize.max) :
    alloc.vec.drain.Drain.Insts.CoreIterTraitsIteratorIterator.next (A := alloc.alloc.Global)
      ((⟨x :: xs, bound⟩ : alloc.vec.Vec T), core.marker.PhantomData.mk) =
      RustM.ok (.Some x, ((⟨xs, by simp only [List.length_cons] at bound; omega⟩ : alloc.vec.Vec T),
        core.marker.PhantomData.mk)) := by
  simp [alloc.vec.drain.Drain.Insts.CoreIterTraitsIteratorIterator.next,
    rust_primitives.sequence.seq_len, rust_primitives.sequence.seq_remove, Slice.len]
  rfl

private theorem push_contract {T : Type} (items : alloc.vec.Vec T) (x : T)
    (bound : (items.val ++ [x]).length ≤ Usize.max) :
    alloc.vec.Vec.push items x = RustM.ok (⟨items.val ++ [x], bound⟩ : alloc.vec.Vec T) := by
  have room : items.val.length < Usize.max := by
    simp only [List.length_append, List.length_singleton] at bound; omega
  simp [alloc.vec.Vec.push, rust_primitives.sequence.seq_push, room]
  rfl

private theorem dedup_loop_contract {T : Type} [DecidableEq T]
    (inst : core.cmp.PartialEq T T)
    (equality : ∀ a b, inst.eq a b = RustM.ok (decide (a = b)))
    (xs seen : List T) (remainingBound : xs.length ≤ Usize.max)
    (seenBound : seen.length ≤ Usize.max) (space : xs.length + seen.length ≤ Usize.max) :
    dedup_preserving_order_loop inst
      ((⟨xs, remainingBound⟩ : alloc.vec.Vec T), core.marker.PhantomData.mk)
      (⟨seen, seenBound⟩ : alloc.vec.Vec T) =
      RustM.ok (⟨firstOccurrences xs seen,
        Nat.le_trans (first_occurrences_length xs seen) space⟩ : alloc.vec.Vec T) := by
  induction xs generalizing seen with
  | nil =>
    unfold dedup_preserving_order_loop
    rw [loop]
    simp [dedup_preserving_order_loop.body, drain_next_nil, firstOccurrences,
      Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    rfl
  | cons x xs ih =>
    have tailBound : xs.length ≤ Usize.max := by simp only [List.length_cons] at remainingBound; omega
    unfold dedup_preserving_order_loop
    rw [loop]
    simp only [dedup_preserving_order_loop.body, drain_next_cons,
      alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
      rust_primitives.sequence.seq_to_slice, core.slice.Slice.contains,
      rust_primitives.slice.slice_contains, contains_contract inst equality,
      Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    by_cases present : x ∈ seen
    · simp only [present, decide_true, ↓reduceIte, firstOccurrences]
      convert (by
        simpa only [dedup_preserving_order_loop, dedup_preserving_order_loop.body,
        alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
        rust_primitives.sequence.seq_to_slice, core.slice.Slice.contains,
        rust_primitives.slice.slice_contains, contains_contract inst equality,
        Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry] using
        ih seen tailBound seenBound (by simp only [List.length_cons] at space; omega)) using 1; rfl
    · have newBound : (seen ++ [x]).length ≤ Usize.max := by
        simp only [List.length_cons] at space
        simp only [List.length_append, List.length_singleton]; omega
      simp only [present, decide_false, Bool.false_eq_true, ↓reduceIte,
        push_contract (⟨seen, seenBound⟩ : alloc.vec.Vec T) x newBound, firstOccurrences]
      convert (by
        simpa only [dedup_preserving_order_loop, dedup_preserving_order_loop.body,
        alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
        rust_primitives.sequence.seq_to_slice, core.slice.Slice.contains,
        rust_primitives.slice.slice_contains, contains_contract inst equality,
        Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry] using
        ih (seen ++ [x]) tailBound newBound (by
        simp only [List.length_cons] at space
        simp only [List.length_append, List.length_singleton]; omega)) using 1; rfl

/-- The actual generic drain loop returns the first occurrences for every representable vector.
The equality dictionary must implement total Rust equality for the element type. -/
theorem dedup_contract {T : Type} [DecidableEq T]
    (inst : core.cmp.PartialEq T T)
    (equality : ∀ a b, inst.eq a b = RustM.ok (decide (a = b)))
    (items : alloc.vec.Vec T) :
    dedup_preserving_order inst items = RustM.ok
      (⟨firstOccurrences items.val [], by
        have h := first_occurrences_length items.val []
        simp only [List.length_nil, Nat.add_zero] at h
        exact Nat.le_trans h items.property⟩ : alloc.vec.Vec T) := by
  have iterates := dedup_loop_contract inst equality items.val [] items.property
    (by simp) (by simpa using items.property)
  unfold dedup_preserving_order
  simp [alloc.vec.Vec.len, rust_primitives.sequence.seq_len,
    alloc.vec.Vec.with_capacity, alloc.vec.Vec.new, rust_primitives.sequence.seq_empty,
    alloc.vec.Vec.drain, core.slice.index.range, core.slice.index.try_range,
    core.slice.index.start_index, core.slice.index.end_index,
    core.ops.range.RangeFull.Insts.CoreOpsRangeRangeBounds.start_bound,
    core.ops.range.RangeFull.Insts.CoreOpsRangeRangeBounds.end_bound,
    rust_primitives.sequence.seq_drain, Slice.len,
    Aeneas.Std.bind, Bind.bind]
  convert iterates using 1; rfl

end Projection
