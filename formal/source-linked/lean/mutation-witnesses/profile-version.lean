import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
/-- An explicitly negotiated future version is overwritten with the v2 floor. -/
theorem mutation_returns_wrong_value :
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from
          { version := 65535#u16,
            transports := ⟨[], by scalar_tac⟩, topologies := ⟨[], by scalar_tac⟩ }
        RustM.ok caps.version) = RustM.ok 2#u16 := by
  simp [SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from,
    project_capabilities, alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref,
    alloc.vec.Vec.as_slice, rust_primitives.sequence.seq_to_slice,
    CoreModels.core.slice.Slice.contains, rust_primitives.slice.slice_contains,
    rust_primitives.slice.slice_contains_go,
    protocol_core.SERVER_MIN_PROTOCOL_VERSION, Aeneas.Std.bind, Bind.bind]
end Projection
