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
A live-handle marker records each processed OPEN and retires on CLOSE.
The driver preserves the full acknowledged first burst until registration.
Only an announced channel with a dropped receiver uses the 256-event orphan cap.
Markers do not retain closed handles.
Events for a retired, unregistered handle cannot recreate a retained queue.
Disconnected event senders are removed during the next driver flush.
No additional channel polling task is needed for a refused announcement.
The driver task has one cancellation owner during construction, use, and close.
Dropping a pending build, a peer, or an interrupted close stops that task.
Explicit close retains its brief graceful stop and callback-specific detach.
It keeps driver cancellation ownership while waiting for the core, even when
other callers retain peer handles.
The changed upstream files are `src/peer_connection/driver.rs` and
`src/peer_connection/mod.rs`.

See [#919](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/919).
Remove this package and its path patch only after a published release passes
the retained cancellation, event-ordering, retired-handle churn, native refusal/reuse, and
browser interoperability controls. Track qualification in
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
The wider resource audit #870 remains open.

The driver maps the RTC queued closing message to `DataChannelEvent::OnClosing`
through the same retained delivery path as channel input. Earlier queued input
keeps its order. The native engine retires the pair before reciprocal reset
finishes. See [#1071](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1071).
Replacement qualification in #912 must retain these controls.

The wrapper maps ordered RTC opening and closure markers to its existing
`OnOpen` and `OnClose` events. Its core drain stops at each closed lifetime and
waits for retained output to reach registered receivers before the next lifetime
opens. This preserves earlier input, old close, new announcement, and new input
when SCTP reuses a stream. The production driver control checks registered
receiver delivery and replacement registration. See
[#1075](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1075).
Published replacement qualification in #912 must retain these controls.

Local channel creation holds the core and event-sender map together until its
receiver is registered. The driver cannot announce that local channel as a
remote channel or deliver its open event to a replacement receiver. Cancellation
while registration waits leaves no core channel. Creation samples the runtime
clock after both locks are acquired and advances the core before dialing, so an
idle core cannot shorten a new DCEP deadline. The retained controls force map
contention, cancel a waiter, route one open and the first gameplay message to
the returned receiver, and check a public marker's timestamp after an idle gap.
The RTC controls separately verify configured deadlines and DCEP ACK handling.
See [#1086](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1086).
Published replacement qualification in #912 must retain these controls.

Fatal driver I/O failure closes the RTC core and publishes its terminal peer
callback before the driver ends. A responder with no admitted remote channel
can therefore observe link loss. Dead peers reject new channel creation.
Creation checks the closing flag after it acquires both registration locks.
Public controls inject a fatal receive error before channel admission and
check terminal handles and fresh-peer gameplay recovery on the same network.
Established reliable and zero-retry unordered channels release blocked binary,
text, and writable operations. Removing driver retirement fails both controls.
Recoverable receive errors and ICE restart keep their existing behavior.
See [#1134](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1134).
Replacement qualification in #912 must retain fatal driver recovery.

Capacity waiters register a listener after the first observed shortage, then
recheck closure and capacity before waiting. A terminal wake during core-lock
contention cannot strand the sender until its polling timer fires. Healthy
sends allocate no listener. The deterministic control holds the core lock,
stops the driver, and requires an immediate Closed error after releasing it.
See [#1135](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1135).
Replacement qualification in #912 must retain this wake ordering.
