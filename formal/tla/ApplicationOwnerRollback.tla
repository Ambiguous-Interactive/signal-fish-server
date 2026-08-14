------------------ MODULE ApplicationOwnerRollback ------------------
(***************************************************************************)
(* Application-owner rollback composition (#220).                         *)
(*                                                                         *)
(* An initially unowned legacy room admits two unpublished same-app        *)
(* players. The first failed admission owns the one application rollback;  *)
(* the second is a bare durable detach. This reachable bound matters: the  *)
(* model proves that multiple pending entries compose while at most one     *)
(* carries owner provenance, rather than inventing two owner claims.        *)
(*                                                                         *)
(* Maintenance snapshots retry keys before entering the room event lane,   *)
(* then re-reads the live route and rollback provenance inside it. A        *)
(* published same-app admission adopts the pending owner and obtains the    *)
(* only reconnectable baseline; its later failed disconnect creates a bare *)
(* retry plus a credential. Failed admissions remain tokenless. Reconnect  *)
(* may take that retry over, then publish or atomically requeue on rejection.*)
(* A claim waiting outside the lane cannot resurrect a room deleted before *)
(* restore begins.                                                         *)
(*                                                                         *)
(* Mapping to production:                                                  *)
(*   FailAdmission         rollback_unpublished_player_admission           *)
(*   SnapshotCleanup       cleanup_pending_durable_player_detaches keys    *)
(*   CommitCleanup         post-lock pending-map re-read + conditional     *)
(*                         application-owner clear                         *)
(*   DiscardLiveCandidate  post-lock current-relay-generation check        *)
(*   PublishAdmission      mark_pending_room_application_claim_adopted     *)
(*   DisconnectPublishedAdopter register_disconnection_for_reconnect +    *)
(*                         durable membership removal outcome              *)
(*   ClaimReconnect        claim_reconnection_with_identity                *)
(*   TakeOverRetry         ReconnectRestoreState::cleared_pending_detach   *)
(*   RejectReconnect*      reject_claimed_reconnect requeue composition    *)
(*   FinishLateRequeue     seeded post-lane delayed repair handoff         *)
(*   PublishReconnect      successful Reconnected baseline adoption        *)
(*   DeleteRoom            deletion while a claim waits for the room lane  *)
(*                                                                         *)
(* Five independent mutations expose provenance duplication, stale value   *)
(* use, a skipped live-route re-read, a delayed post-lane requeue overtaken *)
(* by cleanup, and deleted-room resurrection. Each has a distinct pinned    *)
(* invariant diagnostic; downstream invariants may be corollaries.         *)
(***************************************************************************)
EXTENDS FiniteSets, Naturals

CONSTANTS
    Players,
    PendingPlayers,
    Adopter,
    Apps,
    OriginalApp,
    OtherApp,
    DuplicateOwnerRollbackBug,
    UseStaleCleanupSnapshotBug,
    SkipLiveRouteRereadBug,
    RequeueAfterLaneBug,
    ResurrectDeletedRoomBug

NoApp == "NoApp"

ASSUME /\ Cardinality(Players) = 3
       /\ Cardinality(PendingPlayers) = 2
       /\ PendingPlayers \subseteq Players
       /\ Adopter \in Players \ PendingPlayers
       /\ Cardinality(Apps) = 2
       /\ OriginalApp \in Apps
       /\ OtherApp \in Apps \ {OriginalApp}
       /\ NoApp \notin Apps
       /\ DuplicateOwnerRollbackBug \in BOOLEAN
       /\ UseStaleCleanupSnapshotBug \in BOOLEAN
       /\ SkipLiveRouteRereadBug \in BOOLEAN
       /\ RequeueAfterLaneBug \in BOOLEAN
       /\ ResurrectDeletedRoomBug \in BOOLEAN

ClearMatching(values, app) ==
    [p \in Players |-> IF values[p] = app THEN NoApp ELSE values[p]]

VARIABLES
    roomExists,
    owner,
    members,
    liveMembers,
    memberApps,
    pendingRetries,
    rollbacks,
    cleanupCandidates,
    cleanupSnapshots,
    reconnectCredentials,
    reconnectClaims,
    reconnectTakeovers,
    restoredMemberships,
    takeoverRollbacks,
    lateRequeues,
    lateRollbacks,
    committedOwner,
    deletedEver,
    staleSnapshotClearedOwner,
    cleanupRemovedLiveReconnect,
    reconnectRepairGap,
    roomResurrected

vars == <<roomExists, owner, members, liveMembers, memberApps,
          pendingRetries, rollbacks, cleanupCandidates, cleanupSnapshots,
          reconnectCredentials, reconnectClaims, reconnectTakeovers,
          restoredMemberships, takeoverRollbacks, lateRequeues, lateRollbacks,
          committedOwner, deletedEver, staleSnapshotClearedOwner,
          cleanupRemovedLiveReconnect, reconnectRepairGap,
          roomResurrected>>

Init ==
    /\ roomExists = TRUE
    /\ owner = NoApp
    /\ members = {}
    /\ liveMembers = {}
    /\ memberApps = [p \in Players |-> NoApp]
    /\ pendingRetries = {}
    /\ rollbacks = [p \in Players |-> NoApp]
    /\ cleanupCandidates = {}
    /\ cleanupSnapshots = [p \in Players |-> NoApp]
    /\ reconnectCredentials = {}
    /\ reconnectClaims = {}
    /\ reconnectTakeovers = {}
    /\ restoredMemberships = {}
    /\ takeoverRollbacks = [p \in Players |-> NoApp]
    /\ lateRequeues = {}
    /\ lateRollbacks = [p \in Players |-> NoApp]
    /\ committedOwner = NoApp
    /\ deletedEver = FALSE
    /\ staleSnapshotClearedOwner = FALSE
    /\ cleanupRemovedLiveReconnect = FALSE
    /\ reconnectRepairGap = FALSE
    /\ roomResurrected = FALSE

(* The first same-app admission claims an unowned legacy room and carries   *)
(* its rollback. Later failures under that owner are detach-only.           *)
FailAdmission(p, app) ==
    /\ p \in PendingPlayers
    /\ app = OriginalApp
    /\ roomExists
    /\ reconnectTakeovers = {}
    /\ p \notin members \cup pendingRetries
    /\ p \notin reconnectCredentials \cup reconnectClaims
    /\ p \notin reconnectTakeovers
    /\ owner \in {NoApp, app}
    /\ owner' = IF owner = NoApp THEN app ELSE owner
    /\ members' = members \cup {p}
    /\ memberApps' = [memberApps EXCEPT ![p] = app]
    /\ pendingRetries' = pendingRetries \cup {p}
    /\ rollbacks' = [rollbacks EXCEPT
          ![p] = IF owner = NoApp \/ DuplicateOwnerRollbackBug
                    THEN app
                    ELSE NoApp]
    /\ UNCHANGED <<roomExists, liveMembers, cleanupCandidates,
                   cleanupSnapshots, reconnectCredentials, reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, lateRequeues,
                   lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner, cleanupRemovedLiveReconnect,
                   reconnectRepairGap, roomResurrected>>

(* Candidate discovery is deliberately outside the room event lane.        *)
SnapshotCleanup(p) ==
    /\ p \in pendingRetries
    /\ p \notin cleanupCandidates
    /\ cleanupCandidates' = cleanupCandidates \cup {p}
    /\ cleanupSnapshots' = [cleanupSnapshots EXCEPT ![p] = rollbacks[p]]
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   pendingRetries, rollbacks, reconnectCredentials,
                   reconnectClaims, reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks,
                   lateRequeues, lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

(* Cleanup is candidate-driven. Inside the lane it re-reads the live route  *)
(* and flattens an absent or bare retry to NoApp before removing the row.   *)
CommitCleanup(p) ==
    /\ p \in cleanupCandidates
    /\ p \notin liveMembers
    /\ roomExists
    /\ reconnectTakeovers = {}
    /\ LET chosenRollback ==
              IF UseStaleCleanupSnapshotBug
                THEN cleanupSnapshots[p]
                ELSE IF p \in pendingRetries THEN rollbacks[p] ELSE NoApp
           clearsOwner ==
              chosenRollback # NoApp /\ owner = chosenRollback
       IN /\ owner' = IF clearsOwner THEN NoApp ELSE owner
          /\ staleSnapshotClearedOwner' =
                IF clearsOwner
                     /\ cleanupSnapshots[p]
                          # (IF p \in pendingRetries THEN rollbacks[p] ELSE NoApp)
                     /\ chosenRollback = cleanupSnapshots[p]
                  THEN TRUE
                  ELSE staleSnapshotClearedOwner
    /\ members' = members \ {p}
    /\ memberApps' = [memberApps EXCEPT ![p] = NoApp]
    /\ pendingRetries' = pendingRetries \ {p}
    /\ rollbacks' = [rollbacks EXCEPT ![p] = NoApp]
    /\ cleanupCandidates' = cleanupCandidates \ {p}
    /\ cleanupSnapshots' = [cleanupSnapshots EXCEPT ![p] = NoApp]
    /\ reconnectRepairGap' = (reconnectRepairGap \/ (p \in lateRequeues))
    /\ UNCHANGED <<roomExists, liveMembers, reconnectCredentials,
                   reconnectClaims, reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks,
                   lateRequeues, lateRollbacks, committedOwner, deletedEver,
                   cleanupRemovedLiveReconnect, roomResurrected>>

(* Seeded route-reread bug: after reconnect publishes and releases the lane, *)
(* an old candidate skips the live-generation guard and removes its row.    *)
CommitCleanupAfterLiveReconnect(p) ==
    /\ SkipLiveRouteRereadBug
    /\ p \in cleanupCandidates \cap liveMembers
    /\ p \notin pendingRetries
    /\ roomExists
    /\ reconnectTakeovers = {}
    /\ members' = members \ {p}
    /\ memberApps' = [memberApps EXCEPT ![p] = NoApp]
    /\ cleanupCandidates' = cleanupCandidates \ {p}
    /\ cleanupSnapshots' = [cleanupSnapshots EXCEPT ![p] = NoApp]
    /\ cleanupRemovedLiveReconnect' = TRUE
    /\ UNCHANGED <<roomExists, owner, liveMembers, pendingRetries,
                   rollbacks, reconnectCredentials, reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, lateRequeues,
                   lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner, reconnectRepairGap,
                   roomResurrected>>

(* A currently routed generation wins over an old maintenance candidate.    *)
(* Reconnect already consumed the pending retry before publishing the route, *)
(* so the inside-lane cleanup branch observes a live player and no retry.    *)
DiscardLiveCandidate(p) ==
    /\ p \in cleanupCandidates \cap liveMembers
    /\ reconnectTakeovers = {}
    /\ pendingRetries' = pendingRetries \ {p}
    /\ rollbacks' = [rollbacks EXCEPT ![p] = NoApp]
    /\ cleanupCandidates' = cleanupCandidates \ {p}
    /\ cleanupSnapshots' = [cleanupSnapshots EXCEPT ![p] = NoApp]
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   reconnectCredentials, reconnectClaims, reconnectTakeovers,
                   restoredMemberships, takeoverRollbacks, lateRequeues,
                   lateRollbacks,
                   committedOwner, deletedEver, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

(* Both failed admissions are reachable before the successful adopter gets  *)
(* the only reconnectable baseline and commits the pending owner claim.     *)
PublishAdmission(app) ==
    /\ app \in Apps
    /\ roomExists
    /\ reconnectTakeovers = {}
    /\ committedOwner = NoApp
    /\ Adopter \notin members
    /\ pendingRetries \cap PendingPlayers = PendingPlayers
    /\ owner \in {NoApp, app}
    /\ owner' = app
    /\ members' = members \cup {Adopter}
    /\ liveMembers' = liveMembers \cup {Adopter}
    /\ memberApps' = [memberApps EXCEPT ![Adopter] = app]
    /\ rollbacks' = ClearMatching(rollbacks, app)
    /\ committedOwner' = app
    /\ UNCHANGED <<roomExists, pendingRetries, cleanupCandidates,
                   cleanupSnapshots, reconnectCredentials, reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, lateRequeues,
                   lateRollbacks, deletedEver,
                   staleSnapshotClearedOwner, cleanupRemovedLiveReconnect,
                   reconnectRepairGap, roomResurrected>>

(* A published member owns a valid credential. Disconnect registration      *)
(* precedes either successful row removal or a failed removal + bare retry.  *)
DisconnectPublishedAdopter(detachFails) ==
    /\ detachFails \in BOOLEAN
    /\ roomExists
    /\ owner \in Apps
    /\ committedOwner = owner
    /\ Adopter \in liveMembers
    /\ Adopter \notin pendingRetries
    /\ Adopter \notin reconnectCredentials \cup reconnectClaims
    /\ Adopter \notin reconnectTakeovers
    /\ reconnectTakeovers = {}
    /\ liveMembers' = liveMembers \ {Adopter}
    /\ members' = IF detachFails THEN members ELSE members \ {Adopter}
    /\ memberApps' = IF detachFails
          THEN memberApps
          ELSE [memberApps EXCEPT ![Adopter] = NoApp]
    /\ pendingRetries' = IF detachFails
          THEN pendingRetries \cup {Adopter}
          ELSE pendingRetries
    /\ rollbacks' = [rollbacks EXCEPT ![Adopter] = NoApp]
    /\ reconnectCredentials' = reconnectCredentials \cup {Adopter}
    /\ UNCHANGED <<roomExists, owner, cleanupCandidates, cleanupSnapshots,
                   reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, lateRequeues,
                   lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner, cleanupRemovedLiveReconnect,
                   reconnectRepairGap, roomResurrected>>

ClaimReconnect(p) ==
    /\ p = Adopter
    /\ p \in reconnectCredentials
    /\ p \notin liveMembers
    /\ p \notin reconnectClaims \cup reconnectTakeovers
    /\ reconnectCredentials' = reconnectCredentials \ {p}
    /\ reconnectClaims' = reconnectClaims \cup {p}
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   pendingRetries, rollbacks, cleanupCandidates,
                   cleanupSnapshots, reconnectTakeovers,
                   restoredMemberships, takeoverRollbacks,
                   lateRequeues, lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

(* Entering the room lane consumes any retry into owned restore state.      *)
TakeOverRetry(p) ==
    /\ p \in reconnectClaims
    /\ reconnectTakeovers = {}
    /\ (roomExists \/ ResurrectDeletedRoomBug)
    /\ roomExists' = TRUE
    /\ roomResurrected' = IF roomExists THEN roomResurrected ELSE TRUE
    /\ owner' = IF roomExists THEN owner ELSE OriginalApp
    /\ members' = members \cup {p}
    /\ memberApps' = [memberApps EXCEPT
          ![p] = IF roomExists /\ owner # NoApp THEN owner ELSE OriginalApp]
    /\ pendingRetries' = pendingRetries \ {p}
    /\ takeoverRollbacks' = [takeoverRollbacks EXCEPT
          ![p] = IF p \in pendingRetries THEN rollbacks[p] ELSE NoApp]
    /\ rollbacks' = [rollbacks EXCEPT ![p] = NoApp]
    /\ reconnectClaims' = reconnectClaims \ {p}
    /\ reconnectTakeovers' = reconnectTakeovers \cup {p}
    /\ restoredMemberships' = IF p \in members
          THEN restoredMemberships \ {p}
          ELSE restoredMemberships \cup {p}
    /\ UNCHANGED <<liveMembers, cleanupCandidates, cleanupSnapshots,
                   reconnectCredentials, lateRequeues, lateRollbacks,
                   committedOwner, deletedEver, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap>>

