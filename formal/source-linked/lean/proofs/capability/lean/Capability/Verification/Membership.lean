import Capability.Verification.MembershipInputs

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability session_admission

noncomputable section
namespace Capability

def InvalidHost (stored : ActiveSessionPlan) (members : List SessionMember) : Bool :=
  match stored.topology with
  | .Host => match stored.host with
    | .None => true
    | .Some host => !(members.any (HostMatch stored host))
  | _ => false

def RelayPlan : ActiveSessionPlan := ⟨.Relay, .Relay, .None⟩

/-- Protocol state after a refresh: absence means relay; a usable incumbent stays;
an invalid host is replaced on the same pair or the stored plan is removed. -/
def MembershipSpec (stored : CoreModels.core.option.Option ActiveSessionPlan)
    (members : List SessionMember) (replacement : CoreModels.core.option.Option uuid.Uuid) : MembershipPlan :=
  match stored with
  | .None => ⟨RelayPlan, .None, false⟩
  | .Some plan =>
    if InvalidHost plan members then
      match replacement with
      | .None => ⟨RelayPlan, .Some .None, false⟩
      | .Some host =>
        let repaired := { plan with host := .Some host }
        ⟨repaired, .Some (.Some repaired), true⟩
    else ⟨plan, .None, false⟩

/-- Evaluate the actual production transition with its actual validity and election calls. -/
theorem membership_contract (stored : CoreModels.core.option.Option ActiveSessionPlan)
    (authority : CoreModels.core.option.Option uuid.Uuid) (members : Slice SessionMember) :
    resolveMembership stored authority members = RustM.ok
      (MembershipSpec stored members.val
        (match stored with
         | .None => .None
         | .Some plan => RustOption ((electionScan authority
             (members.val.filter (fun member => Eligible member plan.topology plan.transport)) none).map Prod.snd))) := by
  cases stored with
  | none => try rfl
  | some stored =>
    have validity : ActiveSessionPlan.host_invalid electionIterator stored members =
        RustM.ok (InvalidHost stored members.val) := host_validity_contract stored members
    simp only [resolveMembership, resolve_membership_plan, membershipClone,
      CoreModels.core.Shared0T.Insts.CoreCloneClone,
      CoreModels.core.Shared0T.Insts.CoreCloneClone.clone, validity,
      eligible_election_contract, election_evaluation, Aeneas.Std.bind, Bind.bind]
    cases valid : InvalidHost stored members.val <;>
      simp [MembershipSpec, valid, RelayPlan]
    try split <;> simp_all

/-- No stored plan publishes relay without writing sticky state or counting a replan. -/
theorem membership_absent_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) :
    resolveMembership .None authority members = RustM.ok ⟨RelayPlan, .None, false⟩ := by
  try rfl

/-- A usable incumbent is retained even when an eligible authority differs. -/
theorem membership_preserve_contract (stored : ActiveSessionPlan)
    (authority : CoreModels.core.option.Option uuid.Uuid) (members : Slice SessionMember)
    (valid : InvalidHost stored members.val = false) :
    resolveMembership (.Some stored) authority members = RustM.ok ⟨stored, .None, false⟩ := by
  rw [membership_contract]
  simp [MembershipSpec, valid]

/-- Repair changes only the host and writes the same pair that it publishes. -/
theorem membership_repair_contract (stored : ActiveSessionPlan)
    (authority : CoreModels.core.option.Option uuid.Uuid) (members : Slice SessionMember)
    (host : uuid.Uuid) (invalid : InvalidHost stored members.val = true)
    (selected : elect_eligible_host electionIterator authority members stored.topology stored.transport =
      RustM.ok (.Some host)) :
    resolveMembership (.Some stored) authority members = RustM.ok
      ⟨{ stored with host := .Some host }, .Some (.Some { stored with host := .Some host }), true⟩ := by
  rw [eligible_election_contract, election_evaluation] at selected
  have returned : RustOption ((electionScan authority
      (members.val.filter (fun member => Eligible member stored.topology stored.transport)) none).map Prod.snd) =
      .Some host := by simpa using selected
  rw [membership_contract]
  simp [MembershipSpec, invalid, returned]

/-- An invalid host with no eligible replacement removes sticky state and publishes relay. -/
theorem membership_remove_contract (stored : ActiveSessionPlan)
    (authority : CoreModels.core.option.Option uuid.Uuid) (members : Slice SessionMember)
    (invalid : InvalidHost stored members.val = true)
    (empty : ∀ member ∈ members.val, Eligible member stored.topology stored.transport = false) :
    resolveMembership (.Some stored) authority members = RustM.ok ⟨RelayPlan, .Some .None, false⟩ := by
  have selected := (eligible_empty_contract authority members stored.topology stored.transport).mpr empty
  rw [eligible_election_contract, election_evaluation] at selected
  have returned : RustOption ((electionScan authority
      (members.val.filter (fun member => Eligible member stored.topology stored.transport)) none).map Prod.snd) =
      .None := by simpa using selected
  rw [membership_contract]
  simp [MembershipSpec, invalid, returned]

/-- A replacement is a present, capable member with the required Direct endpoint. -/
theorem membership_replacement_eligible (stored : ActiveSessionPlan)
    (authority : CoreModels.core.option.Option uuid.Uuid) (members : Slice SessionMember)
    (host : uuid.Uuid) (invalid : InvalidHost stored members.val = true)
    (selected : elect_eligible_host electionIterator authority members stored.topology stored.transport =
      RustM.ok (.Some host)) :
    resolveMembership (.Some stored) authority members = RustM.ok
      ⟨{ stored with host := .Some host }, .Some (.Some { stored with host := .Some host }), true⟩ ∧
    ∃ member ∈ members.val, member.player_id = host ∧ Eligible member stored.topology stored.transport = true := by
  exact ⟨membership_repair_contract stored authority members host invalid selected,
    eligible_membership_contract authority members stored.topology stored.transport host selected⟩

/-- Missing saved hosts, valid incumbents, repair and relay reset are reachable. -/
theorem membership_outcomes (member : SessionMember) :
    let ready := { member with capabilities := (⟨3#u16, true, true, true, true, true, true⟩ : SessionCapabilities), direct_endpoint := .None }
    let alone : Slice SessionMember := ⟨[ready], by scalar_tac⟩
    resolveMembership (.Some ⟨.Host, .WebRtc, .Some member.player_id⟩) .None alone =
      RustM.ok ⟨⟨.Host, .WebRtc, .Some member.player_id⟩, .None, false⟩ ∧
    resolveMembership (.Some ⟨.Host, .WebRtc, .None⟩) .None alone =
      RustM.ok ⟨⟨.Host, .WebRtc, .Some member.player_id⟩,
        .Some (.Some ⟨.Host, .WebRtc, .Some member.player_id⟩), true⟩ ∧
    resolveMembership (.Some ⟨.Host, .Direct, .Some member.player_id⟩) .None alone =
      RustM.ok ⟨RelayPlan, .Some .None, false⟩ := by
  simp [membership_contract, MembershipSpec, InvalidHost, HostMatch, Eligible,
    MemberSupport, HasEndpoint, electionScan, earlierCandidate, memberCandidate, RustOption]

/-- Every finite modeled input terminates, including empty or malformed snapshots. -/
theorem membership_total_contract (stored : CoreModels.core.option.Option ActiveSessionPlan)
    (authority : CoreModels.core.option.Option uuid.Uuid) (members : Slice SessionMember) :
    ∃ result, resolveMembership stored authority members = RustM.ok result := by
  exact ⟨_, membership_contract stored authority members⟩

end Capability
