----------------------------- MODULE RateLimitWindow -----------------------------
(***************************************************************************)
(* The fixed-window room-operation rate limiter                            *)
(* (src/rate_limit.rs `RateLimitEntry` / `RoomRateLimiter`) — the seam the  *)
(* formal README previously listed as intentionally unmodeled.             *)
(*                                                                         *)
(* Each player owns one entry with four budgets — room creations, join      *)
(* attempts, WebRTC signals, detailed rejected-signal responses — and a      *)
(* `window_start`. Every attempt first runs `maybe_reset_window`            *)
(* (rate_limit.rs:81): once `window_start.elapsed() >= time_window`, ALL    *)
(* four budgets reset to zero TOGETHER and `window_start := now`. The       *)
(* admission predicates are strict upper bounds:                            *)
(*   try_room_creation  rejects if creations >= max OR joins >= max, then   *)
(*                      increments BOTH atomically (rate_limit.rs:92);      *)
(*   try_join_attempt / try_signal / try_signal_error                      *)
(*                      reject at their own cap, then increment (rs:107-143)*)
(*   signal_available    preflight: runs the reset, consumes NO slot        *)
(*                      (rs:129).                                           *)
(*                                                                         *)
(* THE CONTRACTS checked here:                                             *)
(*   BudgetsNeverExceedCaps — an admission happens only while its counter   *)
(*      is strictly below its cap, so no budget ever exceeds its cap: the   *)
(*      per-window admission bound is structural, not aspirational.         *)
(*   CurrentWindowWithinCap — since the last reset (the current fixed       *)
(*      window), at most MAX admissions of each kind were granted. This is  *)
(*      the honest guarantee the fixed-window design makes.                 *)
(*   WindowAnchored — the reset stamp never runs ahead of wall time.        *)
(*                                                                         *)
(* THE DOCUMENTED TRADE-OFF, pinned as a non-vacuity oracle:                *)
(* `NaiveSlidingWindowBound` claims at most MAX admissions in ANY window of *)
(* WINDOW ticks. The fixed-window design does NOT provide that — a player  *)
(* can spend the whole budget on the last tick of one window and the whole *)
(* budget again on the first tick of the next (the 2x boundary burst,      *)
(* rate_limit.rs:9-15 and the `maybe_reset_window` doc comment:             *)
(* "documented trade-off, not a bug"). The                                  *)
(* `RateLimitWindow_NaiveSlidingBound_ExpectedFailure` cfg pins that       *)
(* counterexample, exactly like the house `ReconnectLossBound_` and        *)
(* `ReconnectReplay_` non-vacuity oracles: if a future change claims        *)
(* sliding-window behavior, this cfg must start failing — loudly — until    *)
(* the implementation actually trims timestamps like                       *)
(* `crate::auth::rate_limiter`.                                            *)
(*                                                                         *)
(* MAPPING (one action per Rust entry point; time is discrete, house rule): *)
(*   TryRoomCreation     `check_room_creation` -> `try_room_creation`       *)
(*   TryJoinAttempt      `check_join_attempt` -> `try_join_attempt`         *)
(*   TrySignal           `check_signal` -> `try_signal`                     *)
(*   TrySignalError      `check_signal_error` -> `try_signal_error`         *)
(*   SignalPreflight     `check_signal_available` -> `signal_available`     *)
(*                       (resets a stale window, consumes no slot)          *)
(*   Tick                one unit of `time_window` granularity passing      *)
(*                       WITHOUT any request; the reset is LAZY — only an   *)
(*                       attempt observes expiry (rs:82), so budgets may    *)
(*                       sit expired between requests. `new_inner`'s        *)
(*                       zero-window clamp (rs:178) and                     *)
(*                       `validate_config_security`'s `time_window > 0`     *)
(*                       keep WINDOW >= 1, per ASSUME.                      *)
(* Per-player isolation (entries keyed by Uuid, rs:163) is not modeled:   *)
(* every variable is one player's entry; independence across players is    *)
(* structural in Rust (one map entry each) and pinned by unit tests.       *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS
    MAX_CREATIONS,   \* max room creations per window      (keep tiny: 1-2)
    MAX_JOINS,       \* max join attempts per window       (keep tiny: 1-2)
    MAX_SIGNALS,     \* max valid signals per window       (keep tiny: 1-2)
    MAX_ERRORS,      \* max detailed rejections per window (keep tiny: 1)
    WINDOW,          \* fixed window width in ticks        (keep tiny: 1-2)
    HORIZON          \* wall-clock bound (keep tiny; >= 2*WINDOW so the full
                     \* boundary window of the burst oracle exists)

ASSUME /\ MAX_CREATIONS \in Nat \ {0}
       /\ MAX_JOINS \in Nat \ {0}
       /\ MAX_SIGNALS \in Nat \ {0}
       /\ MAX_ERRORS \in Nat \ {0}
       \* A zero window is rejected by `validate_config_security` and clamped
       \* by `new_inner`; the model has no zero-width windows.
       /\ WINDOW \in Nat \ {0}
       /\ HORIZON \in Nat
       \* The boundary-burst oracle needs the full second window in range.
       /\ HORIZON >= 2 * WINDOW

VARIABLES
    now,          \* discrete wall time
    windowStart,  \* `window_start`: when the current window began
    creations,    \* room creations spent in the current window
    joins,        \* join attempts spent in the current window
    signals,      \* valid signals spent in the current window
    errs,         \* detailed rejected-signal responses spent
    creationLog,  \* ghost: admissions per tick, for the sliding-window audit
    joinLog       \* ghost: join admissions per tick

vars == <<now, windowStart, creations, joins, signals, errs,
          creationLog, joinLog>>

Init ==
    /\ now = 0
    /\ windowStart = 0
    /\ creations = 0
    /\ joins = 0
    /\ signals = 0
    /\ errs = 0
    /\ creationLog = [t \in 0..HORIZON |-> 0]
    /\ joinLog = [t \in 0..HORIZON |-> 0]

(* `maybe_reset_window` (rate_limit.rs:81) folded into every entry point:   *)
(* once the window has elapsed, ALL budgets reset together and the window   *)
(* anchor moves to now. Lazy: only an attempt observes expiry.              *)
ResetDue == now - windowStart >= WINDOW

c == IF ResetDue THEN 0 ELSE creations
j == IF ResetDue THEN 0 ELSE joins
s == IF ResetDue THEN 0 ELSE signals
e == IF ResetDue THEN 0 ELSE errs
ws == IF ResetDue THEN now ELSE windowStart

(* Post-reset anchor bundle shared by every entry point: the window anchor  *)
(* moves to now exactly when the reset fired, and wall time never moves      *)
(* inside an attempt (requests are instantaneous at this granularity).      *)
WindowAndTime ==
    /\ windowStart' = ws
    /\ UNCHANGED now

TryRoomCreation ==
    /\ WindowAndTime
    /\ signals' = s
    /\ errs' = e
    /\ IF c < MAX_CREATIONS /\ j < MAX_JOINS
       THEN /\ creations' = c + 1
            /\ joins' = j + 1
            /\ creationLog' = [creationLog EXCEPT ![now] = @ + 1]
            /\ joinLog' = [joinLog EXCEPT ![now] = @ + 1]
       ELSE /\ creations' = c
            /\ joins' = j
            /\ UNCHANGED <<creationLog, joinLog>>

TryJoinAttempt ==
    /\ WindowAndTime
    /\ signals' = s
    /\ errs' = e
    /\ IF j < MAX_JOINS
       THEN /\ joins' = j + 1
            /\ joinLog' = [joinLog EXCEPT ![now] = @ + 1]
       ELSE /\ joins' = j
            /\ UNCHANGED joinLog
    /\ creations' = c
    /\ UNCHANGED creationLog

TrySignal ==
    /\ WindowAndTime
    /\ errs' = e
    /\ IF s < MAX_SIGNALS
       THEN signals' = s + 1
       ELSE signals' = s
    /\ creations' = c
    /\ joins' = j
    /\ UNCHANGED <<creationLog, joinLog>>

TrySignalError ==
    /\ WindowAndTime
    /\ IF e < MAX_ERRORS
       THEN errs' = e + 1
       ELSE errs' = e
    /\ creations' = c
    /\ joins' = j
    /\ signals' = s
    /\ UNCHANGED <<creationLog, joinLog>>

(* `signal_available` preflight: runs the (lazy) reset, consumes NO slot.   *)
SignalPreflight ==
    /\ WindowAndTime
    /\ creations' = c
    /\ joins' = j
    /\ signals' = s
    /\ errs' = e
    /\ UNCHANGED <<creationLog, joinLog>>

Tick ==
    /\ now < HORIZON
    /\ now' = now + 1
    /\ UNCHANGED <<windowStart, creations, joins, signals, errs,
                   creationLog, joinLog>>

Next ==
    \/ TryRoomCreation
    \/ TryJoinAttempt
    \/ TrySignal
    \/ TrySignalError
    \/ SignalPreflight
    \/ Tick

Spec == Init /\ [][Next]_vars

----------------------------------------------------------------------------
(* Safety *)

TypeOK ==
    /\ now \in 0..HORIZON
    /\ windowStart \in 0..HORIZON
    /\ creations \in 0..MAX_CREATIONS
    /\ joins \in 0..MAX_JOINS
    /\ signals \in 0..MAX_SIGNALS
    /\ errs \in 0..MAX_ERRORS
    /\ creationLog \in [0..HORIZON -> 0..MAX_CREATIONS]
    /\ joinLog \in [0..HORIZON -> 0..MAX_JOINS]

(* An admission happens only while its counter is strictly below its cap    *)
(* (the strict `<` guards in rate_limit.rs:94-141), so no budget ever       *)
(* exceeds its cap. `saturating_add` never has to absorb an overflow        *)
(* because this invariant is what bounds the counters.                      *)
BudgetsNeverExceedCaps ==
    /\ creations <= MAX_CREATIONS
    /\ joins <= MAX_JOINS
    /\ signals <= MAX_SIGNALS
    /\ errs <= MAX_ERRORS

(* The reset stamp never runs ahead of wall time (`window_start.elapsed()`  *)
(* is a real elapsed-time guard).                                           *)
WindowAnchored == windowStart <= now

(* Sum of a ghost log over a set of ticks (finite; commutative fold). *)
RECURSIVE LogSum(_, _)
LogSum(f, S) ==
    IF S = {} THEN 0
    ELSE LET
           t == CHOOSE x \in S : TRUE
         IN
           f[t] + LogSum(f, S \ {t})

(* THE honest fixed-window guarantee: since the last reset — every admission *)
(* at or after `windowStart` — at most MAX admissions of each kind were      *)
(* granted. All post-reset admissions are counted here: the anchor moves     *)
(* forward on every reset, so pre-reset log entries fall below the filter.   *)
CurrentWindowWithinCap ==
    /\ LogSum(creationLog, {t \in 0..now : t >= windowStart}) <= MAX_CREATIONS
    /\ LogSum(joinLog, {t \in 0..now : t >= windowStart}) <= MAX_JOINS

(* The DOCUMENTED NON-GUARANTEE, pinned by                                  *)
(* `RateLimitWindow_NaiveSlidingBound_ExpectedFailure`: fixed windows do    *)
(* NOT bound admissions inside an arbitrary sliding window of WINDOW ticks  *)
(* — the boundary burst spends MAX at the end of one window and MAX again   *)
(* at the start of the next. The audited span is HALF-OPEN — WINDOW          *)
(* consecutive ticks {t, ..., t + WINDOW - 1}, the same width a real        *)
(* sliding limiter enforces when it trims stamps with                       *)
(* `elapsed >= window` (crate::auth::rate_limiter) — so the oracle fails    *)
(* for exactly the boundary burst and goes quiet if the room limiter ever   *)
(* genuinely converts to sliding-window accounting. Must stay violated by   *)
(* the checked model; the _ExpectedFailure cfg turning green in CI (an      *)
(* unobserved expected failure) is the signal to update this spec           *)
(* deliberately.                                                            *)
NaiveSlidingWindowBound ==
    \A t \in 0..HORIZON :
        LogSum(creationLog,
               {x \in 0..HORIZON : t <= x /\ x < t + WINDOW}) <= MAX_CREATIONS

=============================================================================
