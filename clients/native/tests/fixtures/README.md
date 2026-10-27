# Godot runtime controls

## Native Engine probe

Build the repository-owned JSONL probe before running the Godot driver:

```bash
cp clients/native/tests/fixtures/godot_reference_probe.rs clients/native/examples/godot_reference_probe.rs
cargo build --locked --manifest-path clients/native/Cargo.toml --example godot_reference_probe
rm clients/native/examples/godot_reference_probe.rs
```

Pass `clients/native/target/debug/examples/godot_reference_probe` to
`--native-reference-probe`. The driver at Godot client commit
`f3d3f8611e2c85290f222074da6d40807099e4ec` supports this probe without a patch.
Keep the driver, Godot, extension, server, and probe source and binary hashes
with each attempt. The probe supports the driver's existing text, binary,
batch, startup, signaling, close, removal, and snapshot commands.

`receive_rejected` records the exact Engine failure, physical generation,
channel label, and a transport snapshot taken before link removal. The frozen
failure includes the first failed limit, raw message size, and sampled backlog.
The snapshot includes current callback receipts, RTC channel counters, SCTP
admission counters, ICE/DTLS state, and outstanding send bytes per channel.
These samples are not atomic.
`closed` records physical closure separately with its own transport snapshot.
Snapshots taken after receive rejection return the retained pre-removal sample.
Receipt rows include `receive_boundary` at the first snapshot callback and
snapshot sequence 127 in each generation. Its marker keeps the actual first
sequence, including a missing prefix. A teardown sample can have no SCTP
association; these live samples preserve evidence before that state is removed.
The probe keeps the pinned bridge's existing event names.

Commands must not remove a rejected link ahead of its queued receipt events.
The probe queues those receipts before the terminal row.
It refuses further commands after rejection, except diagnostic snapshots.
It drops stale-generation events before classifying a failure.
One dedicated thread writes stdout. Each receipt keeps its Engine permit until
its row is flushed. A blocked reader must cause an explicit Engine budget
refusal instead of blocking the executor or moving payloads to an uncharged queue.
`probe_us` measures enqueue time. It does not measure stdout flush time.

Run the focused output contract checks with:

```bash
cargo nextest run --locked --manifest-path clients/native/Cargo.toml \
  --test output_e2e -E 'test(godot_reference_probe::tests)'
```

Use `--startup-burst --safe-receive --mixed-traffic --credit-transport webrtc`
with the pinned driver to require exact startup packets and reliable snapshots
in both physical generations. Add `--service-url` and `--service-rejoin` for
same-room service recovery. Keep isolated and loaded attempts separate.
These controls do not explain the historical #1244 prefix loss or #1264 event.

Check blocked stdout separately from a Godot runtime:

```bash
python3 clients/native/tests/fixtures/probe_output_control.py \
  --driver /path/to/godot/scripts/check-webrtc-reference.py \
  --probe "$PWD/clients/native/target/debug/examples/godot_reference_probe" \
  --output /path/to/new-output-directory
```

The control uses the pinned driver's JSONL bridge and two native probes.
Both offer roles exercise count and byte refusal. It holds receiver stdout,
requires observed sender channel closure, queues a snapshot command, then
resumes output. Every admitted reliable packet must appear with exact bytes,
sequence, label, and generation before one typed refusal. The retained snapshot
must still match after link removal. Packet totals may vary with pipe capacity;
the control compares exact receipts with the Engine's admitted total.
Each attempt retains the frozen probe, source hashes, event streams, sender
state, and a separate assessment. Do not reuse an output directory.

## Godot-offer service control (older driver)

Apply `godot-service-offer.patch` to Godot client commit
`988797b4c2f2a870b946870b59046ea5a5f1a297` in a separate checkout.
Use `git apply --unidiff-zero /path/to/server/clients/native/tests/fixtures/godot-service-offer.patch`.
The zero-context format preserves GDScript tabs without patch-context whitespace.
The patch selects an answering reference host and a Godot offerer.
It keeps early service signals until the native peer is paired, counts signals
from the start of each phase, and records the gameplay generation before departure.
It also samples the native transport when reliable snapshot chunk 127 arrives.

