import Capability.Verification.ProofObligations
open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability
namespace Capability
private def getter : CoreModels.core.ops.function.Fn Unit Unit SessionCapabilities where
  FnMutInst := {
    FnOnceInst := { call_once := fun _ _ => RustM.ok ⟨3#u16, false, true, false, false, false, true⟩ }
    call_mut := fun state _ => RustM.ok (⟨3#u16, false, true, false, false, false, true⟩, state) }
  call := fun _ _ => RustM.ok ⟨3#u16, false, true, false, false, false, true⟩

/-- The requested Direct/Mesh pair is present, but the faulty callback checks absent Relay. -/
theorem mutation_returns_wrong_value :
    all_support getter (⟨[()], by scalar_tac⟩ : Slice Unit) () .Mesh .Direct = RustM.ok false := by
  unfold all_support
  apply all_members_contract _ (fun _ => false)
    (fun capture => capture = ((), Topology.Mesh, Transport.Direct))
  · intro capture member valid
    subst capture
    refine ⟨((), Topology.Mesh, Transport.Direct), ?_, rfl⟩
    simp [all_support.closure.Insts.CoreOpsFunctionFnMutTupleSharedTBool.call_mut,
      getter, supports_session, SessionCapabilities.supports_topology,
      SessionCapabilities.supports_transport, Aeneas.Std.bind, Bind.bind]
    rfl
  · rfl
end Capability
