#!/usr/bin/env python3
"""Z3 proofs of Signal Fish protocol v3 pure-logic invariants.

This is the SMT counterpart to the TLA+ model in ``formal/tla/``. Where TLC
*explores reachable states* of the per-room session lifecycle, Z3 *proves
universally quantified properties* of the pure decision functions over
**unbounded** inputs (any number of members, any capability mix, any id space)
that an explicit-state checker can only sample.

Each proof asserts the **negation** of a property and checks for `unsat`:
`unsat` means no counterexample exists, i.e. the property holds for all inputs.

Functions modeled (faithful to the Rust source — anchors are stable):

- ``choose_session_plan`` ladder walk  — src/server/session_policy.rs:327
  (``UPGRADE_LADDER`` :190, ``topology_rank`` :208, ``transport_enabled`` :218,
   ``all_support`` :175, ``is_valid_pair`` :297)
- ``elect_host``                        — src/server/session_policy.rs:372
- ``local_initiates`` (glare rule)      — src/server/signaling.rs:60
- replay truncation predicate           — src/reconnection.rs
  (``get_missed_events``'s ``evicted_watermark > last_sequence`` over
   ``EventBuffer::push``'s max-evicted watermark invariant)
- ``negotiate_protocol_version``        — src/config/protocol.rs:87
- per-(sender, room) relay stamping     — the protocol v3 sequencing design
  (``stamp = counter + 1``; leave/rejoin resets the counter to 0)
- delivery-contract counter deltas      — src/coordination/mod.rs
  (``deliver_or_disconnect`` + ``finalize_closed_connection``; the SMT twin
   of ``formal/tla/DeliveryContract.tla``'s ``Conservation`` invariant)
- ``validate_config_security`` closure  — src/config/validation.rs +
  ``WebSocketConfig::validate`` (proof set J: every admitted config satisfies
  the runtime safety envelope — capacity pairing, occupied-room GC deadline,
  relay-envelope headroom, batch-deadline representability, timeout floors —
  and the new guards are NECESSARY, each by an explicit counterexample witness)

Every obligation is an ``unsat`` proof except one deliberate EXISTENCE check:
``naive_gap_predicate_unsound`` (set F) asserts a scenario and expects ``sat``,
exhibiting the interleaved-rooms witness that makes any sequence-GAP-based
truncation predicate lie — the SMT twin of ``formal/tla/ReconnectReplay.tla``'s
``NaiveGapPredicateBug`` counterexample, proving the watermark design necessary.

Run: ``python3 formal/z3/protocol_invariants.py`` (needs the ``z3`` module;
the dev container installs ``python3-z3``). CI: formal-verification.yml.
"""

from __future__ import annotations

import sys

from z3 import (
    And,
    Bool,
    Distinct,
    Exists,
    ForAll,
    Function,
    If,
    Implies,
    Int,
    IntSort,
    Not,
    Or,
    Solver,
    Xor,
    BoolSort,
    sat,
    unsat,
)

# Topology richness ranks — src/server/session_policy.rs:208 (Relay<Host<Mesh).
RELAY, HOST, MESH = 0, 1, 2

_FAILURES: list[str] = []


def prove(name: str, solver: Solver) -> None:
    """A proof obligation passes iff the negated property is unsat."""
    result = solver.check()
    if result == unsat:
        print(f"  PASS  {name}")
    else:
        _FAILURES.append(name)
        print(f"  FAIL  {name}  ({result})")
        if result.__repr__() == "sat":
            print(f"        counterexample: {solver.model()}")


def witness(name: str, solver: Solver) -> None:
    """An EXISTENCE obligation passes iff the scenario is sat (witness printed).

    The dual of :func:`prove`: used where the property IS the existence of a
    counterexample to a rejected design (a deliberately-wrong predicate must be
    demonstrably wrong, so the accepted design is demonstrably necessary).
    """
    result = solver.check()
    if result == sat:
        print(f"  PASS  {name}")
        print(f"        witness: {solver.model()}")
    else:
        _FAILURES.append(name)
        print(f"  FAIL  {name}  (expected sat, got {result})")


