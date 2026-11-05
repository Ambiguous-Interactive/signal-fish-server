import Negotiation.Extraction

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core

namespace Negotiation

theorem explicit_client_contract (client minimum maximum : U16) :
    ∃ result, protocol_core.negotiate_protocol_version
      (.Some client) minimum maximum = RustM.ok result ∧
      result.val ≤ client.val ∧ result.val ≤ maximum.val ∧
      (client.val ≤ maximum.val → result = client) ∧
      (maximum.val ≤ client.val → result = maximum) := by
  unfold protocol_core.negotiate_protocol_version
  simp only [bind_tc_ok]
  split
  · rename_i h
    refine ⟨client, rfl, Nat.le_refl _, ?_, ?_, ?_⟩
    · exact Nat.le_of_lt h
    · intro _; rfl
    · intro impossible; exact False.elim (Nat.not_le_of_gt h impossible)
  · rename_i h
    change ¬ client.val < maximum.val at h
    refine ⟨maximum, rfl, ?_, Nat.le_refl _, ?_, ?_⟩
    · exact Nat.le_of_not_gt h
    · intro le; apply UScalar.val_eq_imp; omega
    · intro _; rfl

theorem omitted_client_contract (minimum maximum : U16)
    (valid_range : minimum.val ≤ maximum.val) :
    protocol_core.negotiate_protocol_version .None minimum maximum =
      RustM.ok minimum := by
  unfold protocol_core.negotiate_protocol_version
  simp only [bind_tc_ok]
  split
  · rfl
  · rename_i h
    congr 1
    apply UScalar.val_eq_imp
    change ¬ minimum.val < maximum.val at h
    omega

theorem below_floor_preserved (client minimum maximum : U16)
    (valid_range : minimum.val ≤ maximum.val)
    (below_floor : client.val < minimum.val) :
    protocol_core.negotiate_protocol_version (.Some client) minimum maximum =
      RustM.ok client := by
  obtain ⟨result, returns, _, _, preserve, _⟩ :=
    explicit_client_contract client minimum maximum
  rw [returns, preserve (by omega)]

end Negotiation