Copy the patched `tests/smoke/webrtc_native_reference.rs` into the server's
`clients/native/examples/godot_service_probe.rs`, then build it:

```bash
cargo build --manifest-path clients/native/Cargo.toml --example godot_service_probe
```

Use the Godot client's Python environment and existing native/Web prerequisites.
Start a local Signal Fish Server that accepts the `godot-reference` app, then run:

```bash
python scripts/check-webrtc-reference.py \
  --godot /path/to/Godot_v4.7.2-stable_linux.arm64 \
  --native-extension /path/to/addons/webrtc_native \
  --native-reference-probe /path/to/server/clients/native/target/debug/examples/godot_service_probe \
  --reference-revision session-554-admission-counters \
  --runtime all --offerer godot --safe-receive --mixed-traffic \
  --credit-transport webrtc --service-url ws://127.0.0.1:3536/v3/ws \
  --require-gameplay --output /path/to/new-output-directory
```

Run the Python command from the patched Godot checkout. Remove the copied Rust
example after building. Keep the output directory, including failed attempts.
The control requires all 128 exact reliable snapshots per direction and phase,
32 outstanding snapshots, mixed delivery modes, selected-packet retirement,
and a fresh room and physical generation. Snapshot bodies retain the original
1 KiB size. HTTP carries coordination markers only; service WebSockets carry signaling.

