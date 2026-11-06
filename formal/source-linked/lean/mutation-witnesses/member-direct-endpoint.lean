import Capability.Verification.ConcreteCallbacks
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability

/-- A Direct host without an endpoint is wrongly accepted. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    SessionMember.can_host
      { member with
        capabilities := ⟨3#u16, true, true, true, true, true, true⟩,
        direct_endpoint := .None } .Host .Direct = RustM.ok true := by
  simp [SessionMember.can_host, SessionMember.supports_session, supports_value,
    MemberSupport, CoreModels.core.cmp.PartialEq.ne.default,
    Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
    Aeneas.Std.bind, Bind.bind]
end Capability
