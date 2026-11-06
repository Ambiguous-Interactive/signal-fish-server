import Capability.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
namespace Capability
/-- Conjoining the bypass wrongly rejects a fully ready Direct host. -/
theorem mutation_returns_wrong_value (member : SessionMember) (endpoint : DirectEndpoint) :
    let ready := { member with capabilities := ⟨65535#u16, true, true, true, true, true, true⟩, direct_endpoint := .Some endpoint }
    rung_is_executable (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host .Direct = RustM.ok false := by
  simp [rung_is_executable, CoreModels.core.cmp.PartialEq.ne.default,
    Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
    Aeneas.Std.bind, Bind.bind]
end Capability
