import Negotiation.Verification.ProofObligations

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core protocol_core

namespace Negotiation

/-- The route default applies only when the Authenticate frame omits its version. -/
def handshakeRequest (client : core.option.Option U16) (endpoint : U16) : U16 :=
  match client with | .None => endpoint | .Some version => version

/-- Exact version and lifecycle outcome, for all u16 inputs, including invalid ranges. -/
theorem handshake_contract (client : core.option.Option U16) (endpoint minimum maximum : U16)
    (complete : Bool) :
    ∃ version, version.val = min (handshakeRequest client endpoint).val maximum.val ∧
      decide_handshake_version client endpoint minimum maximum complete = RustM.ok
        (if version.val < minimum.val then
          if complete then .Close version else .Retry version
        else .Accepted version) := by
  cases client with
  | none =>
    by_cases bound : endpoint.val < maximum.val
    · refine ⟨endpoint, ?_, ?_⟩
      · simp [handshakeRequest, Nat.min_eq_left (Nat.le_of_lt bound)]
      · simp [decide_handshake_version, core.option.Option.or, negotiate_protocol_version,
          bound, Aeneas.Std.bind, Bind.bind]
        split_ifs <;> rfl
    · refine ⟨maximum, ?_, ?_⟩
      · simp [handshakeRequest, Nat.min_eq_right (Nat.le_of_not_gt bound)]
      · simp [decide_handshake_version, core.option.Option.or, negotiate_protocol_version,
          bound, Aeneas.Std.bind, Bind.bind]
        split_ifs <;> rfl
  | some client =>
    by_cases bound : client.val < maximum.val
    · refine ⟨client, ?_, ?_⟩
      · simp [handshakeRequest, Nat.min_eq_left (Nat.le_of_lt bound)]
      · simp [decide_handshake_version, core.option.Option.or, negotiate_protocol_version,
          bound, Aeneas.Std.bind, Bind.bind]
        split_ifs <;> rfl
    · refine ⟨maximum, ?_, ?_⟩
      · simp [handshakeRequest, Nat.min_eq_right (Nat.le_of_not_gt bound)]
      · simp [decide_handshake_version, core.option.Option.or, negotiate_protocol_version,
          bound, Aeneas.Std.bind, Bind.bind]
        split_ifs <;> rfl

/-- Acceptance respects the client or endpoint ceiling and both deployment bounds.
Refusal is strictly below the floor and chooses close exactly for completed sockets. -/
theorem handshake_safety_contract (client : core.option.Option U16)
    (endpoint minimum maximum : U16) (complete : Bool) :
    ∃ decision, decide_handshake_version client endpoint minimum maximum complete = RustM.ok decision ∧
      match decision with
      | .Accepted version => minimum.val ≤ version.val ∧ version.val ≤ maximum.val ∧
          version.val ≤ (handshakeRequest client endpoint).val
      | .Retry version => version.val < minimum.val ∧ complete = false
      | .Close version => version.val < minimum.val ∧ complete = true := by
  obtain ⟨version, value, returns⟩ := handshake_contract client endpoint minimum maximum complete
  refine ⟨(if version.val < minimum.val then
    if complete then .Close version else .Retry version else .Accepted version), returns, ?_⟩
  by_cases below : version.val < minimum.val
  · cases complete <;> simp [below]
  · simp only [below, ↓reduceIte]
    constructor
    · omega
    · rw [value]
      exact ⟨Nat.min_le_right _ _, Nat.min_le_left _ _⟩

/-- A request at or above the deployment floor succeeds when the range is ordered. -/
theorem handshake_accepts_contract (client : core.option.Option U16)
    (endpoint minimum maximum : U16) (complete : Bool)
    (ordered : minimum.val ≤ maximum.val)
    (supported : minimum.val ≤ (handshakeRequest client endpoint).val) :
    ∃ version, decide_handshake_version client endpoint minimum maximum complete =
      RustM.ok (.Accepted version) ∧
      version.val = min (handshakeRequest client endpoint).val maximum.val := by
  obtain ⟨version, value, returns⟩ := handshake_contract client endpoint minimum maximum complete
  have above : ¬ version.val < minimum.val := by rw [value]; omega
  exact ⟨version, by simpa [above] using returns, value⟩

/-- A below-floor declaration can never keep a completed socket usable. -/
theorem handshake_refuses_contract (client : core.option.Option U16)
    (endpoint minimum maximum : U16) (complete : Bool)
    (unsupported : (handshakeRequest client endpoint).val < minimum.val) :
    ∃ version, version.val < minimum.val ∧
      decide_handshake_version client endpoint minimum maximum complete = RustM.ok
        (if complete then .Close version else .Retry version) := by
  obtain ⟨version, value, returns⟩ := handshake_contract client endpoint minimum maximum complete
  have below : version.val < minimum.val := by rw [value]; omega
  exact ⟨version, below, by simpa [below] using returns⟩

/-- Route defaults, explicit precedence, clamping and both refusal states are reachable. -/
theorem handshake_outcomes :
    decide_handshake_version .None 3#u16 2#u16 3#u16 false = RustM.ok (.Accepted 3#u16) ∧
    decide_handshake_version (.Some 2#u16) 3#u16 2#u16 3#u16 true = RustM.ok (.Accepted 2#u16) ∧
    decide_handshake_version (.Some 65535#u16) 2#u16 2#u16 3#u16 false = RustM.ok (.Accepted 3#u16) ∧
    decide_handshake_version (.Some 2#u16) 3#u16 3#u16 3#u16 false = RustM.ok (.Retry 2#u16) ∧
    decide_handshake_version (.Some 2#u16) 3#u16 3#u16 3#u16 true = RustM.ok (.Close 2#u16) := by
  repeat' constructor

end Negotiation
