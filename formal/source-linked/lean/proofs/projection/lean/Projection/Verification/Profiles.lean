import Projection.Verification.Defaults

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability

namespace Projection

/-- The production From adapter reads this profile's version and both complete vectors. -/
theorem profile_fields_contract (profile : NegotiatedProtocol) :
    SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from profile =
      project_capabilities profile.version profile.transports profile.topologies := by
  simp [SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, Aeneas.Std.bind, Bind.bind]

/-- Every representable stored profile retains its version and exact axis membership. -/
theorem profile_support_contract (profile : NegotiatedProtocol)
    (topology : Topology) (transport : Transport) :
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from profile
        supports_session caps topology transport) = RustM.ok
      (decide (profile.version.val ≥ 3) && decide (topology ∈ profile.topologies.val) &&
        decide (transport ∈ profile.transports.val)) := by
  rw [profile_fields_contract]
  exact projected_support_contract _ _ _ _ _

/-- Both live lookup branches use the extracted fixed server floor. -/
theorem connection_resolution_contract (caps : core.option.Option SessionCapabilities) :
    resolve_connection_capabilities caps = RustM.ok
      (match caps with
       | .Some caps => caps
       | .None => { version := 2#u16, relay_transport := true, direct_transport := false,
                    webrtc_transport := false, relay_topology := true,
                    host_topology := false, mesh_topology := false }) := by
  cases caps <;> simp [resolve_connection_capabilities, resolve_capabilities,
    protocol_core.SERVER_MIN_PROTOCOL_VERSION]

/-- Stored-profile projection, fixed-floor lookup resolution, and support compose exactly. -/
theorem connection_profile_support_contract (profile : NegotiatedProtocol)
    (topology : Topology) (transport : Transport) :
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from profile
        let resolved ← resolve_connection_capabilities (.Some caps)
        supports_session resolved topology transport) = RustM.ok
      (decide (profile.version.val ≥ 3) && decide (topology ∈ profile.topologies.val) &&
        decide (transport ∈ profile.transports.val)) := by
  simp only [connection_resolution_contract, bind_tc_ok]
  exact profile_support_contract _ _ _

/-- The production compact default uses the actual server constant, whose value is two. -/
theorem compact_default_contract :
    SessionCapabilities.Insts.CoreDefaultDefault.default = RustM.ok
      { version := 2#u16, relay_transport := true, direct_transport := false,
        webrtc_transport := false, relay_topology := true, host_topology := false,
        mesh_topology := false } := by
  simp [SessionCapabilities.Insts.CoreDefaultDefault.default, resolve_connection_capabilities, absent_profile_contract,
    protocol_core.SERVER_MIN_PROTOCOL_VERSION]

/-- The allocated production profile default contains exactly one relay on each axis. -/
theorem negotiated_default_contract :
    NegotiatedProtocol.Insts.CoreDefaultDefault.default = RustM.ok
      { version := 2#u16,
        transports := ⟨[Transport.Relay], by scalar_tac⟩,
        topologies := ⟨[Topology.Relay], by scalar_tac⟩ } := by
  simp [NegotiatedProtocol.Insts.CoreDefaultDefault.default,
    protocol_core.SERVER_MIN_PROTOCOL_VERSION, Std.Array.to_slice, Array.make,
    CoreModels.alloc.slice.Slice.into_vec, alloc.slice.Dummy.into_vec, alloc.vec.from_seq,
    rust_primitives.sequence.seq_from_boxed_slice, Aeneas.Std.lift, Aeneas.Std.bind, Bind.bind]

/-- The two actual production defaults agree after the actual From adapter. -/
theorem defaults_agree_contract :
    (do let profile ← NegotiatedProtocol.Insts.CoreDefaultDefault.default
        SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from profile) =
      SessionCapabilities.Insts.CoreDefaultDefault.default := by
  rw [negotiated_default_contract, compact_default_contract]
  simp only [bind_tc_ok, profile_fields_contract, projection_contract]
  rfl

/-- Both default representations reject every session pair at the v2 floor. -/
theorem default_rejects_contract (topology : Topology) (transport : Transport) :
    (do let caps ← SessionCapabilities.Insts.CoreDefaultDefault.default
        supports_session caps topology transport) = RustM.ok false := by
  rw [compact_default_contract]
  simp [support_contract]

/-- Future profiles accept, old profiles reject, and missing axes reject through From. -/
theorem profile_outcomes :
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from
          { version := 65535#u16,
            transports := ⟨[Transport.Direct, .WebRtc, .WebRtc], by scalar_tac⟩,
            topologies := ⟨[Topology.Mesh, .Host, .Mesh], by scalar_tac⟩ }
        supports_session caps .Mesh .WebRtc) = RustM.ok true ∧
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from
          { version := 2#u16,
            transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
            topologies := ⟨[Topology.Mesh], by scalar_tac⟩ }
        supports_session caps .Mesh .WebRtc) = RustM.ok false ∧
    (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from
          { version := 3#u16,
            transports := ⟨[], by scalar_tac⟩,
            topologies := ⟨[Topology.Mesh], by scalar_tac⟩ }
        supports_session caps .Mesh .WebRtc) = RustM.ok false := by
  simp [profile_support_contract]

end Projection
