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

/-- External UUID representation. These contracts never inspect identity bytes. -/
structure uuid.Uuid where
  bytes : Aeneas.Std.Array Aeneas.Std.U8 16#usize

/-- UTC has no stored fields in the locked Chrono dependency. -/
inductive chrono.offset.utc.Utc where
  | Utc

/-- External timestamp interpretation for fields these decisions never read.
Chrono 0.4.45 stores a packed date, seconds, fraction and offset. This interpretation
permits every scalar value, including invalid dates. It adds no timestamp operations,
ordering rules or axioms. Timestamp construction and host election remain outside. -/
structure chrono.datetime.DateTime (Tz Offset : Type) where
  packedDate : I32
  seconds : U32
  fraction : U32
  offset : Offset
