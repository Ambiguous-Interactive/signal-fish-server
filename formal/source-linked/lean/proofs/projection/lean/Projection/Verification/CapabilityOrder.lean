import Projection.Verification.CapabilityNegotiation

namespace Projection

/-- The accumulator specification agrees with Lean's first-occurrence deduplication. -/
theorem first_occurrences_loop {T : Type} [DecidableEq T] (xs seen : List T) :
    firstOccurrences xs seen =
      List.eraseDupsBy.loop (fun a b => decide (a = b)) xs seen.reverse := by
  induction xs generalizing seen with
  | nil => simp [firstOccurrences, List.eraseDupsBy.loop]
  | cons x xs ih =>
    simp only [firstOccurrences, List.eraseDupsBy.loop]
    by_cases present : x ∈ seen
    · have any : seen.reverse.any (fun b => decide (x = b)) = true := by
        simp [List.any_eq_true, present]
      simp [present, any, ih]
    · have any : seen.reverse.any (fun b => decide (x = b)) = false := by
        simp only [List.any_eq_false, List.mem_reverse]
        intro a member same
        have same := of_decide_eq_true same
        subst a
        exact present member
      simp [present, any, ih, List.reverse_append]

/-- The result keeps precisely the first occurrence of each input value, in input order. -/
theorem first_occurrences_order {T : Type} [DecidableEq T] [BEq T] [LawfulBEq T] (xs : List T) :
    firstOccurrences xs [] = xs.eraseDups := by
  simpa [List.eraseDups, List.eraseDupsBy, Bool.beq_eq_decide_eq] using first_occurrences_loop xs []

/-- Deduplication removes no capability and adds no capability. -/
theorem first_occurrences_membership {T : Type} [DecidableEq T]
    (xs seen : List T) (value : T) :
    value ∈ firstOccurrences xs seen ↔ value ∈ xs ∨ value ∈ seen := by
  induction xs generalizing seen with
  | nil => simp [firstOccurrences]
  | cons x xs ih =>
    simp only [firstOccurrences]
    split
    · rename_i present
      rw [ih]
      simp only [List.mem_cons]
      constructor
      · tauto
      · intro cases
        rcases cases with (same | member) | member
        · subst value; exact Or.inr present
        · exact Or.inl member
        · exact Or.inr member
    · rw [ih]
      simp only [List.mem_append, List.mem_cons]
      tauto

/-- A unique emitted prefix remains unique after processing any remaining input. -/
theorem first_occurrences_nodup {T : Type} [DecidableEq T]
    (xs seen : List T) (unique : seen.Nodup) :
    (firstOccurrences xs seen).Nodup := by
  induction xs generalizing seen with
  | nil => simpa [firstOccurrences] using unique
  | cons x xs ih =>
    simp only [firstOccurrences]
    split
    · exact ih seen unique
    · rename_i absent
      have different : ∀ a ∈ seen, ¬ a = x := by
        intro a member same
        subst a
        exact absent member
      exact ih (seen ++ [x]) (by simp [List.nodup_append, unique]; exact different)

/-- The extracted production helper preserves exactly one copy of each input capability,
in the order of its first occurrence. -/
theorem dedup_protocol_contract {T : Type} [DecidableEq T] [BEq T] [LawfulBEq T]
    (inst : CoreModels.core.cmp.PartialEq T T)
    (equality : ∀ a b, inst.eq a b = Aeneas.Std.RustM.ok (decide (a = b)))
    (items : CoreModels.alloc.vec.Vec T) :
    ∃ output : CoreModels.alloc.vec.Vec T,
      signal_fish_proof_core.session_capability.dedup_preserving_order inst items =
        Aeneas.Std.RustM.ok output ∧
      output.val = items.val.eraseDups ∧
      (∀ value, value ∈ output.val ↔ value ∈ items.val) ∧ output.val.Nodup := by
  refine ⟨_, dedup_contract inst equality items, ?_, ?_, ?_⟩
  · exact first_occurrences_order items.val
  · intro value
    simpa only [List.not_mem_nil, or_false] using
      first_occurrences_membership items.val [] value
  · exact first_occurrences_nodup items.val [] (by simp)

end Projection