This fixture qualifies two-seat local sessions. It refuses `--service-rejoin`:
reference-host departure changes host selection and needs a separate role-history
control. Remote networks, larger meshes, stable Godot lifetime behavior, and
the historical Godot 4.3 missing prefix remain open in server #1240/#1244 and
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355).
The general driver and host-departure follow-up is
[Godot #444](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/444).

## Observed UDP loss

`webrtc_loss_control.py` wraps the Godot driver at
`f3d3f8611e2c85290f222074da6d40807099e4ec` (Godot PR #445).
It removes inline SDP candidates and selects one IPv4 UDP host candidate per
seat, replacing its address with a local relay. It retains candidate metadata and learns a Web mDNS endpoint
from its first UDP packet. Other local candidates are counted and omitted.
Each physical generation gets fresh sockets and a fresh loss budget.

After the native channel-open observation, the relay drops the requested number
of DTLS application datagrams of at least 1,000 bytes from each seat. STUN and handshake datagrams keep
flowing. `udp-loss.json` records original and observed endpoints, forwarded and
dropped totals, each dropped datagram's size, position, and monotonic time, and
relay errors.
The wrapper requires two generations and the exact drop budget in each direction.
Add `--reorders-per-direction 1` to hold the next large application datagram
until a later large application datagram from that seat arrives. The relay sends
the later datagram first, then releases the held bytes. Small application packets,
STUN, and handshake traffic continue to flow. Each reorder record retains both
receive positions, byte counts, and hold/release times. The held datagram is
released after one second if no later large datagram arrives; that path records
an error and cannot pass. Each generation must meet both direction budgets with
no held tail.
The normal driver still requires every reliable snapshot, credits, exact modes,
selected-packet retirement, required-channel closure, and fresh-link recovery.

Use the same server, probe, Godot, and Web prerequisites as above:

```bash
python /path/to/server/clients/native/tests/fixtures/webrtc_loss_control.py \
  --driver /path/to/godot/scripts/check-webrtc-reference.py \
  --drops-per-direction 1 \
  --godot /path/to/Godot_v4.7.2-stable_linux.arm64 \
  --native-extension /path/to/addons/webrtc_native \
  --native-reference-probe /path/to/native-probe \
  --runtime all --offerer both --safe-receive --mixed-traffic \
  --credit-transport webrtc --service-url ws://127.0.0.1:3536/v3/ws \
  --service-rejoin --require-gameplay --output /path/to/new-loss-output
```

Run the local socket controls with:

```bash
python -m unittest discover -s clients/native/tests/fixtures -p 'test_webrtc_loss_control.py'
```

This is a local loss and reordering control. It does not identify the encrypted
SCTP chunk in a dropped or reordered datagram. It does not establish the cause of
the historical Godot 4.3 prefix loss.
It does not qualify remote networks or repaired Godot packet lifetimes.

## Loss and reordering service result

Session 556 ran this control with one drop and one reorder per direction and
generation. Six final local Linux ARM64 pairings pass: Godot 4.7.2 native and Web, and
Godot 4.3 native, each with both initial offer roles. All 3,072 required reliable
snapshot receipts pass across 12 physical generations. The relay records 24 drops
and 24 reorder releases. Held datagrams are 1,101–1,113 bytes. No relay error or
held tail remains. The control keeps the original snapshot bytes, sequence checks,
32-credit window, mixed modes, selected-packet retirement, required-channel close,
and same-room rejoin checks.

All six final cases use the retained release server with SHA-256
`82dddf9125488a570943073b9f9cccbd706595c47030f31dd3abbc61d6717c68`.
The first Web Godot-offer attempt stopped before pairing because Chromium could
not create its temporary directory (`ENOSPC`). Its separate retry used a workspace
`TMPDIR` and passed. This launch failure is not a gameplay failure. Three initial
successful 4.7.2 cases used an existing debug server whose hash was not retained
before build-cache cleanup. Those attempts remain separate. Repeating those three
cases with the pinned release server passed. The repeated cases overlap a read-only
capacity-ledger audit. The runs do not establish isolated load or performance.

The driver remains pinned to `f3d3f8611e2c85290f222074da6d40807099e4ec`.
The native probe SHA-256 is
`777bc6c066e799001732d97021dea81f11569c0848de10c1257cccec040222fd`.
The final wrapper SHA-256 is
`255fd92df1d9be81bf9df77b900ea0c389921598388d749a64322a6664e18ec8`.
The initial four-cell attempt used wrapper SHA-256
`381072159236b1c6a880d6877a78f5cb5a6856a4f513ea777ccf0aebfdc774c1`;
its only difference is a Ruff line wrap, with the same Python syntax tree.
All six final cases use the final wrapper. Eight socket and verdict controls
pass. Disabling reordering or suppressing its fallback error makes them fail.

These results qualify the observed local encrypted-datagram fault boundary.
They do not identify reliable SCTP DATA inside the encrypted records, explain the
historical 4.3 missing prefix, or qualify remote networks or corrected Godot
lifetimes. Keep server #1240/#1244 and Godot #355/#433 open.

## Sender expiry evidence

Initial `reference:<phase>:<label>` sends include `send_boundary`, sampled after
the successful enqueue. Its `sctp.last_enqueued_message` identifies the local
message, stream incarnation, SSN, ordering and captured reliability policy.
Compare its ID with `first_abandoned_message` and `last_abandoned_message` in
later live snapshots. IDs belong to one physical association; unordered SSNs
can repeat. Only first/last abandonment records are retained, so an absent ID
cannot exclude an intermediate abandonment. No message payload is retained.

`data_chunks_emitted` counts serialized SCTP DATA, including retries;
`data_chunks_retransmitted` counts the retry subset. These aggregate counters
prove neither UDP delivery nor per-message receipt. `messages_abandoned` counts
partial-reliability retirement once per message. A send row remains an enqueue
result. The extra samples can affect scheduling; retain source and binary hashes
and keep strict gameplay failures visible. This evidence does not attribute the
historical #1244, #1264, or Godot #458 failures.

`transmission_selected=false` on an abandonment means no fragment was selected
for send before retirement. A true value also covers acknowledged fragment
prefixes; it does not prove packet emission or remote receipt.

The pinned driver's native profile can accept an absent initial unreliable
probe. Its `gameplay_passed` flag alone does not prove the strict initial-probe
requirement. Inspect `received` in both phase lists: each direction and physical
generation must contain both `reliable` and `unreliable`. Keep missing-initial
results failed even when the driver accepts finite loss. Preserve the original
assessment beside any independent verdict.