(* Healthy rejection requeues before releasing the lane. The bug moves the  *)
(* exact repair into a late handoff that cleanup can overtake.               *)
RejectReconnectWithRow(p) ==
    /\ p \in reconnectTakeovers
    /\ IF RequeueAfterLaneBug
          THEN /\ pendingRetries' = pendingRetries
               /\ rollbacks' = rollbacks
               /\ lateRequeues' = lateRequeues \cup {p}
               /\ lateRollbacks' = [lateRollbacks EXCEPT
                       ![p] = takeoverRollbacks[p]]
          ELSE /\ pendingRetries' = pendingRetries \cup {p}
               /\ rollbacks' = [rollbacks EXCEPT
                       ![p] = takeoverRollbacks[p]]
               /\ UNCHANGED <<lateRequeues, lateRollbacks>>
    /\ reconnectCredentials' = reconnectCredentials \cup {p}
    /\ reconnectTakeovers' = reconnectTakeovers \ {p}
    /\ restoredMemberships' = restoredMemberships \ {p}
    /\ takeoverRollbacks' = [takeoverRollbacks EXCEPT ![p] = NoApp]
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   cleanupCandidates, cleanupSnapshots, reconnectClaims,
                   committedOwner, deletedEver, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

RejectReconnectAfterRemoval(p) ==
    /\ p \in reconnectTakeovers
    /\ p \in restoredMemberships
    /\ members' = members \ {p}
    /\ memberApps' = [memberApps EXCEPT ![p] = NoApp]
    /\ LET ownerRepairOwed == takeoverRollbacks[p] # NoApp
       IN /\ pendingRetries' =
                 IF ownerRepairOwed /\ ~RequeueAfterLaneBug
                   THEN pendingRetries \cup {p}
                   ELSE pendingRetries \ {p}
          /\ rollbacks' = [rollbacks EXCEPT
                 ![p] = IF ownerRepairOwed /\ ~RequeueAfterLaneBug
                           THEN takeoverRollbacks[p]
                           ELSE NoApp]
          /\ lateRequeues' =
                 IF ownerRepairOwed /\ RequeueAfterLaneBug
                   THEN lateRequeues \cup {p}
                   ELSE lateRequeues
          /\ lateRollbacks' = [lateRollbacks EXCEPT
                 ![p] = IF ownerRepairOwed /\ RequeueAfterLaneBug
                           THEN takeoverRollbacks[p]
                           ELSE @]
    /\ reconnectCredentials' = reconnectCredentials \cup {p}
    /\ reconnectTakeovers' = reconnectTakeovers \ {p}
    /\ restoredMemberships' = restoredMemberships \ {p}
    /\ takeoverRollbacks' = [takeoverRollbacks EXCEPT ![p] = NoApp]
    /\ UNCHANGED <<roomExists, owner, liveMembers, cleanupCandidates,
                   cleanupSnapshots, reconnectClaims, committedOwner,
                   deletedEver, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

