import Capability.Extraction

open CoreModels Aeneas Aeneas.Std

namespace Capability

/-- The numeric timestamp interpretation preserves lexicographic field ordering.
This checks the encoding arithmetic, not the trusted Chrono field interpretation. -/
theorem timestamp_key_order {Left LeftOffset Right RightOffset : Type}
    (left : chrono.datetime.DateTime Left LeftOffset)
    (right : chrono.datetime.DateTime Right RightOffset) :
    ElectionOrder.timeKey left < ElectionOrder.timeKey right ↔
      left.packedDate.val < right.packedDate.val ∨
        left.packedDate.val = right.packedDate.val ∧
          (left.seconds.val < right.seconds.val ∨
            left.seconds.val = right.seconds.val ∧ left.fraction.val < right.fraction.val) := by
  have left_date_lower : -2147483648 ≤ left.packedDate.val := by scalar_tac
  have right_date_lower : -2147483648 ≤ right.packedDate.val := by scalar_tac
  have left_seconds_upper : left.seconds.val < 4294967296 := by scalar_tac
  have right_seconds_upper : right.seconds.val < 4294967296 := by scalar_tac
  have left_fraction_upper : left.fraction.val < 4294967296 := by scalar_tac
  have right_fraction_upper : right.fraction.val < 4294967296 := by scalar_tac
  unfold ElectionOrder.timeKey
  omega

/-- Distinct modeled timestamp fields have distinct keys; timezone offset is ignored. -/
theorem timestamp_key_equal {Left LeftOffset Right RightOffset : Type}
    (left : chrono.datetime.DateTime Left LeftOffset)
    (right : chrono.datetime.DateTime Right RightOffset) :
    ElectionOrder.timeKey left = ElectionOrder.timeKey right ↔
      left.packedDate.val = right.packedDate.val ∧
        left.seconds.val = right.seconds.val ∧ left.fraction.val = right.fraction.val := by
  have left_date_lower : -2147483648 ≤ left.packedDate.val := by scalar_tac
  have right_date_lower : -2147483648 ≤ right.packedDate.val := by scalar_tac
  have left_seconds_upper : left.seconds.val < 4294967296 := by scalar_tac
  have right_seconds_upper : right.seconds.val < 4294967296 := by scalar_tac
  have left_fraction_upper : left.fraction.val < 4294967296 := by scalar_tac
  have right_fraction_upper : right.fraction.val < 4294967296 := by scalar_tac
  unfold ElectionOrder.timeKey
  omega

private def reverseByteKey (bytes : List U8) : Nat :=
  bytes.foldr (fun byte key => key * 256 + byte.val) 0

private theorem reverse_byte_key_injective (left right : List U8)
    (lengths : left.length = right.length)
    (equal : reverseByteKey left = reverseByteKey right) : left = right := by
  induction left generalizing right with
  | nil => simpa using lengths.symm
  | cons head tail ih =>
    cases right with
    | nil => simp at lengths
    | cons other rest =>
      have head_bound : head.val < 256 := by scalar_tac
      have other_bound : other.val < 256 := by scalar_tac
      have encoded : reverseByteKey tail * 256 + head.val =
          reverseByteKey rest * 256 + other.val := by
        simpa [reverseByteKey] using equal
      have digit_equal : head.val = other.val := by omega
      have head_equal : head = other := by scalar_tac
      have rest_equal : reverseByteKey tail = reverseByteKey rest := by omega
      have tail_equal : tail = rest := ih rest (by simpa using lengths) rest_equal
      simp [head_equal, tail_equal]

/-- The fixed-length base-256 interpretation has no UUID identity collisions. -/
theorem uuid_key_equal (left right : uuid.Uuid) :
    ElectionOrder.uuidKey left = ElectionOrder.uuidKey right ↔ left = right := by
  constructor
  · intro equal
    have lengths : left.bytes.val.reverse.length = right.bytes.val.reverse.length := by
      simp only [List.length_reverse, Array.length_eq]
    have reversed : left.bytes.val.reverse = right.bytes.val.reverse :=
      reverse_byte_key_injective _ _ lengths (by
        simpa only [reverseByteKey, ElectionOrder.uuidKey, List.foldl_eq_foldr_reverse] using equal)
    have bytes_equal : left.bytes.val = right.bytes.val := List.reverse_injective reversed
    cases left with
    | mk left =>
      cases right with
      | mk right =>
        have arrays_equal : left = right := Subtype.ext bytes_equal
        simp [arrays_equal]
  · intro equal
    rw [equal]

