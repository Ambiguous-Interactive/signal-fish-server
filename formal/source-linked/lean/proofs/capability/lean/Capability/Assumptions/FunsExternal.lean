import Capability.Compatibility
-- [signal_fish_proof_core]: external functions.
-- Seeded by hax from Extraction/FunsExternal_Template.lean: fill the holes.
-- hax never modifies this file; after re-extraction, compare it against the
-- regenerated template to see what changed.
import Aeneas
import CoreModels
import Capability.Extraction.Types
open CoreModels Aeneas
open Aeneas.Std hiding namespace core alloc
open RustM ControlFlow Error
open Std.Do
set_option linter.dupNamespace false
set_option linter.hashCommand false
set_option linter.unusedVariables false
set_option linter.style.whitespace false
set_option linter.style.setOption false
set_option linter.style.longLine false

/- You can set the `maxHeartbeats` value with the `-max-heartbeats` CLI option -/
set_option maxHeartbeats 1000000

/- You can set the `maxRecDepth` value with the `-max-recdepth` CLI option -/
set_option maxRecDepth 2048
open signal_fish_proof_core

/-- Trusted UUID 1.29.0 equality mapping. The locked dependency derives PartialEq
on its sole [u8; 16] field (src/lib.rs:445,466). The pinned hax invocation omits
nonlocal definitions. This mapping is not a proof of the Rust dependency. -/
noncomputable def uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq (left right : uuid.Uuid) : RustM Bool :=
  RustM.ok (decide (left = right))

noncomputable def uuid.Uuid.Insts.CoreCmpPartialEqUuid : core.cmp.PartialEq uuid.Uuid uuid.Uuid := {
  eq := uuid.Uuid.Insts.CoreCmpPartialEqUuid.eq
  ne := fun left right => RustM.ok (decide (left ≠ right))
}

/- Trusted dependency interpretations: UUID 1.29.0 derives lexicographic order
on its fixed [u8; 16] field. Chrono 0.4.45 DateTime::partial_cmp compares its
NaiveDateTime, whose derived order compares signed packed date, u32 seconds,
then u32 fraction. ElectionOrder.lean checks the encodings, not these Rust layouts. -/
namespace ElectionOrder

def uuidKey (id : uuid.Uuid) : Nat :=
  id.bytes.val.foldl (fun key byte => key * 256 + byte.val) 0

def timeKey {Tz Offset : Type} (time : chrono.datetime.DateTime Tz Offset) : Nat :=
  (((time.packedDate.val + 2147483648).toNat * 4294967296 + time.seconds.val) *
    4294967296 + time.fraction.val)

noncomputable def orderBy {Left Right : Type} (leftKey : Left → Nat) (rightKey : Right → Nat) :
    core.cmp.PartialOrd Left Right := {
  PartialEqInst := {
    eq := fun left right => RustM.ok (decide (leftKey left = rightKey right))
    ne := fun left right => RustM.ok (decide (leftKey left ≠ rightKey right))
  }
  partial_cmp := fun left right => RustM.ok (.Some
    (if leftKey left < rightKey right then .Less
     else if leftKey left = rightKey right then .Equal else .Greater))
  lt := fun left right => RustM.ok (decide (leftKey left < rightKey right))
  le := fun left right => RustM.ok (decide (leftKey left ≤ rightKey right))
  gt := fun left right => RustM.ok (decide (rightKey right < leftKey left))
  ge := fun left right => RustM.ok (decide (rightKey right ≤ leftKey left))
}
end ElectionOrder

/-- Only the associated offset type is needed by the extracted UTC comparison. -/
structure chrono.offset.TimeZone (Self : Type) where
  Offset : Type

abbrev chrono.offset.utc.Utc.Insts.ChronoOffsetTimeZoneUtc : chrono.offset.TimeZone chrono.offset.utc.Utc :=
  ⟨chrono.offset.utc.Utc⟩

noncomputable def chrono.datetime.DateTime.Insts.CoreCmpPartialOrdDateTime
    {Left Right : Type} (left : chrono.offset.TimeZone Left) (right : chrono.offset.TimeZone Right) :
    core.cmp.PartialOrd (chrono.datetime.DateTime Left left.Offset)
      (chrono.datetime.DateTime Right right.Offset) :=
  ElectionOrder.orderBy ElectionOrder.timeKey ElectionOrder.timeKey

noncomputable def uuid.Uuid.Insts.CoreCmpPartialOrdUuid : core.cmp.PartialOrd uuid.Uuid uuid.Uuid :=
  ElectionOrder.orderBy ElectionOrder.uuidKey ElectionOrder.uuidKey

/-- Trusted std tuple comparison chain: compare the first field; on equality
use the second field's lt. This matches the pinned lexical_ord macro. -/
def CoreModels.core.Pair.Insts.CoreCmpPartialOrdPair.lt {A B : Type}
    (first : core.cmp.PartialOrd A A) (second : core.cmp.PartialOrd B B)
    (left right : A × B) : RustM Bool := do
  let comparison ← first.partial_cmp left.1 right.1
  match comparison with
  | .Some .Equal => second.lt left.2 right.2
  | .Some .Less => RustM.ok true
  | _ => RustM.ok false