FinishLateRequeue(p) ==
    /\ p \in lateRequeues
    /\ pendingRetries' = pendingRetries \cup {p}
    /\ rollbacks' = [rollbacks EXCEPT ![p] = lateRollbacks[p]]
    /\ lateRequeues' = lateRequeues \ {p}
    /\ lateRollbacks' = [lateRollbacks EXCEPT ![p] = NoApp]
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   cleanupCandidates, cleanupSnapshots,
                   reconnectCredentials, reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, committedOwner,
                   deletedEver, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

PublishReconnect(p) ==
    /\ p \in reconnectTakeovers
    /\ roomExists
    /\ owner \in Apps
    /\ liveMembers' = liveMembers \cup {p}
    /\ rollbacks' = ClearMatching(rollbacks, owner)
    /\ reconnectTakeovers' = reconnectTakeovers \ {p}
    /\ restoredMemberships' = restoredMemberships \ {p}
    /\ takeoverRollbacks' = [takeoverRollbacks EXCEPT ![p] = NoApp]
    /\ committedOwner' = owner
    /\ UNCHANGED <<roomExists, owner, members, memberApps,
                   pendingRetries, cleanupCandidates, cleanupSnapshots,
                   reconnectCredentials, reconnectClaims, lateRequeues,
                   lateRollbacks, deletedEver, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

