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
