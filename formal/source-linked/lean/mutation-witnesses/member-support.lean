import Capability.Verification.ConcreteCallbacks
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability

/-- A v2 member is wrongly accepted when the member adapter replaces its stored version. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    SessionMember.supports_session
      { member with capabilities := ⟨2#u16, true, false, false, true, false, false⟩ }
      .Relay .Relay = RustM.ok true := by
  simp [SessionMember.supports_session, supports_value, MemberSupport]
end Capability
