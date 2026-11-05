import Capability.Verification.Callbacks
open Aeneas.Std
open signal_fish_proof_core
namespace Capability
theorem mutation_returns_wrong_value :
    protocol_core.nonempty_all_support booleanIdentity
      (⟨[false], by scalar_tac⟩ : Slice Bool) () = RustM.ok true := by rfl
end Capability