ReleaseUnrestoredClaim(p) ==
    /\ p \in reconnectClaims
    /\ reconnectCredentials' = reconnectCredentials \cup {p}
    /\ reconnectClaims' = reconnectClaims \ {p}
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   pendingRetries, rollbacks, cleanupCandidates,
                   cleanupSnapshots, reconnectTakeovers,
                   restoredMemberships, takeoverRollbacks,
                   lateRequeues, lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

DeleteRoom ==
    /\ roomExists
    /\ reconnectTakeovers = {}
    /\ roomExists' = FALSE
    /\ owner' = NoApp
    /\ members' = {}
    /\ liveMembers' = {}
    /\ memberApps' = [p \in Players |-> NoApp]
    /\ committedOwner' = NoApp
    /\ deletedEver' = TRUE
    /\ UNCHANGED <<pendingRetries, rollbacks, cleanupCandidates,
                   cleanupSnapshots, reconnectCredentials, reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, lateRequeues,
                   lateRollbacks, staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

CleanDeletedRetry(p) ==
    /\ ~roomExists
    /\ p \in pendingRetries \cup cleanupCandidates
    /\ pendingRetries' = pendingRetries \ {p}
    /\ rollbacks' = [rollbacks EXCEPT ![p] = NoApp]
    /\ cleanupCandidates' = cleanupCandidates \ {p}
    /\ cleanupSnapshots' = [cleanupSnapshots EXCEPT ![p] = NoApp]
    /\ UNCHANGED <<roomExists, owner, members, liveMembers, memberApps,
                   reconnectCredentials, reconnectClaims,
                   reconnectTakeovers, restoredMemberships,
                   takeoverRollbacks, lateRequeues,
                   lateRollbacks, committedOwner, deletedEver,
                   staleSnapshotClearedOwner,
                   cleanupRemovedLiveReconnect, reconnectRepairGap,
                   roomResurrected>>

