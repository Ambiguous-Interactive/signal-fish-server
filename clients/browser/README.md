# signal-fish-reference-browser

A **browser reference client** for the Signal Fish protocol v3 driving a **real Chromium
`RTCPeerConnection`** (the `chromium-headless-shell` build, launched via
[playwright-core](https://playwright.dev/docs/library)). It exists for **conformance and reference** —
the browser-side counterpart of the [native reference client](../native/README.md), exercising the exact
behaviors only a real browser has (mDNS host-candidate obfuscation, Chromium SDP/ICE, `ondatachannel`
adoption). It is **not a product**: no reconnection logic, no game loop, no API stability promises.

Design rationale (real browser vs Node WebRTC stacks, page/CLI split, mDNS posture, Chromium teardown,
feature-gated cells) lives in [ADR-0005](../../docs/adr/0005-browser-reference-client.md).

The reference engine accepts only `reliable` and `unreliable` data channels.
`reliable` must be ordered with no retry or packet lifetime limit.
`unreliable` must be unordered with `maxRetransmits: 0` and no packet lifetime limit.
The engine closes channels with other delivery settings before admission.
**Migration:** Custom peers must use these settings for the required labels.
It closes other remote labels without retaining them or starting callbacks.
**Migration:** Applications that need extra channels must own their channel
handling. This restriction belongs to these reference clients; the server
continues to relay opaque signaling payloads.

Relay probes do not start at or after the run deadline. A delayed relay timer
keeps the missing send criterion and exits with code 1. An existing successful
harness hold or active exit linger keeps its allowed window.

Relay receipt criteria use the time when the client observes a complete frame.
Timely receipts remain valid through decoding and queued processing.
Late receipts keep the criterion unpaid. The client still validates and reports
late frames. Existing successful holds and active linger windows still apply.

Data-channel receipts use the raw channel callback time before text decoding
and UTF-8 size checks. Decoding cannot turn timely input into a late receipt.

The client drains input admitted within the active run window before deciding
success at the run cutoff or exit linger. Queued membership changes, plans,
and channel callbacks can add new gameplay work. Late input does not extend
the original receipt window.

## Quick start

One line from the repository root (build server + native client → lint/build the browser client → install the
Chromium headless shell → run the browser interop cells):

```bash
bash scripts/run-browser-interop.sh
```

Manually:

```bash
# 1. Build + verify this package (this directory).
npm ci
npm run typecheck      # tsc, strict, page + cli projects
npm run format:check   # prettier
npm run build          # esbuild -> dist/page.js + dist/cli.js

# 2. Install the Chromium headless shell (one-time; cached under ~/.cache/ms-playwright).
node_modules/.bin/playwright-core install chromium-headless-shell

# 3. Drive one client by hand against a running server.
node dist/cli.js --server-url ws://127.0.0.1:3536/v3/ws --create-room --peers 2 --exchange
```

stdout is a machine interface (one JSON event per line); all logging — including forwarded page console
output and Playwright/Chromium noise — goes to stderr.

## Architecture

Two esbuild bundles from one strict-TypeScript source tree:

- **`dist/page.js`** (IIFE) runs INSIDE headless Chromium: WebSocket wire + v3 protocol state machine +
  `RTCPeerConnection` engine. A faithful port of the native client's orchestrator — same ready-barrier
  gating, authoritative full-plan replacement (including Relay/Relay empty plans), server-owned initiator
  roles, trickle-ICE buffering, transport-fallback state transitions, success criteria, and causal event
  ordering (every input is serialized through one promise chain).
- **`dist/cli.js`** (Node ESM) is the process entrypoint: parses argv, launches Chromium, injects the page
  bundle, bridges page events to stdout via `page.exposeFunction` (delivered in call order), enforces the
  watchdog, and maps outcomes to the native client's exit codes.

Chromium can never outlive the CLI: every catchable exit path (normal, watchdog, SIGTERM/SIGINT/SIGHUP,
uncaught exception) tears the browser down with a bounded close-then-kill, and a detached **reaper** process
SIGKILLs Chromium if the CLI itself is SIGKILLed (the interop harness's kill-on-drop path — headless Chromium
does NOT exit on its own when its parent dies). The reaper re-checks `/proc/<pid>/stat` starttime before
killing, so a recycled pid is never signalled. Both halves are pinned by the automated
`browser_cli_signal_teardown_reaps_chromium` interop cell (SIGTERM → graceful teardown + exit 143;
SIGKILL → the reaper clears every Chromium descendant within a bounded window).

### Delivery accountability

The page's v3 receive loop validates exact `DeliveryReport` ranges, cumulative
counters, and equality between each loss-counter delta and current range units
before applying relayed data. It retains ranges across bounded rollover reports
(at most 256 ranges each) and accepts a sequence hole
only when their non-overlapping, causally prior union covers every missing
sequence. `RelayStats`, totals, and supplemental errors are diagnostics only.
RelayStats snapshots are checked for a positive stable interval and monotonic
cumulative counters. Unsupported-format errors are optional rate-limited
advisories: when present they require a prior causal exact report, but need not
be adjacent to it. V2 mode remains reliable FIFO, and raw binary data is
always reliable. The default `game_data_format` is `"json"`, so an incoming
binary frame or text `GameDataBinary` is a protocol error in that mode. With
`--game-data-format message_pack`, the client encodes the relay payload as
MessagePack and decodes received payloads into ordinary `game_data_received`
events. V2 uses the legacy three-field relay envelope. V3 requires delivery
stamps. Object keys stay unchanged, including `__proto__`, empty names, and Unicode.
JSON-source data stays on text in mixed rooms. MessagePack integers
outside JavaScript's exact integer range and non-JSON payload types are rejected.
With `--game-data-format rkyv|protobuf`, the client preserves opaque binary
payloads and also accepts JSON text fallback from compatible senders.
Both versions use the [reference probe sender header](../native/README.md#opaque-relay-probes).
**Migration:** Upgrade both reference clients for opaque v2 or mixed v2/v3 runs;
new opaque sends include this application header on both versions.
Receipt events still contain the original CLI payload bytes. New v3 receivers
also accept older untagged payloads except the reserved `SFRP` prefix.
When the server refuses a requested format, the client applies its JSON
downgrade notice and continues on the text floor.

Priority peer lifecycle control can overtake queued data from the old epoch.
The page continues to account for that tail but does not emit/apply it after
`PlayerLeft` or a newer incarnation announcement. Future epochs must match a
prior `PlayerJoined`/`PlayerReconnected` announcement; older epochs are invalid
after data advances. Room/spectator transitions reset room cursors while
connection counters survive; a recipient reconnect starts a new connection
accounting lifetime from `sender_watermarks`. Thus the shared
`game_data_received` event contract exposes only application-current payloads.

With `--exchange`, receipts must contain the sending peer in `from`, the
receiving channel in `channel`, and numeric `seq` zero. Only the required
`reliable` and `unreliable` channels count. JSON whitespace, key order, and extra
fields are allowed. Invalid traffic still emits `channel_message` and cannot
satisfy the receive criterion.

An admitted data-channel payload keeps its `channel_message` event through a
physical-link timeout or retry, even when output delays its handler. This also
applies without `--exchange`. A peer departure, new incarnation, or authoritative
plan replacement discards queued input from the previous membership or link.

When a peer reconnects or rejoins, `--exchange` requires fresh sends and receipts on both
data channels. Completed exchanges for other peers remain valid. A transport
rebuild alone does not reset the exchange. Each replacement link sends the probes
again within the existing exchange window so a peer can recover a lost old-link
probe. Duplicate valid probes do not add receive obligations. When a new session plan replaces an
established link, the client waits for its replacement channels to open or the
P2P deadline to expire before success. Peer departure clears that wait.
A retained logical pair emits `p2p_pair_reconnected` once when both replacement
channels open, after a retry or a new authoritative plan, even after the P2P
deadline. Timeout alone emits no recovery event. First formation emits only
`p2p_pair_connected`. Departure clears this history, so rejoining creates a new
obligation.

Run, P2P, keepalive, exchange, relay, and exit waits use elapsed time. System
clock adjustments do not shorten or extend these waits or the CLI watchdog.
The CLI passes its elapsed startup time to the page, which has its own clock
origin.

Set `--p2p-retry-count N` on both reference clients to enable the `PairRetry`
signal extension. The browser accepts peer-requested attempt numbers from 1
through N (maximum 255). It does not schedule retries itself. The default is 0
(disabled). Repeated or older attempts do not rebuild a link. A fresh session
plan generation or peer departure clears the attempt history. Each replacement
keeps the server-assigned offerer role. An active retry must finish or reach its
P2P deadline before success. Retry offer creation and local description use the
remaining original establishment window. A connected peer can request a
coordinated rebuild with a new window after the old window
ends, even if its old channel closes before the retry marker arrives.
After a retry, both peers wrap each Offer, Answer, and IceCandidate as
`{"RetrySignal":{"attempt":1,"signal":{"Offer":"..."}}}` with the current
attempt number (#1022). They discard raw or wrong-attempt signals after the
marker. The marker stays `{"PairRetry":1}`. Initial signals keep the normal
Matchbox shape. Use matching reference client versions for this extension;
older versions do not support retry envelopes.

The client keeps the first data channel for each label in a peer connection.
It closes duplicate channels so they cannot replace a working channel.
A required channel that is already closing or closed when announced reports
transport loss immediately. It does not wait for the P2P deadline.
Incoming data-channel messages are limited to 64 KiB of raw input and decoded
UTF-8 text. The client retains at most 256 pending messages and 1 MiB of decoded
text across all peers and connection generations. A message stays charged
until stdout consumes its event line. A stdout failure ends the run with an error.
The client reports an error and closes the affected P2P transport when a limit
is exceeded. Other peers and
signaling remain available. These limits cover the application queue, not the
browser's internal WebRTC buffers. Binary messages use array buffers and decode
as UTF-8 with replacement characters for invalid bytes.
The 64 KiB limit is a message size limit. It does not guarantee delivery on
`unreliable`. This channel does not retry lost fragments. A burst of large
messages can overflow the remote UDP receive buffer, including on localhost.
Use `reliable` for data that must arrive. Use small, replaceable updates on
`unreliable`. An open channel and zero `bufferedAmount` do not prove receipt.
**Migration:** Split payloads larger than 64 KiB and keep consuming output.

### WebSocket and output limits

A failed signaling write ends the session with exit code 3, including writes
from peer negotiation and retries. Start a fresh client after this failure.

Each `Authenticate` and `JoinRoom` command has one 20-second response window.
Prelude frames do not restart it. Browser output waits use the same window.
Expiry closes the client with exit code 3; late replies cannot resume the command.

The browser accepts each server frame up to 64 MiB, the server's portable
maximum. Snapshots and reconnect replies can exceed the default 64 KiB inbound
limit. The browser retains at most 256 pending server frames and 128 MiB of
wire bytes. A frame stays charged through handshake processing, early room
membership storage, streaming processing, and stdout acknowledgment.

The page and CLI each retain at most 256 output events and 128 MiB of serialized
UTF-8 bytes, including newlines. This allows a maximum-size opaque payload's
base64 output. The page reserves one terminal error of up to 4096 bytes. The CLI admits
that error in a separate slot and reserves one more 4096-byte `exiting` event.
A CLI stdout failure uses the error slot before teardown starts.
The CLI also bounds stderr to 256 pending lines and 1 MiB. It truncates each
diagnostic after 16000 UTF-16 units. Callback tasks have a separate 512-entry
limit and 128 MiB signal budget.

Overload or output closure ends the run with exit code 3. The client closes
its signaling socket and P2P links. It does not silently drop a frame or count
a failed output as success. The hard watchdog remains active during output
stalls. Final stdout delivery waits at most one second while Chromium closes.
**Migration:** Drain stdout and stderr continuously. Treat exit 3 as a failed
run and start a fresh client. These limits bound retained application work;
they do not establish a total browser or process memory limit.

## CLI reference

The shared flag surface mirrors the native client's ([canonical reference](../native/README.md#cli-reference)):
`--server-url`, `--create-room`/`--join-code`, `--peers`, `--max-players`, `--expect-total-peers`, `--leave-on-game-start`,
`--game-name`, `--player-name`, `--app-id`, `--platform`, `--exchange`, `--relay-payload`, `--cripple-ice`,
`--p2p-timeout-secs`, `--run-for-secs`, `--max-runtime-secs`, `--success-release-file`, `--protocol-version`,
`--supported-topologies`, `--supported-transports` — matching semantics and defaults except for the numeric
representation limit described below and browser-flavored identity defaults (`--game-name reference-browser`,
`--player-name RefBrowser`, `--app-id reference-browser-app`, `--platform reference-browser`). As with clap,
a known flag token never doubles as a flag's value (`--relay-payload --exchange` is a usage error, exit `2`);
values that merely start with `-` but are not known flags are accepted as-is. Numeric values must be
non-negative decimal integers no larger than JavaScript's largest safe integer. Accepted durations beyond
the host timer ceiling are scheduled in bounded chunks, so they remain distant deadlines rather than
expiring immediately.

Browser-specific additions and deviations:

| Flag / behavior | Meaning |
|-----------------|---------|
| `--mdns-obfuscation` | Leave Chromium's DEFAULT mDNS host-candidate obfuscation ON (candidates become opaque `<uuid>.local` names, exercising the documented cross-stack `.local` trap). Default OFF: Chromium is launched with `--disable-features=WebRtcHideLocalIpsWithMdns` for deterministic loopback host candidates |
| `--cripple-ice` (browser flavor) | The browser cannot filter network interfaces the way the native engine does; determinism comes from dropping outbound candidate signals and ignoring inbound candidate signals and candidates embedded in remote offer/answer SDP (`signal_received` is still emitted). Without remote candidates, no ICE check pair forms |
| Native `--ip-family` | Not exposed by the browser client. Chromium chooses its own interfaces; pinning the ICE address family is a native-engine capability used by the IPv6 interop cell |
| Native `--drop-ice-from` | Not exposed by the browser client. It is a native-only matrix-harness fault for isolating one webrtc-rs peer edge; shared interop and `--cripple-ice` behavior remain identical |
| `--run-for-secs` measurement | The soft window still measures from PROCESS start: the CLI passes the Chromium-launch time already spent into the page engine, which subtracts it |
| Chromium launch failure | Local infrastructure failures (browser launch, page bridge) exit `4`, mirroring the native client's runtime-start failure path |

## JSONL event contract and exit codes

Byte-identical to the native client's for the shared CLI surface — see the
[canonical contract](../native/README.md#jsonl-event-contract) and
[exit codes](../native/README.md#exit-codes). The same Rust harness asserts over native and browser
processes interchangeably. The native-only `--drop-ice-from` fault may additionally emit
`ice_candidate_dropped`; browser/shared-flag runs never do. EPIPE handling matches too: a failed stdout
write is logged to stderr once and
latches suppression; the run continues to its bounded exit. Usage errors exit `2` before the event stream
starts (no `exiting` event), matching the documented clap behavior.

When a P2P window expires, stderr reports each unresolved peer's connection,
ICE, SCTP, and required data-channel states. The report includes channel buffer
sizes and observed open callbacks. It excludes SDP, addresses, and credentials.
The report records the timeout state; it does not identify the failure cause.
The client can still accept a late connection within its run window.
At run timeout, stderr also reports a snapshot for each peer with missing
exchange sends or receipts, including connected peers and departed peers with
unpaid exchange debt. Each report names missing sends, missing receipts, and
logical connection state. A `link: null` field means the physical link was removed.
Compare the reports with both endpoints' `channel_message_sent` and
`channel_message` events. A report does not prove the cause of a missing receipt.
Pending browser offer, answer, and ICE operations use the remaining P2P window,
or a fresh `--p2p-timeout-secs` budget after that window expires. The active run
window also bounds these waits. A zero P2P timeout reports fallback immediately
and still permits late connection. In that mode, each operation uses a
`--run-for-secs` budget, capped by the active run window. Each peer has an ordered
SDP and ICE queue. A stalled peer does not block relay input or other peers.
A rejected SDP operation closes its physical link and reports current transport
availability immediately. A rejected ICE candidate keeps the link available for
other candidates. A stalled operation closes its physical link at its deadline. A new session plan
can build a fresh link. A late completion from the retired link cannot resume
negotiation.

## mDNS `.local` posture (empirically pinned)

With `--mdns-obfuscation`, Chromium advertises only `.local` host candidates, which the native side cannot
resolve (no mDNS responder runs in CI). **Pinned outcome** (asserted by `mesh_n3_browser_mdns_obfuscation`):
P2P still establishes. The browser learns the native side's REAL host candidates from the relayed signals and
initiates ICE connectivity checks toward them; the native agent answers and adopts the browser's transport
address as a **peer-reflexive** candidate. webrtc-rs accepts the `.local` candidate without erroring; every
pair connects, everyone reports `{webrtc, connected: true}`, and the fallback never engages.

## Interop scenario matrix rows

The browser cells live in
[`clients/native/tests/browser_interop_e2e.rs`](../native/tests/browser_interop_e2e.rs) behind the
`browser-interop` cargo feature (the default native suite never compiles them) and are run by
[`scripts/run-browser-interop.sh`](../../scripts/run-browser-interop.sh) /
`.github/workflows/browser-interop.yml`. The harness locates this client via `SIGNAL_FISH_BROWSER_CLI`
(path to the built `dist/cli.js`).

| Scenario | Cell |
|----------|------|
| Mixed mesh N=3 (2 native + 1 browser): full glare matrix, 12-message channel matrix, status fan-out, live relay floor | `mixed_mesh_n3_full_webrtc_with_browser` |
| Browser↔browser mesh (2 browser + 1 native; browser creates the room) | `browser_pair_mesh_n3` |
| Host star N=3 with the browser as a non-host client (star edges only) | `host_star_n3_browser_client` |
| Crippled-ICE browser → `{webrtc,false}` + `fallback_engaged`, zero pairs, served by the relay floor | `mesh_n3_browser_crippled_ice_fallback` |
| mDNS `.local` obfuscation trap (see above) | `mesh_n3_browser_mdns_obfuscation` |
| Pure-v2 browser on `/v2/ws` floors a mesh-preferring room (v3 natives get relay plans; full relay matrix) | `mixed_v2_browser_v3_native_relay_floor` |
| Mid-handshake server close → exactly one `error` (real close reason) + prompt exit `3` | `browser_cli_mid_handshake_close_single_error_exit_3` |
| SIGTERM (graceful teardown, exit 143) and SIGKILL (detached reaper) leave zero Chromium survivors | `browser_cli_signal_teardown_reaps_chromium` |

## Dependency policy

Runtime: `playwright-core` only (no full `playwright`, no bundled test runner, no install-time browser
download). Dev: `typescript`, `esbuild`, `prettier`, `@types/node` — all exact-pinned with a committed
`package-lock.json`. No eslint: the surface is two small bundles enforced by strict `tsc`, prettier, and the
runner script's `console.log` grep (stdout purity); an eslint toolchain would outweigh the code it lints.

## Troubleshooting

P2P timeout, incomplete exchange, and failed-link diagnostics include a snapshot
of the current physical link before teardown. The browser reports signaling,
ICE gathering, ICE connection, DTLS, SCTP, and required-channel states. It also
reports whether the remote description was applied, how many remote candidates
were admitted, and how many still wait for that description. An absent SCTP or
DTLS transport is `null`. `transportStateHistory` retains the newest 32 observed
changes to signaling, ICE gathering, ICE connection, and peer-connection states.
Each entry includes `elapsedMs`, measured from creation of this physical link
with the monotonic browser clock and rounded to milliseconds. The initial state
is recorded; repeated observations add no entries. Events and diagnostic reads
record changes, so the history can show transient progress before a timeout.
Each required channel also reports its SCTP stream `id` and `receipts` counters.
`raw` counts current-channel message callbacks. `admitted` counts callbacks that
pass validation and return from the Engine consumer without a backlog rejection.
`rejected` counts input-size, input-type, and backlog failures. `discarded` counts
callbacks fenced by terminal transport state. Admission is recorded after the
consumer returns, so a diagnostic called inside that consumer can see one raw
callback still in progress. These counts do not prove application-level receipt
or SCTP packet delivery. An `error` object records the browser's `errorDetail`
and `sctpCauseCode` when a channel error supplies them; otherwise it is `null`.
Counters saturate at the largest safe integer. A replacement channel starts fresh.
A replacement link starts a new history. The history contains no SDP, candidate
addresses, or message payloads. Browser events can combine state changes, so
these observations do not prove a failure cause. Retain both endpoint logs and
server logs to compare a failed exchange.

- **`SIGNAL_FISH_BROWSER_CLI is not set` panic:** run `npm ci && npm run build` here and export
  `SIGNAL_FISH_BROWSER_CLI=<repo>/clients/browser/dist/cli.js`, or just use
  `bash scripts/run-browser-interop.sh`, which does everything.
- **Chromium fails to launch:** install the headless shell
  (`node_modules/.bin/playwright-core install chromium-headless-shell`; on bare CI runners add `--with-deps`
  for the apt libraries).
- **Leftover Chromium processes:** should never happen (bounded teardown + the reaper); if observed,
  capture how the CLI exited — that is a bug in this client, not something to `pkill` away.

## Remote ICE input limits

Each physical peer link admits at most 128 remote candidates and 64 KiB of
candidate payload in total. Each payload must fit in 4,096 UTF-8 bytes, including
JSON fields. The limits apply before and after the remote description is set.
Rejected candidates emit an error or diagnostic without their content. They do not clear
accepted candidates or close the peer link. Failed stack applications consume
budget too. A replacement peer link receives a fresh budget.

A stack rejection of a buffered candidate reports a fixed diagnostic and does
not stop SDP negotiation or later candidate application. Rejected candidates
are not retried.