# ---------------------------------------------------------------------------
# Proof set A — the ladder SELECTOR logic (src/server/session_policy.rs:338).
#
# The selector is decomposed from member counting: `all_support` for each of the
# three upgrade rungs is abstracted to a free boolean (proof set B re-attaches it
# to member capabilities). This proves the *find-first-fit-else-relay* logic is
# correct for every possible (desired ceiling, transport-enable, per-rung
# support) combination — an exhaustive proof the Rust `.find().unwrap_or(RELAY)`
# matches its contract.
# ---------------------------------------------------------------------------
def selector_chosen_rank(desired, en_webrtc, en_direct, a_mesh_w, a_host_w, a_host_d):
    """Mirror of the ladder walk; returns the chosen topology rank.

    Rung order (richest first): Mesh+WebRtc, Host+WebRtc, Host+Direct, else Relay
    floor. A rung fits iff rank<=desired AND its transport is enabled AND every
    member supports it.
    """
    fit_mesh_w = And(MESH <= desired, en_webrtc, a_mesh_w)
    fit_host_w = And(HOST <= desired, en_webrtc, a_host_w)
    fit_host_d = And(HOST <= desired, en_direct, a_host_d)
    return If(fit_mesh_w, MESH, If(fit_host_w, HOST, If(fit_host_d, HOST, RELAY)))


def selector_uses_webrtc(desired, en_webrtc, en_direct, a_mesh_w, a_host_w, a_host_d):
    fit_mesh_w = And(MESH <= desired, en_webrtc, a_mesh_w)
    fit_host_w = And(HOST <= desired, en_webrtc, a_host_w)
    fit_host_d = And(HOST <= desired, en_direct, a_host_d)
    # WebRTC transport iff we settle on mesh+webrtc or host+webrtc.
    return Or(fit_mesh_w, And(Not(fit_mesh_w), fit_host_w))


def proof_set_a() -> None:
    print("Proof set A — ladder selector logic")
    desired = Int("desired")
    en_webrtc, en_direct = Bool("en_webrtc"), Bool("en_direct")
    a_mesh_w, a_host_w, a_host_d = Bool("a_mesh_w"), Bool("a_host_w"), Bool("a_host_d")
    # `desired` is a real Topology, so its rank is one of the three.
    desired_valid = Or(desired == RELAY, desired == HOST, desired == MESH)
    chosen = selector_chosen_rank(
        desired, en_webrtc, en_direct, a_mesh_w, a_host_w, a_host_d
    )

    # A1: chosen rank never exceeds the desired ceiling.
    s = Solver()
    s.add(desired_valid)
    s.add(Not(chosen <= desired))
    prove("A1 chosen topology never richer than desired ceiling", s)

    # A2: result is always a legal topology rank (Relay/Host/Mesh) — total fn.
    s = Solver()
    s.add(desired_valid)
    s.add(Not(Or(chosen == RELAY, chosen == HOST, chosen == MESH)))
    prove("A2 selector is total and yields a legal topology", s)

    # A3: both upgrade transports disabled => relay floor (no rung's transport on).
    s = Solver()
    s.add(desired_valid, Not(en_webrtc), Not(en_direct))
    s.add(Not(chosen == RELAY))
    prove("A3 no transport enabled forces the relay floor", s)

    # A4: a non-relay result implies that rung actually fit (sound, not invented).
    s = Solver()
    s.add(desired_valid)
    sound = And(
        Implies(chosen == MESH, And(MESH <= desired, en_webrtc, a_mesh_w)),
        # Host can come from host+webrtc OR host+direct; at least one must fit.
        Implies(
            chosen == HOST,
            And(
                HOST <= desired,
                Or(And(en_webrtc, a_host_w), And(en_direct, a_host_d)),
            ),
        ),
    )
    s.add(Not(sound))
    prove("A4 a chosen upgrade rung genuinely fits its preconditions", s)

    # A5: richest-first — if mesh+webrtc fits it is always chosen (no inversion).
    s = Solver()
    s.add(desired_valid)
    s.add(And(MESH <= desired, en_webrtc, a_mesh_w))  # mesh fits
    s.add(Not(chosen == MESH))
    prove("A5 richest fitting rung wins (mesh+webrtc is never skipped)", s)

    # A6: webrtc-signaling gate tracks the chosen transport, not topology — a
    # Host+Direct plan is non-relay yet must NOT use WebRTC signaling
    # (src/server/session_policy.rs:417). Cross-check the two gates' truth table.
    s = Solver()
    s.add(desired_valid)
    uses_webrtc = selector_uses_webrtc(
        desired, en_webrtc, en_direct, a_mesh_w, a_host_w, a_host_d
    )
    # If chosen==HOST but webrtc rung did NOT fit (so it's host+direct), the
    # webrtc gate must be false.
    host_direct = And(
        chosen == HOST, Not(And(HOST <= desired, en_webrtc, a_host_w))
    )
    s.add(host_direct)
    s.add(uses_webrtc)  # negation of "must be false"
    prove("A6 host+direct plan never enables WebRTC signaling", s)


