import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
/-- Raising the actual floor changes the default profile to v3. -/
theorem mutation_returns_wrong_value :
    (do let caps ← SessionCapabilities.Insts.CoreDefaultDefault.default
        RustM.ok caps.version) = RustM.ok 3#u16 := by
  simp [SessionCapabilities.Insts.CoreDefaultDefault.default, resolve_connection_capabilities, resolve_capabilities,
    protocol_core.SERVER_MIN_PROTOCOL_VERSION, Aeneas.Std.bind, Bind.bind]
end Projection
