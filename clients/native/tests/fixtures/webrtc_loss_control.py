"""Run the pinned Godot driver through an observed local UDP loss path."""

import argparse
import importlib.util
import ipaddress
import json
import select
import socket
import sys
import threading
import time
from pathlib import Path


def without_direct_candidates(sdp: str) -> tuple[str, int]:
    lines = sdp.splitlines(keepends=True)
    retained = [
        line
        for line in lines
        if not line.startswith(("a=candidate:", "a=end-of-candidates"))
    ]
    return "".join(retained), len(lines) - len(retained)


class UdpLossPath:
    def __init__(self, drops: int, minimum_application_bytes: int = 1000) -> None:
        self.drops = drops
        self.minimum_application_bytes = minimum_application_bytes
        self.started_ns = time.monotonic_ns()
        self.armed_ns = None
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.armed = False
        self.targets: dict[str, tuple[str, int]] = {}
        self.candidates: dict[str, tuple[str, int]] = {}
        self.sockets = {}
        self.received = {"native": 0, "godot": 0}
        self.forwarded = {"native": 0, "godot": 0}
        self.dropped = {"native": 0, "godot": 0}
        self.ignored_candidates = {"native": 0, "godot": 0}
        self.sdp_candidates_removed = {"native": 0, "godot": 0}
        self.drop_records: list[dict[str, object]] = []
        self.errors: list[str] = []
        for seat in ("native", "godot"):
            connection = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            connection.bind(("127.0.0.1", 0))
            connection.setblocking(False)
            self.sockets[seat] = connection
        self.thread = threading.Thread(target=self._relay, daemon=True)
        self.thread.start()

    def candidate(self, seat: str, encoded: str) -> str | None:
        if not encoded.strip():
            return encoded
        try:
            candidate = json.loads(encoded)
            bare = False
        except json.JSONDecodeError:
            candidate = {"candidate": encoded}
            bare = True
        fields = candidate.get("candidate", "").split()
        if not fields:
            return encoded
        if len(fields) < 8:
            raise ValueError("loss control candidate is incomplete")
        if fields[2].lower() != "udp" or fields[7] != "host":
            with self.lock:
                self.ignored_candidates[seat] += 1
            return None
        try:
            address = ipaddress.ip_address(fields[4])
        except ValueError:
            if not fields[4].endswith(".local"):
                raise
            address = None
        target = (fields[4], int(fields[5]))
        other = "godot" if seat == "native" else "native"
        with self.lock:
            if seat in self.candidates or (
                address is not None and address.version != 4
            ):
                self.ignored_candidates[seat] += 1
                return None
            self.candidates[seat] = target
            if address is not None:
                self.targets[seat] = target
            fields[4], port = self.sockets[other].getsockname()
            fields[5] = str(port)
        candidate["candidate"] = " ".join(fields)
        return candidate["candidate"] if bare else json.dumps(candidate)

    def arm(self) -> None:
        with self.lock:
            self.armed = True
            if self.armed_ns is None:
                self.armed_ns = time.monotonic_ns()

    def sdp(self, seat: str, value: str) -> str:
        value, removed = without_direct_candidates(value)
        with self.lock:
            self.sdp_candidates_removed[seat] += removed
        return value

    def _relay(self) -> None:
        pending: list[tuple[str, bytes]] = []
        try:
            while not self.stop.is_set():
                ready, _, _ = select.select(list(self.sockets.values()), [], [], 0.05)
                for connection in ready:
                    data, source = connection.recvfrom(65536)
                    seat = next(
                        key
                        for key, value in self.sockets.items()
                        if value is connection
                    )
                    with self.lock:
                        if seat in self.candidates and seat not in self.targets:
                            self.targets[seat] = source
                        self.received[seat] += 1
                    pending.append((seat, data))
                retained = []
                for seat, data in pending:
                    other = "godot" if seat == "native" else "native"
                    with self.lock:
                        target = self.targets.get(other)
                        if target is None:
                            retained.append((seat, data))
                            continue
                        if (
                            self.armed
                            and len(data) >= self.minimum_application_bytes
                            and data[0] == 23
                            and self.dropped[seat] < self.drops
                        ):
                            self.dropped[seat] += 1
                            self.drop_records.append(
                                {
                                    "from": seat,
                                    "bytes": len(data),
                                    "datagram": self.received[seat],
                                    "monotonic_ns": time.monotonic_ns(),
                                }
                            )
                            continue
                        self.sockets[other].sendto(data, target)
                        self.forwarded[seat] += 1
                if len(retained) > 64:
                    raise RuntimeError(
                        "loss control candidate queue exceeded 64 datagrams"
                    )
                pending = retained
        except (OSError, ValueError, RuntimeError) as error:
            with self.lock:
                self.errors.append(str(error))

    def close(self) -> dict[str, object]:
        self.stop.set()
        self.thread.join(timeout=2)
        for connection in self.sockets.values():
            connection.close()
        with self.lock:
            if self.thread.is_alive():
                self.errors.append("loss relay did not stop")
            return {
                "armed": self.armed,
                "minimum_application_bytes": self.minimum_application_bytes,
                "started_monotonic_ns": self.started_ns,
                "armed_monotonic_ns": self.armed_ns,
                "targets": self.targets,
                "candidates": self.candidates,
                "received": self.received,
                "forwarded": self.forwarded,
                "dropped": self.dropped,
                "drop_records": self.drop_records,
                "ignored_candidates": self.ignored_candidates,
                "sdp_candidate_lines_removed": self.sdp_candidates_removed,
                "errors": self.errors,
            }


