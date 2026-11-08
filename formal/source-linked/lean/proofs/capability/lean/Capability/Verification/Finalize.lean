import Capability.Verification.FinalizeInputs

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability session_admission

noncomputable section
namespace Capability

local instance : DecidableEq Topology := Classical.decEq _

/-- The ADR waterfall chooses the pair; Host elects only eligible candidates.
Mesh and Relay never carry a host. -/
def FinalizeSpec (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (desired : Topology) (direct webrtc : Bool) : ActiveSessionPlan :=
  let pair := ((ProtocolLadder.find? (RungFits members desired direct webrtc)).getD (.Relay, .Relay))
  ⟨pair.1, pair.2, if pair.1 = .Host then
    RustOption ((electionScan authority
      (members.filter (fun member => Eligible member pair.1 pair.2)) none).map Prod.snd)
    else .None⟩

/-- Exact production composition over every finite pinned slice and configuration. -/
theorem finalize_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool) :
    finalizePlan authority members desired direct webrtc =
      RustM.ok (FinalizeSpec authority members.val desired direct webrtc) := by
  unfold finalizePlan finalize_session_plan
  simp only [CoreModels.core.Slice.Insts.CoreConvertAsRefSlice.as_ref, selection_contract,
    Aeneas.Std.bind, Bind.bind]
  generalize pair_eq : (ProtocolLadder.find? (RungFits members.val desired direct webrtc)).getD (.Relay, .Relay) = pair
  rcases pair with ⟨topology, transport⟩
  cases topology <;>
    simp [Topology.Insts.CoreCmpPartialEqTopology.eq, Topology.read_discriminant, eligible_election_contract,
      election_evaluation, FinalizeSpec, pair_eq]

/-- Every result has one of the four protocol pairs. -/
theorem finalize_legal_pair (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool) :
    let plan := FinalizeSpec authority members.val desired direct webrtc
    (plan.topology, plan.transport) = (.Mesh, .WebRtc) ∨
    (plan.topology, plan.transport) = (.Host, .WebRtc) ∨
    (plan.topology, plan.transport) = (.Host, .Direct) ∨
    (plan.topology, plan.transport) = (.Relay, .Relay) := by
  simp only [FinalizeSpec, ProtocolLadder, List.find?_cons, List.find?_nil]
  repeat' (first | split | simp_all)

/-- Every upgrade is nonempty and every member supports both selected axes. -/
theorem finalize_member_support (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool)
    (upgrade : (FinalizeSpec authority members.val desired direct webrtc).topology ≠ .Relay) :
    members.val ≠ [] ∧ ∀ member ∈ members.val,
      MemberSupport member.capabilities
        (FinalizeSpec authority members.val desired direct webrtc).topology
        (FinalizeSpec authority members.val desired direct webrtc).transport = true := by
  have selected := selection_contract members desired direct webrtc
  let pair := (ProtocolLadder.find? (RungFits members.val desired direct webrtc)).getD (.Relay, .Relay)
  have not_relay : pair ≠ (.Relay, .Relay) := by
    intro same
    exact upgrade (by simp only [FinalizeSpec]; change pair.1 = .Relay; rw [same])
  have fit := (selection_upgrade_contract members desired direct webrtc pair not_relay selected).2
  simp only [RungFits, Bool.and_eq_true, List.all_eq_true] at fit
  exact ⟨by simpa using fit.1.2.1, fit.1.2.2⟩

/-- Non-Host plans never publish a host identity. -/
theorem finalize_no_host (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool)
    (nonhost : (FinalizeSpec authority members.val desired direct webrtc).topology ≠ .Host) :
    (FinalizeSpec authority members.val desired direct webrtc).host = .None := by
  simp only [FinalizeSpec] at nonhost ⊢
  simp [nonhost]

