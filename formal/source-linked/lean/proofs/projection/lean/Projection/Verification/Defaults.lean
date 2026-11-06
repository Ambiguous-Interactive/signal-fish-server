import Projection.Verification.ProofObligations

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability

namespace Projection

/-- A present lookup result preserves every field, even below the supplied floor. -/
theorem known_profile_contract (caps : SessionCapabilities) (minimum : U16) :
    resolve_capabilities (.Some caps) minimum = RustM.ok caps := by
  simp [resolve_capabilities]

/-- An absent lookup result uses exactly the supplied floor and relay-only capabilities. -/
theorem absent_profile_contract (minimum : U16) :
    resolve_capabilities .None minimum = RustM.ok
      { version := minimum, relay_transport := true, direct_transport := false,
        webrtc_transport := false, relay_topology := true, host_topology := false,
        mesh_topology := false } := by
  simp [resolve_capabilities]

/-- Production projection followed by lookup-result resolution preserves the projection.
Lookup locking and the origin of the supplied floor remain caller obligations. -/
theorem resolved_projection_contract (version minimum : U16)
    (transports : Slice Transport) (topologies : Slice Topology) :
    (do let caps ← project_capabilities version transports topologies
        resolve_capabilities (.Some caps) minimum) =
      project_capabilities version transports topologies := by
  rw [projection_contract]
  simp only [bind_tc_ok, known_profile_contract]

/-- Known profiles retain the exact version and axis support decision after resolution. -/
theorem resolved_support_contract (version minimum : U16)
    (transports : Slice Transport) (topologies : Slice Topology)
    (topology : Topology) (transport : Transport) :
    (do let caps ← project_capabilities version transports topologies
        let resolved ← resolve_capabilities (.Some caps) minimum
        supports_session resolved topology transport) = RustM.ok
      (decide (version.val ≥ 3) && decide (topology ∈ topologies.val) &&
        decide (transport ∈ transports.val)) := by
  simp only [projection_contract, bind_tc_ok, known_profile_contract, support_contract]
  cases topology <;> cases transport <;> rfl

/-- Missing profiles support only Relay/Relay, and only when the floor is at least three.
The production floor is two; this theorem does not assume that caller fact. -/
theorem absent_support_contract (minimum : U16) (topology : Topology) (transport : Transport) :
    (do let caps ← resolve_capabilities .None minimum
        supports_session caps topology transport) = RustM.ok
      (decide (minimum.val ≥ 3) && decide (topology = .Relay) &&
        decide (transport = .Relay)) := by
  rw [absent_profile_contract]
  simp only [bind_tc_ok, support_contract]
  cases topology <;> cases transport <;> simp [topologyMember, transportMember]

/-- Known future profiles accept; missing profiles reject at floor two and allow relay at three. -/
theorem resolution_outcomes :
    (do let caps ← project_capabilities 65535#u16
          (⟨[Transport.WebRtc, Transport.WebRtc], by scalar_tac⟩ : Slice Transport)
          (⟨[Topology.Mesh], by scalar_tac⟩ : Slice Topology)
        let resolved ← resolve_capabilities (.Some caps) 2#u16
        supports_session resolved .Mesh .WebRtc) = RustM.ok true ∧
    (∀ topology transport,
      (do let caps ← resolve_capabilities .None 2#u16
          supports_session caps topology transport) = RustM.ok false) ∧
    (do let caps ← resolve_capabilities .None 3#u16
        supports_session caps .Relay .Relay) = RustM.ok true := by
  constructor
  · simp [resolved_support_contract]
  · constructor
    · intro topology transport
      simp [absent_support_contract]
    · simp [absent_support_contract]

end Projection
