import Capability.Verification.Election
import Capability.Verification.Members

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member session_capability

set_option maxHeartbeats 100000

noncomputable section
namespace Capability

/-- Independent eligibility specification: both negotiated axes and Direct readiness. -/
def Eligible (member : SessionMember) (topology : Topology) (transport : Transport) : Bool :=
  MemberSupport member.capabilities topology transport &&
    (match transport with | .Direct => HasEndpoint member.direct_endpoint | _ => true)

abbrev EligibleState := CoreModels.core.iter.adapters.filter.Filter (Slice SessionMember)
  (elect_eligible_host.closure (Slice SessionMember) (Slice SessionMember))

def eligibleCallback :=
  elect_eligible_host.closure.Insts.CoreOpsFunctionFnMutTupleSharedSharedSessionMemberBool electionIterator

def eligibleIterator := CoreModels.core.iter.traits.collect.IntoIterator.Blanket
  (CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator
    electionIterator.iteratorIteratorInst eligibleCallback)

/-- The extracted closure reads the actual candidate and both selected axes. -/
theorem eligible_callback_contract (member : SessionMember) (topology : Topology) (transport : Transport) :
    eligibleCallback.call_mut (topology, transport) member =
      RustM.ok (Eligible member topology transport, (topology, transport)) := by
  unfold eligibleCallback
  unfold elect_eligible_host.closure.Insts.CoreOpsFunctionFnMutTupleSharedSharedSessionMemberBool
  unfold elect_eligible_host.closure.Insts.CoreOpsFunctionFnMutTupleSharedSharedSessionMemberBool.call_mut
  try change (do let b ← SessionMember.can_host member topology transport
                 RustM.ok (b, (topology, transport))) = _
  try rw [member_host_contract]
  try rfl

private def filteredMembers (state : EligibleState) : List SessionMember :=
  state.iter.val.filter (fun member => Eligible member state.predicate.1 state.predicate.2)

