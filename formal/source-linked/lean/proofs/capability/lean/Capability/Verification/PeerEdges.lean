import Capability.Verification.Members
import Capability.Verification.ElectionFixtures

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_capability session_member

noncomputable section
namespace Capability

/-- The glare rule follows the trusted UUID byte-order interpretation. -/
theorem glare_contract (left right : uuid.Uuid) :
    local_initiates left right = RustM.ok (decide (ElectionOrder.uuidKey left < ElectionOrder.uuidKey right)) := by
  by_cases before : ElectionOrder.uuidKey left < ElectionOrder.uuidKey right
  · simp [local_initiates, CoreModels.core.cmp.PartialOrd.lt.default,
      uuid.Uuid.Insts.CoreCmpPartialOrdUuid, ElectionOrder.orderBy,
      before, Aeneas.Std.bind, Bind.bind]
  · by_cases same : ElectionOrder.uuidKey left = ElectionOrder.uuidKey right <;>
      simp [local_initiates, CoreModels.core.cmp.PartialOrd.lt.default,
        uuid.Uuid.Insts.CoreCmpPartialOrdUuid, ElectionOrder.orderBy,
        before, same, Aeneas.Std.bind, Bind.bind]

/-- The protocol graph: no self or incompatible endpoints; mesh is complete;
Host is a star with client offers; Relay has no edges. No legal-pair or unique-ID
assumption excludes malformed snapshots. -/
def PeerEdge (recipient peer : SessionMember) (topology : Topology)
    (transport : Transport) (host : core.option.Option uuid.Uuid) : core.option.Option Bool :=
  if recipient.player_id = peer.player_id ∨
      MemberSupport recipient.capabilities topology transport = false ∨
      MemberSupport peer.capabilities topology transport = false then .None
  else match topology with
    | .Relay => .None
    | .Mesh => .Some (decide (ElectionOrder.uuidKey recipient.player_id < ElectionOrder.uuidKey peer.player_id))
    | .Host => match host with
      | .None => .None
      | .Some host =>
        if recipient.player_id = host then .Some false
        else if peer.player_id = host then .Some true else .None

/-- Exact result and successful modeled termination for all member fields,
versions, axis pairs and optional saved identities. -/
theorem peer_edge_contract (recipient peer : SessionMember) (topology : Topology)
    (transport : Transport) (host : core.option.Option uuid.Uuid) :
    peer_initiate recipient peer topology transport host = RustM.ok (PeerEdge recipient peer topology transport host) := by
  simp only [peer_initiate, member_support_contract, glare_contract,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq, bind_tc_ok]
  by_cases same : recipient.player_id = peer.player_id
  · simp [same, PeerEdge]
  · cases r : MemberSupport recipient.capabilities topology transport <;>
      cases p : MemberSupport peer.capabilities topology transport <;>
      cases topology <;> cases host <;> simp [same, r, p, PeerEdge]
    all_goals (try split) <;> (try simp_all)
    all_goals (try split) <;> (try simp_all)

/-- Self edges are always absent, including duplicate identities. -/
theorem peer_self_contract (recipient peer : SessionMember) (same : recipient.player_id = peer.player_id)
    (topology : Topology) (transport : Transport) (host : core.option.Option uuid.Uuid) :
    peer_initiate recipient peer topology transport host = RustM.ok .None := by
  simp [peer_edge_contract, PeerEdge, same]

/-- Neither a downgraded recipient nor an unsupported peer can form an edge. -/
theorem peer_capability_contract (recipient peer : SessionMember) (topology : Topology)
    (transport : Transport) (host : core.option.Option uuid.Uuid)
    (unsupported : MemberSupport recipient.capabilities topology transport = false ∨
      MemberSupport peer.capabilities topology transport = false) :
    peer_initiate recipient peer topology transport host = RustM.ok .None := by
  rcases unsupported with h | h <;> simp [peer_edge_contract, PeerEdge, h]

/-- Relay needs no peer connection, for every transport and host snapshot. -/
theorem peer_relay_contract (recipient peer : SessionMember)
    (transport : Transport) (host : core.option.Option uuid.Uuid) :
    peer_initiate recipient peer .Relay transport host = RustM.ok .None := by
  simp [peer_edge_contract, PeerEdge]

/-- For distinct mesh identities, exactly one side offers. -/
theorem glare_antisymmetric (left right : uuid.Uuid) (different : left ≠ right) :
    ∃ offer : Bool, local_initiates left right = RustM.ok offer ∧
      local_initiates right left = RustM.ok (!offer) := by
  have distinct_keys : ElectionOrder.uuidKey left ≠ ElectionOrder.uuidKey right :=
    fun equal => different ((uuid_key_equal left right).mp equal)
  refine ⟨decide (ElectionOrder.uuidKey left < ElectionOrder.uuidKey right), glare_contract _ _, ?_⟩
  rw [glare_contract]
  congr 1
  by_cases before : ElectionOrder.uuidKey left < ElectionOrder.uuidKey right <;> simp [before] <;> omega

