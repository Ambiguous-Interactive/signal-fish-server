import Projection.Verification.Profiles

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_admission

namespace Projection

/-- ICE before finalization requires every policy, room, version and capability gate.
    The domain includes all versions and arbitrary representable capability vectors. -/
theorem pregather_contract (enabled webrtc finalized : Bool) (desired : Topology)
    (profile : NegotiatedProtocol) :
    ice_pregather_eligible enabled webrtc desired finalized profile = RustM.ok
      (enabled && webrtc && decide (desired ≠ .Relay) && !finalized &&
        decide (profile.version.val ≥ 3) &&
        decide (Transport.WebRtc ∈ profile.transports.val) &&
        (decide (desired ∈ profile.topologies.val) ||
          (decide (desired = .Mesh) && decide (Topology.Host ∈ profile.topologies.val)))) := by
  cases enabled <;> cases webrtc <;> cases desired <;> cases finalized <;>
    simp [ice_pregather_eligible, CoreModels.core.cmp.PartialEq.ne.default, topology_eq,
      alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
      rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
      rust_primitives.slice.slice_contains,
      contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
      contains_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq,
      Aeneas.Std.bind, Bind.bind]
  all_goals (try split) <;> (try simp_all)
  all_goals (try split) <;> (try simp_all)
  all_goals (try split) <;> (try simp_all)

/-- The gate agrees with the actual profile projection and WebRTC support predicate. -/
theorem pregather_support_contract (enabled webrtc finalized : Bool) (desired : Topology)
    (profile : NegotiatedProtocol) :
    ice_pregather_eligible enabled webrtc desired finalized profile =
      (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from profile
          let supported ← supports_session caps desired .WebRtc
          let hostSupported ← supports_session caps .Host .WebRtc
          RustM.ok (enabled && webrtc && decide (desired ≠ .Relay) && !finalized &&
            (supported || (decide (desired = .Mesh) && hostSupported)))) := by
  rw [pregather_contract, profile_fields_contract, projection_contract]
  simp only [bind_tc_ok, support_contract]
  cases desired <;>
    simp [topologyMember, transportMember, Bool.and_assoc, Bool.and_comm, Bool.and_left_comm, Bool.and_or_distrib_right]

/-- Both upgraded topologies are reachable, including future versions and duplicate axes. -/
theorem pregather_outcomes :
    ice_pregather_eligible true true .Mesh false
      { version := 3#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[Topology.Host], by scalar_tac⟩ } = RustM.ok true ∧
    ice_pregather_eligible true true .Mesh false
      { version := 3#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[Topology.Mesh], by scalar_tac⟩ } = RustM.ok true ∧
    ice_pregather_eligible true true .Host false
      { version := 65535#u16, transports := ⟨[Transport.Direct, .WebRtc, .WebRtc], by scalar_tac⟩,
        topologies := ⟨[Topology.Mesh, .Host, .Mesh], by scalar_tac⟩ } = RustM.ok true ∧
    ice_pregather_eligible true true .Mesh false
      { version := 2#u16, transports := ⟨[Transport.WebRtc], by scalar_tac⟩,
        topologies := ⟨[Topology.Mesh], by scalar_tac⟩ } = RustM.ok false ∧
    ice_pregather_eligible true true .Host false
      { version := 65535#u16, transports := ⟨[], by scalar_tac⟩,
        topologies := ⟨[Topology.Host], by scalar_tac⟩ } = RustM.ok false := by
  simp [pregather_contract]

end Projection
