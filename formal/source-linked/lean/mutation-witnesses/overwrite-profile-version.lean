import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
/-- The mutant replaces a known future version by the caller's floor. -/
theorem mutation_returns_wrong_value :
    resolve_capabilities (.Some
      { version := 65535#u16, relay_transport := false, direct_transport := true,
        webrtc_transport := true, relay_topology := false, host_topology := true,
        mesh_topology := true }) 2#u16 = RustM.ok
      { version := 2#u16, relay_transport := false, direct_transport := true,
        webrtc_transport := true, relay_topology := false, host_topology := true,
        mesh_topology := true } := by
  rfl
end Projection
