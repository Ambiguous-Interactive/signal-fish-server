----------------------------- MODULE SenderPacingReaper -----------------------------
(***************************************************************************)
(* The sender-pacing vs activity-reaper TIMING contract (BUG-2 / the       *)
(* timeout-inversion hazard the P10.A2 config cross-field check prevents):  *)
(*                                                                         *)
(*   A healthy, actively-sending player is NEVER evicted by the activity    *)
(*   reaper for having its own recorded activity go stale while it is       *)
(*   PARKED broadcasting to a slow recipient — PROVIDED the slow-consumer   *)
(*   grace period cannot outlast the reaper's ping deadline.                *)
(*                                                                         *)
(* The hazard is a race between two independent server clocks measured      *)
(* against the SAME frozen timestamp:                                       *)
(*                                                                         *)
(*   1. A player's message handler records its activity at DISPATCH         *)
(*      (`record_client_activity`, src/server/message_router.rs:15), then   *)
(*      — still on the same task, BEFORE relaying — does a throttled room   *)
(*      refresh `maybe_update_last_seen(...).await` (message_router.rs:23),  *)
(*      which on the throttle boundary performs a DB write and takes the    *)
(*      global `rooms` write lock (src/server/heartbeat.rs). It then relays *)
(*      the frame: `deliver_to_all` awaits `join_all` over every recipient  *)
(*      (src/server.rs:675-716). A slow recipient PARKS that await for up   *)
(*      to `websocket.slow_consumer_timeout_ms` (`deliver_or_disconnect`    *)
(*      grace, src/coordination/mod.rs). The player's receive loop is the   *)
(*      SAME task (connection.rs inline-awaits `handle_client_message`), so *)
(*      while parked it processes NO further inbound frames — its recorded  *)
(*      activity stays frozen at the dispatch instant even as the client    *)
(*      keeps sending (pings pile up unread at the socket).                  *)
(*   2. The activity reaper (`collect_expired_clients`,                     *)
(*      src/server/connection_manager.rs) evicts any client whose recorded  *)
(*      activity is older than `server.ping_timeout`                        *)
(*      (`now.duration_since(last_ping) > ping_timeout`, close 4003), swept *)
(*      each `room_cleanup_interval` tick (src/server/maintenance.rs).      *)
(*                                                                         *)
(* If a park (bounded by SLOW_TIMEOUT) plus the pre-park delay can push the  *)
(* frozen-activity gap past PING_TIMEOUT, the reaper evicts the HEALTHY      *)
(* parked SENDER (4003) before its slow RECIPIENT is ever disconnected —     *)
(* the timeout inversion.                                                   *)
(*                                                                         *)
(* TIME MODEL (Appendix-O house rule: discrete integer `now` + `Tick`,      *)
(* timers as absolute-deadline guards). `Tick` advances time in exactly two *)
(* situations, the only two where the sender waits and cannot refresh its   *)
(* activity: (a) while PARKED on the slow recipient (capped at the grace    *)
(* deadline — the recipient's grace timer WILL fire at EffectiveSlow, so    *)
(* the park never outlasts it), and (b) ONCE while BROADCASTING, modeling   *)
(* the PRE-PARK delay `d` above (the `maybe_update_last_seen` DB-write +     *)
(* `rooms`-lock await that runs after the activity record and before the    *)
(* park). Bounding `d` to a single tick is a deliberate floor: in wall time *)
(* it is usually sub-tick, but under `rooms`-lock contention it is nonzero  *)
(* and unbounded, and even one tick is enough to make the exact boundary    *)
(* `SLOW = PING` unsafe (see the derivation). Processing, enqueuing, and    *)
(* the client sending are otherwise instantaneous at this time scale.       *)
(*                                                                         *)
(* Mapping to the implementation:                                           *)
(*   ClientSend        a client frame (ping / game-data) ARRIVING at the    *)
(*                     socket — the client's liveness, independent of        *)
(*                     whether the server task is free to process it        *)
(*   SenderProcess     the receive loop dispatching one inbound frame:       *)
(*                     record_client_activity (activity := now) then begin   *)
(*                     the relay (message_router.rs -> broadcast)            *)
(*   Tick (broadcast)  the `maybe_update_last_seen` DB-write + `rooms`-lock  *)
(*                     await between the activity record and the park (the   *)
(*                     pre-park delay `d`, at most one tick)                 *)
(*   BroadcastFast     deliver_to_all enqueuing to a recipient that has room *)
(*                     (or is already gone) — the relay completes, no park   *)
(*   BroadcastPark     the join_all await blocking on a FULL recipient queue *)
(*                     (deliver_or_disconnect entering its grace wait)       *)
(*   RecipientDrain    the recipient's socket writer draining one frame —     *)
(*                     scheduled nondeterministically; a recipient that       *)
(*                     stopped reading stalls the whole park (this spec has   *)
(*                     no fairness on any action — see Spec)                  *)
(*   ParkResolve       a drained slot lets the parked broadcast enqueue and   *)
(*                     the handler return (the delivery succeeded in time)    *)
(*   ParkGraceExpire   grace expiry: the recipient is the slow consumer and   *)
(*                     is disconnected (CloseReason::SlowConsumer, close      *)
(*                     4002); the handler returns. Activity is NOT refreshed  *)
(*                     here — only the NEXT dispatched frame refreshes it     *)
(*   RecipientReconnect a fresh recipient takes the seat, so the sender can   *)
(*                     be made to park again (the refill loop)                *)
(*   ReaperSweep       collect_expired_clients evicting a client whose        *)
(*                     recorded activity exceeds ping_timeout (close 4003).   *)
(*                     Reachable ONLY under the inversion (bug arm); in the   *)
(*                     checked safe cfgs the gap never exceeds PING, so this  *)
(*                     action is never enabled — that IS the contract         *)
(*   Tick (parked)     one unit of wall time passing while the sender is      *)
(*                     parked (room_cleanup_interval granularity abstracted   *)
(*                     to the worst case: the reaper may sample every tick,   *)
(*                     which over-approximates the real 60 s sweep, so        *)
(*                     safety here implies safety there)                      *)
(*                                                                         *)
(* THE VERDICT (safety). `sndEvicted` is a ghost that latches TRUE the       *)
(* instant the reaper evicts the sender. Because the sender is healthy by    *)
(* construction (its client never stops sending — ClientSend is always       *)
(* enabled — and the only staleness source is a park, which by construction  *)
(* follows a recent dispatch), ANY reaper eviction is a false positive.      *)
(* HealthySenderNeverReaped == ~sndEvicted is the contract; the stronger     *)
(* GapWithinPingDeadline pins the reaper-visible gap it rests on.            *)
(*                                                                         *)
(* NON-VACUITY (verified during development, keep reproducible): the         *)
(* `TimeoutInversionBug` constant forces the EFFECTIVE grace period to       *)
(* exactly PING_TIMEOUT — the `slow = ping` boundary the cross-field check   *)
(* rejects (legal per-field, forbidden cross-field; legal system-wide before *)
(* A2). TLC then violates `HealthySenderNeverReaped` via the pre-park-delay  *)
(* path: SenderProcess (activity := t) -> a one-tick pre-park Tick (d = 1)   *)
(* -> BroadcastPark (parkedAt := t + 1) -> the grace-region Ticks push the   *)
(* gap to (t + 1 + PING) - t = PING + 1 > PING while the grace (>= PING) is  *)
(* only now satisfiable -> ReaperSweep wins that race and evicts the healthy *)
(* sender. (`slow > ping` is a fortiori unsafe — it evicts even with d = 0.) *)
(* The checked configurations pin `TimeoutInversionBug = FALSE`; flip it     *)
(* locally to watch the invariant catch the inversion.                      *)
(*                                                                         *)
(* THE DERIVED INEQUALITY (the P10.A2 deliverable). With the pre-park delay  *)
(* `d` (0 or 1 tick) modeled, the reaper-visible gap of a parked healthy     *)
(* sender peaks at `d + SLOW`. TLC then derives that `SLOW >= PING` is        *)
(* unsafe — EXACTLY the region `validate_config_security` rejects            *)
(* (`slow_consumer_timeout_ms >= ping_timeout * 1000`): at `SLOW = PING` the *)
(* `d = 1` path pushes the peak to `PING + 1 > PING` and the reaper (strict  *)
(* `>`) evicts. The two checked cfgs pin `SLOW < PING` and are green —       *)
(* `_Small` (SLOW = 2 < PING = 4) and `_Boundary` (SLOW = 3 = PING - 1, the  *)
(* tightest safe: peak gap SLOW + 1 = PING, never `> PING`).                 *)
(*                                                                         *)
(* So the strict `<` the check enforces is the NECESSARY floor the model     *)
(* derives — it eliminates every `SLOW >= PING` inversion, the gross         *)
(* misconfiguration (e.g. 60 s vs 30 s) and the exact boundary alike. It is  *)
(* NOT proven SUFFICIENT: the model bounds `d` to one tick, but the          *)
(* `rooms`-lock/DB-write pre-park delay is unbounded under contention, so a  *)
(* config with a thin margin (`SLOW` just under `PING`) can still invert if  *)
(* `d` exceeds `PING - SLOW`. True safety requires the operator to size the  *)
(* margin `(ping_timeout - slow_consumer_timeout_ms)` above their worst-case *)
(* pre-park delay; the default 25 s margin dwarfs any realistic `d`. The     *)
(* check is the derived guardrail against the provable inversion region, not *)
(* a liveness proof under unbounded contention. (When ping_timeout = 0 the   *)
(* reaper is disabled — no deadline, no inversion — so the A2 check is        *)
(* guarded on ping_timeout > 0, matching this module's ASSUME.)              *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    PING_TIMEOUT,       \* activity-reaper deadline in ticks (keep tiny: 4)
    SLOW_TIMEOUT,       \* slow-consumer grace period in ticks (keep tiny; the
                        \* checked cfgs pin SLOW_TIMEOUT < PING_TIMEOUT)
    HORIZON,            \* wall-clock bound so `now` (and the state space) is
                        \* finite (keep tiny: 6)
    QueueCapacity,      \* bounded recipient outbound queue slots (keep tiny: 1)
    InboxCap,           \* bounded unread client frames at the socket (keep
                        \* tiny: 1) — the client's liveness the reaper ignores
    ReconnectBudget,    \* recipient disconnect+reconnect refills (keep tiny: 1)
    TimeoutInversionBug \* TRUE forces the effective grace to PING_TIMEOUT (the
                        \* `slow = ping` boundary the check rejects); must
                        \* violate HealthySenderNeverReaped; FALSE in checked cfgs

ASSUME /\ PING_TIMEOUT \in Nat \ {0}
       /\ SLOW_TIMEOUT \in Nat \ {0}
       \* The checked configs are safe by construction; the seeded bug, not a
       \* bad SLOW_TIMEOUT, exhibits the inversion.
       /\ SLOW_TIMEOUT < PING_TIMEOUT
       /\ HORIZON \in Nat \ {0}
       /\ QueueCapacity \in Nat \ {0}
       /\ InboxCap \in Nat \ {0}
       /\ ReconnectBudget \in Nat
       /\ TimeoutInversionBug \in BOOLEAN
       \* The horizon must let a full grace period elapse plus the pre-park
       \* tick and one reaper over-run tick, or the inversion could never be
       \* exhibited.
       /\ HORIZON >= PING_TIMEOUT + 2

\* The grace period the park actually waits out. The seeded bug forces the
\* exact `slow = ping` boundary the A2 check rejects; otherwise it is the
\* configured SLOW_TIMEOUT.
EffectiveSlow == IF TimeoutInversionBug THEN PING_TIMEOUT ELSE SLOW_TIMEOUT

VARIABLES
    now,               \* discrete wall time (advances while Parked, or once
                       \* while Broadcasting for the pre-park delay)
    sndPhase,          \* "Reading" | "Broadcasting" | "Parked"
    sndParkedAt,       \* the `now` at which the current park began
    sndLastActivityAt, \* the `now` of the sender's last DISPATCHED frame — the
                       \* timestamp the reaper measures (refreshed only by
                       \* SenderProcess, NOT by a park ending)
    sndInbox,          \* unread client frames sitting at the socket (>0 proves
                       \* the client is alive while the sender is parked)
    sndEvicted,        \* ghost: the reaper false-positive verdict (the bug)
    rcpConn,           \* "Connected" | "Dropped": the slow recipient
    rcpQueue,          \* recipient outbound queue depth (a full queue parks)
    rcpReconnects      \* recipient disconnect+reconnect refills consumed

vars == <<now, sndPhase, sndParkedAt, sndLastActivityAt, sndInbox, sndEvicted,
          rcpConn, rcpQueue, rcpReconnects>>

Init ==
    /\ now = 0
    /\ sndPhase = "Reading"
    /\ sndParkedAt = 0
    /\ sndLastActivityAt = 0
    /\ sndInbox = 0
    /\ sndEvicted = FALSE
    /\ rcpConn = "Connected"
    /\ rcpQueue = 0
    /\ rcpReconnects = 0

(* A client frame arrives at the socket. The client is healthy and keeps     *)
(* sending regardless of whether the server task is free to process it —      *)
(* while the sender is parked these pile up UNREAD, which is precisely the    *)
(* liveness the activity reaper fails to credit.                             *)
ClientSend ==
    /\ ~sndEvicted
    /\ sndInbox < InboxCap
    /\ sndInbox' = sndInbox + 1
    /\ UNCHANGED <<now, sndPhase, sndParkedAt, sndLastActivityAt, sndEvicted,
                   rcpConn, rcpQueue, rcpReconnects>>

(* The receive loop dispatches one inbound frame: it records the sender's     *)
(* activity at dispatch (activity := now) and begins the relay broadcast.     *)
(* This is the ONLY action that refreshes the reaper's timestamp.            *)
SenderProcess ==
    /\ ~sndEvicted
    /\ sndPhase = "Reading"
    /\ sndInbox > 0
    /\ sndInbox' = sndInbox - 1
    /\ sndLastActivityAt' = now
    /\ sndPhase' = "Broadcasting"
    /\ UNCHANGED <<now, sndParkedAt, sndEvicted, rcpConn, rcpQueue,
                   rcpReconnects>>

(* deliver_to_all enqueues the relayed frame to the recipient. Fast path:     *)
(* the recipient has a free slot (enqueue) or is already gone (nothing to     *)
(* enqueue). Either way the broadcast completes without parking.             *)
BroadcastFast ==
    /\ ~sndEvicted
    /\ sndPhase = "Broadcasting"
    /\ (rcpConn = "Dropped" \/ rcpQueue < QueueCapacity)
    /\ rcpQueue' = IF rcpConn = "Connected" THEN rcpQueue + 1 ELSE rcpQueue
    /\ sndPhase' = "Reading"
    /\ UNCHANGED <<now, sndParkedAt, sndLastActivityAt, sndInbox, sndEvicted,
                   rcpConn, rcpReconnects>>

(* The join_all await blocks on a FULL recipient queue: the sender parks,     *)
(* recording when the grace clock starts. `sndParkedAt` may be one tick after *)
(* `sndLastActivityAt` (the pre-park delay `d` above), so the reaper-visible  *)
(* gap during the park is `d + (now - sndParkedAt)`.                          *)
BroadcastPark ==
    /\ ~sndEvicted
    /\ sndPhase = "Broadcasting"
    /\ rcpConn = "Connected"
    /\ rcpQueue = QueueCapacity
    /\ sndPhase' = "Parked"
    /\ sndParkedAt' = now
    /\ UNCHANGED <<now, sndLastActivityAt, sndInbox, sndEvicted, rcpConn,
                   rcpQueue, rcpReconnects>>

(* The recipient's socket writer draining one queued frame. This spec has no  *)
(* fairness on any action (Spec is INVARIANTS-only), so TLC also explores the *)
(* behavior where this is never scheduled — the recipient that stopped        *)
(* reading and stalls the park to its grace limit (the slow consumer).        *)
RecipientDrain ==
    /\ ~sndEvicted
    /\ rcpConn = "Connected"
    /\ rcpQueue > 0
    /\ rcpQueue' = rcpQueue - 1
    /\ UNCHANGED <<now, sndPhase, sndParkedAt, sndLastActivityAt, sndInbox,
                   sndEvicted, rcpConn, rcpReconnects>>

(* A drained slot lets the parked broadcast enqueue and the handler return:   *)
(* the delivery succeeded within grace. Activity is NOT refreshed — the       *)
(* handler returning is not a dispatch; only the next frame refreshes it.     *)
ParkResolve ==
    /\ ~sndEvicted
    /\ sndPhase = "Parked"
    /\ rcpConn = "Connected"
    /\ rcpQueue < QueueCapacity
    /\ rcpQueue' = rcpQueue + 1
    /\ sndPhase' = "Reading"
    /\ UNCHANGED <<now, sndParkedAt, sndLastActivityAt, sndInbox, sndEvicted,
                   rcpConn, rcpReconnects>>

(* Grace expiry: the park has waited EffectiveSlow ticks, so the recipient is *)
(* the slow consumer and is disconnected (CloseReason::SlowConsumer, 4002);   *)
(* the handler returns and the sender is free. Activity is NOT refreshed here *)
(* (see the header): the sender's timestamp is still the pre-park dispatch,   *)
(* so the reaper can still be racing this exact tick.                        *)
ParkGraceExpire ==
    /\ ~sndEvicted
    /\ sndPhase = "Parked"
    /\ now - sndParkedAt >= EffectiveSlow
    /\ rcpConn' = "Dropped"
    /\ rcpQueue' = 0
    /\ sndPhase' = "Reading"
    /\ UNCHANGED <<now, sndParkedAt, sndLastActivityAt, sndInbox, sndEvicted,
                   rcpReconnects>>

(* A fresh recipient takes the disconnected seat (a reconnect / a new joiner) *)
(* so the sender can be driven to park again — the refill loop that lets the  *)
(* model exhibit repeated park cycles within one horizon.                    *)
RecipientReconnect ==
    /\ ~sndEvicted
    /\ rcpConn = "Dropped"
    /\ rcpReconnects < ReconnectBudget
    /\ rcpConn' = "Connected"
    /\ rcpQueue' = 0
    /\ rcpReconnects' = rcpReconnects + 1
    /\ UNCHANGED <<now, sndPhase, sndParkedAt, sndLastActivityAt, sndInbox,
                   sndEvicted>>

(* collect_expired_clients: the reaper evicts a client whose recorded         *)
(* activity is STRICTLY older than the ping deadline (close 4003). In this    *)
(* model the sender is healthy throughout, so a firing here is always a false *)
(* positive — the timeout inversion made observable. Enabled only when the    *)
(* gap has been pushed past PING_TIMEOUT, i.e. only under the inversion.      *)
ReaperSweep ==
    /\ ~sndEvicted
    /\ now - sndLastActivityAt > PING_TIMEOUT
    /\ sndEvicted' = TRUE
    /\ UNCHANGED <<now, sndPhase, sndParkedAt, sndLastActivityAt, sndInbox,
                   rcpConn, rcpQueue, rcpReconnects>>

(* One unit of wall time passes, in the only two waiting states:              *)
(*   - Parked, before the grace deadline: the slow recipient has not drained  *)
(*     and the grace timer has not yet fired (it WILL at EffectiveSlow, so the *)
(*     park never outlasts it — Tick can no longer advance once                *)
(*     now - sndParkedAt = EffectiveSlow, forcing ParkGraceExpire/ParkResolve).*)
(*   - Broadcasting, exactly ONCE (guarded now = sndLastActivityAt, which     *)
(*     holds only until this tick advances it): the pre-park delay `d` — the  *)
(*     `maybe_update_last_seen` DB-write + `rooms`-lock await between the      *)
(*     activity record and the park. Bounded to one tick (see the header).    *)
(* All other phases make instantaneous progress at this time scale.           *)
Tick ==
    /\ ~sndEvicted
    /\ now < HORIZON
    /\ \/ (sndPhase = "Parked" /\ now - sndParkedAt < EffectiveSlow)
       \/ (sndPhase = "Broadcasting" /\ now = sndLastActivityAt)
    /\ now' = now + 1
    /\ UNCHANGED <<sndPhase, sndParkedAt, sndLastActivityAt, sndInbox,
                   sndEvicted, rcpConn, rcpQueue, rcpReconnects>>

(* Explicit terminal stutter for the evicted (bug-arm) state — the only       *)
(* genuinely terminal state, since every other action guards on ~sndEvicted.  *)
(* The checked safe configs never evict; they make perpetual progress         *)
(* (ClientSend / SenderProcess / park cycles) and so have no terminal, relying *)
(* on that progress — not Done — for deadlock-freedom. Done deliberately does *)
(* NOT cover the Broadcasting/Parked phases, so a missing transition there     *)
(* would still surface as a deadlock rather than be masked.                   *)
Done ==
    /\ sndEvicted
    /\ UNCHANGED vars

Next ==
    \/ ClientSend
    \/ SenderProcess
    \/ BroadcastFast
    \/ BroadcastPark
    \/ RecipientDrain
    \/ ParkResolve
    \/ ParkGraceExpire
    \/ RecipientReconnect
    \/ ReaperSweep
    \/ Tick
    \/ Done

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ now \in 0..HORIZON
    /\ sndPhase \in {"Reading", "Broadcasting", "Parked"}
    /\ sndParkedAt \in 0..HORIZON
    /\ sndLastActivityAt \in 0..HORIZON
    /\ sndInbox \in 0..InboxCap
    /\ sndEvicted \in BOOLEAN
    /\ rcpConn \in {"Connected", "Dropped"}
    /\ rcpQueue \in 0..QueueCapacity
    /\ rcpReconnects \in 0..ReconnectBudget

(* THE contract: a healthy, actively-sending sender is never evicted by the   *)
(* activity reaper. Holds in every reachable state exactly when the grace     *)
(* period cannot outlast the ping deadline (SLOW_TIMEOUT < PING_TIMEOUT once   *)
(* the one-tick pre-park delay is counted); the seeded TimeoutInversionBug     *)
(* (grace = ping) violates it.                                               *)
HealthySenderNeverReaped ==
    ~sndEvicted

(* The reaper-relevant boundary, and the actual safe-region characterization: *)
(* while the sender is healthy the reaper never SEES a gap over the ping       *)
(* deadline. This is strictly stronger than HealthySenderNeverReaped (it       *)
(* fails one step BEFORE the eviction) and, unlike ActivityGapBounded, it      *)
(* DISTINGUISHES the safe configs from the bug arm: it holds iff              *)
(* EffectiveSlow + (pre-park delay) <= PING_TIMEOUT, i.e. iff SLOW < PING.     *)
GapWithinPingDeadline ==
    ~sndEvicted => now - sndLastActivityAt <= PING_TIMEOUT

(* Dynamics sanity bound (a lemma, NOT the safe-region boundary): the gap of  *)
(* a healthy sender never exceeds the grace period plus the one-tick pre-park *)
(* delay. Reaches equality (tight), and holds in BOTH the safe and bug arms   *)
(* — which is exactly why it does not by itself certify safety; that is       *)
(* GapWithinPingDeadline's job.                                              *)
ActivityGapBounded ==
    ~sndEvicted => now - sndLastActivityAt <= EffectiveSlow + 1

(* The park clock never runs backwards or beyond wall time: a parked sender's *)
(* grace timer is a real elapsed-time guard.                                 *)
ParkClockSound ==
    sndPhase = "Parked" => sndParkedAt <= now

=============================================================================