/-- Host edges connect only the saved host and a distinct compatible client.
The host answers; the client offers. An absent/unrelated host forms no edge. -/
theorem peer_host_contract (recipient peer : SessionMember) (transport : Transport)
    (host : core.option.Option uuid.Uuid) (different : recipient.player_id ≠ peer.player_id)
    (recipient_ok : MemberSupport recipient.capabilities .Host transport = true)
    (peer_ok : MemberSupport peer.capabilities .Host transport = true) :
    peer_initiate recipient peer .Host transport host = RustM.ok
      (match host with
       | .None => .None
       | .Some host => if recipient.player_id = host then .Some false
         else if peer.player_id = host then .Some true else .None) := by
  simp [peer_edge_contract, PeerEdge, different, recipient_ok, peer_ok]

/-- Every pair of distinct compatible mesh members has an edge. -/
theorem peer_mesh_contract (recipient peer : SessionMember) (transport : Transport)
    (host : core.option.Option uuid.Uuid) (different : recipient.player_id ≠ peer.player_id)
    (recipient_ok : MemberSupport recipient.capabilities .Mesh transport = true)
    (peer_ok : MemberSupport peer.capabilities .Mesh transport = true) :
    peer_initiate recipient peer .Mesh transport host = RustM.ok
      (.Some (decide (ElectionOrder.uuidKey recipient.player_id < ElectionOrder.uuidKey peer.player_id))) := by
  simp [peer_edge_contract, PeerEdge, different, recipient_ok, peer_ok]

/-- A compatible mesh pair has reciprocal edges and exactly one offerer. -/
theorem peer_mesh_pair_contract (left right : SessionMember) (transport : Transport)
    (host : core.option.Option uuid.Uuid) (different : left.player_id ≠ right.player_id)
    (left_ok : MemberSupport left.capabilities .Mesh transport = true)
    (right_ok : MemberSupport right.capabilities .Mesh transport = true) :
    ∃ offer : Bool, peer_initiate left right .Mesh transport host = RustM.ok (.Some offer) ∧
      peer_initiate right left .Mesh transport host = RustM.ok (.Some (!offer)) := by
  obtain ⟨offer, forward, backward⟩ := glare_antisymmetric left.player_id right.player_id different
  refine ⟨offer, ?_, ?_⟩
  · rw [peer_mesh_contract left right transport host different left_ok right_ok]
    rw [glare_contract] at forward
    exact congrArg (fun value => RustM.ok (core.option.Option.Some value)) (RustM.ok.inj forward)
  · rw [peer_mesh_contract right left transport host (Ne.symm different) right_ok left_ok]
    have result := backward
    rw [glare_contract] at result
    exact congrArg (fun value => RustM.ok (core.option.Option.Some value)) (RustM.ok.inj result)

/-- Two Host clients cannot form a peer edge, even with arbitrary capabilities. -/
theorem peer_host_no_client_edges (left right : SessionMember) (transport : Transport)
    (host : uuid.Uuid) (left_client : left.player_id ≠ host) (right_client : right.player_id ≠ host) :
    peer_initiate left right .Host transport (.Some host) = RustM.ok .None := by
  simp [peer_edge_contract, PeerEdge, left_client, right_client]

/-- Accepting, refusing and both offer directions are reachable without a
roster-membership, endpoint, metadata or legal-pair assumption. -/
theorem peer_edge_outcomes (member : SessionMember) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let low := { member with player_id := ElectionFixtures.lowId, capabilities := capable }
    let high := { member with player_id := ElectionFixtures.highId, capabilities := capable }
    peer_initiate low high .Mesh .WebRtc .None = RustM.ok (.Some true) ∧
    peer_initiate high low .Mesh .WebRtc .None = RustM.ok (.Some false) ∧
    peer_initiate low low .Mesh .WebRtc .None = RustM.ok .None ∧
    peer_initiate low high .Host .Direct (.Some low.player_id) = RustM.ok (.Some false) ∧
    peer_initiate low high .Host .Direct (.Some high.player_id) = RustM.ok (.Some true) ∧
    peer_initiate low high .Host .Direct .None = RustM.ok .None ∧
    peer_initiate low high .Relay .Relay .None = RustM.ok .None ∧
    peer_initiate { low with capabilities := { capable with version := 2#u16 } }
      high .Mesh .WebRtc .None = RustM.ok .None ∧
    peer_initiate low { high with capabilities := { capable with direct_transport := false } }
      .Host .Direct (.Some low.player_id) = RustM.ok .None := by
  have reversed := Ne.symm ElectionFixtures.distinct
  have order := ElectionFixtures.id_order
  have reverse_order : ¬ElectionOrder.uuidKey ElectionFixtures.highId <
      ElectionOrder.uuidKey ElectionFixtures.lowId := by omega
  simp [peer_edge_contract, PeerEdge, MemberSupport, ElectionFixtures.distinct,
    reversed, order, reverse_order]

end Capability