private theorem filter_next (members : Slice SessionMember) (topology : Topology) (transport : Transport) :
    ∃ remaining : EligibleState,
      eligibleIterator.iteratorIteratorInst.next ⟨members, (topology, transport)⟩ =
        RustM.ok (RustOption ((members.val.filter (fun member => Eligible member topology transport)).head?), remaining) ∧
      filteredMembers remaining = (members.val.filter (fun member => Eligible member topology transport)).tail ∧
      remaining.predicate = (topology, transport) := by
  suffices ∃ rest : Slice SessionMember,
      CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop
        electionIterator.iteratorIteratorInst eligibleCallback ⟨members, (topology, transport)⟩ =
        RustM.ok (RustOption ((members.val.filter (fun member => Eligible member topology transport)).head?), rest, (topology, transport)) ∧
      rest.val.filter (fun member => Eligible member topology transport) =
        (members.val.filter (fun member => Eligible member topology transport)).tail by
    obtain ⟨rest, result, shape⟩ := this
    refine ⟨⟨rest, (topology, transport)⟩, ?_, shape, rfl⟩
    simp only [eligibleIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
      CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator,
      CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next,
      result]
    rfl
  obtain ⟨xs, bound⟩ := members
  induction xs with
  | nil =>
    refine ⟨⟨[], bound⟩, ?_, rfl⟩
    unfold CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop
    rw [loop]
    simp [CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop.body,
      electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
      next_empty, RustOption, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
    rfl
  | cons head tail ih =>
    have tail_bound : tail.length ≤ Usize.max := by simp only [List.length_cons] at bound; omega
    cases eligible : Eligible head topology transport with
    | true =>
      refine ⟨⟨tail, tail_bound⟩, ?_, ?_⟩
      · unfold CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop
        rw [loop]
        simp [CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop.body,
          electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
          next_cons, eligible_callback_contract, eligible, RustOption,
          Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
        rfl
      · simp [eligible]
    | false =>
      obtain ⟨rest, result, shape⟩ := ih tail_bound
      refine ⟨rest, ?_, ?_⟩
      · unfold CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop
        rw [loop]
        simp [CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop.body,
          electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
          next_cons, eligible_callback_contract, eligible, RustOption,
          Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
        simpa only [CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop,
          electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
          CoreModels.core.iter.adapters.filter.Filter.Insts.CoreIterTraitsIteratorIterator.next_loop.body,
          RustOption, List.head?_filter, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry] using result
      · simpa [eligible] using shape

private theorem filtered_loop_evaluation (authority : CoreModels.core.option.Option uuid.Uuid)
    (xs : List SessionMember) (state : EligibleState)
    (shape : filteredMembers state = xs) (current : Option ElectionCandidate) :
    elect_host_loop eligibleIterator state authority (RustOption current) =
      RustM.ok (RustOption ((electionScan authority xs current).map Prod.snd)) := by
  induction xs generalizing state current with
  | nil =>
    obtain ⟨remaining, next, rest, predicate⟩ := filter_next state.iter state.predicate.1 state.predicate.2
    change state.iter.val.filter (fun member => Eligible member state.predicate.1 state.predicate.2) = _ at shape
    rw [shape] at next rest
    have eta : (⟨state.iter, (state.predicate.1, state.predicate.2)⟩ : EligibleState) = state := by
      have pred_eta : (state.predicate.1, state.predicate.2) = state.predicate := Prod.eta state.predicate
      rw [pred_eta]
    rw [eta] at next
    have next' : eligibleIterator.iteratorIteratorInst.next state = RustM.ok (.None, remaining) := by
      simpa only [List.head?_nil, List.head?_cons, RustOption] using next
    unfold elect_host_loop
    rw [loop]
    cases current <;>
      simp [elect_host_loop.body, next', electionScan, RustOption,
        CoreModels.core.option.Option.map,
        elect_host.closure_1.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidUuid.call_once,
        Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]
  | cons head tail ih =>
    obtain ⟨remaining, next, rest, predicate⟩ := filter_next state.iter state.predicate.1 state.predicate.2
    change state.iter.val.filter (fun member => Eligible member state.predicate.1 state.predicate.2) = _ at shape
    rw [shape] at next rest
    have eta : (⟨state.iter, (state.predicate.1, state.predicate.2)⟩ : EligibleState) = state := by
      have pred_eta : (state.predicate.1, state.predicate.2) = state.predicate := Prod.eta state.predicate
      rw [pred_eta]
    rw [eta] at next
    have next' : eligibleIterator.iteratorIteratorInst.next state = RustM.ok (.Some head, remaining) := by
      simpa only [List.head?_nil, List.head?_cons, RustOption] using next
    have rest' : filteredMembers remaining = tail := by
      simpa only [List.tail_cons] using rest
    have recursive := ih remaining rest' (earlierCandidate current head)
    unfold elect_host_loop
    rw [loop]
    simp only [elect_host_loop.body, next', Aeneas.Std.bind, Bind.bind,
      Aeneas.Std.uncurry, option_identity, election_optional_callback]
    rw [earlier_candidate_replacement] at recursive
    by_cases present : CoreModels.core.option.Option.Some head.player_id = authority
    · simp [present, electionScan, RustOption, memberCandidate]
      cases authority <;> rfl
    · simp only [present, decide_false, Bool.false_eq_true, ↓reduceIte]
      cases replacement : replacementWanted head current <;>
        simpa [elect_host_loop, elect_host_loop.body, electionScan, present, replacement,
          memberCandidate, option_identity, election_optional_callback,
          Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry] using recursive

/-- The production filter and election agree with election over exactly the eligible members. -/
theorem eligible_election_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (topology : Topology) (transport : Transport) :
    elect_eligible_host electionIterator authority members topology transport =
      elect_host electionIterator authority
        (⟨members.val.filter (fun member => Eligible member topology transport),
          le_trans (List.length_filter_le ..) members.property⟩ : Slice SessionMember) := by
  rw [election_evaluation]
  have evaluates := filtered_loop_evaluation authority
    (members.val.filter (fun member => Eligible member topology transport))
    (⟨members, (topology, transport)⟩ : EligibleState) rfl none
  unfold elect_eligible_host
  simp only [electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter,
    CoreModels.core.iter.traits.iterator.Iterator.filter.default,
    CoreModels.core.iter.adapters.filter.Filter.new, Aeneas.Std.bind, Bind.bind]
  try simpa only [elect_host, CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter,
    Aeneas.Std.bind, Bind.bind, eligibleIterator, eligibleCallback,
    electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket, RustOption] using evaluates

/-- No eligible member exists exactly when election returns no identity. -/
theorem eligible_empty_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (topology : Topology) (transport : Transport) :
    elect_eligible_host electionIterator authority members topology transport = RustM.ok .None ↔
      ∀ member ∈ members.val, Eligible member topology transport = false := by
  rw [eligible_election_contract, election_empty_contract]
  simp [List.filter_eq_nil_iff]

/-- An eligible designated authority wins; proxy flags do not change the result. -/
theorem eligible_authority_contract (host : uuid.Uuid)
    (members : Slice SessionMember) (topology : Topology) (transport : Transport)
    (present : ∃ member ∈ members.val, member.player_id = host ∧ Eligible member topology transport = true) :
    elect_eligible_host electionIterator (.Some host) members topology transport = RustM.ok (.Some host) := by
  rw [eligible_election_contract]
  apply election_authority_contract
  obtain ⟨member, membership, identity, eligible⟩ := present
  exact ⟨member, List.mem_filter.mpr ⟨membership, eligible⟩, identity⟩

/-- Every elected identity belongs to an eligible input member. -/
theorem eligible_membership_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (topology : Topology) (transport : Transport) (host : uuid.Uuid)
    (returns : elect_eligible_host electionIterator authority members topology transport = RustM.ok (.Some host)) :
    ∃ member ∈ members.val, member.player_id = host ∧ Eligible member topology transport = true := by
  rw [eligible_election_contract] at returns
  obtain ⟨member, membership, identity⟩ := election_membership_contract _ _ host returns
  obtain ⟨membership, eligible⟩ := List.mem_filter.mp membership
  exact ⟨member, membership, identity, eligible⟩

/-- Without an eligible authority, election minimizes timestamp then UUID among eligible members. -/
theorem eligible_minimum_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (topology : Topology) (transport : Transport) (host : uuid.Uuid)
    (absent : ∀ member ∈ members.val, Eligible member topology transport = true →
      CoreModels.core.option.Option.Some member.player_id ≠ authority)
    (returns : elect_eligible_host electionIterator authority members topology transport = RustM.ok (.Some host)) :
    ∃ selected ∈ members.val, selected.player_id = host ∧ Eligible selected topology transport = true ∧
      ∀ member ∈ members.val, Eligible member topology transport = true →
        electionKey (memberCandidate selected) ≤ electionKey (memberCandidate member) := by
  rw [eligible_election_contract] at returns
  have excluded : ∀ member ∈ members.val.filter (fun member => Eligible member topology transport),
      CoreModels.core.option.Option.Some member.player_id ≠ authority := by
    intro member membership
    obtain ⟨original, eligible⟩ := List.mem_filter.mp membership
    exact absent member original eligible
  obtain ⟨selected, membership, identity, minimum⟩ := election_minimum_contract authority _ host excluded returns
  obtain ⟨membership, eligible⟩ := List.mem_filter.mp membership
  exact ⟨selected, membership, identity, eligible,
    fun member membership eligible => minimum member (List.mem_filter.mpr ⟨membership, eligible⟩)⟩

/-- The production helper terminates for every finite pinned slice enumeration. -/
theorem eligible_total_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (topology : Topology) (transport : Transport) :
    ∃ result, elect_eligible_host electionIterator authority members topology transport = RustM.ok result := by
  rw [eligible_election_contract]
  exact election_total_contract _ _

/-- Eligible and rejected singleton snapshots are reachable for arbitrary metadata and endpoints. -/
theorem eligible_outcomes (member : SessionMember) (endpoint : DirectEndpoint) :
    let capable : SessionCapabilities := ⟨65535#u16, true, true, true, true, true, true⟩
    elect_eligible_host electionIterator .None
      (⟨[{ member with capabilities := capable, direct_endpoint := .None }], by scalar_tac⟩ : Slice SessionMember)
      .Host .WebRtc = RustM.ok (.Some member.player_id) ∧
    elect_eligible_host electionIterator .None
      (⟨[{ member with capabilities := capable, direct_endpoint := .None }], by scalar_tac⟩ : Slice SessionMember)
      .Host .Direct = RustM.ok .None ∧
    elect_eligible_host electionIterator .None
      (⟨[{ member with capabilities := capable, direct_endpoint := .Some endpoint }], by scalar_tac⟩ : Slice SessionMember)
      .Host .Direct = RustM.ok (.Some member.player_id) ∧
    elect_eligible_host electionIterator (.Some member.player_id)
      (⟨[{ member with capabilities := { capable with version := 2#u16 }, direct_endpoint := .Some endpoint }],
        by scalar_tac⟩ : Slice SessionMember)
      .Host .Direct = RustM.ok .None := by
  simp [eligible_election_contract, Eligible, MemberSupport, HasEndpoint,
    election_evaluation, electionScan, earlierCandidate, memberCandidate, RustOption,
    CoreModels.core.option.Option.Some, CoreModels.core.option.Option.None]

end Capability
