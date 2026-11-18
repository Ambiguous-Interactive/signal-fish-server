-- [signal_fish_proof_core]: external types.
-- Seeded by hax from Extraction/TypesExternal_Template.lean: fill the holes.
-- hax never modifies this file; after re-extraction, compare it against the
-- regenerated template to see what changed.
import Aeneas
import CoreModels
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

/-- External UUID representation. Equality is modeled by exact equality of all sixteen bytes. -/
structure uuid.Uuid where
  bytes : Aeneas.Std.Array Aeneas.Std.U8 16#usize

noncomputable instance : DecidableEq uuid.Uuid := Classical.decEq _

/-- External interpretation of a Jiff 1.0.1 timestamp. Jiff delegates comparison
to jiff-core's signed seconds and signed nanoseconds. The model admits every
scalar pair. Timestamp construction and normalization remain outside the proof. -/
structure jiff.timestamp.Timestamp where
  seconds : I64
  fraction : I32
