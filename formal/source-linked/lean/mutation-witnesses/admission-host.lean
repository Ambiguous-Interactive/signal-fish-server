import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_admission
namespace Projection
/-- Requiring a stored host rejects a compatible joiner to a hostless mesh session. -/
theorem mutation_returns_wrong_value :
    joiner_supports_sticky_plan
      { version := 3#u16, transports := ⟨[Transport.Relay, .WebRtc], by scalar_tac⟩,
        topologies := ⟨[Topology.Relay, .Mesh], by scalar_tac⟩ }
      { topology := .Mesh, transport := .WebRtc, host := .None } = RustM.ok false := by
  simp [joiner_supports_sticky_plan, CoreModels.core.option.Option.is_some,
    Aeneas.Std.bind, Bind.bind]
end Projection
