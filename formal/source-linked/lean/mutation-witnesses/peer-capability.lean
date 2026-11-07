import Capability.Verification.ElectionFixtures
import Capability.Verification.Members
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member
noncomputable section
namespace Capability
open ElectionFixtures

/-- A concrete wrong edge returned by the compiled production fault. -/
theorem mutation_returns_wrong_value (member : SessionMember) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let low := { member with player_id := lowId, capabilities := capable }
    let high := { member with player_id := highId, capabilities := capable }
    peer_initiate low { high with capabilities := { capable with webrtc_transport := false } } .Mesh .WebRtc .None = RustM.ok (.Some true) := by
  have reverse_order : ¬ElectionOrder.uuidKey highId < ElectionOrder.uuidKey lowId := by
    have order := id_order
    omega
  simp [peer_initiate, local_initiates, member_support_contract, MemberSupport,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq,
    CoreModels.core.cmp.PartialOrd.lt.default,
    uuid.Uuid.Insts.CoreCmpPartialOrdUuid, ElectionOrder.orderBy,
    distinct, Ne.symm distinct, id_order, reverse_order, Aeneas.Std.bind, Bind.bind]
end Capability
