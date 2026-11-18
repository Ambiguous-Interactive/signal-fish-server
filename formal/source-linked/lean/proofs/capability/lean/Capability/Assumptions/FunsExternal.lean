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
on its fixed [u8; 16] field. Jiff 1.0.1 Timestamp::cmp delegates to jiff-core's
timestamp order, which compares signed seconds, then signed nanoseconds.
ElectionOrder.lean checks the encodings against those modeled fields. -/
namespace ElectionOrder

def uuidKey (id : uuid.Uuid) : Nat :=
  id.bytes.val.foldl (fun key byte => key * 256 + byte.val) 0

def timeKey (time : jiff.timestamp.Timestamp) : Nat :=
  ((time.seconds.val + 9223372036854775808).toNat * 4294967296 +
    (time.fraction.val + 2147483648).toNat)

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

noncomputable def jiff.timestamp.Timestamp.Insts.CoreCmpPartialOrdTimestamp :
    core.cmp.PartialOrd jiff.timestamp.Timestamp jiff.timestamp.Timestamp :=
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