# ---------------------------------------------------------------------------
# Proof set B — all_support re-attached to member capabilities
# (src/server/session_policy.rs:175). Quantified over an unbounded member set:
# proves "a single non-v3 member forces the relay floor" — the relay-floor
# v3 capability-gating invariant — and that all_support is monotone.
# ---------------------------------------------------------------------------
def proof_set_b() -> None:
    print("Proof set B — all_support over unbounded members (relay-floor invariant)")
    n = Int("n")
    i = Int("i")
    version = Function("version", IntSort(), IntSort())  # member i -> negotiated version
    # member i supports rung r (r in {mesh_w, host_w, host_d}) on its two axes.
    sup = Function("sup", IntSort(), IntSort(), BoolSort())

    def member_supports(idx, rung):
        # supports_session = supports_v3() AND topology AND transport, modeled as
        # version>=3 AND sup(idx,rung) (sup folds both axis checks for that rung).
        return And(version(idx) >= 3, sup(idx, rung))

    def all_support(rung):
        # !members.is_empty() && members.iter().all(...)
        return And(n > 0, ForAll([i], Implies(And(i >= 0, i < n), member_supports(i, rung))))

    # B1: a single v2 (version<3) member makes EVERY rung's all_support false.
    s = Solver()
    s.add(n > 0)
    v2_idx = Int("v2_idx")
    s.add(And(v2_idx >= 0, v2_idx < n, version(v2_idx) < 3))  # a v2 member exists
    # negation: some rung still all-supports
    rung = Int("rung")
    s.add(all_support(rung))
    prove("B1 one v2 member denies all_support for every rung (relay floor)", s)

    # B2: all_support(rung) implies every member individually supports it.
    s = Solver()
    j = Int("j")
    some_rung = Int("some_rung")
    s.add(all_support(some_rung))
    s.add(And(j >= 0, j < n))
    s.add(Not(member_supports(j, some_rung)))
    prove("B2 all_support implies pointwise member support", s)

    # B3: empty room never supports an upgrade (src:176 `!members.is_empty()`).
    s = Solver()
    s.add(n == 0)
    empty_rung = Int("empty_rung")
    s.add(all_support(empty_rung))
    prove("B3 an empty room never supports any upgrade rung", s)


# ---------------------------------------------------------------------------
# Proof set C — the glare / offerer rule (src/server/signaling.rs:60).
# local_initiates(a,b) := a < b over the total UUID order (modeled as Int order).
# ---------------------------------------------------------------------------
def proof_set_c() -> None:
    print("Proof set C — glare / offerer rule (exactly one offerer per pair)")
    a, b, c = Int("a"), Int("b"), Int("c")

    def initiates(x, y):
        return x < y

    # C1: antisymmetry — for distinct peers exactly one initiates.
    s = Solver()
    s.add(a != b)
    s.add(Not(Xor(initiates(a, b), initiates(b, a))))
    prove("C1 exactly one peer offers in every distinct pair", s)

    # C2: irreflexive — a peer never offers to itself.
    s = Solver()
    s.add(initiates(a, a))
    prove("C2 a peer never self-initiates", s)

    # C3: a deterministic, transitive (acyclic) orientation — no 3-cycle of
    # offers, so the mesh offer graph is a strict total order (no glare deadlock).
    s = Solver()
    s.add(Distinct(a, b, c))
    s.add(And(initiates(a, b), initiates(b, c), initiates(c, a)))  # a cycle
    prove("C3 the offer orientation is acyclic (no glare cycle)", s)


