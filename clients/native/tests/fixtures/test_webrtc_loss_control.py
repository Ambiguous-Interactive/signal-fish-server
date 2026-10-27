"""Check the loss path with real UDP sockets before gameplay qualification."""

import json
import socket
import unittest

from webrtc_loss_control import (
    UdpLossPath,
    loss_histories_passed,
    without_direct_candidates,
)


def candidate(connection: socket.socket) -> str:
    address, port = connection.getsockname()
    return json.dumps(
        {
            "candidate": f"candidate:1 1 udp 2130706431 {address} {port} typ host",
            "sdpMid": "0",
        }
    )


def destination(encoded: str) -> tuple[str, int]:
    fields = json.loads(encoded)["candidate"].split()
    return fields[4], int(fields[5])


class LossPathTests(unittest.TestCase):
    def test_inline_candidates_cannot_bypass_the_loss_path(self) -> None:
        for ending in ("\n", "\r\n"):
            with self.subTest(ending=ending):
                sdp = ending.join(
                    [
                        "v=0",
                        "a=ice-ufrag:retained",
                        "a=candidate:1 1 udp 1 127.0.0.1 1234 typ host",
                        "a=end-of-candidates",
                        "a=fingerprint:retained",
                        "",
                    ]
                )
                result, removed = without_direct_candidates(sdp)
                self.assertEqual(
                    result,
                    ending.join(
                        ["v=0", "a=ice-ufrag:retained", "a=fingerprint:retained", ""]
                    ),
                )
                self.assertEqual(removed, 2)
        self.assertEqual(without_direct_candidates(""), ("", 0))

    def test_verdict_requires_real_loss_in_both_directions_and_fresh_generations(
        self,
    ) -> None:
        histories = [
            {
                "generation": generation,
                "armed": True,
                "errors": [],
                "dropped": {"native": 1, "godot": 1},
            }
            for generation in (1, 2)
        ]
        self.assertTrue(loss_histories_passed(histories, 1))
        self.assertFalse(loss_histories_passed(histories[:1], 1))
        for key, value in [
            ("generation", 1),
            ("generation", True),
            ("generation", None),
            ("armed", False),
            ("errors", ["relay stopped"]),
            ("dropped", {"native": 0, "godot": 1}),
            ("dropped", {"native": 1, "godot": 0}),
        ]:
            with self.subTest(key=key, value=value):
                changed = [histories[0], {**histories[1], key: value}]
                self.assertFalse(loss_histories_passed(changed, 1))

    def test_mdns_target_is_learned_without_losing_queued_packets(self) -> None:
        path = UdpLossPath(1)
        native = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        godot = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            for connection in (native, godot):
                connection.bind(("127.0.0.1", 0))
                connection.settimeout(1)
            to_native = path.candidate("native", candidate(native))
            to_godot = path.candidate(
                "godot", candidate(godot).replace("127.0.0.1", "peer.local")
            )
            native.sendto(b"before-mdns", destination(to_godot))
            godot.sendto(b"first-mdns-packet", destination(to_native))
            self.assertEqual(godot.recvfrom(65536)[0], b"before-mdns")
            self.assertEqual(native.recvfrom(65536)[0], b"first-mdns-packet")
            proof = path.close()
            self.assertEqual(proof["targets"]["godot"], godot.getsockname())
            self.assertEqual(proof["candidates"]["godot"][0], "peer.local")
            self.assertEqual(proof["dropped"], {"native": 0, "godot": 0})
            self.assertEqual(proof["errors"], [])
        finally:
            path.close()
            native.close()
            godot.close()

    def test_each_direction_forwards_exact_bytes_and_only_drops_the_armed_budget(
        self,
    ) -> None:
        path = UdpLossPath(1)
        native = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        godot = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            for connection in (native, godot):
                connection.bind(("127.0.0.1", 0))
                connection.settimeout(1)
            to_native = path.candidate("native", candidate(native))
            to_godot = path.candidate("godot", candidate(godot))
            self.assertIsNotNone(to_native)
            self.assertIsNotNone(to_godot)
            self.assertIsNone(path.candidate("native", candidate(native)))
            self.assertIsNone(path.candidate("godot", candidate(godot)))
            application = bytes([23, 254, 253]) + bytes(range(253)) * 8
            for sender, receiver, target in (
                (native, godot, destination(to_godot)),
                (godot, native, destination(to_native)),
            ):
                sender.sendto(application, target)
                self.assertEqual(receiver.recvfrom(65536)[0], application)
            path.arm()
            for sender, receiver, target in (
                (native, godot, destination(to_godot)),
                (godot, native, destination(to_native)),
            ):
                short_application = bytes([23]) + bytes(12)
                sender.sendto(short_application, target)
                self.assertEqual(receiver.recvfrom(65536)[0], short_application)
                sender.sendto(application, target)
                sender.sendto(b"after-loss\x00\xff", target)
                self.assertEqual(receiver.recvfrom(65536)[0], b"after-loss\x00\xff")
                sender.sendto(application, target)
                self.assertEqual(receiver.recvfrom(65536)[0], application)
            proof = path.close()
            self.assertEqual(proof["dropped"], {"native": 1, "godot": 1})
            self.assertEqual(proof["forwarded"], {"native": 4, "godot": 4})
            self.assertEqual(proof["ignored_candidates"], {"native": 1, "godot": 1})
            self.assertEqual(proof["errors"], [])
            self.assertEqual(len(proof["drop_records"]), 2)
        finally:
            path.close()
            native.close()
            godot.close()

    def test_candidate_metadata_and_end_marker_remain_exact(self) -> None:
        path = UdpLossPath(1)
        try:
            self.assertEqual(path.candidate("godot", ""), "")
            end = json.dumps({"candidate": "", "sdpMid": "0"})
            self.assertEqual(path.candidate("native", end), end)
            bare = path.candidate(
                "godot", "candidate:1 1 udp 1 127.0.0.1 1234 typ host"
            )
            self.assertTrue(bare.startswith("candidate:"))
            self.assertEqual(bare.split()[4], "127.0.0.1")
            for raw in (
                "candidate:incomplete",
                "candidate:1 1 udp 1 invalid-host 1234 typ host",
            ):
                with self.subTest(candidate=raw), self.assertRaises(ValueError):
                    path.candidate("native", json.dumps({"candidate": raw}))
            self.assertIsNone(
                path.candidate("native", "candidate:1 1 tcp 1 127.0.0.1 1234 typ host")
            )
            self.assertIsNone(
                path.candidate(
                    "native",
                    json.dumps({"candidate": "candidate:1 1 udp 1 ::1 1234 typ host"}),
                )
            )
        finally:
            proof = path.close()
        self.assertFalse(proof["armed"])
        self.assertEqual(proof["dropped"], {"native": 0, "godot": 0})
        self.assertEqual(proof["errors"], [])


if __name__ == "__main__":
    unittest.main()
