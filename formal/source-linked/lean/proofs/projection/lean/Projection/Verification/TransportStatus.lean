import Projection.Verification.Profiles

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability

namespace Projection

abbrev StoredTransportStatus := core.option.Option (uuid.Uuid × Transport × Bool)

/-- Independent report contract: reject first, otherwise compare the whole saved tuple. -/
noncomputable def transportStatusSpec (profile : NegotiatedProtocol) (generation : uuid.Uuid)
    (status : StoredTransportStatus) (transport : Transport) (connected : Bool) :
    TransportStatusUpdate × StoredTransportStatus :=
  if profile.version.val < 3 then (.UnsupportedProtocolVersion, status)
  else if transport ∉ profile.transports.val then (.UnsupportedTransport, status)
  else if status = .Some (generation, transport, connected) then (.Duplicate, status)
  else (.Changed, .Some (generation, transport, connected))

/-- Exact result and final mutable state for every profile, generation and prior report. -/
theorem transport_status_contract (profile : NegotiatedProtocol) (generation : uuid.Uuid)
    (status : StoredTransportStatus) (transport : Transport) (connected : Bool) :
    update_transport_status profile generation status transport connected =
      RustM.ok (transportStatusSpec profile generation status transport connected) := by
  by_cases old : profile.version.val < 3
  · simp [update_transport_status, transportStatusSpec, old]
  · by_cases absent : transport ∉ profile.transports.val
    · simp [update_transport_status, transportStatusSpec, old, absent,
        alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
        rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
        rust_primitives.slice.slice_contains,
        contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
        Aeneas.Std.bind, Bind.bind]
    · cases status with
      | none =>
        simp [update_transport_status, transportStatusSpec, old, absent,
          alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
          rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
          rust_primitives.slice.slice_contains,
          contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
          Aeneas.Std.bind, Bind.bind]
      | some previous =>
        rcases previous with ⟨oldGeneration, oldTransport, oldConnected⟩
        by_cases sameGeneration : oldGeneration = generation <;>
          by_cases sameTransport : oldTransport = transport <;>
          by_cases sameConnected : oldConnected = connected <;>
          simp [old, absent, sameGeneration, sameTransport, sameConnected,
            update_transport_status, transportStatusSpec,
            alloc.vec.Vec.Insts.CoreOpsDerefDerefSlice.deref, alloc.vec.Vec.as_slice,
            rust_primitives.sequence.seq_to_slice, CoreModels.core.slice.Slice.contains,
            rust_primitives.slice.slice_contains,
            contains_contract Transport.Insts.CoreCmpPartialEqTransport transport_eq,
            uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq, transport_eq,
            Aeneas.Std.bind, Bind.bind]

/-- Both rejection paths leave the stored report unchanged. -/
theorem transport_status_rejection_preserves (profile : NegotiatedProtocol) (generation : uuid.Uuid)
    (status : StoredTransportStatus) (transport : Transport) (connected : Bool)
    (rejected : profile.version.val < 3 ∨ transport ∉ profile.transports.val) :
    ∃ result, update_transport_status profile generation status transport connected =
      RustM.ok (result, status) ∧
      (result = .UnsupportedProtocolVersion ∨ result = .UnsupportedTransport) := by
  rw [transport_status_contract]
  unfold transportStatusSpec
  by_cases old : profile.version.val < 3
  · simp [old]
  · have absent := rejected.resolve_left old
    simp [old, absent]

/-- An exact current-generation repeat is a duplicate and does not write. -/
theorem transport_status_duplicate_contract (profile : NegotiatedProtocol) (generation : uuid.Uuid)
    (transport : Transport) (connected : Bool)
    (version : 3 ≤ profile.version.val) (negotiated : transport ∈ profile.transports.val) :
    update_transport_status profile generation (.Some (generation, transport, connected))
      transport connected = RustM.ok (.Duplicate, .Some (generation, transport, connected)) := by
  rw [transport_status_contract]
  simp [transportStatusSpec, Nat.not_lt.mpr version, negotiated]

/-- Every admitted nonduplicate stores the complete current report. -/
theorem transport_status_changed_contract (profile : NegotiatedProtocol) (generation : uuid.Uuid)
    (status : StoredTransportStatus) (transport : Transport) (connected : Bool)
    (version : 3 ≤ profile.version.val) (negotiated : transport ∈ profile.transports.val)
    (different : status ≠ .Some (generation, transport, connected)) :
    update_transport_status profile generation status transport connected =
      RustM.ok (.Changed, .Some (generation, transport, connected)) := by
  rw [transport_status_contract]
  simp [transportStatusSpec, Nat.not_lt.mpr version, negotiated, different]

/-- Repeating an old membership's report refreshes its generation, even if values agree. -/
theorem transport_status_stale_generation (profile : NegotiatedProtocol)
    (generation previous : uuid.Uuid) (transport : Transport) (connected : Bool)
    (version : 3 ≤ profile.version.val) (negotiated : transport ∈ profile.transports.val)
    (stale : previous ≠ generation) :
    update_transport_status profile generation (.Some (previous, transport, connected))
      transport connected = RustM.ok (.Changed, .Some (generation, transport, connected)) := by
  apply transport_status_changed_contract _ _ _ _ _ version negotiated
  intro equal
  exact stale (by cases equal; rfl)

/-- Every report terminates in the extracted model, with no MissingConnection outcome. -/
theorem transport_status_total_contract (profile : NegotiatedProtocol) (generation : uuid.Uuid)
    (status : StoredTransportStatus) (transport : Transport) (connected : Bool) :
    ∃ result finalState,
      update_transport_status profile generation status transport connected =
        RustM.ok (result, finalState) ∧ result ≠ .MissingConnection := by
  rw [transport_status_contract]
  unfold transportStatusSpec
  split <;> try { simp }
  split <;> try { simp }
  split <;> simp

/-- All four reachable outcomes, including the state write on initial acceptance. -/
theorem transport_status_outcomes (generation : uuid.Uuid) :
    update_transport_status
      ⟨2#u16, ⟨[.WebRtc], by scalar_tac⟩, ⟨[], by scalar_tac⟩⟩
      generation .None .WebRtc true = RustM.ok (.UnsupportedProtocolVersion, .None) ∧
    update_transport_status
      ⟨3#u16, ⟨[], by scalar_tac⟩, ⟨[], by scalar_tac⟩⟩
      generation .None .WebRtc true = RustM.ok (.UnsupportedTransport, .None) ∧
    update_transport_status
      ⟨65535#u16, ⟨[.WebRtc, .WebRtc], by scalar_tac⟩, ⟨[], by scalar_tac⟩⟩
      generation .None .WebRtc true =
        RustM.ok (.Changed, .Some (generation, .WebRtc, true)) ∧
    update_transport_status
      ⟨3#u16, ⟨[.WebRtc], by scalar_tac⟩, ⟨[], by scalar_tac⟩⟩
      generation (.Some (generation, .WebRtc, true)) .WebRtc true =
        RustM.ok (.Duplicate, .Some (generation, .WebRtc, true)) := by
  simp [transport_status_contract, transportStatusSpec]

end Projection
