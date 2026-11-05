import Capability.Extraction

open Aeneas.Std

namespace Capability

/-- A total boolean projection used to instantiate the generic callback contract. -/
def booleanIdentity : CoreModels.core.ops.function.FnMut Unit Bool Bool where
  FnOnceInst := { call_once := fun _ member => RustM.ok member }
  call_mut := fun state member => RustM.ok (member, state)

end Capability
