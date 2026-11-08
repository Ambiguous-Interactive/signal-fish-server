import Projection.Verification.CapabilityOrder

open CoreModels Aeneas
open Aeneas.Std hiding namespace core alloc
open signal_fish_proof_core session_capability

namespace Projection

/-- The negotiated preference list is the first-occurrence list, with Relay appended only if absent. -/
def negotiatedAxis {T : Type} [DecidableEq T] (floor : T)
    (advertised : core.option.Option (alloc.vec.Vec T)) : List T :=
  let requested := match advertised with | .None => [floor] | .Some xs => xs.val
  firstOccurrences (if floor ∈ requested then requested else requested ++ [floor]) []

/-- Appending Relay needs one representable slot before the production deduplication runs.
This is an explicit successful-execution assumption, not a proved frame-limit correspondence. -/
def axisHasRoom {T : Type} (advertised : core.option.Option (alloc.vec.Vec T)) : Prop :=
  match advertised with | .None => True | .Some xs => xs.val.length < Usize.max

/-- Exact axis lists for every version and advertised vector satisfying the append bound. -/
theorem negotiated_axes_contract (version : U16)
    (transports : core.option.Option (alloc.vec.Vec Transport))
    (topologies : core.option.Option (alloc.vec.Vec Topology))
    (transportRoom : axisHasRoom transports) (topologyRoom : axisHasRoom topologies) :
    ∃ actualTransports actualTopologies,
      negotiate_capabilities version transports topologies =
        RustM.ok (actualTransports, actualTopologies) ∧
      actualTransports.val = (if version.val < 3 then [Transport.Relay]
        else negotiatedAxis Transport.Relay transports) ∧
      actualTopologies.val = (if version.val < 3 then [Topology.Relay]
        else negotiatedAxis Topology.Relay topologies) := by
  by_cases old : version.val < 3
  · simp [negotiate_capabilities, old, alloc.slice.Slice.into_vec,
      alloc.slice.Dummy.into_vec, rust_primitives.sequence.seq_from_boxed_slice, alloc.vec.from_seq, lift, Array.to_slice, Array.make,
      Aeneas.Std.bind, Bind.bind]
  · cases transports <;> cases topologies <;>
      simp only [axisHasRoom] at transportRoom topologyRoom <;>
      simp [negotiate_capabilities, old, negotiatedAxis, transportRoom, topologyRoom, firstOccurrences,
        core.option.Option.unwrap_or_else,
        negotiate_capabilities.closure.Insts.CoreOpsFunctionFnOnceTupleVecTransport.call_once,
        negotiate_capabilities.closure_1.Insts.CoreOpsFunctionFnOnceTupleVecTopology.call_once,
        alloc.slice.Slice.into_vec, alloc.slice.Dummy.into_vec, rust_primitives.sequence.seq_from_boxed_slice, alloc.vec.from_seq, lift,
        Array.to_slice, Array.make,
        alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
        rust_primitives.sequence.seq_to_slice, core.slice.Slice.contains,
        rust_primitives.slice.slice_contains,
        contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
        contains_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq,
        alloc.vec.Vec.push, rust_primitives.sequence.seq_push,
        dedup_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
        dedup_contract Topology.Insts.CoreCmpPartialEqTopology topology_eq,
        Aeneas.Std.bind, Bind.bind] <;>
      split_ifs <;> simp_all

/-- Every pre-v3 request returns exactly Relay, even if its advertised vectors fill usize. -/
theorem negotiated_axes_legacy (version : U16)
    (transports : core.option.Option (alloc.vec.Vec Transport))
    (topologies : core.option.Option (alloc.vec.Vec Topology)) (legacy : version.val < 3) :
    negotiate_capabilities version transports topologies =
      RustM.ok ((⟨[Transport.Relay], by scalar_tac⟩ : alloc.vec.Vec Transport),
        (⟨[Topology.Relay], by scalar_tac⟩ : alloc.vec.Vec Topology)) := by
  simp [negotiate_capabilities, legacy, alloc.slice.Slice.into_vec,
    alloc.slice.Dummy.into_vec, rust_primitives.sequence.seq_from_boxed_slice,
    alloc.vec.from_seq, lift, Array.to_slice, Array.make, Aeneas.Std.bind, Bind.bind]
  rfl

/-- Exactly the advertised values plus Relay; omitted axes advertise no additional values. -/
def axisAdvertises {T : Type} (value : T)
    (advertised : core.option.Option (alloc.vec.Vec T)) : Prop :=
  match advertised with | .None => False | .Some xs => value ∈ xs.val

theorem negotiated_axis_membership {T : Type} [DecidableEq T] (floor value : T)
    (advertised : core.option.Option (alloc.vec.Vec T)) :
    value ∈ negotiatedAxis floor advertised ↔ value = floor ∨ axisAdvertises value advertised := by
  cases advertised with
  | none => simp [negotiatedAxis, first_occurrences_membership, axisAdvertises]
  | some xs =>
    by_cases present : floor ∈ xs.val
    · simp [negotiatedAxis, first_occurrences_membership, axisAdvertises, present]
      intro same
      subst value
      exact present
    · simp [negotiatedAxis, first_occurrences_membership, axisAdvertises, present, or_comm]

