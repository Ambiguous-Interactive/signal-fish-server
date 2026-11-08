import Negotiation.Verification.Handshake

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core protocol_core

namespace Negotiation

/-- Independent policy: reject below-v2 floors, reversed ranges, then unimplemented ceilings. -/
def boundsPolicy (minimum maximum : U16) : ProtocolBoundsDecision :=
  if minimum.val < 2 then .BelowFloor
  else if maximum.val < minimum.val then .Reversed
  else if 3 < maximum.val then .AboveCeiling
  else .Valid

/-- Exact result and rejection priority for all deployment bounds. -/
theorem bounds_contract (minimum maximum : U16) :
    validate_protocol_version_bounds minimum maximum = RustM.ok (boundsPolicy minimum maximum) := by
  simp [validate_protocol_version_bounds, boundsPolicy, SERVER_MIN_PROTOCOL_VERSION, SERVER_MAX_PROTOCOL_VERSION]
  split_ifs <;> rfl

/-- Acceptance is equivalent to a nonempty range of implemented wire versions. -/
theorem bounds_valid_contract (minimum maximum : U16) :
    validate_protocol_version_bounds minimum maximum = RustM.ok .Valid ↔
      2 ≤ minimum.val ∧ minimum.val ≤ maximum.val ∧ maximum.val ≤ 3 := by
  rw [bounds_contract]
  unfold boundsPolicy
  split_ifs <;> simp_all; omega

/-- Validation always returns, including invalid bounds. -/
theorem bounds_total_contract (minimum maximum : U16) :
    ∃ decision, validate_protocol_version_bounds minimum maximum = RustM.ok decision :=
  ⟨boundsPolicy minimum maximum, bounds_contract minimum maximum⟩

/-- Each refusal and each supported deployment is reachable, with refusal priority. -/
theorem bounds_outcomes :
    validate_protocol_version_bounds 0#u16 65535#u16 = RustM.ok .BelowFloor ∧
    validate_protocol_version_bounds 3#u16 2#u16 = RustM.ok .Reversed ∧
    validate_protocol_version_bounds 65535#u16 4#u16 = RustM.ok .Reversed ∧
    validate_protocol_version_bounds 2#u16 65535#u16 = RustM.ok .AboveCeiling ∧
    validate_protocol_version_bounds 2#u16 2#u16 = RustM.ok .Valid ∧
    validate_protocol_version_bounds 2#u16 3#u16 = RustM.ok .Valid ∧
    validate_protocol_version_bounds 3#u16 3#u16 = RustM.ok .Valid := by
  simp [validate_protocol_version_bounds, SERVER_MIN_PROTOCOL_VERSION, SERVER_MAX_PROTOCOL_VERSION]

/-- Composition: a validated deployment accepts only implemented versions.
This does not assume that a socket supplied its actual deployment or profile. -/
theorem validated_handshake_contract (client : core.option.Option U16)
    (endpoint minimum maximum : U16) (complete : Bool)
    (valid : validate_protocol_version_bounds minimum maximum = RustM.ok .Valid) :
    ∃ decision, decide_handshake_version client endpoint minimum maximum complete = RustM.ok decision ∧
      match decision with
      | .Accepted version => 2 ≤ version.val ∧ version.val ≤ 3 ∧
          minimum.val ≤ version.val ∧ version.val ≤ maximum.val ∧
          version.val ≤ (handshakeRequest client endpoint).val
      | .Retry version => version.val < minimum.val ∧ complete = false
      | .Close version => version.val < minimum.val ∧ complete = true := by
  obtain ⟨floor, ordered, ceiling⟩ := (bounds_valid_contract minimum maximum).mp valid
  obtain ⟨decision, returns, safe⟩ := handshake_safety_contract client endpoint minimum maximum complete
  refine ⟨decision, returns, ?_⟩
  cases decision <;> simp_all; omega

end Negotiation
