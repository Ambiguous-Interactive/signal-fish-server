import Capability.Verification.ElectionOrder
import Capability.Verification.Callbacks

open CoreModels Aeneas Aeneas.Std
open signal_fish_proof_core session_member

namespace Capability.ElectionFixtures

def lowId : uuid.Uuid := ⟨⟨List.replicate 16 0#u8, by simp⟩⟩
def highId : uuid.Uuid := ⟨⟨List.replicate 16 1#u8, by simp⟩⟩

theorem distinct : lowId ≠ highId := by
  intro same
  have bytes := congrArg (fun id : uuid.Uuid => id.bytes.val.head?) same
  simp [lowId, highId] at bytes

theorem id_order : ElectionOrder.uuidKey lowId < ElectionOrder.uuidKey highId := by
  rw [uuid_key_order]
  decide

def early (member : SessionMember) : SessionMember :=
  { member with
    player_id := lowId
    joined_at := { member.joined_at with seconds := 0#u32, fraction := 0#u32 } }

def late (member : SessionMember) : SessionMember :=
  { member with
    player_id := highId
    joined_at := { member.joined_at with seconds := 1#u32, fraction := 0#u32 } }

theorem time_order (member : SessionMember) :
    ElectionOrder.timeKey (early member).joined_at < ElectionOrder.timeKey (late member).joined_at := by
  rw [timestamp_key_order]
  simp [early, late]

attribute [local irreducible] ElectionOrder.timeKey ElectionOrder.uuidKey

theorem pair_key_comparison {A B : Type} (first : A → Nat) (second : B → Nat) (x y : A × B) :
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

abbrev iterator : CoreModels.core.iter.traits.collect.IntoIterator
    (Slice SessionMember) SessionMember (Slice SessionMember) :=
  CoreModels.core.iter.traits.collect.IntoIterator.Blanket
    (CoreModels.core.slice.iter.Iter.Insts.CoreIterTraitsIteratorIteratorSharedAT SessionMember)

end Capability.ElectionFixtures
