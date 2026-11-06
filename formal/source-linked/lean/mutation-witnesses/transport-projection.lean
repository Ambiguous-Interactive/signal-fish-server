import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
theorem mutation_returns_wrong_value :
    project_capabilities 3#u16
      (⟨[Transport.Relay], by scalar_tac⟩ : Slice Transport)
      (⟨[], by scalar_tac⟩ : Slice Topology) =
      RustM.ok ⟨3#u16, true, true, false, false, false, false⟩ := by
  simp [project_capabilities, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains, rust_primitives.slice.slice_contains_go,
    Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant,
    Aeneas.Std.bind, Bind.bind]
end Projection
