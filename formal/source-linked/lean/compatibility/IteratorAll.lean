/-
SPDX-License-Identifier: Apache-2.0
Backport of the slice Iterator::all and Iterator::any aliases from hax-lean v0.3.41,
commit 1d84de68c03bd76457887121853779a52a09e33a,
CoreModels/Core/FunsEpilogue.lean. See LICENSE in this directory.
Only the namespace wrapper and fixed Type universe differ from upstream.
The pinned v0.3.30 model supplies the iterator and the loop semantics.
-/
import CoreModels

namespace CoreModels.core

abbrev slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.all
    {T F : Type} (FnInst : ops.function.FnMut F T Bool) :=
  iter.traits.iterator.Iterator.all.default
    (slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT T) FnInst

abbrev slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT.any
    {T F : Type} (FnInst : ops.function.FnMut F T Bool) :=
  iter.traits.iterator.Iterator.any.default
    (slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT T) FnInst

/- Local dispatch alias for the extracted generic Iterator::filter call.
The pinned library already supplies filter.default and Filter::next. This alias
only exposes that default under the generated method name; it adds no behavior. -/
abbrev iter.traits.iterator.Iterator.filter
    {Self P Item : Type} (IteratorInst : iter.traits.iterator.Iterator Self Item)
    (FnInst : ops.function.FnMut P Item Bool) :=
  iter.traits.iterator.Iterator.filter.default IteratorInst FnInst

end CoreModels.core