private def byteKey (bytes : List U8) : Nat :=
  bytes.foldl (fun key byte => key * 256 + byte.val) 0

private theorem byte_key_shift (bytes : List U8) (initial : Nat) :
    bytes.foldl (fun key byte => key * 256 + byte.val) initial =
      initial * 256 ^ bytes.length + byteKey bytes := by
  induction bytes generalizing initial with
  | nil => simp [byteKey]
  | cons head tail ih =>
    simp only [List.foldl_cons, List.length_cons, byteKey, Nat.zero_mul, Nat.zero_add]
    rw [ih (initial * 256 + head.val), ih head.val]
    simp only [Nat.pow_succ]
    ring

private theorem byte_key_bound (bytes : List U8) : byteKey bytes < 256 ^ bytes.length := by
  induction bytes with
  | nil => simp [byteKey]
  | cons head tail ih =>
    have bound : head.val < 256 := by scalar_tac
    have multiplied : (head.val + 1) * 256 ^ tail.length ≤ 256 * 256 ^ tail.length :=
      Nat.mul_le_mul_right _ (by omega)
    simp only [Nat.add_mul, Nat.one_mul] at multiplied
    simp only [byteKey, List.foldl_cons, Nat.zero_mul, Nat.zero_add, List.length_cons]
    rw [byte_key_shift]
    rw [Nat.pow_succ]
    omega

private theorem radix_order (leftDigit rightDigit leftTail rightTail scale : Nat)
    (left_bound : leftTail < scale) (right_bound : rightTail < scale) :
    leftDigit * scale + leftTail < rightDigit * scale + rightTail ↔
      leftDigit < rightDigit ∨ leftDigit = rightDigit ∧ leftTail < rightTail := by
  rcases Nat.lt_trichotomy leftDigit rightDigit with less | equal | greater
  · have multiplied : (leftDigit + 1) * scale ≤ rightDigit * scale :=
      Nat.mul_le_mul_right _ (by omega)
    simp only [Nat.add_mul, Nat.one_mul] at multiplied
    omega
  · subst rightDigit
    omega
  · have multiplied : (rightDigit + 1) * scale ≤ leftDigit * scale :=
      Nat.mul_le_mul_right _ (by omega)
    simp only [Nat.add_mul, Nat.one_mul] at multiplied
    omega

private theorem byte_key_order (left right : List U8) (lengths : left.length = right.length) :
    byteKey left < byteKey right ↔ List.Lex (fun a b : U8 => a.val < b.val) left right := by
  induction left generalizing right with
  | nil =>
    have right_empty : right = [] := by simpa using lengths.symm
    simp [right_empty]
  | cons head tail ih =>
    cases right with
    | nil => simp at lengths
    | cons other rest =>
      have tail_lengths : tail.length = rest.length := by simpa using lengths
      have digit_equal : head.val = other.val ↔ head = other := by
        constructor
        · intro equality; scalar_tac
        · intro equality; rw [equality]
      simp only [byteKey, List.foldl_cons, Nat.zero_mul, Nat.zero_add]
      rw [byte_key_shift, byte_key_shift, ← tail_lengths,
        radix_order _ _ _ _ _ (byte_key_bound tail) (by simpa [tail_lengths] using byte_key_bound rest),
        ih rest tail_lengths, digit_equal, List.cons_lex_cons_iff]

/-- Numeric UUID keys preserve the first differing byte order for all sixteen bytes. -/
theorem uuid_key_order (left right : uuid.Uuid) :
    ElectionOrder.uuidKey left < ElectionOrder.uuidKey right ↔
      List.Lex (fun a b : U8 => a.val < b.val) left.bytes.val right.bytes.val := by
  exact byte_key_order _ _ (by simp only [Array.length_eq])

end Capability
