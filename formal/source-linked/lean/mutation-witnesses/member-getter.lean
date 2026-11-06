import Capability.Verification.ConcreteCallbacks
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability

/-- A fully capable member is refused because the faulty getter overwrites its version. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    session_member.all_support
      (⟨[{ member with capabilities := ⟨3#u16, true, true, true, true, true, true⟩ }], by scalar_tac⟩ : Slice SessionMember)
      .Mesh .WebRtc = RustM.ok false := by
  unfold session_member.all_support
  rw [concrete_aggregation_contract _ ()
    (fun member => { member.capabilities with version := 0#u16 }) (fun _ => rfl)]
  simp [MemberSupport]
end Capability