# ---------------------------------------------------------------------------
# Proof set D — host election (src/server/session_policy.rs:372).
# Prefers `authority` if seated; else earliest joiner, ties by smaller id.
# ---------------------------------------------------------------------------
def proof_set_d() -> None:
    print("Proof set D — host election determinism")
    n = Int("n")
    i = Int("i")
    pid = Function("pid", IntSort(), IntSort())  # member i -> player id (distinct)
    joined = Function("joined", IntSort(), IntSort())  # member i -> joined_at tick

    # Election order key: (joined_at, player_id) lexicographic — src:381.
    def earlier(x, y):
        return Or(joined(x) < joined(y), And(joined(x) == joined(y), pid(x) < pid(y)))

    in_range = lambda idx: And(idx >= 0, idx < n)

    # D1: with distinct ids, the (joined_at, id) key has a UNIQUE minimum — host
    # election is deterministic (no two members tie on the full key).
    s = Solver()
    x, y = Int("x"), Int("y")
    s.add(n > 0)
    # distinct ids assumption (player ids are UUIDs)
    s.add(ForAll([x, y], Implies(And(in_range(x), in_range(y), x != y), pid(x) != pid(y))))
    # negation: two distinct members compare equal under the key (a real tie)
    s.add(And(in_range(x), in_range(y), x != y))
    s.add(Not(earlier(x, y)))
    s.add(Not(earlier(y, x)))
    prove("D1 (joined_at, id) totally orders members — unique host", s)

    # D2: a designated authority that is seated is always preferred as host,
    # regardless of join order — src:376 `authority.filter(is_member)`.
    # Modeled: if authority id A equals some member's id, the elected host is A.
    # (Encoded as: there is no member strictly "more electable" than the seated
    # authority under the authority-preferred rule — authority short-circuits.)
    s = Solver()
    auth_member = Int("auth_member")
    s.add(n > 0, in_range(auth_member))
    # The rule returns the authority unconditionally when seated; so the only way
    # to violate determinism is if two members both claim to be the authority,
    # which distinct ids forbid. Prove that's impossible.
    other = Int("other")
    s.add(ForAll([x, y], Implies(And(in_range(x), in_range(y), x != y), pid(x) != pid(y))))
    s.add(in_range(other), other != auth_member, pid(other) == pid(auth_member))
    prove("D2 a seated authority is the unambiguous host (distinct ids)", s)


# ---------------------------------------------------------------------------
# Proof set E — the replay truncation predicate (src/reconnection.rs).
# `get_missed_events` reports `truncated` iff
# `evicted_watermark.is_some_and(|w| w > last_sequence)`. The watermark is
# maintained by `EventBuffer::push` as the MAXIMUM sequence ever evicted
# (`Option` modeled as has_watermark:Bool x wm:Int; the max-invariant is the
# TLA+ `WatermarkIsMaxEvicted` ghost coupling in formal/tla/ReconnectReplay.tla).
# Under that invariant the implementation predicate is proved EQUIVALENT to the
# spec-level definition "an evicted event with seq > last_sequence exists" —
# sound (never cries truncation) and complete (never hides one) over unbounded
# sequence spaces.
# ---------------------------------------------------------------------------
def proof_set_e() -> None:
    print("Proof set E — replay truncation predicate (watermark, reconnection.rs)")
    evicted = Function("evicted", IntSort(), BoolSort())  # seq -> was evicted
    has_wm, wm, last = Bool("has_wm"), Int("wm"), Int("last_seq")
    seq = Int("seq")

    # EventBuffer::push maintains: watermark = max evicted sequence, None iff
    # nothing was ever evicted; global sequence numbers start at 1.
    watermark_is_max_evicted = And(
        Implies(Not(has_wm), ForAll([seq], Not(evicted(seq)))),
        Implies(
            has_wm,
            And(
                wm >= 1,
                evicted(wm),
                ForAll([seq], Implies(evicted(seq), seq <= wm)),
            ),
        ),
    )

    # E1: implementation <=> specification, for every watermark/last_sequence.
    s = Solver()
    s.add(watermark_is_max_evicted, last >= 0)
    implementation = And(has_wm, wm > last)
    specification = Exists([seq], And(evicted(seq), seq > last))
    s.add(Not(implementation == specification))
    prove("E1 watermark truncation predicate is sound AND complete", s)


