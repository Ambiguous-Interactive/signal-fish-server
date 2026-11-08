import Projection.Verification.ProofObligations
open CoreModels Aeneas
open Aeneas.Std hiding namespace core alloc
open signal_fish_proof_core session_capability
namespace Projection
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

private theorem prepend_contract {T : Type} (items : alloc.vec.Vec T) (x : T)
    (bound : (x :: items.val).length ≤ Usize.max) :
    alloc.vec.Vec.insert items 0#usize x = RustM.ok (⟨x :: items.val, bound⟩ : alloc.vec.Vec T) := by
  have room : (0 : Nat) < Usize.max := by scalar_tac
  have combined : 1 + items.val.length ≤ Usize.max := by
    simpa only [List.length_cons, Nat.add_comm] using bound
  simp [alloc.vec.Vec.insert, rust_primitives.sequence.seq_len,
    rust_primitives.sequence.seq_drain, rust_primitives.sequence.seq_push,
    rust_primitives.sequence.seq_concat, Slice.len, room, combined,
    Aeneas.Std.bind, Bind.bind]
  rfl

private theorem dedup_start {T : Type} (inst : core.cmp.PartialEq T T)
    (items : alloc.vec.Vec T) :
    dedup_preserving_order inst items = dedup_preserving_order_loop inst
      (items, core.marker.PhantomData.mk) (⟨[], by simp⟩ : alloc.vec.Vec T) := by
  unfold dedup_preserving_order
  simp [alloc.vec.Vec.len, rust_primitives.sequence.seq_len,
    alloc.vec.Vec.with_capacity, alloc.vec.Vec.new, rust_primitives.sequence.seq_empty,
    alloc.vec.Vec.drain, core.slice.index.range, core.slice.index.try_range,
    core.slice.index.start_index, core.slice.index.end_index,
    core.ops.range.RangeFull.Insts.CoreOpsRangeRangeBounds.start_bound,
    core.ops.range.RangeFull.Insts.CoreOpsRangeRangeBounds.end_bound,
    rust_primitives.sequence.seq_drain, Slice.len, Aeneas.Std.bind, Bind.bind]
  rfl

/-- The changed loop reverses the first-occurrence preference order. -/
theorem mutation_returns_wrong_value :
    (do let result ← dedup_preserving_order Transport.Insts.CoreCmpPartialEqTransport
          (⟨[.Relay, .WebRtc], by scalar_tac⟩ : alloc.vec.Vec Transport)
        RustM.ok result.val) = RustM.ok [Transport.WebRtc, .Relay] := by
  have room1 : (1 : Nat) ≤ Usize.max := by scalar_tac
  have room2 : (2 : Nat) ≤ Usize.max := by scalar_tac
  rw [dedup_start]
  unfold dedup_preserving_order_loop
  repeat' ((first | erw [drain_next_cons] | rw [loop]); simp_all [dedup_preserving_order_loop.body,
    drain_next_cons,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, core.slice.Slice.contains,
    rust_primitives.slice.slice_contains,
    contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    prepend_contract,
    Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry])
  erw [drain_next_nil (⟨[], by simp⟩ : alloc.vec.Vec Transport) rfl]
end Projection
