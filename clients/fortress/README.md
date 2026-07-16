# Fortress Rollback interoperability fixture

This standalone crate reproduces the traffic shape reported in
[Fortress Rollback issue 242](https://github.com/wallstop/fortress-rollback/issues/242)
with released game-networking components and the Signal Fish Server built from
the current checkout:

- one separately spawned signaling-server process;
- two separately spawned `fortress-relay-peer` game processes;
- `fortress-rollback` exactly `0.10.0` in each game;
- `signal-fish-client` exactly `0.8.0` using
  `SignalFishPollingClient<WebSocketTransport>`;
- protocol-v3 MessagePack game-data relay over real loopback WebSockets.

Each game advances at least 600 confirmed frames with exactly one client poll
per 60 Hz callback. High-entropy per-frame inputs exercise prediction misses
and rollback repair. The multiprocess test fails unless both peers sustain at
least 120 Fortress messages per second, transfer more than two game-data frames
per callback, drain every adapter- and client-owned send, keep the oldest frame
below 500 ms, stay within the prediction window, compare matching checksums,
and report no stalls, wait recommendations, overflow, malformed frames,
unknown senders, event loss, or protocol-v3 metadata violations.

The adapter prepends the destination player UUID to every Fortress message, as
the issue-242 integration did. Its socket callback only admits bytes to a
bounded FIFO. The game-loop owner drains every admissible frame into the client
and restores a refused head, preserving Fortress's non-blocking socket contract
without a socket-wide stop-and-wait gate. Enqueue timestamps remain live until
the client's cumulative sent counter confirms the WebSocket write, so an empty
adapter FIFO cannot conceal an in-flight send.

Run the complete fixture from the repository root:

```sh
bash scripts/run-fortress-interop.sh
```

The runner builds the server from this checkout, verifies this crate's lockfile,
formats and lints it, and supplies an absolute server binary path to the test.

The fixture's supply-chain policy narrowly ignores `RUSTSEC-2025-0141` because
Fortress 0.10.0 directly depends on unmaintained `bincode` 2.0.1 and the
advisory offers no safe upgrade. This is not a production server dependency;
the exception must be removed when the issue reproduction can move to a
maintained Fortress release.