/-- Every published host belongs to an eligible member of the supplied snapshot. -/
theorem finalize_host_eligible (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool) (host : uuid.Uuid)
    (selected : (FinalizeSpec authority members.val desired direct webrtc).host = .Some host) :
    ∃ member ∈ members.val, member.player_id = host ∧
      Eligible member (FinalizeSpec authority members.val desired direct webrtc).topology
        (FinalizeSpec authority members.val desired direct webrtc).transport = true := by
  let pair := (ProtocolLadder.find? (RungFits members.val desired direct webrtc)).getD (.Relay, .Relay)
  have elected : elect_eligible_host electionIterator authority members pair.1 pair.2 = RustM.ok (.Some host) := by
    rw [eligible_election_contract, election_evaluation]
    simp only [FinalizeSpec] at selected
    split at selected
    · simpa only [pair] using congrArg RustM.ok selected
    · cases selected
  exact eligible_membership_contract authority members pair.1 pair.2 host elected

/-- The Host field is the result of election on the exact pair and snapshot. -/
theorem finalize_host_election (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool)
    (hosted : (FinalizeSpec authority members.val desired direct webrtc).topology = .Host) :
    elect_eligible_host electionIterator authority members
      (FinalizeSpec authority members.val desired direct webrtc).topology
      (FinalizeSpec authority members.val desired direct webrtc).transport =
      RustM.ok (FinalizeSpec authority members.val desired direct webrtc).host := by
  rw [eligible_election_contract, election_evaluation]
  simp only [FinalizeSpec] at hosted ⊢
  simp [hosted]

/-- An eligible designated authority wins in the complete finalized plan. -/
theorem finalize_authority_contract (host : uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool)
    (hosted : (FinalizeSpec (.Some host) members.val desired direct webrtc).topology = .Host)
    (present : ∃ member ∈ members.val, member.player_id = host ∧
      Eligible member .Host (FinalizeSpec (.Some host) members.val desired direct webrtc).transport = true) :
    (FinalizeSpec (.Some host) members.val desired direct webrtc).host = .Some host := by
  have result := eligible_authority_contract host members
    (FinalizeSpec (.Some host) members.val desired direct webrtc).topology
    (FinalizeSpec (.Some host) members.val desired direct webrtc).transport
    (by simpa only [hosted] using present)
  rw [finalize_host_election (.Some host) members desired direct webrtc hosted] at result
  exact RustM.ok.inj result

/-- Without eligible authority, the complete plan chooses the least timestamp/UUID key. -/
theorem finalize_minimum_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool) (host : uuid.Uuid)
    (hosted : (FinalizeSpec authority members.val desired direct webrtc).topology = .Host)
    (absent : ∀ member ∈ members.val,
      Eligible member .Host (FinalizeSpec authority members.val desired direct webrtc).transport = true →
      CoreModels.core.option.Option.Some member.player_id ≠ authority)
    (selected : (FinalizeSpec authority members.val desired direct webrtc).host = .Some host) :
    ∃ member ∈ members.val, member.player_id = host ∧
      Eligible member .Host (FinalizeSpec authority members.val desired direct webrtc).transport = true ∧
      ∀ candidate ∈ members.val,
        Eligible candidate .Host (FinalizeSpec authority members.val desired direct webrtc).transport = true →
        electionKey (memberCandidate member) ≤ electionKey (memberCandidate candidate) := by
  apply eligible_minimum_contract authority members .Host
    (FinalizeSpec authority members.val desired direct webrtc).transport host absent
  have result := finalize_host_election authority members desired direct webrtc hosted
  simpa only [hosted, selected] using result

