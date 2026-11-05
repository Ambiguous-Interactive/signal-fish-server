import Capability.Verification.Callbacks
open Aeneas.Std
open signal_fish_proof_core
namespace Capability
theorem mutation_returns_wrong_value :
    protocol_core.nonempty_all_support booleanIdentity
      (⟨[], by simp⟩ : Slice Bool) () = RustM.ok true := by
  unfold protocol_core.nonempty_all_support
  simp only [CoreModels.core.slice.Slice.iter,
    CoreModels.rust_primitives.sequence.seq_from_slice,
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all,
    CoreModels.core.iter.traits.iterator.Iterator.all.default,
    CoreModels.core.iter.traits.iterator.iter_all,
    CoreModels.core.iter.traits.iterator.iter_all_loop,
    Aeneas.Std.bind, Bind.bind]
  rw [loop]
  simp [CoreModels.core.iter.traits.iterator.iter_all_loop.body,
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next,
    CoreModels.rust_primitives.sequence.seq_len, Slice.len,
    Usize.ofNatCore, UScalar.ofNatCore, Aeneas.Std.uncurry]
end Capability
