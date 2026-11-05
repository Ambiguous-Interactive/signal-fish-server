import Capability.Extraction

open Aeneas.Std

namespace Capability

/-- A total boolean projection used to instantiate the generic callback contract. -/
def booleanIdentity : CoreModels.core.ops.function.FnMut Unit Bool Bool where
  FnOnceInst := { call_once := fun _ member => RustM.ok member }
  call_mut := fun state member => RustM.ok (member, state)

/-- The result depends on the state produced by the preceding invocation. -/
def orderedCounter : CoreModels.core.ops.function.FnMut Nat Nat Bool where
  FnOnceInst := { call_once := fun state member => RustM.ok (decide (member = state)) }
  call_mut := fun state member => RustM.ok (decide (member = state), state + 1)

/-- Zero accepts, one rejects, two fails, and larger values diverge. -/
def partialCallback : CoreModels.core.ops.function.FnMut Unit Nat Bool where
  FnOnceInst := { call_once := fun _ member =>
    if member = 0 then RustM.ok true
    else if member = 1 then RustM.ok false
    else if member = 2 then RustM.fail .panic else RustM.div }
  call_mut := fun state member =>
    if member = 0 then RustM.ok (true, state)
    else if member = 1 then RustM.ok (false, state)
    else if member = 2 then RustM.fail .panic else RustM.div

theorem next_empty {T : Type} (members : Slice T) (empty : members.val = []) :
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next members =
      RustM.ok (.None, members) := by
  simp [CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next,
    CoreModels.rust_primitives.sequence.seq_len, Slice.len, empty, Usize.ofNatCore, UScalar.ofNatCore]
  rfl

theorem next_cons {T : Type} (head : T) (tail : List T)
    (bound : (head :: tail).length ≤ Usize.max) :
    CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next
      (⟨head :: tail, bound⟩ : Slice T) =
      RustM.ok (.Some head, (⟨tail, by simp only [List.length_cons] at bound; omega⟩ : Slice T)) := by
  simp [CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next,
    CoreModels.rust_primitives.sequence.seq_len, CoreModels.rust_primitives.sequence.seq_remove, Slice.len]
  rfl

end Capability
