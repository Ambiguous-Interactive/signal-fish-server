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
(* deliberate SEQUENTIAL ABSTRACTION: the process-local server transfers a shared    *)
(* per-room mutation guard into an owned FIFO job, so a successor cannot mutate the   *)
(* same room before its predecessor reaches a terminal result. Room-scoped routing    *)
(* gates likewise preserve exact publication/baseline ordering without excluding     *)
(* unrelated rooms (formal/README.md "Atomicity argument").                           *)
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
(*   CapabilityReconnect             <-> reconnect publication in                     *)
(*                                       server/reconnection_service.rs               *)
(*   Join fullness gate              <-> src/database/mod.rs add_player_to_room       *)
(*   Depart authority clearing       <-> src/database/mod.rs remove_player_from_room  *)
(*                                                                                    *)
(* Intentionally NOT modeled (rationale in formal/README.md): rate limits, the        *)
(* opaque Signal relay, its wire-generation UUID, and the NewPeer compatibility     *)
(* shape (transport-only plumbing; plan generations do not affect selection),       *)
(* reconnection tokens, TURN/ICE minting, multi-room state, and storage errors.       *)
(***************************************************************************************)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    \* The universe of players. Integers stand in for UUIDs: `<` is the UUID
    \* total order used by the glare rule (signaling.rs local_initiates) and
    \* the host-election tie-break.
    PLAYERS,
    \* Initial per-player negotiated capabilities
    \* (connection_manager's NegotiatedProtocol):
    \* [version : Nat, transports : SUBSET TransportSet,
    \*  topologies : SUBSET TopologySet]
    InitialCaps,
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
    \* Whether this configuration explores a sticky host disconnecting and
    \* later reconnecting with a smaller capability set.
    CAPABILITY_RECONNECT_ENABLED,
    \* Independently seeded bugs for the reconnect refinement. Each has one
    \* expected-failure configuration pinned to its exact invariant.
    SkipCapabilityPublicationBug,
    UseStaleReconnectCapabilitiesBug,
    ReorderReconnectBug,
    OverwriteSuccessorAuthorityBug,
    BypassReconnectV2GateBug,
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
    \* Current negotiated capabilities. They begin at InitialCaps and may
    \* change only when CapabilityReconnect models a fresh authenticated
    \* socket replacing a restored member's old connection.
    caps,
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
    \* Per player, the LAST SessionPlan delivery observation, or NoDelivery.
    \* Each observation retains the physical connection generation and protocol
    \* version that received it. It is deliberately never cleared: a disconnected
    \* v3 generation may retain a historical plan while a fresh v2 generation of
    \* the same player id correctly receives no v3-only SessionPlan.
    delivered,
    \* Physical socket generation for each logical player id. Ordinary joins use
    \* generation zero; a successful reconnect transfers a fresh socket to the
    \* restored id and advances exactly that player's generation.
    connectionGeneration,
    \* What the LAST action emitted: NoEmission, or a record
    \* [kind : {"finalize","replan","latejoin"},
    \*  plans : per-recipient plan views (a function)].
    \* This is auxiliary observation state for emission-time invariants
    \* (fresh peer lists are exact w.r.t. CURRENT members; stale delivered
    \* plans legitimately are not).
    lastEmission,
    \* Remaining churn budget (model-only bound).
    churn,
    \* One pending production-shaped disconnect/reconnect refinement. The
    \* saved order is the restored PlayerInfo.connected_at order.
    pendingReconnect,
    pendingReconnectPredecessors,
    pendingReconnectWasAuthority,
    \* Observation state for semantic postconditions of the most recent
    \* capability reconnect. Expected values are captured independently of
    \* the seeded implementation branches.
    lastCapabilityReconnect,
    lastReconnectPlayer,
    expectedReconnectOrder,
    expectedReconnectAuthority

vars == <<members, caps, lobbyState, authority, storedPlan, delivered,
          connectionGeneration,
          lastEmission, churn, pendingReconnect, pendingReconnectPredecessors,
          pendingReconnectWasAuthority, lastCapabilityReconnect,
          lastReconnectPlayer, expectedReconnectOrder,
          expectedReconnectAuthority>>

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

DeliveryObservations ==
    [plan       : PlanViews,
     generation : Nat,
     version    : Nat]

\* The stored room-wide decision (session_policy.rs ActiveSessionPlan).
StoredPlans ==
    [topology  : TopologySet,
     transport : TransportSet,
     host      : PLAYERS \cup {NoPlayer}]

EmissionKinds == {"finalize", "replan", "latejoin"}

---------------------------------------------------------------------------------------
(* Capability profiles (Authenticate negotiation results, Appendix D).
   Referenced from the .cfg files via InitialCaps <- <Scenario>Caps. *)

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

CapabilityProfiles ==
    {ProfileV2, ProfileV3RelayOnly, ProfileV3MeshWebRtc,
     ProfileV3HostWebRtcOnly, ProfileV3HostDirect, ProfileV3Full}

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
\* Focused reconnect refinements: every member begins fully capable. The host
\* scenario returns its elected host as v3 relay-only; the mesh scenario returns
\* any member on a fresh v2 connection generation.
ReconnectScenarioCaps ==
    (1 :> ProfileV3Full) @@ (2 :> ProfileV3Full) @@ (3 :> ProfileV3Full)

---------------------------------------------------------------------------------------
(* Pure selection core — session_policy.rs. *)

MemberSet == {members[i] : i \in DOMAIN members}

SeqMembers(seq) == {seq[i] : i \in DOMAIN seq}

\* Reinsert p at its saved connected_at position among surviving snapshot
\* members. Members that joined while p was disconnected have later timestamps
\* and remain after that restored prefix, in their current relative order.
MembersBefore(seq, p) ==
    {q \in SeqMembers(seq) :
        \E i, j \in DOMAIN seq : seq[i] = q /\ seq[j] = p /\ i < j}

RestoreReconnectOrder(current, predecessors, p) ==
    SelectSeq(current, LAMBDA q : q \in predecessors)
        \o <<p>>
        \o SelectSeq(current, LAMBDA q : q \notin predecessors)

Version(capMap, p) == capMap[p].version

\* SessionMember::supports_session — the single capability predicate shared by
\* selection (all_support), host re-election, and plan peer lists: protocol v3
\* plus both axes of the pair.
SupportsSession(capMap, p, topology, transport) ==
    /\ Version(capMap, p) >= 3
    /\ topology \in capMap[p].topologies
    /\ transport \in capMap[p].transports

\* session_policy.rs all_support: every member supports the pair; an empty
\* room never supports an upgrade.
AllSupportOver(capMap, S, topology, transport) ==
    /\ S # {}
    /\ \A p \in S : SupportsSession(capMap, p, topology, transport)

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
ChoosePair(capMap, S) ==
    LET fits == {i \in DOMAIN UpgradeLadder :
                    /\ TopologyRank(UpgradeLadder[i].topology) <= TopologyRank(DESIRED)
                    /\ TransportEnabled(UpgradeLadder[i].transport)
                    /\ AllSupportOver(capMap, S, UpgradeLadder[i].topology,
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
Pairable(capMap, plan, p) ==
    SupportsSession(capMap, p, plan.topology, plan.transport)

\* ActiveSessionPlan::host_invalid: a host-topology plan whose stored host is
\* absent, or seated but not capable of the session pair.
HostInvalid(capMap, plan, S) ==
    /\ plan.topology = "host"
    /\ ~(plan.host \in S /\ Pairable(capMap, plan, plan.host))

\* SessionPlanDecision::plan_for(recipient): the per-recipient plan view.
\*  - A non-pairable recipient gets an EMPTY peer list (truthful: the relay
\*    floor is its data path); topology/transport/host are still reported.
\*  - Mesh: every OTHER pairable member, initiate per the glare rule
\*    (signaling.rs local_initiates: the smaller id offers).
\*  - Host: the host answers every pairable client (initiate FALSE); a client
\*    offers to the host only (initiate TRUE). The hostless and host-missing
\*    arms mirror plan_for's defensive branches.
\*  - Relay: explicit authoritative reset with an empty peer list.
PlanFor(capMap, plan, mseq, r) ==
    LET S == SeqMembers(mseq)
        peers ==
            IF ~Pairable(capMap, plan, r) THEN {}
            ELSE IF plan.topology = "mesh" THEN
                {[peer |-> q, initiate |-> r < q] :
                    q \in {q \in S \ {r} : Pairable(capMap, plan, q)}}
            ELSE IF plan.topology = "host" THEN
                IF plan.host = NoPlayer THEN {}  \* degenerate hostless host plan
                ELSE IF r = plan.host THEN
                    {[peer |-> q, initiate |-> FALSE] :
                        q \in {q \in S \ {plan.host} : Pairable(capMap, plan, q)}}
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

V3Members(capMap, S) == {p \in S : Version(capMap, p) >= 3}

\* Deliver `plan` to every v3 member of mseq (send_session_plans_to_members):
\* the per-recipient plan function for finalize and replan emissions.
PlansForAll(capMap, plan, mseq) ==
    [p \in V3Members(capMap, SeqMembers(mseq)) |->
        PlanFor(capMap, plan, mseq, p)]

\* Fold an emission's plans into the persistent per-player delivery state.
MergeDelivered(old, plans, capMap, generations) ==
    [p \in PLAYERS |->
        IF p \in DOMAIN plans
        THEN [plan       |-> plans[p],
              generation |-> generations[p],
              version    |-> Version(capMap, p)]
        ELSE old[p]]

\* Build one authoritative plan publication. A room with no v3 recipients has
\* no v3 session effect, so its observation remains NoEmission.
PlanPublication(capMap, generations, retained, plan, mseq, oldDelivered, kind) ==
    LET plans == PlansForAll(capMap, plan, mseq)
    IN [stored    |-> retained,
        delivered |-> MergeDelivered(oldDelivered, plans, capMap, generations),
        emission  |-> IF DOMAIN plans = {} THEN NoEmission
                     ELSE [kind |-> kind, plans |-> plans]]

\* Apply the recipient's CURRENT negotiated-version gate independently of the
\* capability snapshot used to resolve peer construction. Production builds a
\* SessionPlan from the membership decision, then build_session_plan_message
\* separately rejects a recipient whose live connection is not v3. Keeping
\* those inputs distinct makes the stale-capability and wire-gate bug seeds
\* independent: stale resolution can fabricate a v2 actor as a peer without
\* also delivering the v3-only frame to that actor.
GatePublication(result, gateCaps, generations, mseq, oldDelivered) ==
    IF result.emission = NoEmission THEN result
    ELSE
        LET recipients == V3Members(gateCaps, SeqMembers(mseq))
            plans == [p \in DOMAIN result.emission.plans \cap recipients |->
                         result.emission.plans[p]]
        IN [stored    |-> result.stored,
            delivered |-> MergeDelivered(oldDelivered, plans, gateCaps, generations),
            emission  |-> IF DOMAIN plans = {} THEN NoEmission
                         ELSE [kind |-> result.emission.kind, plans |-> plans]]

\* replan_host_session: capability-filtered re-election (the authority
\* preference passes the SAME capability gate), entry drop + silence when no
\* member qualifies, else stored-host rewrite + a fresh full emission.
\* Returns [stored, delivered, emission] for the caller to assign.
ReplanResult(capMap, generations, stored, mseq, auth, oldDelivered) ==
    LET electableSeq  == SelectSeq(mseq, LAMBDA q : Pairable(capMap, stored, q))
        electableSet  == SeqMembers(electableSeq)
        electableAuth == IF auth \in electableSet THEN auth ELSE NoPlayer
    IN IF electableSet = {} THEN
           \* Nobody can run the stored session: drop the entry, emit nothing.
           [stored |-> NoPlan, delivered |-> oldDelivered, emission |-> NoEmission]
       ELSE
           LET updated == [stored EXCEPT !.host = ElectHost(electableAuth, electableSeq)]
               plans   == PlansForAll(capMap, updated, mseq)
           IN [stored    |-> updated,
               delivered |-> MergeDelivered(oldDelivered, plans, capMap, generations),
               emission  |-> [kind |-> "replan", plans |-> plans]]

\* signaling.rs publish_finalized_join_membership, with the POST-join
\* membership mseq:
\*   1. Finalized gate, 2. derive the explicit relay floor when no plan is
\*   stored, 3. heal an invalid host, 4. publish a complete per-recipient plan
\*   to EVERY current v3 member. The latest SessionPlan replaces prior state.
\* Ordinary serialized departures repair the host in their own step. This
\* defensive heal arm remains reachable from a finalized join if the model is
\* initialized or extended with legacy/stale stored state; see formal/README.md.
LateJoinResult(capMap, generations, mseq, auth) ==
    IF lobbyState # "finalized" THEN
        [stored |-> storedPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE IF storedPlan = NoPlan THEN
        PlanPublication(capMap, generations, NoPlan, RelayPlan, mseq, delivered, "latejoin")
    ELSE IF HostInvalid(capMap, storedPlan, SeqMembers(mseq)) THEN
        LET result == ReplanResult(capMap, generations, storedPlan, mseq, auth, delivered)
        IN IF result.stored = NoPlan
           THEN PlanPublication(capMap, generations, NoPlan, RelayPlan, mseq, delivered, "latejoin")
           ELSE result
    ELSE
        PlanPublication(capMap, generations, storedPlan, storedPlan, mseq, delivered, "latejoin")

\* handle_session_member_departure with the POST-removal membership mseq and
\* post-removal authority: stored-plan gate -> Finalized gate -> last-member
\* entry drop -> host_invalid => replan; anything else changes nothing
\* (PlayerLeft alone suffices; topology/transport are sticky).
DepartureResult(capMap, mseq, auth) ==
    IF storedPlan = NoPlan \/ lobbyState # "finalized" THEN
        [stored |-> storedPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE IF SeqMembers(mseq) = {} THEN
        [stored |-> NoPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE IF ~HostInvalid(capMap, storedPlan, SeqMembers(mseq)) THEN
        [stored |-> storedPlan, delivered |-> delivered, emission |-> NoEmission]
    ELSE
        ReplanResult(capMap, connectionGeneration, storedPlan, mseq, auth, delivered)

---------------------------------------------------------------------------------------
(* Actions. *)

Init ==
    /\ members = <<>>
    /\ caps = InitialCaps
    /\ lobbyState = "waiting"
    /\ authority = NoPlayer
    /\ storedPlan = NoPlan
    /\ delivered = [p \in PLAYERS |-> NoDelivery]
    /\ connectionGeneration = [p \in PLAYERS |-> 0]
    /\ lastEmission = NoEmission
    /\ churn = CHURN_BUDGET
    /\ pendingReconnect = NoPlayer
    /\ pendingReconnectPredecessors = {}
    /\ pendingReconnectWasAuthority = FALSE
    /\ lastCapabilityReconnect = FALSE
    /\ lastReconnectPlayer = NoPlayer
    /\ expectedReconnectOrder = <<>>
    /\ expectedReconnectAuthority = NoPlayer

\* A player joins (room_service.rs handle_join_room; database
\* add_player_to_room gates ONLY on fullness, so seat-filling a Finalized
\* non-full room is legal). Joining an active session runs the late-join
\* semantics atomically. Reconnect capability replacement has its own action
\* below because production preserves the restored member's original
\* connected_at instead of appending it as a new join.
Join(p) ==
    /\ churn > 0
    /\ p # pendingReconnect
    /\ p \notin MemberSet
    /\ Len(members) < MAX_PLAYERS
    /\ members' = Append(members, p)
    /\ LET result == LateJoinResult(caps, connectionGeneration,
                                    Append(members, p), authority)
       IN /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
    /\ lastCapabilityReconnect' = FALSE
    /\ UNCHANGED <<caps, connectionGeneration, lobbyState, authority, pendingReconnect,
                   pendingReconnectPredecessors, pendingReconnectWasAuthority,
                   lastReconnectPlayer, expectedReconnectOrder,
                   expectedReconnectAuthority>>
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
           result == DepartureResult(caps, mseq, auth2)
       IN /\ members' = mseq
          /\ authority' = auth2
          /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
    /\ pendingReconnectPredecessors' = pendingReconnectPredecessors \ {p}
    /\ lastCapabilityReconnect' = FALSE
    /\ UNCHANGED <<caps, connectionGeneration, lobbyState, pendingReconnect,
                   pendingReconnectWasAuthority,
                   lastReconnectPlayer, expectedReconnectOrder,
                   expectedReconnectAuthority>>
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
    /\ lastCapabilityReconnect' = FALSE
    /\ UNCHANGED <<members, caps, connectionGeneration, lobbyState, storedPlan, delivered,
                   pendingReconnect, pendingReconnectPredecessors,
                   pendingReconnectWasAuthority, lastReconnectPlayer,
                   expectedReconnectOrder, expectedReconnectAuthority>>
    /\ churn' = churn - 1

\* Begin an ordinary production reconnect history. P94's host scenario selects
\* the sticky host and composes departure failover. P95's mesh scenario permits
\* any capable member: mesh has no host parameter to repair, so PlayerLeft is
\* sufficient until reconnect publishes the fresh exact-membership snapshot.
\* Both retain the saved connected_at order and authority-restoration intent.
BeginCapabilityDisconnect(p) ==
    /\ CAPABILITY_RECONNECT_ENABLED
    /\ churn > 0
    /\ pendingReconnect = NoPlayer
    /\ lobbyState = "finalized"
    /\ storedPlan # NoPlan
    /\ (IF DESIRED = "mesh"
        THEN storedPlan.topology = "mesh"
        ELSE /\ storedPlan.topology = "host"
             /\ storedPlan.host = p)
    /\ p \in MemberSet
    /\ Pairable(caps, storedPlan, p)
    /\ LET mseq == SelectSeq(members, LAMBDA q : q # p)
           wasAuthority == authority = p
           auth2 == IF wasAuthority THEN NoPlayer ELSE authority
           result == DepartureResult(caps, mseq, auth2)
       IN /\ members' = mseq
          /\ authority' = auth2
          /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
          /\ pendingReconnect' = p
          /\ pendingReconnectPredecessors' = MembersBefore(members, p)
          /\ pendingReconnectWasAuthority' = wasAuthority
    /\ lastCapabilityReconnect' = FALSE
    /\ UNCHANGED <<caps, connectionGeneration, lobbyState, lastReconnectPlayer,
                   expectedReconnectOrder, expectedReconnectAuthority>>
    /\ churn' = churn - 1

\* A fresh authenticated reconnect socket advertises the scenario's smaller
\* profile: v3 relay-only for P94 host, v2 relay-only for P95 mesh. Production
\* advances the physical connection generation, restores the saved PlayerInfo
\* in its original order, attempts
\* authority restoration only when the disconnected player formerly held it
\* AND the role is still vacant, transfers the fresh NegotiatedProtocol, and
\* publishes the finalized membership against that fresh profile.
CapabilityReconnect(p) ==
    /\ CAPABILITY_RECONNECT_ENABLED
    /\ churn > 0
    /\ lobbyState = "finalized"
    /\ pendingReconnect = p
    /\ p \notin MemberSet
    /\ Len(members) < MAX_PLAYERS
    /\ LET freshProfile == IF DESIRED = "mesh" THEN ProfileV2 ELSE ProfileV3RelayOnly
           caps2 == [caps EXCEPT ![p] = freshProfile]
           generations2 == [connectionGeneration EXCEPT ![p] = @ + 1]
           expectedOrder ==
               RestoreReconnectOrder(members, pendingReconnectPredecessors, p)
           members2 == IF ReorderReconnectBug
                       THEN Append(members, p)
                       ELSE expectedOrder
           expectedAuthority ==
               IF pendingReconnectWasAuthority /\ authority = NoPlayer
               THEN p
               ELSE authority
           authority2 ==
               IF OverwriteSuccessorAuthorityBug /\ pendingReconnectWasAuthority
               THEN p
               ELSE expectedAuthority
           publicationCaps ==
               IF UseStaleReconnectCapabilitiesBug THEN caps ELSE caps2
           publicationResult ==
               LateJoinResult(publicationCaps, generations2, members2, authority2)
           baseResult == IF SkipCapabilityPublicationBug
                         THEN [stored |-> storedPlan,
                               delivered |-> delivered,
                               emission |-> NoEmission]
                         ELSE GatePublication(publicationResult, caps2, generations2,
                                              members2, delivered)
           \* Independent P95 seed: the membership decision uses fresh v2
           \* capabilities, but the final per-recipient Appendix-K gate is
           \* bypassed for the actor's new physical generation.
           bugPlans ==
               IF baseResult.emission = NoEmission
               THEN [q \in {} |-> q]
               ELSE [q \in DOMAIN baseResult.emission.plans \cup {p} |->
                         IF q = p
                         THEN PlanFor(caps2,
                                      IF baseResult.stored = NoPlan
                                      THEN RelayPlan
                                      ELSE baseResult.stored,
                                      members2, p)
                         ELSE baseResult.emission.plans[q]]
           result ==
               IF BypassReconnectV2GateBug /\ DESIRED = "mesh" /\
                  baseResult.emission # NoEmission
               THEN [stored |-> baseResult.stored,
                     delivered |-> MergeDelivered(baseResult.delivered, bugPlans,
                                                   caps2, generations2),
                     emission |-> [kind |-> baseResult.emission.kind,
                                    plans |-> bugPlans]]
               ELSE baseResult
       IN /\ caps' = caps2
          /\ connectionGeneration' = generations2
          /\ members' = members2
          /\ authority' = authority2
          /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
          /\ pendingReconnect' = NoPlayer
          /\ pendingReconnectPredecessors' = {}
          /\ pendingReconnectWasAuthority' = FALSE
          /\ lastCapabilityReconnect' = TRUE
          /\ lastReconnectPlayer' = p
          /\ expectedReconnectOrder' = expectedOrder
          /\ expectedReconnectAuthority' = expectedAuthority
    /\ UNCHANGED lobbyState
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
    /\ pendingReconnect = NoPlayer
    /\ members # <<>>
    /\ lobbyState' = "finalized"
    /\ LET pair == ChoosePair(caps, MemberSet)
           host == IF pair.topology = "host"
                   THEN ElectHost(authority, members)
                   ELSE NoPlayer
           plan == [topology |-> pair.topology,
                    transport |-> pair.transport,
                    host |-> host]
           retained == IF pair = RelayPair THEN NoPlan ELSE plan
           result == PlanPublication(caps, connectionGeneration, retained, plan,
                                     members, delivered, "finalize")
       IN /\ storedPlan' = result.stored
          /\ delivered' = result.delivered
          /\ lastEmission' = result.emission
    /\ lastCapabilityReconnect' = FALSE
    /\ UNCHANGED <<members, caps, connectionGeneration, authority, churn, pendingReconnect,
                   pendingReconnectPredecessors, pendingReconnectWasAuthority,
                   lastReconnectPlayer, expectedReconnectOrder,
                   expectedReconnectAuthority>>

\* Explicit termination stutter once the churn budget is exhausted, so the
\* budget's terminal states are self-looping rather than deadlocked — TLC's
\* deadlock checking stays ON and would still catch a real wedged state
\* (a reachable mid-protocol state with no enabled action).
Done ==
    /\ churn = 0
    /\ UNCHANGED vars

Next ==
    \/ \E p \in PLAYERS : Join(p) \/ Depart(p) \/ GrantAuthority(p)
    \/ \E p \in PLAYERS : BeginCapabilityDisconnect(p)
    \/ \E p \in PLAYERS : CapabilityReconnect(p)
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
    /\ caps \in [PLAYERS -> CapabilityProfiles]
    /\ lobbyState \in {"waiting", "finalized"}
    /\ authority \in PLAYERS \cup {NoPlayer}
    /\ storedPlan = NoPlan \/ storedPlan \in StoredPlans
    /\ connectionGeneration \in [PLAYERS -> Nat]
    /\ \A p \in PLAYERS :
           delivered[p] = NoDelivery \/
               /\ delivered[p] \in DeliveryObservations
               /\ delivered[p].generation <= connectionGeneration[p]
    /\ churn \in 0..CHURN_BUDGET
    /\ pendingReconnect \in PLAYERS \cup {NoPlayer}
    /\ pendingReconnectPredecessors \subseteq PLAYERS
    /\ pendingReconnectWasAuthority \in BOOLEAN
    /\ lastCapabilityReconnect \in BOOLEAN
    /\ lastReconnectPlayer \in PLAYERS \cup {NoPlayer}
    /\ \A i \in DOMAIN expectedReconnectOrder :
           expectedReconnectOrder[i] \in PLAYERS
    /\ \A i, j \in DOMAIN expectedReconnectOrder :
           i # j => expectedReconnectOrder[i] # expectedReconnectOrder[j]
    /\ expectedReconnectAuthority \in PLAYERS \cup {NoPlayer}
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

\* I4. V2Gating (Appendix K): every persistent observation records the protocol
\* version and physical connection generation that actually received it. A
\* historical v3 delivery therefore remains valid after that logical player id
\* reconnects on v2; the fresh-generation postcondition below separately proves
\* that the new v2 socket receives no SessionPlan.
V2Gating ==
    \A p \in PLAYERS : delivered[p] # NoDelivery => delivered[p].version >= 3

\* I5. HostValid: a stored host-topology plan always names a CURRENT member
\* capable of the session pair. In the model, healing (replan_host_session)
\* is atomic with the event that could invalidate the host, so every
\* reachable state satisfies this. It is a theorem OF THE ATOMIC-EVENT
\* ABSTRACTION implemented by the process-local room mutation guard and owned
\* FIFO event lane. It is not a claim about an unavailable process or a future
\* multi-node coordinator (formal/README.md "Atomicity argument").
HostValid ==
    (storedPlan # NoPlan /\ storedPlan.topology = "host") =>
        /\ storedPlan.host \in MemberSet
        /\ Pairable(caps, storedPlan, storedPlan.host)

\* I6. CeilingRespected: the stored topology never exceeds the desired
\* ceiling (choose_session_plan's topology_rank gate; host failover never
\* touches the pair).
CeilingRespected ==
    storedPlan # NoPlan =>
        TopologyRank(storedPlan.topology) <= TopologyRank(DESIRED)

\* I7. PeerCapability: no delivered peer list — including every reconnect
\* refresh — ever names the recipient itself or a player
\* that cannot run that plan's pair (plan_for's two-sided filter). Ordinary
\* immutable-capability scenarios keep this quantified across stale departed
\* clients too. Reconnect refinements check only observations delivered to the
\* current physical generation: an old generation's retained historical view
\* is inert, while every current v3 generation is refreshed atomically.
PeerCapability ==
    \A p \in PLAYERS :
        (delivered[p] # NoDelivery /\
         (~CAPABILITY_RECONNECT_ENABLED \/
          (p \in MemberSet /\
           delivered[p].generation = connectionGeneration[p]))) =>
            \A e \in delivered[p].plan.peers :
                /\ e.peer # p
                /\ SupportsSession(caps, e.peer, delivered[p].plan.topology,
                                   delivered[p].plan.transport)

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
                                SupportsSession(caps, q, pv.topology, pv.transport)}
            IN pv.topology = "mesh" =>
                IF SupportsSession(caps, r, pv.topology, pv.transport)
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
                                SupportsSession(caps, q, pv.topology, pv.transport)}
            IN pv.topology = "host" =>
                IF ~SupportsSession(caps, r, pv.topology, pv.transport)
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
        /\ Pairable(caps, storedPlan, storedPlan.host)
        /\ DOMAIN lastEmission.plans = V3Members(caps, MemberSet)

\* I13. PublicationCoverage: every plan publication is a complete snapshot for
\* exactly the current v3 membership. In particular, finalized joins/reconnects
\* refresh incumbents as well as the actor; there is no additive delta seam.
PublicationCoverage ==
    lastEmission # NoEmission =>
        DOMAIN lastEmission.plans = V3Members(caps, MemberSet)

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
            /\ delivered[p].version >= 3
            /\ delivered[p].plan.topology = "relay"
            /\ delivered[p].plan.transport = "relay"
            /\ delivered[p].plan.host = NoPlayer
            /\ delivered[p].plan.peers = {}

\* I15-I18. Semantic postconditions of the most recent capability reconnect.
\* The expected order and authority are captured from the disconnect snapshot
\* and live pre-reconnect room state independently of the seeded implementation
\* branches. The fresh-profile check requires an actual complete publication,
\* not merely that the bug branch stayed disabled.
CapabilityReconnectPublishesCompleteSnapshot ==
    lastCapabilityReconnect =>
        /\ lastEmission # NoEmission
        /\ lastEmission.kind = "latejoin"
        /\ DOMAIN lastEmission.plans = V3Members(caps, MemberSet)

CapabilityReconnectUsesFreshProfile ==
    lastCapabilityReconnect =>
        LET p == lastReconnectPlayer
        IN /\ p \in MemberSet
           /\ caps[p] = ProfileV3RelayOnly
           /\ lastEmission # NoEmission
           /\ lastEmission.kind = "latejoin"
           /\ delivered[p].plan.peers = {}
           /\ \A r \in DOMAIN lastEmission.plans :
                  \A e \in lastEmission.plans[r].peers : e.peer # p

CapabilityReconnectPreservesJoinPriority ==
    lastCapabilityReconnect => members = expectedReconnectOrder

CapabilityReconnectPreservesSuccessorAuthority ==
    lastCapabilityReconnect => authority = expectedReconnectAuthority

\* P95. A member that finalized mesh+webrtc on generation zero may reconnect
\* through a fresh v2 socket. The old v3 observation remains historical, while
\* the new generation receives no v3-only plan and every v3 incumbent receives
\* one exact refresh that excludes the actor from its WebRTC peer set.
ReconnectV2PublishesIncumbents ==
    (lastCapabilityReconnect /\ DESIRED = "mesh") =>
        LET incumbents == V3Members(caps, MemberSet)
        IN IF incumbents = {}
           THEN lastEmission = NoEmission
           ELSE /\ lastEmission # NoEmission
                /\ lastEmission.kind = "latejoin"
                /\ DOMAIN lastEmission.plans = incumbents

ReconnectV2WireGating ==
    (lastCapabilityReconnect /\ DESIRED = "mesh") =>
        LET p == lastReconnectPlayer
        IN /\ caps[p] = ProfileV2
           /\ \/ lastEmission = NoEmission
              \/ p \notin DOMAIN lastEmission.plans

ReconnectV2ExcludesActorFromPeers ==
    (lastCapabilityReconnect /\ DESIRED = "mesh") =>
        LET p == lastReconnectPlayer
        IN \/ lastEmission = NoEmission
           \/ \A r \in DOMAIN lastEmission.plans :
                  \A e \in lastEmission.plans[r].peers : e.peer # p

ReconnectV2PreservesHistoricalV3Delivery ==
    (lastCapabilityReconnect /\ DESIRED = "mesh") =>
        LET p == lastReconnectPlayer
        IN /\ delivered[p] # NoDelivery
           /\ delivered[p].version >= 3
           /\ delivered[p].generation < connectionGeneration[p]

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
       => IF \E q \in MemberSet' : Pairable(caps, storedPlan, q)
          THEN /\ storedPlan' # NoPlan
               /\ storedPlan'.host \in MemberSet'
               /\ Pairable(caps, storedPlan', storedPlan'.host)
          ELSE storedPlan' = NoPlan]_vars


=======================================================================================
