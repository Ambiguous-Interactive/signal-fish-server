import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
theorem mutation_returns_wrong_value :
    supports_session ⟨2#u16, true, true, true, true, true, true⟩ .Mesh .WebRtc =
      RustM.ok true := by
  simp [supports_session, SessionCapabilities.supports_topology,
    SessionCapabilities.supports_transport, Aeneas.Std.bind, Bind.bind]
end Projection