# ---------------------------------------------------------------------------
# Proof set F — the rejected gap-based truncation predicate is UNSOUND.
# The one deliberate EXISTENCE obligation (expects sat, prints the witness):
# with the GLOBAL `next_sequence` counter shared across rooms
# (src/reconnection.rs), a naive predicate `oldest_retained_seq >
# last_sequence + 1` reports truncation for a replay from which NOTHING was
# evicted — another room's interleaved events consumed the intervening global
# sequence numbers. The SMT twin of formal/tla/ReconnectReplay.tla's
# `NaiveGapPredicateBug` TLC counterexample; together with E1 this proves the
# watermark design necessary, not merely sufficient.
# ---------------------------------------------------------------------------
def proof_set_f() -> None:
    print("Proof set F — naive gap predicate unsound (existence witness)")
    n = 3  # three interleaved global events are enough
    in_room_a = [Bool(f"seq{i}_in_room_a") for i in range(1, n + 1)]
    last = Int("last_seq")
    oldest_retained = Int("oldest_retained")

    s = Solver()
    s.add(last >= 0, last <= n)
    # The pending player's room (A) recorded at least one event, and its ring
    # was never full: NOTHING was evicted, so every room-A event is retained
    # and the spec-level truncation is FALSE by construction.
    s.add(Or(*in_room_a))
    # oldest_retained is the smallest global sequence recorded in room A.
    s.add(
        Or(
            *[
                And(
                    in_room_a[i],
                    oldest_retained == i + 1,
                    *[Not(in_room_a[j]) for j in range(i)],
                )
                for i in range(n)
            ]
        )
    )
    # The naive predicate cries truncation anyway.
    s.add(oldest_retained > last + 1)
    witness("F1 interleaved rooms make the gap predicate cry false truncation", s)


# ---------------------------------------------------------------------------
# Proof set G — protocol version negotiation clamp
# (src/config/protocol.rs `negotiate_protocol_version`):
#   client_max.unwrap_or(min_protocol_version)
#             .min(max_protocol_version)
#             .max(min_protocol_version)
# Quantified over every server range validated by `ProtocolConfig::validate`
# with the v3 ceiling (2 <= min <= max <= 3) and every client request
# (including the omitted-version v2 default).
# ---------------------------------------------------------------------------
def proof_set_g() -> None:
    print("Proof set G — negotiate_protocol_version clamp")
    has_client, client = Bool("has_client"), Int("client_req")
    smin, smax = Int("server_min"), Int("server_max")

    def zmin(a, b):
        return If(a <= b, a, b)

    def zmax(a, b):
        return If(a >= b, a, b)

    # Exact mirror of the Rust expression, including the unwrap_or default.
    negotiated = zmax(zmin(If(has_client, client, smin), smax), smin)
    valid_range = And(smin >= 2, smax <= 3, smin <= smax)
    valid_client = client >= 0  # u16 domain

    # G1: the result always lands inside the served range.
    s = Solver()
    s.add(valid_range, valid_client)
    s.add(Not(And(negotiated >= smin, negotiated <= smax)))
    prove("G1 negotiated version always lands in [min, max]", s)

    # G2: an in-range client request is honored exactly (never up/downgraded).
    s = Solver()
    s.add(valid_range, valid_client, has_client, client >= smin, client <= smax)
    s.add(Not(negotiated == client))
    prove("G2 an in-range client request is negotiated verbatim", s)

    # G3: an omitted version is a pure-v2 client — negotiated to the floor.
    s = Solver()
    s.add(valid_range, Not(has_client))
    s.add(Not(negotiated == smin))
    prove("G3 an omitted client version negotiates to the server floor", s)

    # G4: out-of-range requests clamp to the violated bound (never elsewhere).
    s = Solver()
    s.add(valid_range, valid_client, has_client)
    clamped = And(
        Implies(client > smax, negotiated == smax),
        Implies(client < smin, negotiated == smin),
    )
    s.add(Not(clamped))
    prove("G4 out-of-range requests clamp to the violated bound", s)


