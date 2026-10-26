# Signal Fish RTC patch

`RTCSctpTransport::association_stats` exposes local SCTP DATA/SACK counts and
retransmission/acknowledgment timer observations for #1244. DATA counts precede
reassembly and include duplicates and refusals. An absent association returns
`None`. This adds no counters or retained per-packet state to the receive path.
The transport controls preserve distinct counters, repeatable reads, and absence.
Replacement qualification in #912 must retain this diagnostic API or migrate it.

Source: published `rtc` 0.21.1, MIT/Apache-2.0.
Upstream commit: `2a732df55ca4bcf26b80914fa0f570cfaf74cfcb`.
Crates.io package checksum:
`4e2ca201de5ccc83aa3c08d49a2f08d7ed0aeeb2052fb1ccc517a8b77a6927ce`.

The native reference client patches this package through a local path.
The SCTP package has its own qualified local patch for stream reset and reuse.
Other RTC packages retain their registry sources and lockfile versions.
The server package does not use this patch. Git dependencies remain prohibited.

The copy contains the published library sources and both license files.
Trailing whitespace is removed from the source copy.
Its normalized manifest omits upstream example and integration targets.
Library tests retain their required development dependencies.
Development dependencies used only by omitted examples are removed.
CI scans this test graph with the native client supply-chain policy.

