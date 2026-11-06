import Capability.Verification.ProofObligations

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability

namespace Capability

/-- The support decision required for each member in the production callback. -/
def MemberSupport (caps : SessionCapabilities) (topology : Topology) (transport : Transport) : Bool :=
  decide (caps.version.val ≥ 3) &&
    (match topology with
    | .Relay => caps.relay_topology
    | .Host => caps.host_topology
    | .Mesh => caps.mesh_topology) &&
    (match transport with
    | .Relay => caps.relay_transport
    | .Direct => caps.direct_transport
    | .WebRtc => caps.webrtc_transport)

theorem supports_value (caps : SessionCapabilities) (topology : Topology) (transport : Transport) :
    supports_session caps topology transport = RustM.ok (MemberSupport caps topology transport) := by
  cases topology <;> cases transport <;>
    simp [supports_session, SessionCapabilities.supports_topology,
      SessionCapabilities.supports_transport, MemberSupport, Aeneas.Std.bind, Bind.bind]
  all_goals (try split) <;> (try simp_all)
  all_goals (try split) <;> (try simp_all)

/-- The getter terminates successfully and returns the same capability value for each member.
This is an explicit boundary: the production member-field getter is checked by adapter tests. -/
def GetterContract {T G : Type}
    (getter : CoreModels.core.ops.function.Fn G T SessionCapabilities) (state : G)
    (project : T → SessionCapabilities) : Prop :=
  ∀ member, getter.call state member = RustM.ok (project member)

/-- The actual extracted Rust closure satisfies the generic callback contract.
Its captured getter and requested axes stay unchanged after every modeled call. -/
theorem concrete_callback_contract {T G : Type}
    (getter : CoreModels.core.ops.function.Fn G T SessionCapabilities) (state : G)
    (project : T → SessionCapabilities) (contract : GetterContract getter state project)
    (topology : Topology) (transport : Transport) :
    CallbackContract
      (all_support.closure.Insts.CoreOpsFunctionFnMutTupleSharedTBool getter)
      (fun member => MemberSupport (project member) topology transport)
      (fun capture => capture = (state, topology, transport)) := by
  intro capture member valid
  subst capture
  refine ⟨(state, topology, transport), ?_, rfl⟩
  simp [all_support.closure.Insts.CoreOpsFunctionFnMutTupleSharedTBool.call_mut,
    contract member, supports_value, Aeneas.Std.bind, Bind.bind]

/-- All representable finite production-member slices, all u16 versions, all capability bits,
and all nine requested axis pairs, under the stated member-getter boundary. -/
theorem concrete_aggregation_contract {T G : Type}
    (getter : CoreModels.core.ops.function.Fn G T SessionCapabilities) (state : G)
    (project : T → SessionCapabilities) (contract : GetterContract getter state project)
    (members : Slice T) (topology : Topology) (transport : Transport) :
    all_support getter members state topology transport = RustM.ok
      (!members.val.isEmpty && members.val.all
        (fun member => MemberSupport (project member) topology transport)) := by
  unfold all_support
  apply all_members_contract _ _ (fun capture => capture = (state, topology, transport))
  · exact concrete_callback_contract getter state project contract topology transport
  · rfl

/-- Identity getter for capability-only reachability witnesses; the aggregation closure is extracted. -/
def capabilityGetter : CoreModels.core.ops.function.Fn Unit SessionCapabilities SessionCapabilities where
  FnMutInst := {
    FnOnceInst := { call_once := fun _ member => RustM.ok member }
    call_mut := fun state member => RustM.ok (member, state) }
  call := fun _ member => RustM.ok member

/-- Empty rooms reject, future versions can accept, and a missing axis or old member rejects
in either member ordering. No getter or capability assumptions hide these cases. -/
theorem concrete_aggregation_outcomes :
    let supported : SessionCapabilities := ⟨65535#u16, false, false, true, false, false, true⟩
    let old : SessionCapabilities := ⟨2#u16, false, false, true, false, false, true⟩
    let missing : SessionCapabilities := ⟨3#u16, false, false, false, false, false, true⟩
    all_support capabilityGetter (⟨[], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok false ∧
    all_support capabilityGetter (⟨[supported, supported], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok true ∧
    all_support capabilityGetter (⟨[missing], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok false ∧
    all_support capabilityGetter (⟨[missing, supported], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok false ∧
    all_support capabilityGetter (⟨[supported, missing], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok false ∧
    all_support capabilityGetter (⟨[old, supported], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok false ∧
    all_support capabilityGetter (⟨[supported, old], by scalar_tac⟩ : Slice SessionCapabilities) () .Mesh .WebRtc = RustM.ok false := by
  dsimp only
  simp only [concrete_aggregation_contract capabilityGetter () id (fun _ => rfl)]
  simp [MemberSupport]

end Capability