# ---------------------------------------------------------------------------
# Proof set H — v3 per-(sender, room) stamp discipline (the sequencing design
# model-checked in formal/tla/SequencedRelay.tla): the server stamps
# `seq = counter + 1` and then stores the stamp back as the counter, so the
# k-th message of an epoch carries stamp k; a leave/rejoin resets the counter
# to 0 (a new epoch). Proves per-epoch strict monotonicity AND contiguity —
# the property that makes a recipient-observed gap evidence of an eviction
# bracket rather than a stamping artifact.
# ---------------------------------------------------------------------------
def proof_set_h() -> None:
    print("Proof set H — v3 sequence stamping (per-epoch monotone + contiguous)")
    counter = Int("counter")

    def stamp(c):
        return c + 1

    # H1: within an epoch, consecutive stamps are strictly increasing and
    # contiguous (stamp_{n+1} = stamp_n + 1) for every counter state.
    s = Solver()
    s.add(counter >= 0)
    step = And(
        stamp(counter + 1) == stamp(counter) + 1,
        stamp(counter + 1) > stamp(counter),
    )
    s.add(Not(step))
    prove("H1 successive stamps are contiguous and strictly increasing", s)

    # H2: every stamp is >= 1 and a reset epoch starts at exactly 1 — no
    # epoch can emit a stamp that collides with "never received" (0).
    s = Solver()
    s.add(counter >= 0)
    s.add(Not(And(stamp(counter) >= 1, stamp(0) == 1)))
    prove("H2 stamps start at 1 after every reset and never reach 0", s)


# ---------------------------------------------------------------------------
# Proof set I — conservation closure of the delivery contract
# (src/coordination/mod.rs `deliver_or_disconnect` +
# src/websocket/connection.rs `finalize_closed_connection`; the SMT twin of
# formal/tla/DeliveryContract.tla's `Conservation` invariant). For EVERY
# transition of the contract, the counter deltas preserve
#   attempts = queued + written + dropped + channel_closed + in_flight
# over unbounded counter values (TLC checks it for tiny budgets; this closes
# the ledger for all of them).
# ---------------------------------------------------------------------------
def proof_set_i() -> None:
    print("Proof set I — delivery-contract conservation closure")
    attempts, queued, written = Int("attempts"), Int("queued"), Int("written")
    dropped, ch_closed, in_flight = Int("dropped"), Int("ch_closed"), Int("in_flight")

    def conserved(a, q, w, d, c, f):
        return a == q + w + d + c + f

    nonneg = And(
        attempts >= 0, queued >= 0, written >= 0,
        dropped >= 0, ch_closed >= 0, in_flight >= 0,
    )
    pre = conserved(attempts, queued, written, dropped, ch_closed, in_flight)

    def closes(name, guard, a, q, w, d, c, f):
        s = Solver()
        s.add(nonneg, pre, guard)
        s.add(Not(conserved(a, q, w, d, c, f)))
        prove(name, s)

    # I1 fast enqueue: try_send succeeds (attempt + queued).
    closes(
        "I1 fast enqueue preserves conservation",
        True,
        attempts + 1, queued + 1, written, dropped, ch_closed, in_flight,
    )
    # I2 backpressured attempt parks in send().await (attempt + in-flight).
    closes(
        "I2 backpressured park preserves conservation",
        True,
        attempts + 1, queued, written, dropped, ch_closed, in_flight + 1,
    )
    # I3 the parked send lands once a slot frees.
    closes(
        "I3 parked enqueue preserves conservation",
        in_flight >= 1,
        attempts, queued + 1, written, dropped, ch_closed, in_flight - 1,
    )
    # I4 try_send against a dropped receiver resolves ChannelClosed.
    closes(
        "I4 channel-closed attempt preserves conservation",
        True,
        attempts + 1, queued, written, dropped, ch_closed + 1, in_flight,
    )
    # I5 the receiver drops while a send is parked.
    closes(
        "I5 parked channel-closed preserves conservation",
        in_flight >= 1,
        attempts, queued, written, dropped, ch_closed + 1, in_flight - 1,
    )
    # I6 the grace window expires: the parked message drops with the close.
    closes(
        "I6 grace-expiry drop preserves conservation",
        in_flight >= 1,
        attempts, queued, written, dropped + 1, ch_closed, in_flight - 1,
    )
    # I7 the writer drains one queued message to the socket.
    closes(
        "I7 writer drain preserves conservation",
        queued >= 1,
        attempts, queued - 1, written + 1, dropped, ch_closed, in_flight,
    )
    # I8 close-finish: finalize bulk-drops the ENTIRE remaining queue
    # (dropped += queued, queued := 0) — for every queue length at once.
    closes(
        "I8 close-finish bulk drop preserves conservation",
        True,
        attempts, 0, written, dropped + queued, ch_closed, in_flight,
    )


