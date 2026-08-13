#!/usr/bin/env python3
"""Compile production-shaped sequenced-relay JSONL into a TLC replay bundle."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

SCHEMA = "signal-fish.sequenced-relay/v1"
MAX_INPUT_BYTES = 8 * 1024 * 1024
MAX_TRACES = 256
MAX_EVENTS_PER_TRACE = 4096
MAX_EPOCH = 4096
MAX_SEQUENCE = 4096
MAX_SENDER_COUNT = 4096
MAX_TLA_DENSE_CELLS = 65_536

TRACE_ID_RE = re.compile(r"[A-Za-z0-9._-]{1,128}\Z")
RECEIVER_RE = re.compile(r"r[1-9][0-9]*\Z")
SENDER_RE = re.compile(r"s[1-9][0-9]*\Z")
GAP_REASONS = {
    "latest_superseded",
    "latest_dropped_full",
    "volatile_dropped",
    "unsupported_format",
}
ACTION_FIELDS = {
    "ReceiverSnapshot": {"receiver", "sender_count"},
    "ReceiverBaseline": {"receiver", "sender", "epoch", "baseline_seq"},
    "Data": {"receiver", "sender", "epoch", "data_seq"},
    "DeliveryGap": {
        "receiver",
        "sender",
        "epoch",
        "from_seq",
        "to_seq",
        "reason",
    },
    "PlayerLeft": {"receiver", "sender", "epoch", "final_seq"},
    "PlayerJoined": {"receiver", "sender", "epoch", "baseline_seq"},
    "PlayerReconnected": {"receiver", "sender", "epoch", "baseline_seq"},
    "ReceiverReconnect": {"receiver", "sender_count"},
    "ReceiverReset": {"receiver"},
}


class TraceInputError(ValueError):
    """The JSONL corpus is outside the declared replay domain."""


@dataclass(frozen=True)
class Event:
    action: str
    receiver: str
    sender: str = ""
    epoch: int = 0
    value1: int = 0
    value2: int = 0
    reason: str = ""


@dataclass(frozen=True)
class Trace:
    trace_id: str
    events: tuple[Event, ...]
    pairs: tuple[tuple[str, str], ...]
    receivers: tuple[str, ...]


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    record: dict[str, Any] = {}
    for key, value in pairs:
        if key in record:
            raise TraceInputError(f"duplicate JSON field {key!r}")
        record[key] = value
    return record


def require_object(value: Any, line_number: int) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise TraceInputError(f"line {line_number}: every JSONL record must be an object")
    return value


def require_exact_keys(record: dict[str, Any], required: set[str], context: str) -> None:
    missing = required - record.keys()
    extra = record.keys() - required
    if missing:
        raise TraceInputError(f"{context}: missing field(s): {', '.join(sorted(missing))}")
    if extra:
        raise TraceInputError(f"{context}: unknown field(s): {', '.join(sorted(extra))}")


def require_uint(
    record: dict[str, Any],
    field: str,
    context: str,
    *,
    positive: bool,
    maximum: int,
) -> int:
    value = record[field]
    minimum = 1 if positive else 0
    if (
        not isinstance(value, int)
        or isinstance(value, bool)
        or value < minimum
        or value > maximum
    ):
        qualifier = "positive" if positive else "nonnegative"
        raise TraceInputError(
            f"{context}: {field} must be a {qualifier} integer no greater than {maximum}"
        )
    return value


def parse_event(record: dict[str, Any], context: str) -> Event:
    action = record.get("action")
    if not isinstance(action, str):
        raise TraceInputError(f"{context}: action must be a string")
    if action not in ACTION_FIELDS:
        raise TraceInputError(f"{context}: unknown action {action!r}")
    require_exact_keys(
        record,
        {"kind", "schema", "trace_id", "seq", "action"} | ACTION_FIELDS[action],
        context,
    )
    receiver = record["receiver"]
    if not isinstance(receiver, str) or RECEIVER_RE.fullmatch(receiver) is None:
        raise TraceInputError(f"{context}: receiver must match r[1-9][0-9]*")
    if action == "ReceiverReset":
        return Event(action, receiver)
    if action in {"ReceiverSnapshot", "ReceiverReconnect"}:
        return Event(
            action,
            receiver,
            value1=require_uint(
                record,
                "sender_count",
                context,
                positive=False,
                maximum=MAX_SENDER_COUNT,
            ),
        )

    sender = record["sender"]
    if not isinstance(sender, str) or SENDER_RE.fullmatch(sender) is None:
        raise TraceInputError(f"{context}: sender must match s[1-9][0-9]*")
    epoch = require_uint(
        record, "epoch", context, positive=True, maximum=MAX_EPOCH
    )
    if action == "Data":
        return Event(
            action,
            receiver,
            sender,
            epoch,
            require_uint(
                record,
                "data_seq",
                context,
                positive=True,
                maximum=MAX_SEQUENCE,
            ),
        )
    if action == "DeliveryGap":
        from_seq = require_uint(
            record, "from_seq", context, positive=True, maximum=MAX_SEQUENCE
        )
        to_seq = require_uint(
            record, "to_seq", context, positive=True, maximum=MAX_SEQUENCE
        )
        if from_seq > to_seq:
            raise TraceInputError(f"{context}: from_seq must not exceed to_seq")
        reason = record["reason"]
        if not isinstance(reason, str) or reason not in GAP_REASONS:
            raise TraceInputError(
                f"{context}: reason must be one of {', '.join(sorted(GAP_REASONS))}"
            )
        return Event(action, receiver, sender, epoch, from_seq, to_seq, reason)
    field = "final_seq" if action == "PlayerLeft" else "baseline_seq"
    return Event(
        action,
        receiver,
        sender,
        epoch,
        require_uint(
            record, field, context, positive=False, maximum=MAX_SEQUENCE
        ),
    )


def validate_snapshot_blocks(events: list[Event], trace_id: str) -> None:
    present: dict[str, set[str]] = {}
    high_water: dict[tuple[str, str], tuple[int, int]] = {}
    view_open: set[str] = set()
    index = 0
    while index < len(events):
        event = events[index]
        if event.action == "ReceiverReset":
            if event.receiver not in view_open:
                raise TraceInputError(
                    f"trace {trace_id!r} event {index + 1}: ReceiverReset requires an active receiver view"
                )
            view_open.remove(event.receiver)
            present.pop(event.receiver, None)
            high_water = {
                pair: stamp
                for pair, stamp in high_water.items()
                if pair[0] != event.receiver
            }
            index += 1
            continue
        if event.action in {"ReceiverSnapshot", "ReceiverReconnect"}:
            if event.action == "ReceiverSnapshot" and event.receiver in view_open:
                raise TraceInputError(
                    f"trace {trace_id!r} event {index + 1}: ReceiverSnapshot requires no active receiver view"
                )
            if event.action == "ReceiverReconnect" and event.receiver not in view_open:
                raise TraceInputError(
                    f"trace {trace_id!r} event {index + 1}: ReceiverReconnect requires an active receiver view"
                )
            actual: list[str] = []
            cursor = index + 1
            while cursor < len(events):
                baseline = events[cursor]
                if baseline.action != "ReceiverBaseline" or baseline.receiver != event.receiver:
                    break
                actual.append(baseline.sender)
                cursor += 1
            if len(actual) != len(set(actual)):
                raise TraceInputError(
                    f"trace {trace_id!r} event {index + 1}: {event.action} baselines contain a duplicate sender"
                )
            if len(actual) != event.value1:
                raise TraceInputError(
                    f"trace {trace_id!r} event {index + 1}: {event.action} declares sender_count "
                    f"{event.value1} but is followed by {len(actual)} immediate ReceiverBaseline event(s)"
                )
            if event.action == "ReceiverReconnect":
                for baseline in events[index + 1 : cursor]:
                    previous = high_water.get((event.receiver, baseline.sender))
                    if previous is None:
                        continue
                    previous_epoch, previous_seq = previous
                    if baseline.epoch < previous_epoch or (
                        baseline.epoch == previous_epoch
                        and baseline.value1 < previous_seq
                    ):
                        raise TraceInputError(
                            f"trace {trace_id!r} event {index + 1}: ReceiverReconnect baseline for "
                            f"{baseline.sender} moved backward from epoch/seq "
                            f"{previous_epoch}/{previous_seq} to {baseline.epoch}/{baseline.value1}"
                        )
            elif event.action == "ReceiverSnapshot":
                high_water = {
                    pair: stamp
                    for pair, stamp in high_water.items()
                    if pair[0] != event.receiver
                }
            for baseline in events[index + 1 : cursor]:
                high_water[(event.receiver, baseline.sender)] = (
                    baseline.epoch,
                    baseline.value1,
                )
            view_open.add(event.receiver)
            present[event.receiver] = set(actual)
            index = cursor
            continue
        if event.action == "ReceiverBaseline":
            raise TraceInputError(
                f"trace {trace_id!r} event {index + 1}: ReceiverBaseline is legal only inside an immediate counted snapshot block"
            )
        if event.receiver not in view_open:
            raise TraceInputError(
                f"trace {trace_id!r} event {index + 1}: {event.action} requires an active receiver view"
            )
        if event.action in {"PlayerJoined", "PlayerReconnected"}:
            present.setdefault(event.receiver, set()).add(event.sender)
        elif event.action == "PlayerLeft":
            present.setdefault(event.receiver, set()).discard(event.sender)
        if event.sender:
            pair = (event.receiver, event.sender)
            previous_epoch, previous_seq = high_water.get(pair, (0, 0))
            sequence = event.value2 if event.action == "DeliveryGap" else event.value1
            if event.epoch > previous_epoch:
                high_water[pair] = (event.epoch, sequence)
            elif event.epoch == previous_epoch:
                high_water[pair] = (event.epoch, max(previous_seq, sequence))
        index += 1


def parse_corpus(path: Path) -> list[Trace]:
    size = path.stat().st_size
    if size == 0:
        raise TraceInputError("trace input is empty")
    if size > MAX_INPUT_BYTES:
        raise TraceInputError(f"trace input is {size} bytes; limit is {MAX_INPUT_BYTES} bytes")

    traces: list[Trace] = []
    active_id: str | None = None
    active_events: list[Event] = []
    seen_ids: set[str] = set()
    with path.open("r", encoding="utf-8") as source:
        for line_number, raw_line in enumerate(source, start=1):
            if not raw_line.strip():
                raise TraceInputError(f"line {line_number}: blank JSONL records are forbidden")
            try:
                record = require_object(
                    json.loads(raw_line, object_pairs_hook=reject_duplicate_keys),
                    line_number,
                )
            except json.JSONDecodeError as error:
                raise TraceInputError(f"line {line_number}: invalid JSON: {error.msg}") from error
            if record.get("schema") != SCHEMA:
                raise TraceInputError(f"line {line_number}: schema must be exactly {SCHEMA!r}")
            kind = record.get("kind")
            context = f"line {line_number} ({kind or 'missing kind'})"
            if kind == "header":
                require_exact_keys(
                    record,
                    {"kind", "schema", "trace_id", "protocol_version"},
                    context,
                )
                if active_id is not None:
                    raise TraceInputError(f"{context}: previous trace has no footer")
                trace_id = record["trace_id"]
                if not isinstance(trace_id, str) or TRACE_ID_RE.fullmatch(trace_id) is None:
                    raise TraceInputError(
                        f"{context}: trace_id must match [A-Za-z0-9._-]{{1,128}}"
                    )
                if trace_id in seen_ids:
                    raise TraceInputError(f"{context}: duplicate trace_id {trace_id!r}")
                if record["protocol_version"] != 3 or isinstance(record["protocol_version"], bool):
                    raise TraceInputError(f"{context}: protocol_version must be exactly 3")
                active_id = trace_id
                active_events = []
            elif kind == "event":
                if active_id is None:
                    raise TraceInputError(f"{context}: event appears outside a trace")
                if record.get("trace_id") != active_id or not isinstance(record.get("trace_id"), str):
                    raise TraceInputError(f"{context}: trace_id does not match active header")
                expected_seq = len(active_events) + 1
                seq = record.get("seq")
                if not isinstance(seq, int) or isinstance(seq, bool) or seq != expected_seq:
                    raise TraceInputError(f"{context}: seq must be contiguous; expected {expected_seq}")
                if len(active_events) >= MAX_EVENTS_PER_TRACE:
                    raise TraceInputError(f"{context}: trace exceeds {MAX_EVENTS_PER_TRACE} events")
                active_events.append(parse_event(record, context))
            elif kind == "footer":
                require_exact_keys(
                    record,
                    {"kind", "schema", "trace_id", "event_count"},
                    context,
                )
                if active_id is None:
                    raise TraceInputError(f"{context}: footer appears outside a trace")
                if record["trace_id"] != active_id or not isinstance(record["trace_id"], str):
                    raise TraceInputError(f"{context}: trace_id does not match active header")
                count = record["event_count"]
                if not isinstance(count, int) or isinstance(count, bool) or count != len(active_events):
                    raise TraceInputError(f"{context}: event_count must equal {len(active_events)}")
                if not active_events:
                    raise TraceInputError(f"{context}: an empty trace is not useful")
                validate_snapshot_blocks(active_events, active_id)
                pairs = sorted({(event.receiver, event.sender) for event in active_events if event.sender})
                receivers = sorted({event.receiver for event in active_events})
                traces.append(Trace(active_id, tuple(active_events), tuple(pairs), tuple(receivers)))
                if len(traces) > MAX_TRACES:
                    raise TraceInputError(f"corpus exceeds {MAX_TRACES} traces")
                seen_ids.add(active_id)
                active_id = None
                active_events = []
            else:
                raise TraceInputError(f"{context}: kind must be header, event, or footer")
    if active_id is not None:
        raise TraceInputError("final trace has no footer")
    if not traces:
        raise TraceInputError("trace corpus contains no complete traces")
    max_epoch = max(event.epoch for trace in traces for event in trace.events)
    total_pairs = sum(len(trace.pairs) for trace in traces)
    dense_cells = max(1, max_epoch) * total_pairs
    if dense_cells > MAX_TLA_DENSE_CELLS:
        raise TraceInputError(
            "corpus exceeds the formal-domain complexity budget: "
            f"{total_pairs} sender/receiver pair(s) x max epoch {max(1, max_epoch)} "
            f"= {dense_cells} dense cells; limit is {MAX_TLA_DENSE_CELLS}"
        )
    return traces


def seed_trace(trace: Trace, bug: str) -> Trace:
    events = list(trace.events)
    last: dict[tuple[str, str, int], int] = {}
    last_data: dict[tuple[str, str, int], int] = {}
    highest_epoch: dict[tuple[str, str], int] = {}
    for index, event in enumerate(events):
        if event.action in {"ReceiverReconnect", "ReceiverReset"}:
            last = {key: value for key, value in last.items() if key[0] != event.receiver}
            last_data = {
                key: value for key, value in last_data.items() if key[0] != event.receiver
            }
            highest_epoch = {
                key: value for key, value in highest_epoch.items() if key[0] != event.receiver
            }
            continue
        stream = (event.receiver, event.sender, event.epoch)
        pair = (event.receiver, event.sender)
        if event.action in {"ReceiverBaseline", "PlayerJoined", "PlayerReconnected"}:
            previous_epoch = highest_epoch.get(pair, 0)
            if (
                bug == "backward-epoch"
                and event.action in {"PlayerJoined", "PlayerReconnected"}
                and previous_epoch > 0
                and event.epoch > previous_epoch
            ):
                events[index] = replace(event, epoch=max(1, previous_epoch - 1))
                return replace(trace, events=tuple(events))
            highest_epoch[pair] = max(previous_epoch, event.epoch)
            last[stream] = event.value1
        elif event.action == "Data":
            previous_seq = last.get(stream)
            previous_data_seq = last_data.get(stream)
            if previous_data_seq is not None and bug == "duplicate-data":
                events[index] = replace(event, value1=previous_data_seq)
                return replace(trace, events=tuple(events))
            if previous_seq is not None and bug == "silent-gap":
                if event.value1 == MAX_SEQUENCE:
                    raise TraceInputError(
                        f"trace {trace.trace_id!r} cannot seed {bug!r}: sequence is at the input limit"
                    )
                events[index] = replace(event, value1=event.value1 + 1)
                return replace(trace, events=tuple(events))
            last[stream] = event.value1
            last_data[stream] = event.value1

    if bug == "late-lifecycle":
        for left_index, event in enumerate(events):
            if event.action != "PlayerLeft":
                continue
            for data_index in range(left_index + 1, len(events)):
                data = events[data_index]
                if (
                    data.action == "Data"
                    and data.receiver == event.receiver
                    and data.sender == event.sender
                    and data.epoch > event.epoch
                ):
                    delayed = events.pop(left_index)
                    events.insert(data_index, delayed)
                    return replace(trace, events=tuple(events))
    raise TraceInputError(f"trace {trace.trace_id!r} cannot seed {bug!r}")


def tla_string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def tla_set(values: list[str]) -> str:
    return "{" + ", ".join(values) + "}"


def generate_input(traces: list[Trace]) -> str:
    ids = [tla_string(trace.trace_id) for trace in traces]
    max_epoch = max(event.epoch for trace in traces for event in trace.events)
    max_sequence = max(max(event.value1, event.value2) for trace in traces for event in trace.events)
    lines = [
        "-------------------- MODULE GeneratedSequencedRelayTrace --------------------",
        "EXTENDS Naturals, Sequences",
        "",
        f"TraceIds == {tla_set(ids)}",
        f"MaxEpoch == {max(1, max_epoch)}",
        f"MaxSequence == {max(1, max_sequence)}",
        "",
    ]
    for name, render in [
        (
            "TracePairs",
            lambda trace: tla_set(
                [f"<<{tla_string(receiver)}, {tla_string(sender)}>>" for receiver, sender in trace.pairs]
            ),
        ),
        ("TraceReceivers", lambda trace: tla_set([tla_string(value) for value in trace.receivers])),
    ]:
        lines.append(f"{name} == [id \\in TraceIds |-> CASE")
        for index, trace in enumerate(traces):
            prefix = "    " if index == 0 else " [] "
            lines.append(f"{prefix}id = {tla_string(trace.trace_id)} -> {render(trace)}")
        lines.extend(["]", ""])
    lines.append("Traces == [id \\in TraceIds |-> CASE")
    for index, trace in enumerate(traces):
        rendered = []
        for event in trace.events:
            rendered.append(
                "[action |-> "
                + tla_string(event.action)
                + ", receiver |-> "
                + tla_string(event.receiver)
                + ", sender |-> "
                + tla_string(event.sender)
                + f", epoch |-> {event.epoch}, value1 |-> {event.value1}, value2 |-> {event.value2}"
                + ", reason |-> "
                + tla_string(event.reason)
                + "]"
            )
        sequence = "<<" + ",\n            ".join(rendered) + ">>"
        prefix = "    " if index == 0 else " [] "
        lines.append(f"{prefix}id = {tla_string(trace.trace_id)} -> {sequence}")
    lines.extend(["]", "", "=============================================================================", ""])
    return "\n".join(lines)


def write_bundle(output_dir: Path, traces: list[Trace]) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    if any(output_dir.iterdir()):
        raise TraceInputError(f"output directory must be empty: {output_dir}")
    repo_root = Path(__file__).resolve().parent.parent
    shutil.copyfile(repo_root / "formal/tla/SequencedRelayTrace.tla", output_dir / "SequencedRelayTrace.tla")
    (output_dir / "GeneratedSequencedRelayTrace.tla").write_text(generate_input(traces), encoding="utf-8")
    (output_dir / "SequencedRelayTrace_Generated.cfg").write_text(
        "SPECIFICATION TraceSpec\n"
        "INVARIANTS\n"
        "    TypeOK\n"
        "    SequencedRelayRefinement\n",
        encoding="utf-8",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path, help="sequenced-relay JSONL corpus")
    parser.add_argument("--output-dir", required=True, type=Path, help="empty bundle directory")
    parser.add_argument(
        "--seeded-bug",
        choices=["duplicate-data", "silent-gap", "backward-epoch", "late-lifecycle"],
        help="deterministically corrupt each trace with the selected contract bug",
    )
    args = parser.parse_args()
    try:
        traces = parse_corpus(args.input)
        if args.seeded_bug:
            seeded_traces = []
            seeded = False
            for trace in traces:
                if seeded:
                    seeded_traces.append(trace)
                    continue
                try:
                    seeded_traces.append(seed_trace(trace, args.seeded_bug))
                    seeded = True
                except TraceInputError:
                    seeded_traces.append(trace)
            if not seeded:
                raise TraceInputError(
                    f"corpus cannot seed {args.seeded_bug!r}; add a trace containing the required lifecycle/data shape"
                )
            traces = seeded_traces
        write_bundle(args.output_dir, traces)
    except (OSError, TraceInputError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    event_count = sum(len(trace.events) for trace in traces)
    mode = args.seeded_bug or "positive"
    print(f"Generated {mode} TLC bundle: {len(traces)} trace(s), {event_count} event(s) -> {args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
