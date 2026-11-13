#!/usr/bin/env python3
"""Measure raw DEFLATE over exact exported server frames; no runtime adoption."""

import argparse
import base64
import hashlib
import json
import platform
import statistics
import subprocess
import sys
import tempfile
import time
import unittest
import zlib
from pathlib import Path


TAIL = b"\x00\x00\xff\xff"


def encode(frames, level, takeover):
    contexts = {}
    encoded = []
    for recipient, kind, payload in frames:
        compressor = contexts.get(recipient) if takeover else None
        if compressor is None:
            compressor = zlib.compressobj(level, zlib.DEFLATED, -15)
            if takeover:
                contexts[recipient] = compressor
        compressed = compressor.compress(payload) + compressor.flush(zlib.Z_SYNC_FLUSH)
        if not compressed.endswith(TAIL):
            raise ValueError("compressor did not return the SYNC_FLUSH boundary")
        encoded.append((recipient, kind, compressed[:-4]))
    return encoded


def decode(frames, takeover):
    contexts = {}
    decoded = []
    for recipient, kind, payload in frames:
        decompressor = contexts.get(recipient) if takeover else None
        if decompressor is None:
            decompressor = zlib.decompressobj(-15)
            if takeover:
                contexts[recipient] = decompressor
        restored = decompressor.decompress(payload + TAIL)
        if decompressor.unconsumed_tail or decompressor.unused_data:
            raise ValueError("decoder did not consume exactly one message")
        decoded.append((recipient, kind, restored))
    return decoded


def frame_bytes(payload):
    length = len(payload)
    return length + (2 if length <= 125 else 4 if length <= 65535 else 10)


def digest_frames(frames):
    digest = hashlib.sha256()
    for _, kind, payload in frames:
        digest.update(bytes([0 if kind == "text" else 1]))
        digest.update(len(payload).to_bytes(8, "little"))
        digest.update(payload)
    return digest.hexdigest()


def read_case(directory, case):
    content = (directory / case["file"]).read_bytes()
    if hashlib.sha256(content).hexdigest() != case["corpus_sha256"]:
        raise ValueError(f"corpus digest mismatch: {case['file']}")
    frames = []
    for line in content.splitlines():
        row = json.loads(line)
        if row["kind"] not in ("text", "binary"):
            raise ValueError("capture contains a non-data frame")
        frames.append((row["recipient"], row["kind"], base64.b64decode(row["payload"], validate=True)))
    if len(frames) != case["frames"]:
        raise ValueError("capture frame count mismatch")
    if sum(len(payload) for _, _, payload in frames) != case["wire_bytes"]:
        raise ValueError("capture byte count mismatch")
    if digest_frames(frames) != case["output_sha256"]:
        raise ValueError("capture does not match the production wire ledger")
    return frames


def timed(operation, repeats):
    samples = []
    result = operation()
    for _ in range(repeats):
        started = time.process_time_ns()
        result = operation()
        samples.append(time.process_time_ns() - started)
    return result, samples


def git_output(*arguments):
    return subprocess.check_output(["git", *arguments])