Done ==
    /\ pendingRetries = {}
    /\ reconnectClaims = {}
    /\ reconnectTakeovers = {}
    /\ UNCHANGED vars

Next ==
    \/ \E p \in Players, app \in Apps : FailAdmission(p, app)
    \/ \E p \in Players : SnapshotCleanup(p)
    \/ \E p \in Players : CommitCleanup(p)
    \/ \E p \in Players : CommitCleanupAfterLiveReconnect(p)
    \/ \E p \in Players : DiscardLiveCandidate(p)
    \/ \E app \in Apps : PublishAdmission(app)
    \/ \E detachFails \in BOOLEAN : DisconnectPublishedAdopter(detachFails)
    \/ \E p \in Players : ClaimReconnect(p)
    \/ \E p \in Players : TakeOverRetry(p)
    \/ \E p \in Players : RejectReconnectWithRow(p)
    \/ \E p \in Players : RejectReconnectAfterRemoval(p)
    \/ \E p \in Players : FinishLateRequeue(p)
    \/ \E p \in Players : PublishReconnect(p)
    \/ \E p \in Players : ReleaseUnrestoredClaim(p)
    \/ DeleteRoom
    \/ \E p \in Players : CleanDeletedRetry(p)
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ roomExists \in BOOLEAN
    /\ owner \in Apps \cup {NoApp}
    /\ members \subseteq Players
    /\ liveMembers \subseteq Players
    /\ memberApps \in [Players -> Apps \cup {NoApp}]
    /\ pendingRetries \subseteq Players
    /\ rollbacks \in [Players -> Apps \cup {NoApp}]
    /\ cleanupCandidates \subseteq Players
    /\ cleanupSnapshots \in [Players -> Apps \cup {NoApp}]
    /\ reconnectCredentials \subseteq Players
    /\ reconnectClaims \subseteq Players
    /\ reconnectTakeovers \subseteq Players
    /\ restoredMemberships \subseteq Players
    /\ takeoverRollbacks \in [Players -> Apps \cup {NoApp}]
    /\ lateRequeues \subseteq Players
    /\ lateRollbacks \in [Players -> Apps \cup {NoApp}]
    /\ committedOwner \in Apps \cup {NoApp}
    /\ deletedEver \in BOOLEAN
    /\ staleSnapshotClearedOwner \in BOOLEAN
    /\ cleanupRemovedLiveReconnect \in BOOLEAN
    /\ reconnectRepairGap \in BOOLEAN
    /\ roomResurrected \in BOOLEAN

