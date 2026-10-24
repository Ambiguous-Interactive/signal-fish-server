# Signal Fish SCTP reset patch

Source: published `rtc-sctp` 0.21.1, MIT/Apache-2.0.
Upstream commit: `2a732df55ca4bcf26b80914fa0f570cfaf74cfcb`.
Crates.io package checksum:
`2eb430b379d90e7984306036d1646454a26b300c9c3256009eb8f7e0615e90b9`.

The native reference client and vendored RTC test graph patch this package
through local paths. The server package does not use this patch.
Registry versions of the other RTC packages remain unchanged.
The copy retains published library sources, upstream author attribution,
and both license files from the same upstream commit.
Trailing whitespace is removed. The manifest omits example targets.
Library tests retain their development dependencies. Unsafe code is forbidden.

The repair addresses stream reset and reuse in
[#872](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/872).
It retains each stream until its incoming reset drains and its outgoing reset
is acknowledged. Reciprocal requests wait behind queued outgoing DATA.
Only one outgoing reset request is in flight at a time.
Incoming request sequences start at the peer's initial TSN and wrap at 32 bits.
The last response is cached. Duplicate requests replay that response without
changing the current stream generation. Stale and future sequences are refused.
InProgress retains and retries the outgoing request without consuming the
reset retransmission error budget. A busy receiver retains the same request
sequence for retry with the normal retransmission error budget.
Denied resets keep the SID reserved and permit an explicit retry.
Reset retry exhaustion closes the association at its configured retry limit.
It closes every stream once, stops all timers, and emits a typed terminal
association event. The RTC layer uses this event to retire the physical transport.
Controls retain limits 0, 1, and 5, plus public blocked-send recovery.
See [#1133](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1133).

DATA for a new stream generation can arrive before the final reset response.
The receiver acknowledges it at once and saves its reassembly separately until
the old stream is removed. The receive window charges payload and all retained
chunk headers and queue metadata. TSN lookup rejects duplicates without a
linear scan. FORWARD TSN removes abandoned incomplete saved messages and keeps
complete received messages. A FWD TSN beyond a pending reset boundary updates
the held future generation even before the old reader drains. Ordered progress with no saved DATA uses one small
marker per SID, applied when an actual stream is opened. A marker does not
reserve the freed SID or emit a stream-open event.
Outgoing FORWARD TSN is capped at an outstanding reset's sender TSN boundary.
The capped stream list is reconstructed from only the abandoned DATA through
that boundary, so abandonment on another SID cannot carry an old reset SID's
SSN into its next generation. Reset acknowledgment retires the old cached SSN
and fences old unacknowledged DATA through that boundary. The fence is removed
when the cumulative SACK catches up. After reset acknowledgment, forwarding
resumes through the newest abandoned TSN. DATA on other SIDs can still be sent
while a reset waits; their abandonment beyond the boundary waits for the reset
response. If only abandoned DATA remains beyond a fence that cannot advance
the cumulative ACK, T3 pauses while RECONFIG handles the local wait. Live DATA
retains ordinary retransmission and congestion behavior. A terminal reset
response resumes forward recovery and rearms an idle T3 timer, preserving any
existing deadline and the current RTO. A control holds InProgress beyond the
published T3 retry budget, loses the first lifted FWD after success or denial,
and verifies retransmission and cumulative acknowledgment.
The paired wire control withholds SACKs and the final reset ACK,
uses old SSN5, and verifies genuine new SSN0 abandonment followed by SSN1 delivery.
FORWARD TSN, application reads, and local read shutdown retry deferred resets.
No stream-close event is emitted before both reset directions complete.
Stream handles bind to a generation. An old read loop cannot consume a new
stream after its final read completes a reset. Closed SIDs leave the accept queue.
Queued notifications from the old stream are removed before its close event.

Changed upstream files: `src/association/mod.rs`, `stream.rs`, `timer.rs`,
and `association_test.rs`. `src/queue/reassembly_queue.rs` uses wrapping TSN
adjacency for ordered and unordered fragmented messages.
`src/endpoint/endpoint_test.rs` uses negotiated request sequences and verifies
SID reservation until reciprocal acknowledgment in its old-message controls.
The added `stream_reset_test.rs` contains deterministic
Sans-I/O controls for both reset directions, sequence replay and wrap, simultaneous
close, delayed DATA, DATA/reset response reordering, drain, forward TSN,
metadata limits, denied requests, retries, and outgoing fragment ordering.
These controls qualify the local repair. They do not claim full RFC 6525 support
for parameter types that upstream does not implement.
The procedures follow [RFC 6525](https://www.rfc-editor.org/rfc/rfc6525.html),
sections 5.1.1, 5.2.1, 5.2.2, and 5.2.7. Transport acknowledgment follows
[RFC 9260](https://www.rfc-editor.org/rfc/rfc9260.html), section 6.2.

Remove the vendored package and path patches after a published release passes
these controls and the RTC/native close-and-reopen controls.
Track replacement with the existing dependency follow-up
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).

The #1095 repair adds `data_channel_bytes` to `BufferedAmountReleased`.
The existing `n_bytes` and stream buffer thresholds retain physical payload
accounting. The new delta counts String and Binary application payloads only.
DCEP and empty-message filler bytes receive zero application credit.
Cumulative ACK, gap ACK, and abandonment compute both deltas from each released
chunk's PPID. Existing generation checks still exclude retired stream data.

DATA and shutdown recovery now enforce their existing configured retry limits.
Exhaustion retires the association through the same typed terminal event as
reset failure. It stops timers, closes streams once, and prevents local stream
creation on a retired association. Thresholds remain unchanged. The DATA
retry documentation now matches the existing default of five attempts.
The deterministic controls retain limits 0, 1, and 5 and both shutdown roles.
The public peer control drops encrypted SCTP records while keeping ICE consent,
requires blocked binary/text/writable callers to return Closed, and then
exchanges gameplay on fresh peers. See
[#1140](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1140).
Replacement qualification in #912 must retain these recovery controls.

SHUTDOWN now reports the cumulative TSN of received peer DATA. Its implicit
acknowledgment releases only the acknowledged outgoing DATA, using the same
byte, timer, and forward-TSN accounting as SACK. It does not supply selective
acknowledgments or a receive-window update. Simultaneous and repeated shutdown
replay the acknowledgment without releasing bytes twice. Pending unsent DATA
prevents a premature SHUTDOWN ACK. Paired-wire controls lose the final SACK
and first SHUTDOWN ACK for binary and text gameplay, including TSN wrap.
See [#1143](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1143).
Graceful shutdown also drains admitted pending DATA and preserves complete
unread gameplay on read-enabled streams until the application reads or stops
them. It never reopens stopped reads. It refuses new writes immediately.
Both outgoing queues must clear before SHUTDOWN is sent.
The endpoint keeps routing through terminal packet transmission and unread
message drain. Paired controls cover fragments, small receive windows, DATA
loss, shutdown and ACK loss, simultaneous close, and delayed application reads.
See [#1145](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1145).
Reading a full receive queue now advertises its reopened window at the next
transmit poll. Pending gameplay and graceful shutdown drain can resume without
waiting for a zero-window probe. Reads with remaining credit keep the existing
delayed-ACK policy. Empty and terminal reads do not send window updates.
Paired-wire controls preserve exact admitted tail order without advancing time
and verify delayed-ACK and empty-read behavior. Replacement qualification must
retain these controls.
Endpoints now answer repeated out-of-association SHUTDOWN ACK packets with a
standalone SHUTDOWN COMPLETE. The reply reflects the received tag and sets
its T bit. The Sans-I/O driver drains endpoint replies even without a live
association. Accepted handshake events register the peer tag and SCTP ports
for reflected-COMPLETE routing. Stale INIT ACK packets cannot change that route.
Complete-packet validation checks the T bit, tag, and ports before closure.
A graceful association with unread gameplay also answers repeated ACK packets
without consuming that gameplay or restarting timers. Handshake-time ACK
packets receive reflected replies while their existing T1 timer remains armed.
Deterministic controls drop the first COMPLETE in both endpoint roles and
simultaneous close, preserve an unread tail through recovery, and cover
malformed packets, ABORT precedence, unrelated controls, and tag/port mismatch.
See [#1147](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/1147).
Replacement qualification in #912 must retain these acknowledgment controls.
Stopping reads during graceful shutdown now discards the stopped generation's
receive queue and advertises the released window. Later DATA for that stopped
generation is acknowledged without retaining unreadable payloads. Admitted
outgoing DATA still drains. Saved reset generations keep their own readers.
The same cleanup runs when graceful shutdown starts after reads were stopped.
Ordinary established stream reset keeps its existing receive pressure.
Paired controls cover local and peer shutdown, stopping before and during drain,
a full receive window, repeated stop, exact outgoing bytes, and final retirement.
State controls cover all four shutdown states and saved next-generation reads.

The #1158 repair computes selective-acknowledgment offsets with wrapping TSN distance.
A reordered gameplay tail across TSN zero reports the exact received chunks.
Offsets above 65535 remain queued until cumulative progress makes them
reportable; they no longer truncate to another TSN. Gap grouping also handles
the largest 16-bit offset without overflow. Paired-wire controls preserve exact
ordered gameplay and graceful shutdown with a missing first chunk at TSN wrap.
Queue controls cover wrapped offsets, the 65535 boundary, retained farther DATA,
and later cumulative progress. Replacement qualification in #912 must retain
these controls.

The #1163 repair fits selective acknowledgments to the association packet MTU
and 16-bit chunk length. It reports the earliest gap blocks first and includes
duplicate TSNs only in the remaining space. Unreported received DATA stays
queued for later cumulative progress. Paired histories retain all 800 ordered
gameplay messages and close gracefully after alternate DATA arrives late,
including TSN wrap. Budget controls cover gap priority, duplicate reports,
minimum packet size, and the largest chunk size. This follows
[RFC 9260 section 6.2](https://www.rfc-editor.org/rfc/rfc9260.html#section-6.2).
The #1164 repair appends serially increasing TSNs directly. Reordered arrivals
retain binary insertion. A 1024-row ordered window requires at most 1024 serial
comparisons, down from 11242. Wrapped sequence and deque controls retain exact
pop order and byte accounting. These controls are included in the existing
shutdown and payload_queue_gap_ack selectors. Replacement qualification in
#912 must retain them.
