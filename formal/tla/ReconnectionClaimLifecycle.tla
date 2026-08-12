------------------ MODULE ReconnectionClaimLifecycle ------------------
(***************************************************************************)
(* Atomic reconnection-credential claim lifecycle (#220 / P87).            *)
(*                                                                         *)
(* A disconnected-player record is a retryable single-use credential. Two  *)
(* fresh sockets may present the same valid credential concurrently, while  *)
(* an invalid identity may race them. Exactly one valid socket can reserve  *)
(* the record. The reservation survives window expiry and blocks cleanup;   *)
(* downstream restore failure releases it, and only the matching claim      *)
(* handle can release or consume it.                                        *)
(*                                                                         *)
(* Mapping to src/reconnection.rs and server/reconnection_service.rs:       *)
(*   Claim                  claim_reconnection_with_identity                *)
(*   ReleaseClaim           release_reconnection_claim / guard Drop         *)
(*   CompleteClaim          complete_claimed_reconnection                   *)
(*   RestoreFailure         reject_claimed_reconnect                        *)
(*   AdvancePastWindow      wall-clock reconnection-window expiry           *)
(*   CleanupExpired         expired_cleanup_candidates +                    *)
(*                          remove_expired_reconnection                     *)
(*                                                                         *)
(* Claim epochs abstract production UUID claim_id values. Retained handles  *)
(* let TLC try an old handle after a later claimant has reserved the same   *)
(* record. The expected-failure configurations independently prove that     *)
(* identity validation, handle matching, cleanup exclusion, and failed-     *)
(* restore release are each necessary.                                      *)
(***************************************************************************)
EXTENDS FiniteSets, Naturals

CONSTANTS
    Claimants,
    ValidClaimants,
    MaxClaims,
    AcceptInvalidCredentialBug,
    IgnoreClaimHandleBug,
    CleanupClaimedRecordBug,
    ConsumeFailedRestoreBug

NoClaim == "NoClaim"
ClaimEpochs == 1..MaxClaims
InvalidClaimants == Claimants \ ValidClaimants

ASSUME /\ Cardinality(Claimants) = 3
       /\ Cardinality(ValidClaimants) = 2
       /\ ValidClaimants \subseteq Claimants
       /\ MaxClaims = 2
       /\ NoClaim \notin Claimants
       /\ AcceptInvalidCredentialBug \in BOOLEAN
       /\ IgnoreClaimHandleBug \in BOOLEAN
       /\ CleanupClaimedRecordBug \in BOOLEAN
       /\ ConsumeFailedRestoreBug \in BOOLEAN

VARIABLES
    recordState,              \* Pending / Claimed / Consumed / Cleaned
    windowExpired,
    nextClaimEpoch,
    activeClaimant,
    activeClaimEpoch,
    handles,                  \* every claim handle retained by its caller
    releasedClaims,
    completedClaims,
    invalidRejected,
    invalidCredentialAdmitted,
    staleHandleMutation,
    cleanedWhileClaimed,
    failedRestoreConsumed

vars == <<recordState, windowExpired, nextClaimEpoch, activeClaimant,
          activeClaimEpoch, handles, releasedClaims, completedClaims,
          invalidRejected, invalidCredentialAdmitted, staleHandleMutation,
          cleanedWhileClaimed, failedRestoreConsumed>>

Init ==
    /\ recordState = "Pending"
    /\ windowExpired = FALSE
    /\ nextClaimEpoch = 1
    /\ activeClaimant = NoClaim
    /\ activeClaimEpoch = 0
    /\ handles = [c \in Claimants |-> {}]
    /\ releasedClaims = {}
    /\ completedClaims = {}
    /\ invalidRejected = {}
    /\ invalidCredentialAdmitted = FALSE
    /\ staleHandleMutation = FALSE
    /\ cleanedWhileClaimed = FALSE
    /\ failedRestoreConsumed = FALSE

Claim(c) ==
    /\ c \in ValidClaimants
    /\ recordState = "Pending"
    /\ ~windowExpired
    /\ nextClaimEpoch \in ClaimEpochs
    /\ recordState' = "Claimed"
    /\ activeClaimant' = c
    /\ activeClaimEpoch' = nextClaimEpoch
    /\ handles' = [handles EXCEPT ![c] = @ \cup {nextClaimEpoch}]
    /\ nextClaimEpoch' = nextClaimEpoch + 1
    /\ UNCHANGED <<windowExpired, releasedClaims, completedClaims,
                   invalidRejected, invalidCredentialAdmitted,
                   staleHandleMutation, cleanedWhileClaimed,
                   failedRestoreConsumed>>

(* A mismatched token/certificate is rejected under the same atomic record  *)
(* lock. The bug arm demonstrates why validation must precede reservation.  *)
AttemptInvalidClaim(c) ==
    /\ c \in InvalidClaimants
    /\ recordState = "Pending"
    /\ ~windowExpired
    /\ nextClaimEpoch \in ClaimEpochs
    /\ IF AcceptInvalidCredentialBug
          THEN /\ recordState' = "Claimed"
               /\ activeClaimant' = c
               /\ activeClaimEpoch' = nextClaimEpoch
               /\ handles' = [handles EXCEPT ![c] = @ \cup {nextClaimEpoch}]
               /\ nextClaimEpoch' = nextClaimEpoch + 1
               /\ invalidCredentialAdmitted' = TRUE
               /\ UNCHANGED invalidRejected
          ELSE /\ invalidRejected' = invalidRejected \cup {c}
               /\ UNCHANGED <<recordState, activeClaimant, activeClaimEpoch,
                              handles, nextClaimEpoch,
                              invalidCredentialAdmitted>>
    /\ UNCHANGED <<windowExpired, releasedClaims, completedClaims,
                   staleHandleMutation, cleanedWhileClaimed,
                   failedRestoreConsumed>>

AdvancePastWindow ==
    /\ ~windowExpired
    /\ windowExpired' = TRUE
    /\ UNCHANGED <<recordState, nextClaimEpoch, activeClaimant,
                   activeClaimEpoch, handles, releasedClaims,
                   completedClaims, invalidRejected,
                   invalidCredentialAdmitted, staleHandleMutation,
                   cleanedWhileClaimed, failedRestoreConsumed>>

ReleaseClaim(c, epoch) ==
    /\ c \in Claimants
    /\ epoch \in handles[c]
    /\ recordState = "Claimed"
    /\ IF c = activeClaimant /\ epoch = activeClaimEpoch
          THEN /\ releasedClaims' = releasedClaims \cup {activeClaimEpoch}
               /\ UNCHANGED staleHandleMutation
          ELSE /\ IgnoreClaimHandleBug
               /\ releasedClaims' = releasedClaims \cup {activeClaimEpoch}
               /\ staleHandleMutation' = TRUE
    /\ recordState' = "Pending"
    /\ activeClaimant' = NoClaim
    /\ activeClaimEpoch' = 0
    /\ UNCHANGED <<windowExpired, nextClaimEpoch, handles,
                   completedClaims, invalidRejected,
                   invalidCredentialAdmitted, cleanedWhileClaimed,
                   failedRestoreConsumed>>

CompleteClaim(c, epoch) ==
    /\ c \in Claimants
    /\ epoch \in handles[c]
    /\ recordState = "Claimed"
    /\ IF c = activeClaimant /\ epoch = activeClaimEpoch
          THEN /\ completedClaims' = completedClaims \cup {activeClaimEpoch}
               /\ UNCHANGED staleHandleMutation
          ELSE /\ IgnoreClaimHandleBug
               /\ completedClaims' = completedClaims \cup {activeClaimEpoch}
               /\ staleHandleMutation' = TRUE
    /\ recordState' = "Consumed"
    /\ activeClaimant' = NoClaim
    /\ activeClaimEpoch' = 0
    /\ UNCHANGED <<windowExpired, nextClaimEpoch, handles,
                   releasedClaims, invalidRejected,
                   invalidCredentialAdmitted, cleanedWhileClaimed,
                   failedRestoreConsumed>>

(* A downstream room/route/baseline failure must release the reservation.   *)
(* It may not turn a failed restore into token consumption; a later retry    *)
(* remains subject to the original reconnection window.                      *)
RestoreFailure(c, epoch) ==
    /\ c = activeClaimant
    /\ epoch = activeClaimEpoch
    /\ recordState = "Claimed"
    /\ IF ConsumeFailedRestoreBug
          THEN /\ recordState' = "Consumed"
               /\ failedRestoreConsumed' = TRUE
               /\ UNCHANGED releasedClaims
          ELSE /\ recordState' = "Pending"
               /\ releasedClaims' = releasedClaims \cup {activeClaimEpoch}
               /\ UNCHANGED failedRestoreConsumed
    /\ activeClaimant' = NoClaim
    /\ activeClaimEpoch' = 0
    /\ UNCHANGED <<windowExpired, nextClaimEpoch, handles,
                   completedClaims, invalidRejected,
                   invalidCredentialAdmitted, staleHandleMutation,
                   cleanedWhileClaimed>>

CleanupExpired ==
    /\ windowExpired
    /\ recordState = "Pending"
    /\ recordState' = "Cleaned"
    /\ UNCHANGED <<windowExpired, nextClaimEpoch, activeClaimant,
                   activeClaimEpoch, handles, releasedClaims,
                   completedClaims, invalidRejected,
                   invalidCredentialAdmitted, staleHandleMutation,
                   cleanedWhileClaimed, failedRestoreConsumed>>

(* Seeded bug: cleanup snapshots/removes a record without excluding its      *)
(* active reservation. Production filters claim.is_none() twice.             *)
CleanupClaimed ==
    /\ CleanupClaimedRecordBug
    /\ windowExpired
    /\ recordState = "Claimed"
    /\ recordState' = "Cleaned"
    /\ activeClaimant' = NoClaim
    /\ activeClaimEpoch' = 0
    /\ cleanedWhileClaimed' = TRUE
    /\ UNCHANGED <<windowExpired, nextClaimEpoch, handles,
                   releasedClaims, completedClaims, invalidRejected,
                   invalidCredentialAdmitted, staleHandleMutation,
                   failedRestoreConsumed>>

Done ==
    /\ recordState \in {"Consumed", "Cleaned"}
    /\ UNCHANGED vars

Next ==
    \/ \E c \in Claimants : Claim(c)
    \/ \E c \in Claimants : AttemptInvalidClaim(c)
    \/ AdvancePastWindow
    \/ \E c \in Claimants, epoch \in ClaimEpochs : ReleaseClaim(c, epoch)
    \/ \E c \in Claimants, epoch \in ClaimEpochs : CompleteClaim(c, epoch)
    \/ \E c \in Claimants, epoch \in ClaimEpochs : RestoreFailure(c, epoch)
    \/ CleanupExpired
    \/ CleanupClaimed
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ recordState \in {"Pending", "Claimed", "Consumed", "Cleaned"}
    /\ windowExpired \in BOOLEAN
    /\ nextClaimEpoch \in 1..(MaxClaims + 1)
    /\ activeClaimant \in Claimants \cup {NoClaim}
    /\ activeClaimEpoch \in 0..MaxClaims
    /\ handles \in [Claimants -> SUBSET ClaimEpochs]
    /\ releasedClaims \subseteq ClaimEpochs
    /\ completedClaims \subseteq ClaimEpochs
    /\ invalidRejected \subseteq InvalidClaimants
    /\ invalidCredentialAdmitted \in BOOLEAN
    /\ staleHandleMutation \in BOOLEAN
    /\ cleanedWhileClaimed \in BOOLEAN
    /\ failedRestoreConsumed \in BOOLEAN

ClaimOwnershipExact ==
    /\ (recordState = "Claimed")
         <=> (activeClaimant \in Claimants /\ activeClaimEpoch \in ClaimEpochs)
    /\ (recordState # "Claimed")
         <=> (activeClaimant = NoClaim /\ activeClaimEpoch = 0)
    /\ (recordState = "Claimed" => activeClaimEpoch \in handles[activeClaimant])
    /\ \A left, right \in Claimants :
         left # right => handles[left] \cap handles[right] = {}

ClaimOutcomeConservation ==
    /\ releasedClaims \cap completedClaims = {}
    /\ Cardinality(completedClaims) <= 1
    /\ (recordState = "Consumed" =>
          Cardinality(completedClaims) = 1 \/ failedRestoreConsumed)
    /\ (recordState = "Cleaned" => windowExpired)

NoInvalidCredentialAdmission ==
    /\ ~invalidCredentialAdmitted
    /\ activeClaimant \notin InvalidClaimants

NoStaleClaimHandleMutation == ~staleHandleMutation

ClaimedRecordProtected == ~cleanedWhileClaimed

FailedRestoreDoesNotConsume == ~failedRestoreConsumed

=============================================================================
