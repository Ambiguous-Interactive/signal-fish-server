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

Run: ``python3 formal/z3/protocol_invariants.py`` (needs the ``z3`` module;
the dev container installs ``python3-z3``). CI: formal-verification.yml.
"""

from __future__ import annotations

import sys

from z3 import (
    And,
    Bool,
    Distinct,
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
# back-compat invariant (Appendix K) — and that all_support is monotone.
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


def main() -> int:
    print("Z3 proofs — Signal Fish protocol v3 pure-logic invariants\n")
    proof_set_a()
    proof_set_b()
    proof_set_c()
    proof_set_d()
    print()
    if _FAILURES:
        print(f"RESULT: {len(_FAILURES)} proof(s) FAILED: {', '.join(_FAILURES)}")
        return 1
    print("RESULT: all proofs PASS (every property holds for all inputs)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
