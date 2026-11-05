import Capability.Verification.Callbacks
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core
namespace Capability
theorem mutation_returns_wrong_value :
    protocol_core.nonempty_all_support orderedCounter
      (⟨[0, 1], by scalar_tac⟩ : Slice Nat) 0 = RustM.ok false := by
  change (do let (result, _) ← CoreModels.core.iter.traits.iterator.iter_all_loop
              (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT Nat)
              orderedCounter (⟨[0, 1], by scalar_tac⟩ : Slice Nat) 1
             RustM.ok result) = RustM.ok false
  have next : CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next
      (⟨[0, 1], by scalar_tac⟩ : Slice Nat) =
      RustM.ok (.Some 0, (⟨[1], by scalar_tac⟩ : Slice Nat)) := by
    simp [CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next,
      rust_primitives.sequence.seq_len, rust_primitives.sequence.seq_remove, Slice.len]
    rfl
  unfold CoreModels.core.iter.traits.iterator.iter_all_loop
  rw [loop]
  simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body, next,
    orderedCounter, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
end Capability
