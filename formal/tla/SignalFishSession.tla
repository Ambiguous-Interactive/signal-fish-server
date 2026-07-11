------------------------------ MODULE SignalFishSession ------------------------------
(***************************************************************************************)
(* Formal model of the Signal Fish Protocol v3 per-room session lifecycle:            *)
(* finalize-time plan selection, authoritative per-recipient SessionPlan refreshes,   *)
(* late-join / seat-fill membership publication, and host-failover re-planning.        *)
(*                                                                                    *)
(* The spec mirrors the IMPLEMENTATION, not an idealization. Every operator maps to   *)
(* a concrete function in the Rust code (file references below and in                 *)
(* formal/README.md). Each TLA+ action models one membership-touching event TOGETHER  *)
(* with all of its session side effects as one atomic step (join + late-join          *)
(* emission, departure + departure hook, finalize + plan emission). That is a         *)
(* deliberate SEQUENTIAL ABSTRACTION: the server runs one event's side effects on     *)
(* one task, but distinct events on the same room are NOT serialized against each     *)
(* other — so invariants like HostValid are theorems of this abstraction; the         *)
(* running system holds them between heals and repairs a transient violation at the   *)
(* next membership-touching event (formal/README.md "Atomicity argument").            *)
(*                                                                                    *)
(* Code correspondence (one line each; the full table lives in formal/README.md):     *)
(*   UpgradeLadder / RelayPair       <-> src/server/session_policy.rs                 *)
(*                                       UPGRADE_LADDER, RELAY_FLOOR                  *)
(*   TopologyRank / TransportEnabled <-> topology_rank, transport_enabled             *)
(*   AllSupportOver / ChoosePair     <-> all_support, choose_session_plan             *)
(*   ElectHost                       <-> elect_host                                   *)
(*   HostInvalid                     <-> ActiveSessionPlan::host_invalid              *)
(*   PlanFor                         <-> SessionPlanDecision::plan_for                *)
(*   ReplanResult                    <-> EnhancedGameServer::replan_host_session      *)
(*   Finalize trigger                <-> RoomOperationCoordinator::handle_start_game *)
(*   Finalize emission               <-> EnhancedGameServer::emit_session_plan        *)
(*   DepartureEffects                <-> handle_session_member_departure              *)
(*                                       (run from room_service.rs leave_room)        *)
(*   LateJoinResult                 <-> src/server/signaling.rs                       *)
(*                                      publish_finalized_join_membership             *)
(*   Join fullness gate              <-> src/database/mod.rs add_player_to_room       *)
(*   Depart authority clearing       <-> src/database/mod.rs remove_player_from_room  *)
(*                                                                                    *)
(* Intentionally NOT modeled (rationale in formal/README.md): rate limits, the        *)
(* opaque Signal relay and the NewPeer compatibility shape (transport-only plumbing), *)
(* reconnection                                                                       *)
(* tokens, TURN/ICE minting, multi-room state, storage errors, and                    *)
(* capability-downgrading reconnects (capabilities are per-player constants).         *)
(***************************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    \* The universe of players. Integers stand in for UUIDs: `<` is the UUID
    \* total order used by the glare rule (signaling.rs local_initiates) and
    \* the host-election tie-break.
    PLAYERS,
    \* Per-player negotiated capabilities, fixed for the whole behavior
    \* (connection_manager's NegotiatedProtocol; reconnect-with-downgraded-
    \* capabilities races are out of scope — see formal/README.md):
    \* [version : Nat, transports : SUBSET TransportSet,
    \*  topologies : SUBSET TopologySet]
    Caps,
    \* The desired-topology ceiling for the room's game
    \* (SessionConfig game_topology_mappings / default_topology).
    DESIRED,
    \* SessionConfig.enable_webrtc / enable_direct upgrade gates.
    WEBRTC_ENABLED,
    DIRECT_ENABLED,
    \* Room capacity (Room::max_players).
    MAX_PLAYERS,
    \* Bound on membership-churn actions (join / depart / grant-authority) to
    \* keep the state space finite. Not part of the modeled system.
    CHURN_BUDGET,
    \* Absence markers, assigned MODEL VALUES in the .cfg files (model values
    \* compare unequal to every ordinary value, mirroring Rust's Option::None
    \* and map-entry absence without untyped string/int comparisons):
    NoPlayer,    \* Option<PlayerId>::None
    NoPlan,      \* no ActiveSessionPlan entry for the room
    NoDelivery,  \* this player has never received a SessionPlan
    NoEmission   \* the last action emitted nothing

VARIABLES
    \* Current members in JOIN ORDER (sequence index = joined_at order;
    \* PlayerInfo::connected_at strictly increases per join in this model, so
    \* the elect_host UUID tie-break for equal joined_at is structurally
    \* unreachable here — it is covered by the Rust property tests instead).
    members,
    \* "waiting" or "finalized". The Waiting<->Lobby readiness shuffle is
    \* abstracted away: what matters to session policy is that finalize fires
    \* from a NON-EMPTY room via an explicit StartGame (coordinator
    \* handle_start_game; Room::should_enter_lobby now requires only non-empty,
    \* and max_players is a ceiling, not a required count) and that Finalized is
    \* terminal (no definalization flow exists — the leave path no longer
    \* regresses a partial lobby back to Waiting).
    lobbyState,
    \* The room's designated authority (Room::authority_player), or NoPlayer.
    authority,
    \* The room's sticky ActiveSessionPlan ([topology, transport, host]) or
    \* NoPlan (relay-floor rooms store nothing).
    storedPlan,
    \* Per player, the LAST SessionPlan view delivered to it, or NoDelivery.
    \* Deliberately never cleared: real clients keep their last plan after the
    \* server drops the room's entry after a departure (the relay floor carries
    \* them), so stale client-held plans are honest model state. A later
    \* membership publication replaces them with an explicit relay plan.
    delivered,
    \* What the LAST action emitted: NoEmission, or a record
    \* [kind : {"finalize","replan","latejoin"},
    \*  plans : per-recipient plan views (a function)].
    \* This is auxiliary observation state for emission-time invariants
    \* (fresh peer lists are exact w.r.t. CURRENT members; stale delivered
    \* plans legitimately are not).
    lastEmission,
    \* Remaining churn budget (model-only bound).
    churn

vars == <<members, lobbyState, authority, storedPlan, delivered, lastEmission, churn>>

---------------------------------------------------------------------------------------
(* Domains. *)

TransportSet == {"relay", "direct", "webrtc"}
TopologySet  == {"relay", "host", "mesh"}

\* One peer entry of a per-recipient plan (protocol::SessionPeer). The wire
\* payload also carries player_name and is_authority; both are informational
\* (never drive pairing or validation), so they are not modeled.
PeerEntries == [peer : PLAYERS, initiate : BOOLEAN]

\* A per-recipient plan view (protocol::SessionPlanPayload minus ice_servers
\* and the constant fallback="relay"; ICE minting is orthogonal to the state
\* machine and covered by Rust property tests).
PlanViews ==
    [topology  : TopologySet,
     transport : TransportSet,
     host      : PLAYERS \cup {NoPlayer},
     peers     : SUBSET PeerEntries]

\* The stored room-wide decision (session_policy.rs ActiveSessionPlan).
StoredPlans ==
    [topology  : TopologySet,
     transport : TransportSet,
     host      : PLAYERS \cup {NoPlayer}]

EmissionKinds == {"finalize", "replan", "latejoin"}

---------------------------------------------------------------------------------------
(* Capability profiles (Authenticate negotiation results, Appendix D).
   Referenced from the .cfg files via Caps <- <Scenario>Caps substitution. *)

ProfileV2 ==
    [version |-> 2, transports |-> {"relay"}, topologies |-> {"relay"}]
ProfileV3RelayOnly ==
    [version |-> 3, transports |-> {"relay"}, topologies |-> {"relay"}]
ProfileV3MeshWebRtc ==
    [version |-> 3, transports |-> {"relay", "webrtc"}, topologies |-> {"relay", "mesh"}]
ProfileV3HostWebRtcOnly ==
    [version |-> 3, transports |-> {"relay", "webrtc"}, topologies |-> {"relay", "host"}]
ProfileV3HostDirect ==
    [version |-> 3, transports |-> {"relay", "direct"}, topologies |-> {"relay", "host"}]
ProfileV3Full ==
    [version |-> 3,
     transports |-> {"relay", "direct", "webrtc"},
     topologies |-> {"relay", "host", "mesh"}]

\* Scenario capability assignments (one per .cfg):
\* Mesh scenario: two full members, a mesh-only member, a host-only member and
\* a v3 relay-only member — reaches the mesh rung, the host-under-mesh-ceiling
\* fallback rung, the relay floor, and both "v3 but not session-capable"
\* seat-fill shapes.
MeshScenarioCaps ==
    (1 :> ProfileV3Full) @@ (2 :> ProfileV3Full) @@ (3 :> ProfileV3MeshWebRtc) @@
    (4 :> ProfileV3HostWebRtcOnly) @@ (5 :> ProfileV3RelayOnly)
\* Host scenario: host+webrtc sessions with a v2 member for Appendix-K gating
\* (a v2 seat-filler must observe nothing) and host-failover re-election.
HostScenarioCaps ==
    (1 :> ProfileV3Full) @@ (2 :> ProfileV3Full) @@
    (3 :> ProfileV3HostWebRtcOnly) @@ (4 :> ProfileV2)
\* Host+Direct scenario (webrtc disabled): reaches the host+direct rung and
\* checks that authoritative refreshes preserve its non-WebRTC transport.
HostDirectScenarioCaps ==
    (1 :> ProfileV3HostDirect) @@ (2 :> ProfileV3Full) @@
    (3 :> ProfileV3Full) @@ (4 :> ProfileV3RelayOnly)

---------------------------------------------------------------------------------------
(* Pure selection core — session_policy.rs. *)

MemberSet == {members[i] : i \in DOMAIN members}

SeqMembers(seq) == {seq[i] : i \in DOMAIN seq}

Version(p) == Caps[p].version

\* SessionMember::supports_session — the single capability predicate shared by
\* selection (all_support), host re-election, and plan peer lists: protocol v3
\* plus both axes of the pair.
SupportsSession(p, topology, transport) ==
    /\ Version(p) >= 3
    /\ topology \in Caps[p].topologies
    /\ transport \in Caps[p].transports

\* session_policy.rs all_support: every member supports the pair; an empty
\* room never supports an upgrade.
AllSupportOver(S, topology, transport) ==
    /\ S # {}
    /\ \A p \in S : SupportsSession(p, topology, transport)

\* session_policy.rs UPGRADE_LADDER — richest rung first.
UpgradeLadder ==
    <<[topology |-> "mesh", transport |-> "webrtc"],
      [topology |-> "host", transport |-> "webrtc"],
      [topology |-> "host", transport |-> "direct"]>>

\* session_policy.rs RELAY_FLOOR.
RelayPair == [topology |-> "relay", transport |-> "relay"]
RelayPlan == [topology |-> "relay", transport |-> "relay", host |-> NoPlayer]

\* session_policy.rs topology_rank: Relay < Host < Mesh.
TopologyRank(topology) ==
    CASE topology = "relay" -> 0
      [] topology = "host"  -> 1
      [] topology = "mesh"  -> 2

\* session_policy.rs transport_enabled: relay is the mandatory floor; direct
\* and webrtc are opt-in config gates.
TransportEnabled(transport) ==
    CASE transport = "relay"  -> TRUE
      [] transport = "direct" -> DIRECT_ENABLED
      [] transport = "webrtc" -> WEBRTC_ENABLED

\* session_policy.rs is_valid_pair: the three rungs plus the floor.
IsValidPair(topology, transport) ==
    \/ \E i \in DOMAIN UpgradeLadder :
        UpgradeLadder[i] = [topology |-> topology, transport |-> transport]
    \/ [topology |-> topology, transport |-> transport] = RelayPair

Min(S) == CHOOSE x \in S : \A y \in S : x <= y

\* session_policy.rs choose_session_plan ladder walk: the FIRST rung that fits
\* the desired ceiling, has its transport enabled, and is supported by every
\* member of S; otherwise the relay floor.
ChoosePair(S) ==
    LET fits == {i \in DOMAIN UpgradeLadder :
                    /\ TopologyRank(UpgradeLadder[i].topology) <= TopologyRank(DESIRED)
                    /\ TransportEnabled(UpgradeLadder[i].transport)
                    /\ AllSupportOver(S, UpgradeLadder[i].topology,
                                      UpgradeLadder[i].transport)}
    IN IF fits = {} THEN RelayPair ELSE UpgradeLadder[Min(fits)]

\* session_policy.rs elect_host over a member sequence in join order: the
\* designated authority if seated, else the earliest joiner. The Rust
\* smaller-UUID tie-break for equal joined_at never fires here because join
\* times are strictly ordered in the model (see the `members` comment).
ElectHost(auth, seq) ==
    IF auth \in SeqMembers(seq) THEN auth
    ELSE IF seq = <<>> THEN NoPlayer
    ELSE seq[1]

\* SessionPlanDecision::pairable / ActiveSessionPlan::supported_by: can this
\* player run the plan's sticky pair?
Pairable(plan, p) == SupportsSession(p, plan.topology, plan.transport)

\* ActiveSessionPlan::host_invalid: a host-topology plan whose stored host is
\* absent, or seated but not capable of the session pair.
HostInvalid(plan, S) ==
    /\ plan.topology = "host"
    /\ ~(plan.host \in S /\ Pairable(plan, plan.host))

\* SessionPlanDecision::plan_for(recipient): the per-recipient plan view.
\*  - A non-pairable recipient gets an EMPTY peer list (truthful: the relay
\*    floor is its data path); topology/transport/host are still reported.
\*  - Mesh: every OTHER pairable member, initiate per the glare rule
\*    (signaling.rs local_initiates: the smaller id offers).
\*  - Host: the host answers every pairable client (initiate FALSE); a client
\*    offers to the host only (initiate TRUE). The hostless and host-missing
\*    arms mirror plan_for's defensive branches.
\*  - Relay: explicit authoritative reset with an empty peer list.
PlanFor(plan, mseq, r) ==
    LET S == SeqMembers(mseq)
        peers ==
            IF ~Pairable(plan, r) THEN {}
            ELSE IF plan.topology = "mesh" THEN
                {[peer |-> q, initiate |-> r < q] :
                    q \in {q \in S \ {r} : Pairable(plan, q)}}
            ELSE IF plan.topology = "host" THEN
                IF plan.host = NoPlayer THEN {}  \* degenerate hostless host plan
                ELSE IF r = plan.host THEN
                    {[peer |-> q, initiate |-> FALSE] :
                        q \in {q \in S \ {plan.host} : Pairable(plan, q)}}
                ELSE IF plan.host \in S THEN
                    {[peer |-> plan.host, initiate |-> TRUE]}
                ELSE {}                          \* host not seated: no fabricated pairs
            ELSE {}                              \* relay: explicit no-peer reset
    IN [topology  |-> plan.topology,
        transport |-> plan.transport,
        host      |-> plan.host,
        peers     |-> peers]

---------------------------------------------------------------------------------------
(* Emission helpers. Delivery is v3-gated PER RECIPIENT
   (send_session_plan_to's Appendix-K defense-in-depth gate). *)

V3Members(S) == {p \in S : Version(p) >= 3}

\* Deliver `plan` to every v3 member of mseq (send_session_plans_to_members):
\* the per-recipient plan function for finalize and replan emissions.
PlansForAll(plan, mseq) ==
    [p \in V3Members(SeqMembers(mseq)) |-> PlanFor(plan, mseq, p)]

\* Fold an emission's plans into the persistent per-player delivery state.
MergeDelivered(old, plans) ==
    [p \in PLAYERS |-> IF p \in DOMAIN plans THEN plans[p] ELSE old[p]]

\* Build one authoritative plan publication. A room with no v3 recipients has
\* no v3 session effect, so its observation remains NoEmission.
PlanPublication(retained, plan, mseq, oldDelivered, kind) ==
    LET plans == PlansForAll(plan, mseq)
    IN [stored    |-> retained,
        delivered |-> MergeDelivered(oldDelivered, plans),
        emission  |-> IF DOMAIN plans = {} THEN NoEmission
                     ELSE [kind |-> kind, plans |-> plans]]

\* replan_host_session: capability-filtered re-election (the authority
\* preference passes the SAME capability gate), entry drop + silence when no
\* member qualifies, else stored-host rewrite + a fresh full emission.
\* Returns [stored, delivered, emission] for the caller to assign.
ReplanResult(stored, mseq, auth, oldDelivered) ==
    LET electableSeq  == SelectSeq(mseq, LAMBDA q : Pairable(stored, q))
        electableSet  == SeqMembers(electableSeq)
        electableAuth == IF auth \in electableSet THEN auth ELSE NoPlayer
    IN IF electableSet = {} THEN
           \* Nobody can run the stored session: drop the entry, emit nothing.
           [stored |-> NoPlan, delivered |-> oldDelivered, emission |-> NoEmission]
       ELSE
           LET updated == [stored EXCEPT !.host = ElectHost(electableAuth, electableSeq)]
               plans   == PlansForAll(updated, mseq)
           IN [stored    |-> updated,
               delivered |-> MergeDelivered(oldDelivered, plans),
               emission  |-> [kind |-> "replan", plans |-> plans]]

\* signaling.rs publish_finalized_join_membership, with the POST-join
\* membership mseq:
\*   1. Finalized gate, 2. derive the explicit relay floor when no plan is
\*   stored, 3. heal an invalid host, 4. publish a complete per-recipient plan
\*   to EVERY current v3 member. The latest SessionPlan replaces prior state.
\* In this atomic model the heal arm is structurally unreachable (departures
\* always repaired the host in their own step), but it is modeled because the
\* code path exists; see formal/README.md.
LateJoinResult(mseq) ==
    IF lobbyState # "finalized" THEN
        [stored |-> storedPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE IF storedPlan = NoPlan THEN
        PlanPublication(NoPlan, RelayPlan, mseq, delivered, "latejoin")
    ELSE IF HostInvalid(storedPlan, SeqMembers(mseq)) THEN
        LET result == ReplanResult(storedPlan, mseq, authority, delivered)
        IN IF result.stored = NoPlan
           THEN PlanPublication(NoPlan, RelayPlan, mseq, delivered, "latejoin")
           ELSE result
    ELSE
        PlanPublication(storedPlan, storedPlan, mseq, delivered, "latejoin")

\* handle_session_member_departure with the POST-removal membership mseq and
\* post-removal authority: stored-plan gate -> Finalized gate -> last-member
\* entry drop -> host_invalid => replan; anything else changes nothing
\* (PlayerLeft alone suffices; topology/transport are sticky).
DepartureResult(mseq, auth) ==
    IF storedPlan = NoPlan \/ lobbyState # "finalized" THEN
        [stored |-> storedPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE IF SeqMembers(mseq) = {} THEN
        [stored |-> NoPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE IF ~HostInvalid(storedPlan, SeqMembers(mseq)) THEN
        [stored |-> storedPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE
        ReplanResult(storedPlan, mseq, auth, delivered)

---------------------------------------------------------------------------------------
(* Actions. *)

Init ==
    /\ members = <<>>
    /\ lobbyState = "waiting"
    /\ authority = NoPlayer
    /\ storedPlan = NoPlan
    /\ delivered = [p \in PLAYERS |-> NoDelivery]
    /\ lastEmission = NoEmission
    /\ churn = CHURN_BUDGET

\* A player joins (room_service.rs handle_join_room / reconnect; database
\* add_player_to_room gates ONLY on fullness, so seat-filling a Finalized
\* non-full room is legal). Joining an active session runs the late-join
\* semantics atomically. A reconnect is modeled as depart-then-join of the
\* same player — with one known difference: the real reconnect restores the
\* snapshot PlayerInfo with its ORIGINAL connected_at
\* (reconnection_service.rs), re-entering the host-election order at its old
\* position, while this model re-enters at the end. That changes only WHICH
\* capable member election picks, and no checked invariant or property
\* depends on the elected identity (see formal/README.md).
Join(p) ==
    /\ churn > 0
    /\ p \notin MemberSet
    /\ Len(members) < MAX_PLAYERS
    /\ members' = Append(members, p)
    /\ LET result == LateJoinResult(Append(members, p))
       IN /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
    /\ UNCHANGED <<lobbyState, authority>>
    /\ churn' = churn - 1

\* A player departs (room_service.rs leave_room — the single choke point for
\* explicit LeaveRoom and disconnects). remove_player_from_room clears the
\* authority when the authority departs; the departure hook then runs over
\* the post-removal membership.
Depart(p) ==
    /\ churn > 0
    /\ p \in MemberSet
    /\ LET mseq  == SelectSeq(members, LAMBDA q : q # p)
           auth2 == IF authority = p THEN NoPlayer ELSE authority
           result == DepartureResult(mseq, auth2)
       IN /\ members' = mseq
          /\ authority' = auth2
          /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
    /\ UNCHANGED lobbyState
    /\ churn' = churn - 1

\* A member acquires the room authority (database request_room_authority:
\* grantable only while no authority is held; deliberately NO version gate, so
\* a v2 or relay-only member can hold authority — the replan capability filter
\* must therefore never let authority outrank capability). Authority changes
\* emit no session messages.
GrantAuthority(p) ==
    /\ churn > 0
    /\ p \in MemberSet
    /\ authority = NoPlayer
    /\ authority' = p
    /\ lastEmission' = NoEmission
    /\ UNCHANGED <<members, lobbyState, storedPlan, delivered>>
    /\ churn' = churn - 1

\* Lobby finalization (coordinator handle_start_game -> emit_session_plan):
\* runs the ladder ONCE over the current room, stores a non-relay decision
\* (the relay floor stores nothing), and delivers a per-recipient plan to every
\* v3 member. Relay is an explicit no-peer reset; v2 members receive no plan.
\*
\* TRIGGER (changed): finalization is now driven by an EXPLICIT StartGame, not
\* by the room becoming full. The OLD `Len(members) = MAX_PLAYERS` fullness
\* gate is GONE: Room::should_enter_lobby now returns true for any non-empty
\* Waiting room, and max_players is a CEILING, not a required count.
\* handle_start_game finalizes when the room is not already Finalized, every
\* current player is ready, and the sender is authorized — the room's
\* authority_player if one is set, else ANY member. Minimum 1 player (solo is
\* allowed). The precondition here therefore models only what is observable in
\* this abstraction: the room is non-empty and a finalize is explicitly
\* triggered. Readiness and the identity of the triggering member are
\* abstracted away — the authorization rule (authority-if-set, else any
\* member) places no constraint on WHICH non-empty rooms can finalize, because
\* an authorized starter always exists (AuthorityIsCurrentMember: a set
\* authority is a current member; with no authority, any of the >=1 members
\* qualifies). So the only reachability change versus the old gate is dropping
\* fullness: finalize may now fire below MAX_PLAYERS, on any non-empty Waiting
\* room.
Finalize ==
    /\ lobbyState = "waiting"
    /\ members # <<>>
    /\ lobbyState' = "finalized"
    /\ LET pair == ChoosePair(MemberSet)
           host == IF pair.topology = "host"
                   THEN ElectHost(authority, members)
                   ELSE NoPlayer
           plan == [topology |-> pair.topology,
                    transport |-> pair.transport,
                    host |-> host]
           retained == IF pair = RelayPair THEN NoPlan ELSE plan
           result == PlanPublication(retained, plan, members, delivered, "finalize")
       IN /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
    /\ UNCHANGED <<members, authority, churn>>

\* Explicit termination stutter once the churn budget is exhausted, so the
\* budget's terminal states are self-looping rather than deadlocked — TLC's
\* deadlock checking stays ON and would still catch a real wedged state
\* (a reachable mid-protocol state with no enabled action).
Done ==
    /\ churn = 0
    /\ UNCHANGED vars

Next ==
    \/ \E p \in PLAYERS : Join(p) \/ Depart(p) \/ GrantAuthority(p)
    \/ Finalize
    \/ Done

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------------------
(* INVARIANTS — each named, commented, and mapped to the code it pins.        *)

\* I1. Domains of every variable (plus membership well-formedness: join-order
\* sequence has no duplicates and never exceeds capacity).
TypeOK ==
    /\ \A i \in DOMAIN members : members[i] \in PLAYERS
    /\ Len(members) <= MAX_PLAYERS
    /\ \A i, j \in DOMAIN members : i # j => members[i] # members[j]
    /\ lobbyState \in {"waiting", "finalized"}
    /\ authority \in PLAYERS \cup {NoPlayer}
    /\ storedPlan = NoPlan \/ storedPlan \in StoredPlans
    /\ \A p \in PLAYERS : delivered[p] = NoDelivery \/ delivered[p] \in PlanViews
    /\ churn \in 0..CHURN_BUDGET
    /\ \/ lastEmission = NoEmission
       \/ /\ lastEmission.kind \in EmissionKinds
          /\ DOMAIN lastEmission.plans \subseteq PLAYERS
          /\ \A p \in DOMAIN lastEmission.plans : lastEmission.plans[p] \in PlanViews

\* I2. The authority is always a current member or absent —
\* remove_player_from_room clears it when its holder departs.
AuthorityIsCurrentMember ==
    authority \in MemberSet \cup {NoPlayer}

\* I3. PlanLegality: a stored decision is always one of the three ladder
\* rungs; the relay floor is never stored
\* and illegal pairs (mesh+direct, host+relay, ...) are unrepresentable
\* (session_policy.rs is_valid_pair / the choose_session_plan debug_assert).
PlanLegality ==
    storedPlan # NoPlan =>
        /\ IsValidPair(storedPlan.topology, storedPlan.transport)
        /\ storedPlan.topology # "relay"

\* I4. V2Gating (Appendix K): no SessionPlan is ever delivered to a sub-v3
\* player (send_session_plan_to's gate). Quantified over the PERSISTENT
\* delivery state, so it covers all of history, not just the last action.
V2Gating ==
    \A p \in PLAYERS : delivered[p] # NoDelivery => Version(p) >= 3

\* I5. HostValid: a stored host-topology plan always names a CURRENT member
\* capable of the session pair. In the model, healing (replan_host_session)
\* is atomic with the event that could invalidate the host, so every
\* reachable state satisfies this. It is a theorem OF THE ATOMIC-EVENT
\* ABSTRACTION: the running server does not serialize distinct events, so a
\* concurrent-membership interleaving can transiently violate it there; the
\* widened host_invalid trigger then repairs (or drops) the entry at the next
\* membership-touching event. What the model proves is the sequential half:
\* no SEQUENCE of whole events ever wedges a room (formal/README.md
\* "Atomicity argument").
HostValid ==
    (storedPlan # NoPlan /\ storedPlan.topology = "host") =>
        /\ storedPlan.host \in MemberSet
        /\ Pairable(storedPlan, storedPlan.host)

\* I6. CeilingRespected: the stored topology never exceeds the desired
\* ceiling (choose_session_plan's topology_rank gate; host failover never
\* touches the pair).
CeilingRespected ==
    storedPlan # NoPlan =>
        TopologyRank(storedPlan.topology) <= TopologyRank(DESIRED)

\* I7. PeerCapability: no delivered peer list — INCLUDING stale ones, since
\* capabilities are constant — ever names the recipient itself or a player
\* that cannot run that plan's pair (plan_for's two-sided filter).
PeerCapability ==
    \A p \in PLAYERS :
        delivered[p] # NoDelivery =>
            \A e \in delivered[p].peers :
                /\ e.peer # p
                /\ SupportsSession(e.peer, delivered[p].topology,
                                   delivered[p].transport)

\* I8. MeshPlanExactness: a FRESHLY emitted mesh plan (members are current at
\* emission time — the action is atomic) lists EXACTLY the other
\* session-capable members, each with the glare-rule initiate flag
\* (local_initiates: the smaller id offers); a non-pairable recipient gets
\* exactly the empty list. Stale delivered plans are exempt by construction —
\* mesh departures re-emit nothing (PlayerLeft prunes client-side).
MeshPlanExactness ==
    lastEmission # NoEmission =>
        \A r \in DOMAIN lastEmission.plans :
            LET pv == lastEmission.plans[r]
                capable == {q \in MemberSet \ {r} :
                                SupportsSession(q, pv.topology, pv.transport)}
            IN pv.topology = "mesh" =>
                IF SupportsSession(r, pv.topology, pv.transport)
                THEN pv.peers = {[peer |-> q, initiate |-> r < q] : q \in capable}
                ELSE pv.peers = {}

\* I9. GlareAntisymmetry: across the per-recipient views of one emission,
\* every mutually listed mesh pair has EXACTLY ONE initiator — the smaller id
\* (signaling.rs local_initiates is antisymmetric and irreflexive).
GlareAntisymmetry ==
    lastEmission # NoEmission =>
        \A r1, r2 \in DOMAIN lastEmission.plans :
            (r1 # r2 /\ lastEmission.plans[r1].topology = "mesh") =>
                \A e1 \in lastEmission.plans[r1].peers :
                    \A e2 \in lastEmission.plans[r2].peers :
                        (e1.peer = r2 /\ e2.peer = r1) =>
                            /\ e1.initiate # e2.initiate
                            /\ e1.initiate = (r1 < r2)

\* I10. StarProperty: a freshly emitted host plan is a star around the stored
\* host — the host's peers are exactly the session-capable non-host members
\* (initiate FALSE: the host answers), a capable client's peers are exactly
\* {host} (initiate TRUE: clients offer), and a non-pairable v3 member's
\* peers are exactly {} (plan_for / host_peers_for).
StarProperty ==
    lastEmission # NoEmission =>
        \A r \in DOMAIN lastEmission.plans :
            LET pv == lastEmission.plans[r]
                capable == {q \in MemberSet \ {r} :
                                SupportsSession(q, pv.topology, pv.transport)}
            IN pv.topology = "host" =>
                IF ~SupportsSession(r, pv.topology, pv.transport)
                THEN pv.peers = {}
                ELSE IF r = pv.host
                THEN pv.peers = {[peer |-> q, initiate |-> FALSE] : q \in capable}
                ELSE pv.peers = {[peer |-> pv.host, initiate |-> TRUE]}

\* I11. EmissionMatchesSessionState: every fresh view carries the retained
\* sticky decision, or an explicit relay/relay no-peer reset when no decision
\* is stored. Clients may retain a stale non-relay delivery after a departure
\* drops the entry; the next membership publication replaces it with relay.
EmissionMatchesSessionState ==
    lastEmission # NoEmission =>
        \A r \in DOMAIN lastEmission.plans :
            IF storedPlan = NoPlan
            THEN /\ lastEmission.plans[r].topology = "relay"
                 /\ lastEmission.plans[r].transport = "relay"
                 /\ lastEmission.plans[r].host = NoPlayer
                 /\ lastEmission.plans[r].peers = {}
            ELSE /\ lastEmission.plans[r].topology = storedPlan.topology
                 /\ lastEmission.plans[r].transport = storedPlan.transport
                 /\ lastEmission.plans[r].host = storedPlan.host

\* I12. NoEmissionWithoutQualifier: a replan emission implies a re-elected
\* host that is a current, session-capable member (replan_host_session's
\* no-qualifier arm drops the entry and emits NOTHING), and a replan delivers
\* to exactly the current v3 members.
NoEmissionWithoutQualifier ==
    (lastEmission # NoEmission /\ lastEmission.kind = "replan") =>
        /\ storedPlan # NoPlan
        /\ storedPlan.host \in MemberSet
        /\ Pairable(storedPlan, storedPlan.host)
        /\ DOMAIN lastEmission.plans = V3Members(MemberSet)

\* I13. PublicationCoverage: every plan publication is a complete snapshot for
\* exactly the current v3 membership. In particular, finalized joins/reconnects
\* refresh incumbents as well as the actor; there is no additive delta seam.
PublicationCoverage ==
    lastEmission # NoEmission =>
        DOMAIN lastEmission.plans = V3Members(MemberSet)

\* I14. RelayFloorOnly — listed ONLY by the Floor model, where both upgrade
\* transports are config-disabled (WEBRTC_ENABLED = DIRECT_ENABLED = FALSE).
\* Pins the transport_enabled denial path end-to-end: however capable the
\* members, no ladder rung can clear the config gate, so ChoosePair always
\* returns the relay floor and the room never stores a decision. V3 members
\* receive explicit relay/relay plans while v2 members receive none.
\* Deliberately FALSE in the other models — they
\* exist to reach the rungs — so only SignalFishSession_Floor.cfg checks it.
RelayFloorOnly ==
    /\ storedPlan = NoPlan
    /\ \/ lastEmission = NoEmission
       \/ \A r \in DOMAIN lastEmission.plans :
            /\ lastEmission.plans[r].topology = "relay"
            /\ lastEmission.plans[r].transport = "relay"
            /\ lastEmission.plans[r].host = NoPlayer
            /\ lastEmission.plans[r].peers = {}
    /\ \A p \in PLAYERS :
        delivered[p] = NoDelivery \/
            /\ Version(p) >= 3
            /\ delivered[p].topology = "relay"
            /\ delivered[p].transport = "relay"
            /\ delivered[p].host = NoPlayer
            /\ delivered[p].peers = {}

---------------------------------------------------------------------------------------
(* TEMPORAL (action) PROPERTIES.                                              *)
(* Healing is atomic with its triggering event, so the interesting "liveness" *)
(* facts are single-step consequences — they are stated as action properties  *)
(* ([][...]_vars), which need no fairness assumption. Classic weak-fairness   *)
(* liveness would only constrain the ENVIRONMENT (clients must keep joining   *)
(* or departing), which the server does not and cannot assume; see            *)
(* formal/README.md for the full justification.                               *)

\* P1. StickyPairProperty: once stored, the (topology, transport) pair never
\* changes until the entry is dropped — host failover rewrites ONLY the host
\* (replan_host_session builds `updated` with `..stored`), and the ladder is
\* never re-run mid-session.
StickyPairProperty ==
    [][(storedPlan # NoPlan /\ storedPlan' # NoPlan) =>
        /\ storedPlan'.topology = storedPlan.topology
        /\ storedPlan'.transport = storedPlan.transport]_vars

\* P2. HostDepartureHealedSameStep: any step that removes the stored host of
\* a live host-topology session either re-elects a current capable host (when
\* a qualifier survives) or drops the entry (when none does) — IN THAT SAME
\* STEP. This is the action-property form of "the room is never left wedged
\* on a departed host".
HostDepartureHealedSameStep ==
    [][(/\ storedPlan # NoPlan
        /\ storedPlan.topology = "host"
        /\ storedPlan.host \in MemberSet
        /\ storedPlan.host \notin MemberSet')
       => IF \E q \in MemberSet' : Pairable(storedPlan, q)
          THEN /\ storedPlan' # NoPlan
               /\ storedPlan'.host \in MemberSet'
               /\ Pairable(storedPlan', storedPlan'.host)
          ELSE storedPlan' = NoPlan]_vars


=======================================================================================
