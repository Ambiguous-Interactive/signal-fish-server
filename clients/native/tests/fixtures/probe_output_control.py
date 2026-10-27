"""Check exact native probe receipts across blocked stdout and receive refusal."""

import argparse
import base64
import hashlib
import importlib.util
import json
import shutil
import threading
import time
import traceback
from pathlib import Path


class GatedOutput:
    def __init__(self, stream, gate):
        self.stream = stream
        self.gate = gate

    def __iter__(self):
        for line in self.stream:
            self.gate.wait()
            yield line

    def close(self):
        self.stream.close()


class NotifyingInput:
    def __init__(self, stream, submitted):
        self.stream = stream
        self.submitted = submitted

    def write(self, value):
        return self.stream.write(value)

    def flush(self):
        self.stream.flush()
        self.submitted.set()

    def close(self):
        self.stream.close()


def run_case(bridge, probe, directory, receiver_offers, packet_bytes):
    class PausedReference(bridge.NativeReference):
        def __init__(self, *args):
            self.gate = threading.Event()
            self.gate.set()
            super().__init__(*args)

        def _read(self):
            self.process.stdout = GatedOutput(self.process.stdout, self.gate)
            super()._read()

    peers = []
    report = {"receiver_offers": receiver_offers, "packet_bytes": packet_bytes}
    try:
        for name, kind in [
            ("sender", bridge.NativeReference),
            ("receiver", PausedReference),
        ]:
            path = directory / name
            path.mkdir()
            peers.append(kind(probe, path))
        sender, receiver = peers
        offerer, answerer = (
            (receiver, sender) if receiver_offers else (sender, receiver)
        )
        offer = offerer.command({"op": "pair", "offer": True, "ice_servers": []})[
            "offer"
        ]
        answerer.command({"op": "pair", "offer": False, "ice_servers": []})
        answer = answerer.command({"op": "offer", "sdp": offer})
        offerer.command({"op": "answer", "sdp": answer})
        opened = [set(), set()]
        deadline = time.monotonic() + 15
        required_labels = {"reliable", "ordered", "unreliable"}
        while not all(labels == required_labels for labels in opened):
            if time.monotonic() >= deadline:
                raise TimeoutError(f"required channels did not open: {opened}")
            for index, peer in enumerate(peers):
                for event in peer.drain():
                    if event.get("event") == "candidate":
                        peers[1 - index].command(
                            {"op": "candidate", "candidate": event["candidate"]}
                        )
                    elif event.get("event") == "open":
                        opened[index].add(event["label"])
            time.sleep(0.01)
        receiver.gate.clear()
        payloads = [
            (f"probe:{i:04d}:".encode() + b"x" * packet_bytes)[:packet_bytes]
            for i in range(1024)
        ]
        packets = [
            {
                "op": "send_binary",
                "label": "reliable",
                "data_base64": base64.b64encode(data).decode(),
            }
            for data in payloads
        ]
        for offset in range(0, len(packets), 128):
            try:
                sender.command(
                    {"op": "send_batch", "packets": packets[offset : offset + 128]}
                )
            except RuntimeError as error:
                report["send_error"] = str(error)
                break
        report["sent_packets"] = sum(
            event.get("event") == "sent" for event in sender.drain()
        )
        deadline = time.monotonic() + 15
        while True:
            snapshot = sender.command({"op": "snapshot"})
            states = {
                channel["label"]: channel["state"]
                for channel in snapshot["transport_channels"]
            }
            if states.get("reliable") in ("closing", "closed"):
                report["sender_before_resume"] = snapshot
                break
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"receiver did not close while stdout was held: {snapshot}"
                )
            time.sleep(0.01)

        sample = {}

        def sample_snapshot():
            try:
                sample["value"] = receiver.command({"op": "snapshot"})
            except (RuntimeError, OSError, ValueError) as error:
                sample["error"] = str(error)

        submitted = threading.Event()
        receiver.process.stdin = NotifyingInput(receiver.process.stdin, submitted)
        sampler = threading.Thread(target=sample_snapshot, daemon=True)
        sampler.start()
        if not submitted.wait(timeout=5):
            raise TimeoutError("snapshot command was not sent before stdout resume")
        receiver.gate.set()
        events = []
        deadline = time.monotonic() + 15
        while not any(event.get("event") == "receive_rejected" for event in events):
            if time.monotonic() >= deadline:
                raise TimeoutError(f"missing rejection after {len(events)} events")
            events.extend(receiver.drain())
            time.sleep(0.01)
        sampler.join(timeout=12)
        if sampler.is_alive():
            raise TimeoutError("snapshot command did not finish after stdout resumed")
        report["concurrent_snapshot"] = sample
        report["events"] = events
        (failure,) = [
            event for event in events if event.get("event") == "receive_rejected"
        ]
        terminal_index = next(
            index
            for index, event in enumerate(events)
            if event.get("event") == "receive_rejected"
        )
        assert not any(
            event.get("event") == "binary" for event in events[terminal_index + 1 :]
        ), events
        receipts = [event for event in events if event.get("event") == "binary"]
        snapshot = receiver.command({"op": "snapshot"})
        report["snapshot_after_removal"] = snapshot
        counters = snapshot["channels"]["reliable"]["receipts"]
        assert "error" not in sample, sample
        assert failure["failure"]["reason"] == (
            "pending_messages" if packet_bytes == 4096 else "pending_bytes"
        ), failure
        assert failure["failure"]["message_bytes"] == packet_bytes, failure
        assert failure["failure"]["pending_messages"] == 1048576 // packet_bytes, (
            failure
        )
        assert failure["failure"]["pending_bytes"] == 1048576, failure
        assert len(receipts) == counters["admitted"], (len(receipts), counters)
        assert (
            counters["raw"]
            == counters["admitted"] + counters["rejected"] + counters["discarded"]
        ), counters
        assert counters["rejected"] == 1, counters
        assert all(
            event["label"] == "reliable"
            and event["generation"] == failure["generation"]
            for event in receipts
        )
        assert not any(event.get("event") == "closed" for event in events), events
        assert [
            base64.b64decode(event["data_base64"]) for event in receipts
        ] == payloads[: len(receipts)]
        assert snapshot["receive_failure"] == failure["failure"], snapshot
        assert snapshot == failure["snapshot"], snapshot
        assert {
            (channel["stream_id"], channel["label"])
            for channel in snapshot["transport_channels"]
        } == {(1, "reliable"), (2, "ordered"), (3, "unreliable")}, snapshot
        assert snapshot["sctp"]["data_chunks_accepted"] > 0, snapshot
        assert snapshot["sctp"]["messages_abandoned"] == 0, snapshot
        assert snapshot["sctp"]["first_abandoned_message"] is None, snapshot
        assert snapshot["sctp"]["last_abandoned_message"] is None, snapshot
        sender_snapshot = report["sender_before_resume"]
        assert sender_snapshot["sctp"]["data_chunks_emitted"] > 0, sender_snapshot
        assert sender_snapshot["sctp"]["messages_abandoned"] == 0, sender_snapshot
        assert sender_snapshot["sctp"]["last_enqueued_message"]["stream_id"] == 1, (
            sender_snapshot
        )
        assert sender_snapshot["sctp"]["last_enqueued_message"]["reliability"] == "reliable", (
            sender_snapshot
        )
        report["exact_receipts"] = len(receipts)
        report["passed"] = True
    except Exception:
        report["passed"] = False
        report["error"] = traceback.format_exc()
        raise
    finally:
        for peer in peers:
            if isinstance(peer, PausedReference):
                peer.gate.set()
            peer.close()
        (directory / "assessment.json").write_text(json.dumps(report, indent=2))
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    frozen_probe = args.output / ("native-reference-probe" + args.probe.suffix)
    shutil.copy2(args.probe, frozen_probe)
    bridge_path = args.driver.parent / "webrtc_native_reference.py"
    spec = importlib.util.spec_from_file_location("native_probe_bridge", bridge_path)
    bridge = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bridge)
    manifest = {
        str(path): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in (
            frozen_probe,
            bridge_path,
            Path(__file__),
            Path(__file__).with_name("godot_reference_probe.rs"),
        )
    }
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2))
    for receiver_offers in (False, True):
        for packet_bytes in (4096, 8192):
            directory = (
                args.output / f"receiver-offers-{receiver_offers}-{packet_bytes}"
            )
            directory.mkdir()
            report = run_case(
                bridge, frozen_probe, directory, receiver_offers, packet_bytes
            )
            print(
                f"{directory.name}: exact_receipts={report['exact_receipts']}; passed=True",
                flush=True,
            )


if __name__ == "__main__":
    main()
