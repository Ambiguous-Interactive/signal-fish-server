import Capability.Verification.Search
import Capability.Verification.ElectionOrder
import Mathlib.Data.Prod.Lex

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member

set_option maxHeartbeats 50000
set_option maxRecDepth 256

noncomputable section
namespace Capability

abbrev ElectionCandidate := chrono.datetime.DateTime chrono.offset.utc.Utc chrono.offset.utc.Utc × uuid.Uuid

def electionKey (candidate : ElectionCandidate) : Lex (Nat × Nat) :=
  toLex (ElectionOrder.timeKey candidate.1, ElectionOrder.uuidKey candidate.2)

def memberCandidate (member : SessionMember) : ElectionCandidate :=
  (member.joined_at, member.player_id)

def earlierCandidate (current : Option ElectionCandidate) (member : SessionMember) : Option ElectionCandidate :=
  match current with
  | none => some (memberCandidate member)
  | some candidate => if electionKey (memberCandidate member) < electionKey candidate
      then some (memberCandidate member) else current

def electionScan (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (current : Option ElectionCandidate) : Option ElectionCandidate :=
  match members with
  | [] => current
  | member :: tail =>
      if CoreModels.core.option.Option.Some member.player_id = authority then some (memberCandidate member)
      else electionScan authority tail (earlierCandidate current member)

def electionIterator : CoreModels.core.iter.traits.collect.IntoIterator
    (Slice SessionMember) SessionMember (Slice SessionMember) :=
  CoreModels.core.iter.traits.collect.IntoIterator.Blanket
  (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT SessionMember)

private theorem iterator_next (members : Slice SessionMember) :
    electionIterator.iteratorIteratorInst.next members =
      CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.next members := rfl

attribute [local irreducible] electionIterator

attribute [local irreducible] ElectionOrder.timeKey ElectionOrder.uuidKey

private theorem candidate_comparison (left right : ElectionCandidate) :
    CoreModels.core.Pair.Insts.CoreCmpPartialOrdPair.lt
      (chrono.datetime.DateTime.Insts.CoreCmpPartialOrdDateTime
        chrono.offset.utc.Utc.Insts.ChronoOffsetTimeZoneUtc
        chrono.offset.utc.Utc.Insts.ChronoOffsetTimeZoneUtc)
      uuid.Uuid.Insts.CoreCmpPartialOrdUuid left right =
        RustM.ok (decide (electionKey left < electionKey right)) := by
  have compare_keys {A B : Type} (first : A → Nat) (second : B → Nat) (x y : A × B) :
      CoreModels.core.Pair.Insts.CoreCmpPartialOrdPair.lt
        (ElectionOrder.orderBy first first) (ElectionOrder.orderBy second second) x y =
          RustM.ok (decide (first x.1 < first y.1 ∨ first x.1 = first y.1 ∧ second x.2 < second y.2)) := by
    simp only [CoreModels.core.Pair.Insts.CoreCmpPartialOrdPair.lt, ElectionOrder.orderBy,
      Aeneas.Std.bind, Bind.bind, CoreModels.core.option.Option.Some]
    by_cases earlier : first x.1 < first y.1
    · rw [if_pos earlier]
      simp only [earlier, true_or, decide_true]
    · rw [if_neg earlier]
      by_cases same : first x.1 = first y.1
      · rw [if_pos same]
        simp only [same, Nat.lt_irrefl, false_or, true_and]
      · rw [if_neg same]
        simp only [earlier, same, false_and, false_or, decide_false]
  change CoreModels.core.Pair.Insts.CoreCmpPartialOrdPair.lt
    (ElectionOrder.orderBy ElectionOrder.timeKey ElectionOrder.timeKey)
    (ElectionOrder.orderBy ElectionOrder.uuidKey ElectionOrder.uuidKey) left right = _
  rw [compare_keys]
  apply congrArg RustM.ok
  apply decide_eq_decide.mpr
  exact (Prod.Lex.toLex_lt_toLex
    (x := (ElectionOrder.timeKey left.1, ElectionOrder.uuidKey left.2))
    (y := (ElectionOrder.timeKey right.1, ElectionOrder.uuidKey right.2))).symm

theorem option_identity (id : uuid.Uuid) (authority : CoreModels.core.option.Option uuid.Uuid) :
    CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq
      uuid.Uuid.Insts.CoreCmpPartialEqUuid (.Some id) authority =
      RustM.ok (decide (CoreModels.core.option.Option.Some id = authority)) := by
  cases authority <;> simp [CoreModels.core.option.Option.Insts.CoreCmpPartialEqOption.eq,
    uuid.Uuid.Insts.CoreCmpPartialEqUuid, uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq]

private theorem election_next_empty (authority : CoreModels.core.option.Option uuid.Uuid)
    (bound : ([] : List SessionMember).length ≤ Usize.max) (current : Option ElectionCandidate) :
    elect_host_loop.body electionIterator authority (⟨[], bound⟩ : Slice SessionMember)
      (RustOption current) = RustM.ok (.done (RustOption (current.map Prod.snd))) := by
  cases current <;>
    simp [elect_host_loop.body, iterator_next, next_empty,
      CoreModels.core.option.Option.map,
      elect_host.closure_1.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidUuid.call_once,
      RustOption, Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry]

private theorem election_candidate_callback
    {T I : Type} (iterator : CoreModels.core.iter.traits.collect.IntoIterator
      T SessionMember I) (head : SessionMember) (candidate : ElectionCandidate) :
      (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iterator).call_once
        head candidate = RustM.ok (decide (electionKey (memberCandidate head) < electionKey candidate)) := by
  unfold elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool
  unfold elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool.call_once
  simp only [memberCandidate]
  -- A compiled source fault leaves this semantic contract unsolved.
  try exact candidate_comparison (head.joined_at, head.player_id) candidate

def replacementWanted (head : SessionMember) (current : Option ElectionCandidate) : Bool :=
  match current with
  | none => true
  | some candidate => decide (electionKey (memberCandidate head) < electionKey candidate)

private theorem optional_some {F T : Type} (callback : CoreModels.core.ops.function.FnOnce F T Bool)
    (value : T) (state : F) :
    CoreModels.core.option.Option.is_none_or callback (.Some value) state = callback.call_once state value := rfl

theorem election_optional_callback
    {T I : Type} (iterator : CoreModels.core.iter.traits.collect.IntoIterator
      T SessionMember I)
    (head : SessionMember) (current : Option ElectionCandidate) :
    CoreModels.core.option.Option.is_none_or
      (elect_host.closure.Insts.CoreOpsFunctionFnOnceTuplePairDateTimeUtcUtcUuidBool iterator)
      (RustOption current) head = RustM.ok (replacementWanted head current) := by
  cases current with
  | none => rfl
  | some candidate =>
    simp only [RustOption, replacementWanted]
    rw [optional_some, election_candidate_callback]

theorem earlier_candidate_replacement (head : SessionMember) (current : Option ElectionCandidate) :
    RustOption (earlierCandidate current head) =
      if replacementWanted head current then .Some (memberCandidate head) else RustOption current := by
  cases current with
  | none => rfl
  | some candidate =>
    simp only [replacementWanted, earlierCandidate, decide_eq_true_eq]
    split_ifs <;> rfl

private theorem election_next_cons_generic
    (iterator : CoreModels.core.iter.traits.collect.IntoIterator
      (Slice SessionMember) SessionMember (Slice SessionMember)) (authority : CoreModels.core.option.Option uuid.Uuid)
    (head : SessionMember) (tail : List SessionMember) (bound : (head :: tail).length ≤ Usize.max)
    (current : Option ElectionCandidate)
    (next : iterator.iteratorIteratorInst.next (⟨head :: tail, bound⟩ : Slice SessionMember) =
      RustM.ok (.Some head, (⟨tail, by simp only [List.length_cons] at bound; omega⟩ : Slice SessionMember))) :
    elect_host_loop.body iterator authority (⟨head :: tail, bound⟩ : Slice SessionMember)
      (RustOption current) = RustM.ok
        (if CoreModels.core.option.Option.Some head.player_id = authority then .done (.Some head.player_id)
         else .cont ((⟨tail, by simp only [List.length_cons] at bound; omega⟩ : Slice SessionMember),
           RustOption (earlierCandidate current head))) := by
  unfold elect_host_loop.body
  rw [next]
  simp only [Aeneas.Std.bind, Bind.bind, Aeneas.Std.uncurry, option_identity,
    election_optional_callback]
  rw [earlier_candidate_replacement]
  by_cases present : CoreModels.core.option.Option.Some head.player_id = authority
  · simp only [present, decide_true, ↓reduceIte]
  · simp only [present, decide_false, Bool.false_eq_true, ↓reduceIte]
    try (cases replacementWanted head current <;> rfl)

private theorem election_next_cons (authority : CoreModels.core.option.Option uuid.Uuid)
    (head : SessionMember) (tail : List SessionMember) (bound : (head :: tail).length ≤ Usize.max)
    (current : Option ElectionCandidate) :
    elect_host_loop.body electionIterator authority (⟨head :: tail, bound⟩ : Slice SessionMember)
      (RustOption current) = RustM.ok
        (if CoreModels.core.option.Option.Some head.player_id = authority then .done (.Some head.player_id)
         else .cont ((⟨tail, by simp only [List.length_cons] at bound; omega⟩ : Slice SessionMember),
           RustOption (earlierCandidate current head))) := by
  apply election_next_cons_generic
  rw [iterator_next]
  exact next_cons head tail bound

theorem election_loop_evaluation (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (bound : members.length ≤ Usize.max)
    (current : Option ElectionCandidate) :
    elect_host_loop electionIterator (⟨members, bound⟩ : Slice SessionMember) authority
      (RustOption current) = RustM.ok (RustOption ((electionScan authority members current).map Prod.snd)) := by
  induction members generalizing current with
  | nil =>
    unfold elect_host_loop
    rw [loop]
    simp only [election_next_empty, electionScan]
  | cons head tail ih =>
    have tail_bound : tail.length ≤ Usize.max := by simp only [List.length_cons] at bound; omega
    unfold elect_host_loop
    rw [loop]
    simp only [election_next_cons]
    by_cases present : CoreModels.core.option.Option.Some head.player_id = authority
    · simp [present, electionScan, RustOption, memberCandidate]
      cases authority <;> rfl
    · simp only [present, ↓reduceIte]
      simpa only [elect_host_loop, electionScan, present, ↓reduceIte]
        using ih tail_bound (earlierCandidate current head)

private theorem earlier_candidate_properties (current : Option ElectionCandidate) (member : SessionMember) :
    ∃ selected, earlierCandidate current member = some selected ∧
      (selected = memberCandidate member ∨ current = some selected) ∧
      electionKey selected ≤ electionKey (memberCandidate member) ∧
      ∀ prior, current = some prior → electionKey selected ≤ electionKey prior := by
  cases current with
  | none => exact ⟨memberCandidate member, rfl, Or.inl rfl, le_rfl, by simp⟩
  | some prior =>
    by_cases earlier : electionKey (memberCandidate member) < electionKey prior
    · refine ⟨memberCandidate member, by simp [earlierCandidate, earlier], Or.inl rfl, le_rfl, ?_⟩
      intro previous same
      cases same
      exact le_of_lt earlier
    · refine ⟨prior, by simp [earlierCandidate, earlier], Or.inr rfl, le_of_not_gt earlier, ?_⟩
      intro previous same
      cases same
      exact le_rfl

private theorem scan_none (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (current : Option ElectionCandidate) :
    electionScan authority members current = none ↔ members = [] ∧ current = none := by
  induction members generalizing current with
  | nil => simp [electionScan]
  | cons head tail ih =>
    obtain ⟨candidate, improved, _⟩ := earlier_candidate_properties current head
    by_cases present : CoreModels.core.option.Option.Some head.player_id = authority
    · simp [electionScan, present]
    · simp [electionScan, present, improved, ih]

private theorem scan_membership (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (current : Option ElectionCandidate)
    (selected : ElectionCandidate) (result : electionScan authority members current = some selected) :
    selected ∈ members.map memberCandidate ∨ current = some selected := by
  induction members generalizing current with
  | nil => exact Or.inr result
  | cons head tail ih =>
    by_cases present : CoreModels.core.option.Option.Some head.player_id = authority
    · have same : memberCandidate head = selected := by simpa [electionScan, present] using result
      exact Or.inl (by simp [← same])
    · obtain ⟨candidate, improved, provenance, _⟩ := earlier_candidate_properties current head
      have recursive : electionScan authority tail (some candidate) = some selected := by
        simpa [electionScan, present, improved] using result
      rcases ih (some candidate) recursive with remaining | previous
      · exact Or.inl (List.mem_cons_of_mem _ remaining)
      · have same : candidate = selected := Option.some.inj previous
        subst candidate
        rcases provenance with first | prior
        · exact Or.inl (by simp [first])
        · exact Or.inr prior

private theorem scan_authority (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (current : Option ElectionCandidate)
    (present : ∃ member ∈ members, CoreModels.core.option.Option.Some member.player_id = authority)
    (selected : ElectionCandidate) (result : electionScan authority members current = some selected) :
    CoreModels.core.option.Option.Some selected.2 = authority := by
  induction members generalizing current with
  | nil => simp at present
  | cons head tail ih =>
    by_cases first : CoreModels.core.option.Option.Some head.player_id = authority
    · have same : memberCandidate head = selected := by simpa [electionScan, first] using result
      simpa [← same, memberCandidate] using first
    · obtain ⟨member, membership, identity⟩ := present
      have remaining : member ∈ tail := by
        rcases List.mem_cons.mp membership with same | remaining
        · subst member; exact False.elim (first identity)
        · exact remaining
      exact ih (earlierCandidate current head) ⟨member, remaining, identity⟩
        (by simpa [electionScan, first] using result)

private theorem scan_minimum (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : List SessionMember) (current : Option ElectionCandidate)
    (absent : ∀ member ∈ members, CoreModels.core.option.Option.Some member.player_id ≠ authority)
    (selected : ElectionCandidate) (result : electionScan authority members current = some selected) :
    (∀ member ∈ members, electionKey selected ≤ electionKey (memberCandidate member)) ∧
    (∀ prior, current = some prior → electionKey selected ≤ electionKey prior) := by
  induction members generalizing current with
  | nil =>
    refine ⟨by simp, ?_⟩
    intro prior same
    have selected_eq : selected = prior := Option.some.inj (result.symm.trans same)
    simp [selected_eq]
  | cons head tail ih =>
    obtain ⟨candidate, improved, _, head_bound, prior_bound⟩ := earlier_candidate_properties current head
    have first := absent head (by simp)
    have recursive : electionScan authority tail (some candidate) = some selected := by
      simpa [electionScan, first, improved] using result
    obtain ⟨tail_minimum, candidate_minimum⟩ := ih (some candidate)
      (fun member membership => absent member (List.mem_cons_of_mem _ membership)) recursive
    have selected_le_candidate := candidate_minimum candidate rfl
    constructor
    · intro member membership
      rcases List.mem_cons.mp membership with same | remaining
      · subst member; exact le_trans selected_le_candidate head_bound
      · exact tail_minimum member remaining
    · intro prior same
      exact le_trans selected_le_candidate (prior_bound prior same)

theorem election_evaluation (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) :
    elect_host electionIterator authority members =
      RustM.ok (RustOption ((electionScan authority members.val none).map Prod.snd)) := by
  simpa [elect_host, electionIterator, CoreModels.core.iter.traits.collect.IntoIterator.Blanket,
    CoreModels.core.iter.traits.collect.IntoIterator.Blanket.into_iter,
    Aeneas.Std.bind, Bind.bind, RustOption]
    using election_loop_evaluation authority members.val members.property none

/-- A finite enumeration returns no host exactly when it contains no candidates. -/
theorem election_empty_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) :
    elect_host electionIterator authority members = RustM.ok .None ↔ members.val = [] := by
  rw [election_evaluation]
  cases result : electionScan authority members.val none with
  | none =>
    have empty := (scan_none authority members.val none).mp result
    simp [RustOption, empty.1]
  | some candidate =>
    have nonempty : members.val ≠ [] := by
      intro empty
      simp [electionScan, empty] at result
    simp [RustOption, nonempty]

/-- An explicitly designated candidate wins regardless of timestamps and proxy flags. -/
theorem election_authority_contract (host : uuid.Uuid) (members : Slice SessionMember)
    (present : ∃ member ∈ members.val, member.player_id = host) :
    elect_host electionIterator (.Some host) members = RustM.ok (.Some host) := by
  rw [election_evaluation]
  cases result : electionScan (.Some host) members.val none with
  | none =>
    have empty := (scan_none (.Some host) members.val none).mp result
    obtain ⟨member, membership, _⟩ := present
    simp [empty.1] at membership
  | some candidate =>
    have identity := scan_authority (.Some host) members.val none
      (by obtain ⟨member, membership, identity⟩ := present
          exact ⟨member, membership, congrArg CoreModels.core.option.Option.Some identity⟩) candidate result
    simpa [RustOption] using congrArg RustM.ok identity

/-- Every elected identity names an input candidate. No legal membership assumption is needed. -/
theorem election_membership_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (host : uuid.Uuid)
    (returns : elect_host electionIterator authority members = RustM.ok (.Some host)) :
    ∃ member ∈ members.val, member.player_id = host := by
  rw [election_evaluation] at returns
  cases result : electionScan authority members.val none with
  | none => simp [result, RustOption] at returns
  | some candidate =>
    have identity : candidate.2 = host := by simpa [result, RustOption] using returns
    have provenance := scan_membership authority members.val none candidate result
    rcases provenance with membership | impossible
    · obtain ⟨member, membership, same⟩ := List.mem_map.mp membership
      exact ⟨member, membership, by simpa [← same, memberCandidate] using identity⟩
    · simp at impossible

/-- Without a seated authority the elected candidate minimizes timestamp, then UUID.
Ordering uses the explicit trusted dependency interpretations. The loop itself is extracted. -/
theorem election_minimum_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) (host : uuid.Uuid)
    (absent : ∀ member ∈ members.val, CoreModels.core.option.Option.Some member.player_id ≠ authority)
    (returns : elect_host electionIterator authority members = RustM.ok (.Some host)) :
    ∃ selected ∈ members.val, selected.player_id = host ∧
      ∀ member ∈ members.val, electionKey (memberCandidate selected) ≤ electionKey (memberCandidate member) := by
  rw [election_evaluation] at returns
  cases result : electionScan authority members.val none with
  | none => simp [result, RustOption] at returns
  | some candidate =>
    have identity : candidate.2 = host := by simpa [result, RustOption] using returns
    have provenance := scan_membership authority members.val none candidate result
    rcases provenance with membership | impossible
    · obtain ⟨member, membership, same⟩ := List.mem_map.mp membership
      refine ⟨member, membership, ?_, ?_⟩
      · simpa [← same, memberCandidate] using identity
      · simpa [← same] using (scan_minimum authority members.val none absent candidate result).1
    · simp at impossible

/-- Every finite candidate enumeration terminates successfully in the pinned iterator model. -/
theorem election_total_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members : Slice SessionMember) :
    ∃ result, elect_host electionIterator authority members = RustM.ok result :=
  ⟨_, election_evaluation authority members⟩

/-- Empty and singleton cases are reachable for arbitrary member metadata and authority flags. -/
theorem election_outcomes (member : SessionMember) :
    elect_host electionIterator .None (⟨[], by scalar_tac⟩ : Slice SessionMember) = RustM.ok .None ∧
    elect_host electionIterator .None (⟨[member], by scalar_tac⟩ : Slice SessionMember) = RustM.ok (.Some member.player_id) := by
  constructor
  · exact (election_empty_contract _ _).2 rfl
  · rw [election_evaluation]
    simp [electionScan, earlierCandidate, memberCandidate, RustOption,
      CoreModels.core.option.Option.Some, CoreModels.core.option.Option.None]

/-- Reordering a finite enumeration cannot change an elected identity, even with duplicate records. -/
theorem election_permutation_contract (authority : CoreModels.core.option.Option uuid.Uuid)
    (members reordered : Slice SessionMember) (permutation : members.val.Perm reordered.val)
    (first second : uuid.Uuid)
    (first_returns : elect_host electionIterator authority members = RustM.ok (.Some first))
    (second_returns : elect_host electionIterator authority reordered = RustM.ok (.Some second)) :
    first = second := by
  by_cases seated : ∃ member ∈ members.val, CoreModels.core.option.Option.Some member.player_id = authority
  · obtain ⟨member, membership, identity⟩ := seated
    have first_wins := election_authority_contract member.player_id members ⟨member, membership, rfl⟩
    have second_wins := election_authority_contract member.player_id reordered
      ⟨member, permutation.mem_iff.mp membership, rfl⟩
    rw [← identity] at first_returns second_returns
    have first_identity : member.player_id = first := by simpa [first_wins] using first_returns
    have second_identity : member.player_id = second := by simpa [second_wins] using second_returns
    exact first_identity.symm.trans second_identity
  · have absent : ∀ member ∈ members.val, CoreModels.core.option.Option.Some member.player_id ≠ authority := by
      intro member membership identity
      exact seated ⟨member, membership, identity⟩
    have absent_reordered : ∀ member ∈ reordered.val, CoreModels.core.option.Option.Some member.player_id ≠ authority :=
      fun member membership => absent member (permutation.mem_iff.mpr membership)
    obtain ⟨left, left_member, left_identity, left_minimum⟩ :=
      election_minimum_contract authority members first absent first_returns
    obtain ⟨right, right_member, right_identity, right_minimum⟩ :=
      election_minimum_contract authority reordered second absent_reordered second_returns
    have keys := le_antisymm
      (left_minimum right (permutation.mem_iff.mpr right_member))
      (right_minimum left (permutation.mem_iff.mp left_member))
    have bytes := congrArg (fun key : Lex (Nat × Nat) => (ofLex key).2) keys
    have equal_ids : left.player_id = right.player_id := (uuid_key_equal _ _).mp
      (by simpa [electionKey, memberCandidate] using bytes)
    exact left_identity.symm.trans (equal_ids.trans right_identity)

end Capability
