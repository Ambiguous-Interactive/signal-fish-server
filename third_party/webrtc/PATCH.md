# Signal Fish WebRTC wrapper patch

Source: published `webrtc` 0.21.1, MIT/Apache-2.0.
Upstream commit: `d658b5a803dd8dc389547450e245877310c9cbed`.
Crates.io package checksum:
`b621e736302bef784948cc8d8099fd0a8fdb5757693c80a344ffbe38c49387d2`.

The native reference client patches this package through a local path.
The copy contains the published library sources and both license files.
Trailing whitespace is removed from the source copy.
Its normalized manifest omits example and integration targets and their unused
build dependencies. Library tests retain their required development dependencies.
Registry versions for other dependencies remain unchanged.
The standalone test graph uses the qualified RTC and SCTP patches.
CI scans all tracked graphs with the existing supply-chain policy.
The signaling server package does not use these patches.
Git dependencies remain prohibited.

The driver releases undeliverable event queues after channel closure.
It preserves messages queued before registration while the core channel exists,
and preserves message-before-close ordering for registered slow receivers.
Events for a retired, unregistered handle cannot recreate a retained queue.
Disconnected event senders are removed during the next driver flush.
No additional channel polling task is needed for a refused announcement.
The changed upstream file is `src/peer_connection/driver.rs`.

See [#919](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/919).
Remove this package and its path patch only after a published release passes
the retained event-ordering, retired-handle churn, native refusal/reuse, and
browser interoperability controls. Track qualification in
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
The wider resource audit #870 remains open.