theorem negotiated_axis_unique {T : Type} [DecidableEq T] (floor : T)
    (advertised : core.option.Option (alloc.vec.Vec T)) :
    (negotiatedAxis floor advertised).Nodup := by
  apply first_occurrences_nodup
  simp

/-- The actual v3+ result has no duplicates, includes Relay and has no unadvertised upgrades. -/
theorem negotiated_axes_protocol_contract (version : U16)
    (transports : core.option.Option (alloc.vec.Vec Transport))
    (topologies : core.option.Option (alloc.vec.Vec Topology))
    (modern : 3 ≤ version.val)
    (transportRoom : axisHasRoom transports) (topologyRoom : axisHasRoom topologies) :
    ∃ ts ps, negotiate_capabilities version transports topologies = RustM.ok (ts, ps) ∧
      ts.val.Nodup ∧ ps.val.Nodup ∧ Transport.Relay ∈ ts.val ∧ Topology.Relay ∈ ps.val ∧
      (∀ value, value ∈ ts.val ↔ value = Transport.Relay ∨ axisAdvertises value transports) ∧
      (∀ value, value ∈ ps.val ↔ value = Topology.Relay ∨ axisAdvertises value topologies) := by
  obtain ⟨ts, ps, returns, exactTs, exactPs⟩ :=
    negotiated_axes_contract version transports topologies transportRoom topologyRoom
  have old : ¬ version.val < 3 := Nat.not_lt.mpr modern
  simp only [old, ↓reduceIte] at exactTs exactPs
  refine ⟨ts, ps, returns, ?_⟩
  rw [exactTs, exactPs]
  exact ⟨negotiated_axis_unique _ _, negotiated_axis_unique _ _,
    (negotiated_axis_membership _ _ _).mpr (Or.inl rfl),
    (negotiated_axis_membership _ _ _).mpr (Or.inl rfl),
    fun value => negotiated_axis_membership _ value _,
    fun value => negotiated_axis_membership _ value _⟩

/-- Observe both exact lists without hiding the produced vector values. -/
theorem negotiated_axes_values (version : U16)
    (transports : core.option.Option (alloc.vec.Vec Transport))
    (topologies : core.option.Option (alloc.vec.Vec Topology))
    (transportRoom : axisHasRoom transports) (topologyRoom : axisHasRoom topologies) :
    (do let result ← negotiate_capabilities version transports topologies
        RustM.ok (result.1.val, result.2.val)) =
      RustM.ok ((if version.val < 3 then [Transport.Relay] else negotiatedAxis Transport.Relay transports),
        (if version.val < 3 then [Topology.Relay] else negotiatedAxis Topology.Relay topologies)) := by
  obtain ⟨ts, ps, returns, exactTs, exactPs⟩ :=
    negotiated_axes_contract version transports topologies transportRoom topologyRoom
  rw [returns]
  simp [Aeneas.Std.bind, Bind.bind, exactTs, exactPs]

/-- Legacy refusal, omitted/empty defaults and future-version ordered deduplication are reachable. -/
theorem negotiated_axes_outcomes :
    negotiate_capabilities 2#u16
      (.Some ⟨[Transport.WebRtc], by scalar_tac⟩)
      (.Some ⟨[Topology.Mesh], by scalar_tac⟩) =
        RustM.ok ((⟨[Transport.Relay], by scalar_tac⟩ : alloc.vec.Vec Transport),
          (⟨[Topology.Relay], by scalar_tac⟩ : alloc.vec.Vec Topology)) ∧
    (do let result ← negotiate_capabilities 3#u16 .None .None
        RustM.ok (result.1.val, result.2.val)) = RustM.ok ([Transport.Relay], [Topology.Relay]) ∧
    (do let result ← negotiate_capabilities 3#u16
          (.Some ⟨[], by scalar_tac⟩) (.Some ⟨[], by scalar_tac⟩)
        RustM.ok (result.1.val, result.2.val)) = RustM.ok ([Transport.Relay], [Topology.Relay]) ∧
    (do let result ← negotiate_capabilities 65535#u16
          (.Some ⟨[Transport.WebRtc, .Relay, .Direct, .WebRtc], by scalar_tac⟩)
          (.Some ⟨[Topology.Mesh, .Host, .Mesh], by scalar_tac⟩)
        RustM.ok (result.1.val, result.2.val)) =
          RustM.ok ([Transport.WebRtc, .Relay, .Direct], [Topology.Mesh, .Host, .Relay]) := by
  refine ⟨negotiated_axes_legacy _ _ _ (by scalar_tac), ?_⟩
  repeat' constructor
  all_goals rw [negotiated_axes_values _ _ _ (by simp [axisHasRoom] <;> scalar_tac)
    (by simp [axisHasRoom] <;> scalar_tac)]
  all_goals simp [negotiatedAxis, firstOccurrences]

end Projection
