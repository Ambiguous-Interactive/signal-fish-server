import Projection.Verification.Profiles

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_admission

namespace Projection

/-- Admission uses the actual stored axes, for all versions, vectors and stored plans. -/
theorem admission_contract (profile : NegotiatedProtocol) (stored : ActiveSessionPlan) :
    joiner_supports_sticky_plan profile stored = RustM.ok
      (decide (profile.version.val ≥ 3) && decide (stored.topology ∈ profile.topologies.val) &&
        decide (stored.transport ∈ profile.transports.val)) := by
  simp [joiner_supports_sticky_plan,
    alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
    rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
    rust_primitives.slice.slice_contains,
    contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
    contains_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq,
    Aeneas.Std.bind, Bind.bind]
  all_goals (try split) <;> (try simp_all)
  all_goals (try split) <;> (try simp_all)

/-- Admission agrees with the actual projection/support path used by session selection. -/
theorem admission_support_contract (profile : NegotiatedProtocol) (stored : ActiveSessionPlan) :
    joiner_supports_sticky_plan profile stored =
      (do let caps ← SessionCapabilities.Insts.CoreConvertFromShared0NegotiatedProtocol.from profile
          supports_session caps stored.topology stored.transport) := by
  rw [admission_contract, profile_support_contract]

/-- Ordinary client admission requires neither a stored host nor a Direct endpoint. -/
theorem admission_host_independent (profile : NegotiatedProtocol) (stored : ActiveSessionPlan)
    (host : core.option.Option uuid.Uuid) :
    joiner_supports_sticky_plan profile { stored with host := host } =
      joiner_supports_sticky_plan profile stored := by
  simp only [admission_contract]

/-- Direct clients are reachable; old versions and either missing axis are refused. -/
theorem admission_outcomes :
    joiner_supports_sticky_plan
      { version := 3#u16, transports := ⟨[Transport.Direct], by scalar_tac⟩,
        topologies := ⟨[Topology.Host], by scalar_tac⟩ }
      { topology := .Host, transport := .Direct, host := .None } = RustM.ok true ∧
    joiner_supports_sticky_plan
      { version := 2#u16, transports := ⟨[Transport.Direct], by scalar_tac⟩,
        topologies := ⟨[Topology.Host], by scalar_tac⟩ }
      { topology := .Host, transport := .Direct, host := .None } = RustM.ok false ∧
    joiner_supports_sticky_plan
      { version := 65535#u16, transports := ⟨[Transport.WebRtc, .Direct, .WebRtc], by scalar_tac⟩,
        topologies := ⟨[], by scalar_tac⟩ }
      { topology := .Host, transport := .Direct, host := .None } = RustM.ok false ∧
    joiner_supports_sticky_plan
      { version := 65535#u16, transports := ⟨[], by scalar_tac⟩,
        topologies := ⟨[Topology.Mesh, .Host, .Mesh], by scalar_tac⟩ }
      { topology := .Host, transport := .Direct, host := .None } = RustM.ok false := by
  simp [admission_contract]

end Projection