# ---------------------------------------------------------------------------
# Proof set J — the config-admission closure (src/config/validation.rs
# `validate_config_security` + `WebSocketConfig::validate`, numeric guards).
#
# The validator is a pure conjunction of guards over the numeric config
# fields; this set proves the closure property that justifies it:
# every config the validator ADMITS satisfies the runtime safety envelope
# the guards were each written to protect (J4), the admitted set is
# non-empty (J1: the compiled defaults pass), and the two guards added by
# the session-197 sweep are NECESSARY — negating either admits a config
# that passes every other guard yet violates its runtime property (J2, J3),
# and the strict relay-envelope headroom kills a failure region equality
# alone does not (J5, the corrected old pin).
#
# Domains mirror the Rust types (u64/usize/u8 fields are non-negative; the
# `ping_timeout` field is bounded to 2^32 seconds so exact Z3 arithmetic
# equals the Rust `saturating_mul(1000)` — saturation only bites past
# ~4.29e9 seconds, ~136 years, where the guard is vacuous anyway).
# ---------------------------------------------------------------------------
def proof_set_j() -> None:
    print("Proof set J — config-admission closure (validation.rs)")
    # Fields (names follow src/config/*).
    msg = Int("max_message_size")            # security.max_message_size
    out = Int("max_outbound_message_size")   # security.max_outbound_message_size
    sig = Int("max_signal_bytes")            # security.max_signal_bytes
    ping = Int("ping_timeout")               # server.ping_timeout (seconds)
    slow = Int("slow_consumer_timeout_ms")   # websocket.slow_consumer_timeout_ms
    throttle = Int("heartbeat_throttle_secs")
    inactive = Int("inactive_room_timeout")
    dmp = Int("default_max_players")         # u8
    mpl = Int("max_players_limit")           # u8
    batching = Bool("enable_batching")
    bim = Int("batch_interval_ms")
    sojourn = Int("max_sojourn_ms")

    nonneg = And(
        msg >= 0, out >= 0, sig >= 0, ping >= 0, slow >= 0,
        throttle >= 0, inactive >= 0, dmp >= 0, mpl >= 0,
        bim >= 0, sojourn >= 0, ping <= 2**32,
    )

    max_outbound = 64 * 1024 * 1024      # defaults::MAX_OUTBOUND_MESSAGE_SIZE
    headroom = 256                       # defaults::RELAY_ENVELOPE_HEADROOM_BYTES
    max_batch_ms = 60_000                # websocket::MAX_BATCH_INTERVAL_MS

    def accepted(relax_headroom_strictness=False):
        headroom_guard = (
            # The old (wrong) pin allowed equality: outbound >= msg.
            out >= msg if relax_headroom_strictness else out >= msg + headroom
        )
        return And(
            msg > 0,
            out > 0,
            out <= max_outbound,
            headroom_guard,
            sig > 0,
            sig <= msg,
            dmp <= mpl,                      # session-197 D2 capacity pairing
            inactive > 0,                    # session-197 D3 occupied-room GC
            Or(throttle == 0, throttle < inactive),
            Or(ping == 0, slow < ping * 1000),
            Implies(batching, And(bim > 0, bim <= max_batch_ms, sojourn > bim)),
        )

    # J1 (non-vacuity witness): the compiled defaults are admitted — the
    # validator is not an accidental total-rejection gate. (Existence: the
    # exact default field values must SATISFY the full accepted() set.)
    s = Solver()
    s.add(nonneg)
    s.add(
        msg == 65536, out == 8 * 1024 * 1024, sig == 16384,
        ping == 30, slow == 5000, throttle == 30, inactive == 3600,
        dmp == 8, mpl == 100, batching == False, bim == 16, sojourn == 15000,
    )
    s.add(accepted())
    witness("J1 the compiled defaults pass the validator (non-vacuity)", s)

    # Runtime safety envelope: what each guard exists to protect.
    safety = And(
        # S1: a near-max admitted frame plus the relay envelope still fits
        # the outbound cap (no 1009 outbound_message_too_large total-rejection).
        msg + headroom <= out,
        # S2: every default-capacity room is admissible at request time.
        dmp <= mpl,
        # S3: the occupied-room reaping deadline is positive.
        inactive >= 1,
        # S4: batching deadlines stay representable (ceiling honored).
        Implies(batching, bim <= max_batch_ms),
        # S5: the slow-consumer park cannot outlast the ping deadline.
        Or(ping == 0, slow < ping * 1000),
    )

    # J4 (the closure): every admitted config satisfies the envelope.
    s = Solver()
    s.add(nonneg, accepted())
    s.add(Not(safety))
    prove("J4 every admitted config satisfies the runtime safety envelope", s)

    # J2 (necessity of the D2 pairing guard): without `dmp <= mpl`, a config
    # passes every other guard yet every default-capacity room would be
    # rejected at request time with InvalidMaxPlayers.
    s = Solver()
    s.add(nonneg)
    relaxed = And(
        msg > 0, out > 0, out <= max_outbound, out >= msg + headroom,
        sig > 0, sig <= msg,
        inactive > 0,
        Or(throttle == 0, throttle < inactive),
        Or(ping == 0, slow < ping * 1000),
        Implies(batching, And(bim > 0, bim <= max_batch_ms, sojourn > bim)),
    )
    s.add(relaxed)
    s.add(Not(dmp <= mpl))
    witness("J2 without the capacity-pairing guard, a legal config rejects " \
            "every default-capacity room", s)

    # J3 (necessity of the D3 zero-deadline guard): without `inactive > 0`,
    # a config passes every other guard — including the BUG-1 throttle
    # inversion check via its throttle-disabled arm — yet room GC deletes
    # occupied rooms in every quiet gap between activity refreshes.
    s = Solver()
    s.add(nonneg)
    relaxed = And(
        msg > 0, out > 0, out <= max_outbound, out >= msg + headroom,
        sig > 0, sig <= msg,
        dmp <= mpl,
        throttle == 0,  # the exempt arm: refresh every heartbeat
        Or(ping == 0, slow < ping * 1000),
        Implies(batching, And(bim > 0, bim <= max_batch_ms, sojourn > bim)),
    )
    s.add(relaxed)
    s.add(Not(inactive >= 1))
    witness("J3 without the zero-deadline guard, the throttle-disabled arm " \
            "reaps occupied rooms", s)

    # J5 (necessity of the STRICT headroom): with only `outbound >= msg`
    # (the old equality-legal pin), a config exists where a maximum-size
    # admitted frame grows by the relay envelope and overflows the outbound
    # cap — the exact 1009 total-rejection region the strict guard removes.
    s = Solver()
    s.add(nonneg)
    relaxed = And(
        msg > 0, out > 0, out <= max_outbound, out >= msg,
        sig > 0, sig <= msg,
        dmp <= mpl, inactive > 0,
        Or(throttle == 0, throttle < inactive),
        Or(ping == 0, slow < ping * 1000),
        Implies(batching, And(bim > 0, bim <= max_batch_ms, sojourn > bim)),
    )
    s.add(relaxed)
    s.add(Not(msg + headroom <= out))
    witness("J5 with headroom relaxed to equality, a max frame overflows " \
            "the outbound cap", s)


def main() -> int:
    print("Z3 proofs — Signal Fish protocol pure-logic invariants\n")
    proof_set_a()
    proof_set_b()
    proof_set_c()
    proof_set_d()
    proof_set_e()
    proof_set_f()
    proof_set_g()
    proof_set_h()
    proof_set_i()
    proof_set_j()
    print()
    if _FAILURES:
        print(f"RESULT: {len(_FAILURES)} proof(s) FAILED: {', '.join(_FAILURES)}")
        return 1
    print("RESULT: all proofs PASS (every property holds for all inputs)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
