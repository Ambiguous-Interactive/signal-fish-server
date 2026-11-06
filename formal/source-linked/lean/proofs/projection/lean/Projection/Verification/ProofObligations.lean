import Projection.Extraction

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability

namespace Projection

instance : DecidableEq Transport := by intro a b; cases a <;> cases b <;> first | exact isTrue rfl | exact isFalse (by intro h; cases h)
instance : DecidableEq Topology := by intro a b; cases a <;> cases b <;> first | exact isTrue rfl | exact isFalse (by intro h; cases h)

private theorem transport_eq (a b : Transport) :
    Transport.Insts.CoreCmpPartialEqTransport.eq a b = RustM.ok (decide (a = b)) := by
  cases a <;> cases b <;> simp [Transport.Insts.CoreCmpPartialEqTransport.eq, Transport.read_discriminant]

private theorem topology_eq (a b : Topology) :
    Topology.Insts.CoreCmpPartialEqTopology.eq a b = RustM.ok (decide (a = b)) := by
  cases a <;> cases b <;> simp [Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant]

private theorem contains_contract {T : Type} [DecidableEq T]
    (inst : CoreModels.core.cmp.PartialEq T T)
    (eq_contract : ∀ a b, inst.eq a b = RustM.ok (decide (a = b)))
    (xs : List T) (x : T) :
    rust_primitives.slice.slice_contains_go inst x xs = RustM.ok (decide (x ∈ xs)) := by
  induction xs with
  | nil => rfl
  | cons head tail ih =>
    simp only [rust_primitives.slice.slice_contains_go, eq_contract]
    by_cases equal : head = x
    · subst head; simp [Aeneas.Std.bind, Bind.bind]
    · simp [Aeneas.Std.bind, Bind.bind, equal, ih, Ne.symm equal]

def transportMember (caps : SessionCapabilities) (transport : Transport) : Bool :=
  match transport with
  | .Relay => caps.relay_transport
  | .Direct => caps.direct_transport
  | .WebRtc => caps.webrtc_transport

def topologyMember (caps : SessionCapabilities) (topology : Topology) : Bool :=
  match topology with
  | .Relay => caps.relay_topology
  | .Host => caps.host_topology
  | .Mesh => caps.mesh_topology

/-- Every version and capability assignment, with both requested axes required. -/
theorem support_contract (caps : SessionCapabilities) (topology : Topology) (transport : Transport) :
    supports_session caps topology transport = RustM.ok
      (decide (caps.version.val ≥ 3) && topologyMember caps topology && transportMember caps transport) := by
  cases topology <;> cases transport <;>
    simp [supports_session, SessionCapabilities.supports_topology,
      SessionCapabilities.supports_transport, topologyMember, transportMember,
      Aeneas.Std.bind, Bind.bind]
  all_goals (try split) <;> (try simp_all)
  all_goals (try split) <;> (try simp_all)

/-- Projection preserves all six membership bits, for arbitrary representable slices. -/
theorem projection_contract (version : U16) (transports : Slice Transport) (topologies : Slice Topology) :
    project_capabilities version transports topologies = RustM.ok
      { version := version,
        relay_transport := decide (Transport.Relay ∈ transports.val),
        direct_transport := decide (Transport.Direct ∈ transports.val),
        webrtc_transport := decide (Transport.WebRtc ∈ transports.val),
        relay_topology := decide (Topology.Relay ∈ topologies.val),
        host_topology := decide (Topology.Host ∈ topologies.val),
        mesh_topology := decide (Topology.Mesh ∈ topologies.val) } := by
  simp [project_capabilities, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains, contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    contains_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq, Aeneas.Std.bind, Bind.bind]

/-- Composition of the two extracted production decisions; no subset or version assumptions. -/
theorem projected_support_contract (version : U16)
    (transports : Slice Transport) (topologies : Slice Topology)
    (topology : Topology) (transport : Transport) :
    (do let caps ← project_capabilities version transports topologies
        supports_session caps topology transport) = RustM.ok
      (decide (version.val ≥ 3) && decide (topology ∈ topologies.val) &&
        decide (transport ∈ transports.val)) := by
  rw [projection_contract]
  simp only [bind_tc_ok, support_contract]
  cases topology <;> cases transport <;> rfl

/-- Successful, old-version, and missing-axis cases are all reachable. -/
theorem support_outcomes :
    (do let caps ← project_capabilities 3#u16
          (⟨[Transport.WebRtc], by scalar_tac⟩ : Slice Transport)
          (⟨[Topology.Mesh], by scalar_tac⟩ : Slice Topology)
        supports_session caps .Mesh .WebRtc) = RustM.ok true ∧
    (do let caps ← project_capabilities 2#u16
          (⟨[Transport.WebRtc], by scalar_tac⟩ : Slice Transport)
          (⟨[Topology.Mesh], by scalar_tac⟩ : Slice Topology)
        supports_session caps .Mesh .WebRtc) = RustM.ok false ∧
    (do let caps ← project_capabilities 65535#u16
          (⟨[], by scalar_tac⟩ : Slice Transport)
          (⟨[Topology.Mesh], by scalar_tac⟩ : Slice Topology)
        supports_session caps .Mesh .WebRtc) = RustM.ok false := by
  simp [projected_support_contract]

end Projection
