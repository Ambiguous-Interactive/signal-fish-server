import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
/-- The mutant silently promotes the missing profile from supplied floor two to three. -/
theorem mutation_returns_wrong_value :
    resolve_capabilities .None 2#u16 = RustM.ok
      { version := 3#u16, relay_transport := true, direct_transport := false,
        webrtc_transport := false, relay_topology := true, host_topology := false,
        mesh_topology := false } := by
  rfl
end Projection