def measure(directory, repeats):
    manifest_bytes = (directory / "manifest.json").read_bytes()
    manifest = json.loads(manifest_bytes)
    if manifest["version"] != 1:
        raise ValueError("unsupported capture version")
    report = {
        "python": sys.version,
        "zlib_build": zlib.ZLIB_VERSION,
        "zlib_runtime": zlib.ZLIB_RUNTIME_VERSION,
        "host": platform.platform(),
        "machine": platform.machine(),
        "source_head": git_output("rev-parse", "HEAD").decode().strip(),
        "tracked_patch_sha256": hashlib.sha256(git_output("diff", "HEAD")).hexdigest(),
        "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
        "exporter_executable_sha256": manifest["executable_sha256"],
        "measurement_script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "study_source_sha256": {
            str(path): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in (Path("Cargo.toml"), Path("benches/relay_compression_frames.rs"),
                         Path("benches/support/relay_serialization.rs"))
        },
        "repeats": repeats,
        "clock": "process_time_ns",
        "cases": [],
    }
    workloads = [(case, read_case(directory, case)) for case in manifest["cases"]]
    for length in (6, 96, 1024):
        frames = [("opaque-control", "binary", hashlib.shake_256(f"{length}:{index}".encode()).digest(length))
                  for index in range(1024)]
        workloads.append(({
            "scenario": f"synthetic_opaque_{length}", "synthetic_control": True,
            "frames": len(frames), "wire_bytes": length * len(frames),
            "output_sha256": digest_frames(frames),
        }, frames))
    for case, frames in workloads:
        source_bytes = sum(frame_bytes(payload) for _, _, payload in frames)
        if not frames:
            report["cases"].append({**case, "verdict": "no materialized frames"})
            continue
        _, baseline = timed(lambda: [(r, k, bytes(p)) for r, k, p in frames], repeats)
        for takeover in (False, True):
            for level in (1, 6):
                compressed, compress_cpu = timed(lambda: encode(frames, level, takeover), repeats)
                restored, decompress_cpu = timed(lambda: decode(compressed, takeover), repeats)
                if restored != frames:
                    raise ValueError("compression changed recipient, opcode, order, or exact payload")
                compressed_bytes = sum(frame_bytes(payload) for _, _, payload in compressed)
                row = {
                    **case,
                    "takeover": takeover,
                    "level": level,
                    "recipients": len({recipient for recipient, _, _ in frames}),
                    "uncompressed_frame_bytes": source_bytes,
                    "compressed_frame_bytes": compressed_bytes,
                    "frame_bytes_saved_percent": 100 * (1 - compressed_bytes / source_bytes),
                    "baseline_cpu_ns": baseline,
                    "compress_cpu_ns": compress_cpu,
                    "decompress_cpu_ns": decompress_cpu,
                    "compress_median_ns_per_frame": statistics.median(compress_cpu) / len(frames),
                    "decompress_median_ns_per_frame": statistics.median(decompress_cpu) / len(frames),
                    "roundtrip": "exact",
                }
                report["cases"].append(row)
    return report


class CompressionControls(unittest.TestCase):
    def test_exact_payload_opcode_recipient_and_interleaved_contexts(self):
        frames = [(recipient, kind, payload) for _ in range(3) for recipient, kind, payload in [
            ("a", "text", "雪\0gameplay".encode() * 128),
            ("b", "binary", bytes(range(256)) * 8),
            ("a", "binary", b""),
        ]]
        for level in (1, 6):
            for takeover in (False, True):
                self.assertEqual(decode(encode(frames, level, takeover), takeover), frames)

    def test_takeover_requires_its_own_history(self):
        payload = bytes(range(256)) * 16
        frames = [("a", "binary", payload), ("a", "binary", payload)]
        compressed = encode(frames, 6, True)
        self.assertLess(len(compressed[1][2]), len(compressed[0][2]))
        with self.assertRaises(zlib.error):
            decode(compressed[1:], True)
        self.assertEqual(decode(encode(frames[1:], 6, True), True), frames[1:])

    def test_ledger_rejects_changed_bytes_and_opcode(self):
        frames = [("a", "text", b"gameplay")]
        self.assertNotEqual(digest_frames(frames), digest_frames([("a", "text", b"Gameplay")]))
        self.assertNotEqual(digest_frames(frames), digest_frames([("a", "binary", b"gameplay")]))

    def test_server_frame_header_thresholds(self):
        for length, header in ((0, 2), (125, 2), (126, 4), (65535, 4), (65536, 10)):
            self.assertEqual(frame_bytes(bytes(length)), length + header)

    def test_capture_validation_rejects_corruption_and_incorrect_ledger(self):
        frames = [("a", "text", b"gameplay")]
        content = json.dumps({"recipient": "a", "kind": "text", "payload": "Z2FtZXBsYXk="}).encode() + b"\n"
        case = {
            "file": "case.jsonl", "corpus_sha256": hashlib.sha256(content).hexdigest(),
            "frames": 1, "wire_bytes": 8, "output_sha256": digest_frames(frames),
        }
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / case["file"]).write_bytes(content)
            self.assertEqual(read_case(directory, case), frames)
            with self.assertRaisesRegex(ValueError, "production wire ledger"):
                read_case(directory, {**case, "output_sha256": "0" * 64})
            (directory / case["file"]).write_bytes(content + b"\n")
            with self.assertRaisesRegex(ValueError, "corpus digest"):
                read_case(directory, case)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, nargs="?")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--repeats", type=int, default=9)
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    if arguments.self_test:
        result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(CompressionControls))
        return 0 if result.wasSuccessful() else 1
    if arguments.directory is None or arguments.output is None or arguments.repeats < 3:
        parser.error("supply a capture directory, --output, and at least three repeats")
    report = measure(arguments.directory, arguments.repeats)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Retained {len(report['cases'])} compression cells in {arguments.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
