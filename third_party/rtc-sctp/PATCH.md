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

The #1165 repair records duplicate DATA before stream and saved-generation
admission. Duplicate-only packets trigger an immediate SACK under the normal
ACK policy, including after the first SACK is lost. Cumulative and selective
duplicates report their exact TSNs within the existing packet and chunk budget.
Paired controls retain receive-window credit, exact application delivery, and
one release of outgoing bytes across TSN wrap, graceful drain, and deferred
reset generations. Real duplicate packets also qualify report trimming at the
minimum and small packet budgets. Replacement qualification in #912 must retain
the duplicate_data controls.

The #1168 repair tracks nonadjacent fragment neighbors during native ordered
message insertion. Completeness checks no longer scan the retained message
after its ending fragment arrives early. A 1,024-fragment tail-first control
reduces adjacency checks from 523,776 to at most 3,072. Constructor vectors,
every six-fragment arrival permutation, duplicates, TSN wrap, and a paired
gameplay history preserve exact payloads and release credit. Replacement
qualification in #912 must retain the reassembly_ controls.

The #1170 repair assigns ordered message arrival on first completion, matching
unordered completion order when both classes are readable. Delayed ordered
fragments no longer overtake an unordered message that completed first.
Queue and paired-wire controls cover both completion orders, TSN wrap, exact
payloads, credit release, and graceful shutdown. SSN order remains intact.
The #1171 repair queues complete single-chunk unordered messages directly,
without inserting them into or scanning the incomplete-fragment collection.
A 1,024-message control with 1,024 retained fragments reduces fragment visits
from 1,049,600 to zero. Retained bytes and FORWARD TSN cleanup stay exact.
The #1172 repair applies the packet's gap-report budget during SACK generation.
It preserves the entire last reportable contiguous range and stops before
building later reports. A 2,048-gap control scans zero chunks at budget zero,
and at most budget plus one chunks for positive budgets with disjoint gaps.
Contiguous runs still require scanning through their last TSN. Controls retain
16-bit offset limits, TSN wrap, duplicate priority, DATA retention and later
cumulative progress. Replacement qualification in #912 must retain these
reassembly_, shutdown_selective_ack_, and payload_queue_gap_ack_ controls.

The #1175 repair indexes the endpoints of contiguous incomplete unordered
fragment runs. Each arrival joins at most two neighboring runs without a
completion scan. A 1,024-fragment in-order message drops from 524,800 fragment
visits to 2,046 endpoint probes. Reverse and alternating arrivals have the same
probe bound. Completion still extracts and checks each message once. Reordered
vector insertion and FORWARD TSN suffix rebuilding retain linear work; this is
not a bound on all queue operations or a measured throughput result. The index
uses at most two entries per incomplete run. Complete single-chunk messages
still bypass it. Controls cover every arrival order of adjacent three-fragment
messages, retained duplicates, partial abandonment, TSN wrap, exact payloads,
and paired association send-credit release. Replacement qualification in #912
must retain these reassembly_ controls.

The #1177 repair tracks remaining payload bytes separately from consumed array
positions in public `BytesArray` sources. Partial chunks reduce the byte count;
empty chunks only advance the position. Controls cover empty arrays and chunks,
zero limits, uneven chunk sizes, partial reads, exact payloads, and consumed
chunk counts. The current stream writer checks the count before its single
source pop; this finding does not establish corruption in that wrapper path.
Replacement qualification in #912 must retain the `bytes_array_` controls.

The #1145 readiness repair reports streams as unwritable in every graceful
shutdown state. Writes keep their existing refusal and leave caller buffers
untouched. Paired histories cover pending, sent, received, and ACK-sent states
while preserving admitted gameplay through terminal reads.

The #1178 repair separates ordered message admission from application reads.
Contiguous complete messages enter a ready queue and advance the protocol SSN
immediately. Unread messages remain available across repeated 16-bit SSN cycles.
The ready queue preserves stream order and original completion timestamps for
selection against unordered messages. FORWARD TSN preserves complete messages,
discards incomplete abandoned messages, and promotes newly eligible successors.
Saved reset generations and their retained TSNs include ready ordered messages.
Replacement qualification in #912 must retain the reassembly and paired shutdown
sequence-wrap controls.

The #1145 admission repair refuses new local streams during every graceful
shutdown state. Pre-handshake stream creation remains available. Incoming DATA
on a previously unseen stream still drains, and saved reset generations keep
their readers. The paired readiness control checks refusal without stream or
generation changes and preserves exact gameplay through terminal reads.

The #636 repair appends serially increasing ordered message fragments without
a binary search. A 1,024-fragment control reduces ordering comparisons from
10,230 to 1,023, including TSN wrap. Reordered arrivals retain binary insertion.
Duplicate first, middle, and last fragments preserve exact payloads. This is an
insertion-work result, not a measured throughput claim. Replacement qualification
in #912 must retain the `reassembly_` and `shutdown` controls.
