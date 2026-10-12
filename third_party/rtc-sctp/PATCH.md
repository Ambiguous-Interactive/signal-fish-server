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