/-- Every selected Host pair has an eligible input member; no existence premise is assumed. -/
theorem finalize_host_exists (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool)
    (hosted : (FinalizeSpec authority members.val desired direct webrtc).topology = .Host) :
    ∃ host, (FinalizeSpec authority members.val desired direct webrtc).host = .Some host := by
  let plan := FinalizeSpec authority members.val desired direct webrtc
  let pair := (plan.topology, plan.transport)
  have selected : select_session_pair members desired direct webrtc = RustM.ok pair := by
    simpa only [pair, plan, FinalizeSpec] using selection_contract members desired direct webrtc
  have upgrade : pair ≠ (.Relay, .Relay) := by
    intro same
    have same_topology := congrArg Prod.fst same
    simp only [pair, plan, hosted] at same_topology
    cases same_topology
  obtain ⟨legal, fit⟩ := selection_upgrade_contract members desired direct webrtc pair upgrade selected
  have eligible : ∃ member ∈ members.val, Eligible member plan.topology plan.transport = true := by
    simp only [ProtocolLadder, List.mem_cons, List.mem_nil_iff, or_false, pair] at legal
    rcases legal with mesh | webrtc_pair | direct_pair
    · have same := congrArg Prod.fst mesh
      simp only [plan, hosted] at same
      cases same
    · have support := finalize_member_support authority members desired direct webrtc (by simp [hosted])
      obtain ⟨member, membership⟩ := List.exists_mem_of_ne_nil members.val support.1
      refine ⟨member, membership, ?_⟩
      have supported := support.2 member membership
      have transport : plan.transport = .WebRtc := congrArg Prod.snd webrtc_pair
      simp only [Eligible, transport, Bool.and_true]
      change MemberSupport member.capabilities plan.topology plan.transport = true at supported
      simpa only [transport] using supported
    · have transport : plan.transport = .Direct := congrArg Prod.snd direct_pair
      simp only [RungFits, pair, transport, Bool.and_eq_true] at fit
      obtain ⟨member, membership, ready⟩ := List.any_eq_true.mp fit.2
      exact ⟨member, membership, by simpa only [Eligible, transport, DirectReady] using ready⟩
  have result := finalize_host_election authority members desired direct webrtc hosted
  cases output : plan.host with
  | some host => exact ⟨host, rfl⟩
  | none =>
    have empty : elect_eligible_host electionIterator authority members plan.topology plan.transport = RustM.ok .None := by
      change elect_eligible_host electionIterator authority members plan.topology plan.transport = RustM.ok plan.host at result
      rw [output] at result
      exact result
    have no_eligible := (eligible_empty_contract authority members plan.topology plan.transport).mp empty
    obtain ⟨member, membership, qualifies⟩ := eligible
    have impossible := no_eligible member membership
    rw [qualifies] at impossible
    cases impossible

/-- The composition terminates in the extracted model for every finite slice. -/
theorem finalize_total_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (desired : Topology) (direct webrtc : Bool) :
    ∃ plan, finalizePlan authority members desired direct webrtc = RustM.ok plan :=
  ⟨_, finalize_contract authority members desired direct webrtc⟩

/-- Mesh, both Host transports, empty/v2 refusal and relay ceilings are reachable. -/
theorem finalize_outcomes (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    let ready := { member with capabilities := capable, direct_endpoint := .Some endpoint }
    finalizePlan .None (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Mesh true true =
      RustM.ok ⟨.Mesh, .WebRtc, .None⟩ ∧
    finalizePlan .None (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host true true =
      RustM.ok ⟨.Host, .WebRtc, .Some member.player_id⟩ ∧
    finalizePlan .None (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Host true false =
      RustM.ok ⟨.Host, .Direct, .Some member.player_id⟩ ∧
    finalizePlan .None (⟨[], by scalar_tac⟩ : Slice SessionMember) .Mesh true true =
      RustM.ok ⟨.Relay, .Relay, .None⟩ ∧
    finalizePlan .None (⟨[{ ready with capabilities := { capable with version := 2#u16 } }], by scalar_tac⟩ : Slice SessionMember)
      .Mesh true true = RustM.ok ⟨.Relay, .Relay, .None⟩ ∧
    finalizePlan .None (⟨[ready], by scalar_tac⟩ : Slice SessionMember) .Relay true true =
      RustM.ok ⟨.Relay, .Relay, .None⟩ := by
  simp [finalize_contract, FinalizeSpec, ProtocolLadder, RungFits, Rank, MemberSupport,
    DirectReady, HasEndpoint, Eligible, electionScan, earlierCandidate, memberCandidate, RustOption]

end Capability
