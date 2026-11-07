import Capability.Verification.Executability

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member

namespace Capability

/-- Lift the pinned iterator result through the actual production selector.
The predicate remains arbitrary here. Selection.lean supplies the protocol contract. -/
theorem selector_evaluation (members : Slice SessionMember) (desired : Topology)
    (direct webrtc : Bool) (predicate : (Topology × Transport) → Bool)
    (contract : CallbackContract
      select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool predicate
      (fun capture => capture = (desired, direct, webrtc, members))) :
    select_session_pair members desired direct webrtc = RustM.ok
      ((UPGRADE_LADDER.val.find? predicate).getD RELAY_FLOOR) := by
  let callback := select_session_pair.closure.Insts.CoreOpsFunctionFnMutTupleSharedPairTopologyTransportBool
  obtain ⟨remaining, iterates⟩ := array_find_contract 3#usize callback predicate _ contract
    UPGRADE_LADDER.to_slice.val UPGRADE_LADDER.to_slice.property
    (desired, direct, webrtc, members) rfl
  unfold select_session_pair
  simp only [CoreModels.core.Array.Insts.CoreIterTraitsCollectIntoIteratorTIntoIter.into_iter,
    CoreModels.rust_primitives.sequence.seq_from_array,
    CoreModels.core.iter.traits.iterator.Iterator.find.default,
    CoreModels.core.iter.traits.iterator.iter_find, Aeneas.Std.bind, Bind.bind]
  change (do
    let (result, _) ← CoreModels.core.iter.traits.iterator.iter_find_loop
      (CoreModels.core.array.iter.IntoIter.Insts.CoreIterTraitsIteratorIterator _ 3#usize)
      callback (⟨UPGRADE_LADDER.to_slice.val, _⟩ : Slice (Topology × Transport))
      (desired, direct, webrtc, members)
    CoreModels.core.option.Option.unwrap_or result RELAY_FLOOR) = _
  rw [iterates]
  cases result : UPGRADE_LADDER.val.find? predicate <;>
    simp [RustOption, CoreModels.core.option.Option.unwrap_or, Aeneas.Std.bind, Bind.bind,
      Array.to_slice, result]

end Capability