def loss_histories_passed(histories: list[dict[str, object]], drops: int) -> bool:
    if len(histories) != 2:
        return False
    generations = [history.get("generation") for history in histories]
    return (
        all(type(generation) is int and generation > 0 for generation in generations)
        and generations[0] != generations[1]
        and all(
            history.get("armed") is True
            and history.get("errors") == []
            and history.get("dropped") == {"native": drops, "godot": drops}
            for history in histories
        )
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--drops-per-direction", type=int, default=1)
    parser.add_argument("--minimum-application-bytes", type=int, default=1000)
    options, remaining = parser.parse_known_args()
    if not 1 <= options.drops_per_direction <= 8:
        parser.error("--drops-per-direction must be in 1..8")
    if not 13 <= options.minimum_application_bytes <= 65535:
        parser.error("--minimum-application-bytes must be in 13..65535")
    spec = importlib.util.spec_from_file_location(
        "godot_reference_driver", options.driver
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load Godot reference driver")
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    original = driver.native_reference.NativeReference
    instances = []

    class LossReference(original):
        def __init__(self, binary: Path, directory: Path) -> None:
            self.path = None
            self.generation = None
            self.pending_events = []
            self.histories = []
            self.directory = directory
            super().__init__(binary, directory)
            instances.append(self)

        def finish_path(self) -> None:
            if self.path is not None:
                self.histories.append(
                    {**self.path.close(), "generation": self.generation}
                )
                self.path = None
                (self.directory / "udp-loss.json").write_text(
                    json.dumps(self.histories, indent=2)
                )

        def command(self, value: dict[str, object]) -> object:
            if value.get("op") == "pair":
                self.finish_path()
                self.generation = None
                self.path = UdpLossPath(
                    options.drops_per_direction, options.minimum_application_bytes
                )
            if value.get("op") == "candidate":
                candidate = self.path.candidate("godot", str(value["candidate"]))
                if candidate is None:
                    return None
                value = {**value, "candidate": candidate}
            if value.get("op") in ("offer", "answer"):
                value = {**value, "sdp": self.path.sdp("godot", str(value["sdp"]))}
            result = super().command(value)
            if value.get("op") == "pair":
                self.generation = result["generation"]
                if result.get("offer"):
                    result = {
                        **result,
                        "offer": self.path.sdp("native", result["offer"]),
                    }
            elif value.get("op") == "offer":
                result = self.path.sdp("native", result)
            return result

        def drain(self) -> list[dict[str, object]]:
            pending, self.pending_events = self.pending_events + super().drain(), []
            if self.generation is None:
                if len(pending) > 512:
                    raise RuntimeError(
                        "loss control pair-event queue exceeded 512 events"
                    )
                self.pending_events = pending
                return []
            events = []
            for value in pending:
                if value.get("generation") != self.generation:
                    events.append(value)
                    continue
                if value.get("event") == "candidate":
                    candidate = self.path.candidate("native", str(value["candidate"]))
                    if candidate is None:
                        continue
                    value = {**value, "candidate": candidate}
                if value.get("event") == "open":
                    self.path.arm()
                events.append(value)
            return events

        def close(self) -> None:
            self.finish_path()
            super().close()

    driver.native_reference.NativeReference = LossReference
    sys.argv = [str(options.driver), *remaining]
    driver.main()
    if not instances or any(
        not loss_histories_passed(instance.histories, options.drops_per_direction)
        for instance in instances
    ):
        raise SystemExit(
            "loss control did not observe every required two-generation drop"
        )


if __name__ == "__main__":
    main()
