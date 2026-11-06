import Projection.Extraction
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Projection
/-- Dropping the stored transport vector loses explicitly negotiated WebRTC. -/
theorem mutation_returns_wrong_value :
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from
          { version := 3#u16,
            transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
            topologies := ⟨[Topology.Mesh], by scalar_tac⟩ }
        RustM.ok caps.webrtc_transport) = RustM.ok false := by
  simp [SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from,
    project_capabilities, alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref,
    alloc.vec.Vec.as_slice, rust_primitives.sequence.seq_to_slice,
    CoreModels.core.slice.Slice.contains, rust_primitives.slice.slice_contains,
    rust_primitives.slice.slice_contains_go,
    Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant,
    Std.Array.empty, Std.Array.to_slice, Aeneas.Std.lift,
    Aeneas.Std.bind, Bind.bind]
end Projection
