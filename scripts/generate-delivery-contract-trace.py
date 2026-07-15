#!/usr/bin/env python3
"""Compile Signal Fish delivery JSONL into a bounded TLC replay bundle."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

SCHEMA = "signal-fish.delivery-contract/v1"
MAX_INPUT_BYTES = 8 * 1024 * 1024
MAX_TRACES = 256
MAX_EVENTS_PER_TRACE = 4096

START_ACTIONS = {"SendFast", "SendFull", "SendChannelClosed"}
PARKED_ACTIONS = {"ParkedEnqueue", "GraceExpired", "ParkedChannelClosed"}
WRITE_START_ACTIONS = {"WriterStart", "CloseFlushStart"}
WRITE_FINISH_ACTIONS = {"WriterDrain", "CloseFlushDrain"}
PLAIN_ACTIONS = {
    "LifecycleClose",
    "QueueClose",
    "CloseFinish",
}
CORRELATED_ACTIONS = START_ACTIONS | PARKED_ACTIONS | WRITE_START_ACTIONS | WRITE_FINISH_ACTIONS
ALLOWED_ACTIONS = CORRELATED_ACTIONS | PLAIN_ACTIONS
TRACE_ID_RE = re.compile(r"[A-Za-z0-9._-]{1,128}\Z")


class TraceInputError(ValueError):
    """The JSONL corpus is not a valid trace-validation input."""


@dataclass(frozen=True)
class Event:
    action: str
    delivery_id: int | None


@dataclass(frozen=True)
class Trace:
    trace_id: str
    capacity: int
    events: tuple[Event, ...]
    senders: tuple[int, ...]


def require_object(value: Any, line_number: int) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise TraceInputError(f"line {line_number}: every JSONL record must be an object")
    return value


def require_exact_keys(
    record: dict[str, Any], required: set[str], optional: set[str], context: str
) -> None:
    missing = required - record.keys()
    extra = record.keys() - required - optional
    if missing:
        raise TraceInputError(f"{context}: missing field(s): {', '.join(sorted(missing))}")
    if extra:
        raise TraceInputError(f"{context}: unknown field(s): {', '.join(sorted(extra))}")


def parse_corpus(path: Path) -> list[Trace]:
    size = path.stat().st_size
    if size == 0:
        raise TraceInputError("trace input is empty")
    if size > MAX_INPUT_BYTES:
        raise TraceInputError(
            f"trace input is {size} bytes; limit is {MAX_INPUT_BYTES} bytes"
        )

    traces: list[Trace] = []
    active_header: tuple[str, int] | None = None
    active_events: list[Event] = []
    active_attempts: dict[int, str] = {}
    seen_trace_ids: set[str] = set()

    with path.open("r", encoding="utf-8") as source:
        for line_number, raw_line in enumerate(source, start=1):
            if not raw_line.strip():
                raise TraceInputError(f"line {line_number}: blank JSONL records are forbidden")
            try:
                record = require_object(json.loads(raw_line), line_number)
            except json.JSONDecodeError as error:
                raise TraceInputError(
                    f"line {line_number}: invalid JSON: {error.msg}"
                ) from error

            if record.get("schema") != SCHEMA:
                raise TraceInputError(
                    f"line {line_number}: schema must be exactly {SCHEMA!r}"
                )
            kind = record.get("kind")
            context = f"line {line_number} ({kind or 'missing kind'})"

            if kind == "header":
                require_exact_keys(
                    record,
                    {"kind", "schema", "trace_id", "queue_kind", "queue_capacity"},
                    set(),
                    context,
                )
                if active_header is not None:
                    raise TraceInputError(f"{context}: previous trace has no footer")
                trace_id = record["trace_id"]
                if record["queue_kind"] != "v2_legacy_reliable_fifo":
                    raise TraceInputError(
                        f"{context}: queue_kind must be exactly 'v2_legacy_reliable_fifo'"
                    )
                capacity = record["queue_capacity"]
                if not isinstance(trace_id, str) or TRACE_ID_RE.fullmatch(trace_id) is None:
                    raise TraceInputError(
                        f"{context}: trace_id must match [A-Za-z0-9._-]{{1,128}}"
                    )
                if trace_id in seen_trace_ids:
                    raise TraceInputError(f"{context}: duplicate trace_id {trace_id!r}")
                if not isinstance(capacity, int) or isinstance(capacity, bool) or capacity <= 0:
                    raise TraceInputError(f"{context}: queue_capacity must be a positive integer")
                if capacity > 65536:
                    raise TraceInputError(f"{context}: queue_capacity exceeds 65536")
                active_header = (trace_id, capacity)
                active_events = []
                active_attempts = {}
                continue

            if kind == "event":
                require_exact_keys(
                    record,
                    {"kind", "schema", "trace_id", "seq", "action"},
                    {"delivery_id", "detail"},
                    context,
                )
                if active_header is None:
                    raise TraceInputError(f"{context}: event appears outside a trace")
                trace_id, _ = active_header
                if not isinstance(record["trace_id"], str) or record["trace_id"] != trace_id:
                    raise TraceInputError(f"{context}: trace_id does not match active header")
                expected_seq = len(active_events) + 1
                seq = record["seq"]
                if not isinstance(seq, int) or isinstance(seq, bool) or seq != expected_seq:
                    raise TraceInputError(
                        f"{context}: seq must be contiguous; expected {expected_seq}"
                    )
                if len(active_events) >= MAX_EVENTS_PER_TRACE:
                    raise TraceInputError(
                        f"{context}: trace exceeds {MAX_EVENTS_PER_TRACE} events"
                    )
                action = record["action"]
                if not isinstance(action, str):
                    raise TraceInputError(f"{context}: action must be a string")
                if "detail" in record and not isinstance(record["detail"], str):
                    raise TraceInputError(f"{context}: detail must be a string")
                if action == "Unsupported":
                    detail = record.get("detail", "unspecified")
                    raise TraceInputError(
                        f"{context}: implementation event is outside "
                        f"v2_legacy_reliable_fifo ({detail})"
                    )
                if action not in ALLOWED_ACTIONS:
                    raise TraceInputError(f"{context}: unknown action {action!r}")
                delivery_id = record.get("delivery_id")
                if action in CORRELATED_ACTIONS:
                    if (
                        not isinstance(delivery_id, int)
                        or isinstance(delivery_id, bool)
                        or delivery_id <= 0
                    ):
                        raise TraceInputError(
                            f"{context}: {action} requires a positive delivery_id"
                        )
                elif delivery_id is not None:
                    raise TraceInputError(f"{context}: {action} must not carry delivery_id")

                if action in START_ACTIONS:
                    if delivery_id in active_attempts:
                        raise TraceInputError(
                            f"{context}: delivery_id {delivery_id} starts more than once"
                        )
                    active_attempts[delivery_id] = action
                elif action in PARKED_ACTIONS:
                    if active_attempts.get(delivery_id) != "SendFull":
                        raise TraceInputError(
                            f"{context}: {action} must resolve a preceding SendFull"
                        )
                    active_attempts[delivery_id] = (
                        "enqueued" if action == "ParkedEnqueue" else f"resolved:{action}"
                    )
                elif action in WRITE_START_ACTIONS:
                    if active_attempts.get(delivery_id) not in {"SendFast", "enqueued"}:
                        raise TraceInputError(
                            f"{context}: {action} requires a queued delivery_id"
                        )
                    active_attempts[delivery_id] = f"in_flight:{action}"
                elif action in WRITE_FINISH_ACTIONS:
                    expected_start = {
                        "WriterDrain": "WriterStart",
                        "CloseFlushDrain": "CloseFlushStart",
                    }[action]
                    if active_attempts.get(delivery_id) != f"in_flight:{expected_start}":
                        raise TraceInputError(
                            f"{context}: {action} requires an in-flight delivery_id "
                            f"started by {expected_start}"
                        )
                    active_attempts[delivery_id] = "resolved:written"

                active_events.append(Event(action, delivery_id))
                continue

            if kind == "footer":
                require_exact_keys(
                    record,
                    {"kind", "schema", "trace_id", "event_count"},
                    set(),
                    context,
                )
                if active_header is None:
                    raise TraceInputError(f"{context}: footer appears outside a trace")
                trace_id, capacity = active_header
                if not isinstance(record["trace_id"], str) or record["trace_id"] != trace_id:
                    raise TraceInputError(f"{context}: trace_id does not match active header")
                event_count = record["event_count"]
                if (
                    not isinstance(event_count, int)
                    or isinstance(event_count, bool)
                    or event_count != len(active_events)
                ):
                    raise TraceInputError(
                        f"{context}: event_count must equal {len(active_events)}"
                    )
                if not active_events:
                    raise TraceInputError(f"{context}: an empty trace is not useful")
                senders = tuple(sorted(active_attempts))
                if not senders:
                    raise TraceInputError(f"{context}: trace contains no delivery attempts")
                traces.append(Trace(trace_id, capacity, tuple(active_events), senders))
                if len(traces) > MAX_TRACES:
                    raise TraceInputError(f"corpus exceeds {MAX_TRACES} traces")
                seen_trace_ids.add(trace_id)
                active_header = None
                active_events = []
                active_attempts = {}
                continue

            raise TraceInputError(f"{context}: kind must be header, event, or footer")

    if active_header is not None:
        raise TraceInputError("final trace has no footer")
    if not traces:
        raise TraceInputError("trace corpus contains no complete traces")
    return traces


def tla_string(value: str) -> str:
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def tla_set(values: list[str]) -> str:
    return "{" + ", ".join(values) + "}"


def generate_input(traces: list[Trace]) -> str:
    ids = [tla_string(trace.trace_id) for trace in traces]
    lines = [
        "------------------ MODULE GeneratedDeliveryContractTrace ------------------",
        "EXTENDS Naturals, Sequences",
        "",
        f"TraceIds == {tla_set(ids)}",
        "",
        "TraceCapacity == [id \\in TraceIds |-> CASE",
    ]
    for index, trace in enumerate(traces):
        prefix = "    " if index == 0 else " [] "
        lines.append(f"{prefix}id = {tla_string(trace.trace_id)} -> {trace.capacity}")
    lines.extend(["]", "", "TraceSenders == [id \\in TraceIds |-> CASE"])
    for index, trace in enumerate(traces):
        senders = tla_set([tla_string(f"d{sender}") for sender in trace.senders])
        prefix = "    " if index == 0 else " [] "
        lines.append(f"{prefix}id = {tla_string(trace.trace_id)} -> {senders}")
    lines.extend(["]", "", "Traces == [id \\in TraceIds |-> CASE"])
    for index, trace in enumerate(traces):
        events = []
        for event in trace.events:
            sender = "" if event.delivery_id is None else f"d{event.delivery_id}"
            events.append(
                "[action |-> "
                + tla_string(event.action)
                + ", sender |-> "
                + tla_string(sender)
                + "]"
            )
        sequence = "<<" + ",\n            ".join(events) + ">>"
        prefix = "    " if index == 0 else " [] "
        lines.append(f"{prefix}id = {tla_string(trace.trace_id)} -> {sequence}")
    lines.extend(["]", "", "=============================================================================", ""])
    return "\n".join(lines)


def write_bundle(output_dir: Path, traces: list[Trace], seeded_bug: bool) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    if any(output_dir.iterdir()):
        raise TraceInputError(f"output directory must be empty: {output_dir}")
    repo_root = Path(__file__).resolve().parent.parent
    shutil.copyfile(
        repo_root / "formal/tla/DeliveryContractTrace.tla",
        output_dir / "DeliveryContractTrace.tla",
    )
    (output_dir / "GeneratedDeliveryContractTrace.tla").write_text(
        generate_input(traces), encoding="utf-8"
    )
    (output_dir / "DeliveryContractTrace_Generated.cfg").write_text(
        "SPECIFICATION TraceSpec\n"
        f"CONSTANT TraceActionBug = {'TRUE' if seeded_bug else 'FALSE'}\n"
        "INVARIANTS\n"
        "    TypeOK\n"
        "    Conservation\n"
        "    NoSilentLoss\n"
        "    ClosedQueueEmpty\n",
        encoding="utf-8",
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path, help="JSONL trace corpus")
    parser.add_argument("--output-dir", required=True, type=Path, help="empty bundle directory")
    parser.add_argument(
        "--seeded-bug",
        action="store_true",
        help="replace step 1 with WriterDrain; TLC must deadlock",
    )
    parser.add_argument(
        "--require-production-socket",
        action="store_true",
        help="require one socket-* trace to cover writer and teardown hooks",
    )
    args = parser.parse_args()
    try:
        traces = parse_corpus(args.input)
        if args.require_production_socket:
            required = {"WriterStart", "WriterDrain", "QueueClose", "CloseFinish"}
            covered = any(
                trace.trace_id.startswith("socket-")
                and required.issubset({event.action for event in trace.events})
                for trace in traces
            )
            if not covered:
                raise TraceInputError(
                    "corpus lacks a socket-* trace covering WriterStart, WriterDrain, "
                    "QueueClose, and CloseFinish"
                )
        write_bundle(args.output_dir, traces, args.seeded_bug)
    except (OSError, TraceInputError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 2
    event_count = sum(len(trace.events) for trace in traces)
    mode = "seeded-negative" if args.seeded_bug else "positive"
    print(
        f"Generated {mode} TLC bundle: {len(traces)} trace(s), "
        f"{event_count} event(s) -> {args.output_dir}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
