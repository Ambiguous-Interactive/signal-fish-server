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
reset retransmission error budget. Denied resets keep the SID reserved and
permit an explicit retry.

DATA for a new stream generation can arrive before the final reset response.
The receiver saves it until the old stream is removed. The receive window
charges payload and retained metadata. TSN lookup rejects duplicates without
a linear scan. The receiver then admits saved DATA in TSN order.
FORWARD TSN, application reads, and local read shutdown retry deferred resets.
No stream-close event is emitted before both reset directions complete.
Stream handles bind to a generation. An old read loop cannot consume a new
stream after its final read completes a reset. Closed SIDs leave the accept queue.
Queued notifications from the old stream are removed before its close event.

Changed upstream files: `src/association/mod.rs`, `stream.rs`, `timer.rs`,
and `association_test.rs`.
`src/endpoint/endpoint_test.rs` uses negotiated request sequences and verifies
SID reservation until reciprocal acknowledgment in its old-message controls.
The added `stream_reset_test.rs` contains deterministic
Sans-I/O controls for both reset directions, sequence replay and wrap, simultaneous
close, delayed DATA, DATA/reset response reordering, drain, forward TSN,
metadata limits, denied requests, retries, and outgoing fragment ordering.
These controls qualify the local repair. They do not claim full RFC 6525 support
for parameter types that upstream does not implement.
The procedures follow [RFC 6525](https://www.rfc-editor.org/rfc/rfc6525.html),
sections 5.1.1, 5.2.1, 5.2.2, and 5.2.7.

Remove the vendored package and path patches after a published release passes
these controls and the RTC/native close-and-reopen controls.
Track replacement with the existing dependency follow-up
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