OwnerRollbackUnique ==
    Cardinality({p \in pendingRetries : rollbacks[p] # NoApp})
      + Cardinality(
          {p \in reconnectTakeovers : takeoverRollbacks[p] # NoApp}
        )
      + Cardinality({p \in lateRequeues : lateRollbacks[p] # NoApp})
      <= 1

RollbackPlacementExact ==
    /\ \A p \in Players :
          (rollbacks[p] # NoApp => p \in pendingRetries)
    /\ \A p \in Players :
          (takeoverRollbacks[p] # NoApp => p \in reconnectTakeovers)
    /\ \A p \in Players :
          (lateRollbacks[p] # NoApp => p \in lateRequeues)
    /\ pendingRetries \cap reconnectTakeovers = {}
    /\ reconnectClaims \cap reconnectTakeovers = {}
    /\ reconnectCredentials \cap reconnectClaims = {}
    /\ reconnectCredentials \cap reconnectTakeovers = {}
    /\ restoredMemberships \subseteq reconnectTakeovers

CleanupUsesFreshRollbackProvenance == ~staleSnapshotClearedOwner

CleanupCannotRemoveLiveReconnect == ~cleanupRemovedLiveReconnect

RejectedReconnectRequeuesBeforeLaneRelease == ~reconnectRepairGap

DeletedRoomCannotBeResurrected == ~roomResurrected

LiveMembershipExact ==
    /\ liveMembers \subseteq members
    /\ (liveMembers # {} => roomExists /\ committedOwner = owner)
    /\ \A p \in liveMembers : memberApps[p] = owner

CommittedOwnerStable ==
    committedOwner # NoApp => roomExists /\ owner = committedOwner

UncommittedOwnerHasRollback ==
    roomExists /\ committedOwner = NoApp /\ owner # NoApp
      => \/ \E p \in pendingRetries : rollbacks[p] = owner
         \/ \E p \in reconnectTakeovers : takeoverRollbacks[p] = owner
         \/ \E p \in lateRequeues : lateRollbacks[p] = owner

DeletedRoomIsTerminal ==
    /\ (~roomExists => owner = NoApp /\ members = {} /\ liveMembers = {})
    /\ (deletedEver => ~roomExists)

=============================================================================