The repair preserves remote DCEP ordering, retry limits, and lifetime limits.
Partial-mode parameters must fit the WebRTC API's 16-bit properties.
Reliable modes ignore the irrelevant reliability parameter.
The responder configures its outgoing SCTP stream before acknowledging a
remote channel. Refused channels request a reset of their SCTP stream.
The changed upstream files and regression evidence are recorded in audit
[#911](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/911).

Remove this package and its path patch after a published release passes the
same controls. Track that work in
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
The reset repair keeps channel state at `Closing` until SCTP completes both
reset directions. Repeated closes request only one reset. The handler releases
stream reservations on completion and applies stream events in read order before
a new DCEP OPEN on the reused stream. Late old-channel data does not reach application callbacks.
See [SCTP patch notes](../rtc-sctp/PATCH.md) and
[#872](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/872).
The reset delta changes `src/data_channel/internal.rs`, `mod.rs`,
`src/peer_connection/handler/datachannel.rs`, `endpoint.rs`, `sctp.rs`, and
`src/peer_connection/message/internal.rs`. The retained controls cover stream
reservation, closing state, late data, and old/new generation event ordering.
Qualify both packages before removing their patches.

The channel statistics patch retains all live summaries and the 64 most recently
closed summaries per peer connection. Older closed details are removed; lifetime
opened and closed totals remain cumulative. Duplicate closure notifications do
not refresh or recreate a retired summary. Both stream closure and DCEP timeout
use this policy. Regression histories cover ordered and legacy stream events,
DTLS shutdown, timeout/reset, active records, and closure order.
See [#919](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/919).
Published replacement qualification in #912 must retain these controls.

Deferred local channel setup fails each unavailable channel separately.
Valid channels still dial and keep their first DCEP deadline across repeated
connected events. Closed undialed channels release their reservations before
assignment. Fixed IDs must fit the negotiated stream count.
Public creation rejects reserved SID 65535 before registration and checks a
fixed SID against an already connected transport before registration.
Failed handles retire with one ordered close and one closed statistics record.
Known stream IDs remain in those summaries.
The delta changes `src/data_channel/registry.rs` and
`src/peer_connection/handler/datachannel.rs`, plus
`src/peer_connection/mod.rs` for public creation checks.
Controls cover both roles, both required modes, closed pending channels,
fixed IDs, overflow, repeated connection events, paired-wire DCEP and payload
delivery, reset, and fresh stream reuse.
See [#1081](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1081).
Published replacement qualification in #912 must retain these controls.

Failed local channel creation removes its new registry entry before returning
the error. It releases a stream index only while the same handle owns that
index. Failed handles remain retired. Repeated exhaustion and successful
replacement controls cover both DTLS roles, including the final valid stream.
See [#1080](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1080).
Published replacement qualification in #912 must retain this rollback.

Stream allocation treats the negotiated stream count as an exclusive bound.
Both DTLS roles can allocate the last valid SID of their parity.
A wider cursor prevents overflow at the end of the 16-bit SID range.
The delta changes `src/data_channel/registry.rs`.
Controls cover small counts, the full range, the unknown-count fallback,
occupied SIDs, repeated exhaustion, and final-SID reuse with a fresh handle.
A paired SCTP control negotiates counts 1 through 4 and receives payload sent
through the RTC pipeline on the final SID in both required delivery modes.
See [#1079](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1079).
Published replacement qualification in #912 must retain these controls.

Local close queues `RTCMessage::DataChannelClosing` behind input already queued
for the application. Repeated close queues no duplicate. The wrapper can report
pair loss while the channel waits for the remote SCTP reset. The delta changes
`src/data_channel/mod.rs`, `internal.rs`, `src/peer_connection/message/`,
and `src/peer_connection/handler/`.
Local reset completion follows the closing marker in the same data queue.
A close before SCTP connects queues both markers immediately and counts the
closure once. Later transport teardown adds no duplicate notification or count.
The retained controls check input order, one notification, and a held reciprocal
reset in the native engine. See [#1071](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1071).
Replacement qualification in #912 must retain these controls.

Channel opening and remote stream reset queue `RTCMessage::DataChannelOpened`
and `DataChannelClosed` beside channel input. The endpoint no longer moves these
markers to the separate lifecycle queue. Event-first consumers cannot observe
remote closure before earlier input or a reused stream's new open before its old
close. The delta changes `src/peer_connection/handler/endpoint.rs`, `mod.rs`,
`sctp.rs`, and `src/peer_connection/message/mod.rs`. The core controls cover both
required channel modes and input/close/open/input on a reused stream.
See [#1075](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1075).
Published replacement qualification in #912 must retain these controls.

Public text and binary sends enter SCTP during admission, before a peer reset
can stop the outgoing stream. Local close queues its reset after those writes.
The driver still generates and batches transport packets. A negotiated channel's
pending local initialization precedes its first payload. If inbound DATA already
created its SCTP stream, negotiated initialization configures that existing
stream. It does not reopen a closed stream. In-band duplicate OPEN still fails.
A failed own initialization discards the remaining own batch before payload can
emit. Earlier driver control errors do not reject the new payload. Refused payloads add no outstanding byte
charge or sent statistics and leave no body for a later flush.
The delta changes `src/data_channel/mod.rs` and the peer-connection write and
SCTP handlers. Controls inspect accepted text/binary DATA before reciprocal
RECONFIG, the reset's sender TSN, complete peer ingestion, reset completion,
media FIFO, stale controls, and size/shutdown refusal. Both required delivery
modes retain their original reliability settings. These controls do not require
application delivery after the peer has closed its reader.
See [#1076](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1076)
and [#1121](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1121).
Published replacement qualification in #912 must retain these controls.

Public creation rejects an occupied fixed stream ID before registry insertion.
The original owner keeps gameplay routing and reset ownership until it retires.
Controls cover fixed, automatically assigned, and accepted channels before and
after connection, including closing reservations and reuse after release.
See [#1085](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1085).

Channels created after SCTP connects use the same initialization as deferred
channels. Channels created during the SCTP handshake wait until it completes. Negotiated channels publish one ordered open marker and opened
statistics. In-band channels keep the configured DCEP timeout, including when
an ACK never arrives. Repeated connected events do not renew that deadline.
Controls cover both roles, disabled timeouts, public event-first consumers,
paired-wire payloads, and stream reuse.
See [#1086](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1086).
Published replacement qualification in #912 must retain these controls.

Public text and binary sends require the channel to be `Open`. An allocated
in-band stream stays `Connecting` until the remote DCEP acknowledgment arrives.
Rejected pre-ACK sends queue no payload and charge no outstanding bytes.
Controls process an actual DCEP ACK through the handler, then verify successful
sends and terminal closure. Negotiated channels retain immediate opening.
The reset-order control now completes its in-band handshake before sending.
See [#1091](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1091).
Published replacement qualification in #912 must retain these controls.

DTLS teardown and legacy SCTP closure events queue channel close markers beside
prior application input. Event-first consumers cannot receive channel closure
before that input. The control covers already-open negotiated and in-band
channels, both required modes, both closure paths, duplicate closure, and closed
statistics. It does not establish delivery across an independent failed peer
connection. The delta changes `src/peer_connection/handler/datachannel.rs` and
`mod.rs`. See [#1093](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1093).
Published replacement qualification in #912 must retain these controls.

The #1095 repair consumes PPID-aware application-byte release credit from SCTP.
DCEP OPEN/ACK bytes and empty-message filler bytes do not reduce pending gameplay
bytes. Real paired SCTP acknowledgments preserve queued gameplay until its own
acknowledgment releases it. SCTP fragment, gap-ACK, duplicate, and abandonment
controls retain separate application and transport byte totals.

The [#1124](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1124)
repair delivers SCTP timer events before removing drained associations and
collects events from transmit-time expiry.
Abandoned gameplay releases pending send bytes without waiting for another packet.
The timer path preserves each association's transport metadata and read budget.
Controls drop initial DATA, advance its actual T3 deadline, and verify one release
for zero-retry DATA while reliable DATA keeps its charge. Public text and binary
sends retain the same behavior. An unsent timed-message control expires its
lifetime before the first transmit flush and releases its bytes without a T3 timer.
The core event pump also delivers these reads when no lifecycle event is raised.
The public controls use the async driver's event-first and split-read call order.
The [#1125](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1125)
repair keeps a payload's read slot charged until its handler sweep ends.
A paired-wire control holds 320 messages in SCTP, limits each event-first pump
to 256 queued messages, and drains every retained message in later pumps.

SCTP handshake retry exhaustion closes the failed association and all pending
channels. Undialed channels have no DCEP deadline, so they must receive their
terminal close from the SCTP failure. The terminal transport rejects late SCTP input and new channel creation. Deterministic controls lose INIT and COOKIE ACK separately, check
one close per pending channel, reject late INIT input, and deliver gameplay
on a fresh peer.
See [#1128](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1128).
Published replacement qualification in #912 must retain these controls.

DTLS handshake retry exhaustion publishes a failed peer-connection state before
retiring the transport. A responder with no admitted remote channels receives
that callback. Pending local channels receive their terminal close through the
existing ordered pipeline. Late input and new channels cannot restart the failed
physical transport. Final queued transport output is retained before retirement.
The delta changes `src/peer_connection/handler/dtls.rs` and
`src/peer_connection/mod.rs`. Public async controls preserve ICE traffic while
losing handshake records and drive the actual retry timers.
See [#1131](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1131).
Published replacement qualification in #912 must retain these controls.

Established SCTP reset retry exhaustion follows the same terminal transport
cleanup as handshake failure. It closes pending and established channels and
rejects new channels or late input on that physical transport.
The wrapper control loses encrypted SCTP packets only after channels open,
fills a sibling channel, and checks blocked binary, text, and writable calls.
See [#1133](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1133).
Replacement qualification in #912 must retain reset exhaustion and fresh-peer recovery.

Graceful SCTP shutdown keeps admitted gameplay readable after terminal wire
closure. The handler defers association retirement while bounded application
reads remain. Its transmit flush collects terminal endpoint events only after
outbound packets and stream events reach their queues. The scoped SCTP handler
controls cover a tail larger than one read budget, final-packet ownership,
unrelated association traffic, and abrupt teardown after graceful completion.
See [#1145](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1145).
Replacement qualification in #912 must retain these drain controls.

The SCTP handler now drains stateless endpoint replies for a lost final
SHUTDOWN COMPLETE. It uses the triggering DTLS transport even after association
retirement. Handshake peer-route events update the endpoint without retiring
live associations. A paired handler control drops the first COMPLETE and
closes the peer on its repeated ACK without retaining local association state.
See [#1147](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1147).

Peer connection close now marks all retained channel handles Closed, clears
handshake deadlines, releases outstanding application bytes, and drops channel
transport queues after the admitted-write and ABORT flush. Closure statistics
advance once per channel. Repeated close does not add counts or reset work.
Controls cover connecting, open, closing, and already closed channels. Native
room-leave controls retain and query connecting and established public handles.
See [#1151](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1151).
Replacement qualification in #912 must retain these terminal-state controls.

Data-channel receive statistics count application bytes after empty-message
normalization. StringEmpty and BinaryEmpty SCTP padding does not increase
`bytes_received`. Message counts still advance for empty messages.
The live native control checks both channel modes and offerer roles, exact
empty and nonempty text and binary receipts, and final statistics after reset.
See [#1219](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1219).
Replacement qualification in #912 must retain this control.

The #636 SCTP receive handler consumes complete messages with
`Chunks::into_payload`. A uniquely owned single packet can supply the delivered
buffer without another payload copy. Shared and fragmented messages retain
the bounded copy fallback. The paired handler control covers small and
fragmented gameplay and retains exact delivered storage after transport drop.
The #1145 graceful-tail control now keeps delivered buffers through endpoint
retirement instead of copying them into separate vectors first.
Replacement qualification in #912 must retain
`sctp_received_payload_reuses_single_packet_storage_after_retirement` and
`graceful_shutdown_retains_tail_across_bounded_application_reads`.

Association connection now shares the ordered read queue with following DATA.
The separate event queue previously let reliable startup packets reach an
uninitialized negotiated channel. SCTP had already consumed those packets,
so a later connection event could not recover their prefix.
The delta changes `src/peer_connection/handler/sctp.rs` and
`src/peer_connection/message/internal.rs`. The paired-wire control sends 32
exact packets before or after COOKIE-ACK and defers event polling through
the burst. The public Engine control sends 128 exact snapshot packets as soon
as the reliable channel opens in each direction, then repeats on a fresh link.
See [#1250](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1250).
Replacement qualification in #912 must retain
`negotiated_startup_burst_preserves_every_reliable_prefix_before_event_poll`
and `godot_profile_preserves_startup_snapshot_prefix_in_both_fresh_generations`.
The cause of the captured Godot 4.3 incident in #1244 remains separate until
matched runtime evidence establishes it.

The #1244 SCTP snapshot also reports accepted, redundant, and refused DATA
attempts. The conversion preserves association-owned counters without resetting
them. Accepted chunks enter the receive TSN space; they do not prove reassembly
or application delivery. Redundant positions include FORWARD TSN progress.
The native held-poller control retains the complete fragmented reliable prefix
while comparing classified SCTP attempts with RTC and Engine observations.
