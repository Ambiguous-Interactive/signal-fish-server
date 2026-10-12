# Signal Fish RTC patch

Source: published `rtc` 0.21.1, MIT/Apache-2.0.
Upstream commit: `2a732df55ca4bcf26b80914fa0f570cfaf74cfcb`.
Crates.io package checksum:
`4e2ca201de5ccc83aa3c08d49a2f08d7ed0aeeb2052fb1ccc517a8b77a6927ce`.

The native reference client patches only this package through a local path.
All other RTC packages retain their registry sources and lockfile versions.
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
remote channel. Refused channels reset their SCTP stream before returning.
The changed upstream files and regression evidence are recorded in audit
[#911](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/911).

Remove this package and its path patch after a published release passes the
same controls. Track that work in
[#912](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/912).
SCTP stream reset/reuse remains separate work in
[#872](https://github.com/Ambiguous-Interactive/signal-fish-server/issues/872).
