# signal-fish-reference-browser

A **browser reference client** for the Signal Fish protocol v3 driving a **real Chromium
`RTCPeerConnection`** (the `chromium-headless-shell` build, launched via
[playwright-core](https://playwright.dev/docs/library)). It exists for **conformance and reference** —
the browser-side counterpart of the [native reference client](../native/README.md), exercising the exact
behaviors only a real browser has (mDNS host-candidate obfuscation, Chromium SDP/ICE, `ondatachannel`
adoption). It is **not a product**: no reconnection logic, no game loop, no API stability promises.

Design rationale (real browser vs Node WebRTC stacks, page/CLI split, mDNS posture, Chromium teardown,
feature-gated cells) lives in [ADR-0005](../../docs/adr/0005-browser-reference-client.md).

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
  gating, server-owned initiator roles, trickle-ICE buffering, Appendix G one-shot transport status, success
  criteria, and causal event ordering (every input is serialized through one promise chain).
- **`dist/cli.js`** (Node ESM) is the process entrypoint: parses argv, launches Chromium, injects the page
  bundle, bridges page events to stdout via `page.exposeFunction` (delivered in call order), enforces the
  watchdog, and maps outcomes to the native client's exit codes.

Chromium can never outlive the CLI: every catchable exit path (normal, watchdog, SIGTERM/SIGINT/SIGHUP,
uncaught exception) tears the browser down with a bounded close-then-kill, and a detached **reaper** process
SIGKILLs Chromium if the CLI itself is SIGKILLed (the interop harness's kill-on-drop path — headless Chromium
does NOT exit on its own when its parent dies). The reaper re-checks `/proc/<pid>/stat` starttime before
killing, so a recycled pid is never signalled. Both halves are pinned by the automated
`browser_cli_signal_teardown_reaps_chromium` interop cell (SIGTERM → graceful teardown + exit 143;
SIGKILL → the reaper clears every `headless_shell` descendant within a bounded window).

## CLI reference

The flag surface mirrors the native client's ([canonical reference](../native/README.md#cli-reference)):
`--server-url`, `--create-room`/`--join-code`, `--peers`, `--expect-total-peers`, `--leave-on-game-start`,
`--game-name`, `--player-name`, `--app-id`, `--platform`, `--exchange`, `--relay-payload`, `--cripple-ice`,
`--p2p-timeout-secs`, `--run-for-secs`, `--max-runtime-secs`, `--protocol-version`,
`--supported-topologies`, `--supported-transports` — identical semantics and defaults, except the
identity defaults are browser-flavored (`--game-name reference-browser`, `--player-name RefBrowser`,
`--app-id reference-browser-app`, `--platform reference-browser`). As with clap, a known flag token never
doubles as a flag's value (`--relay-payload --exchange` is a usage error, exit `2`); values that merely
start with `-` but are not known flags are accepted as-is.

Browser-specific additions and deviations:

| Flag / behavior | Meaning |
|-----------------|---------|
| `--mdns-obfuscation` | Leave Chromium's DEFAULT mDNS host-candidate obfuscation ON (candidates become opaque `<uuid>.local` names — the PLAN P7 `.local` trap). Default OFF: Chromium is launched with `--disable-features=WebRtcHideLocalIpsWithMdns` for deterministic loopback host candidates |
| `--cripple-ice` (browser flavor) | The browser cannot filter network interfaces the way the native engine does; determinism comes from dropping ALL outbound candidates AND ignoring all inbound ones (`signal_received` is still emitted). With zero remote candidates on both sides, no ICE check pair ever forms |
| `--run-for-secs` measurement | The soft window still measures from PROCESS start: the CLI passes the Chromium-launch time already spent into the page engine, which subtracts it |
| Chromium launch failure | Local infrastructure failures (browser launch, page bridge) exit `4`, mirroring the native client's runtime-start failure path |

## JSONL event contract and exit codes

Byte-identical to the native client's — see the
[canonical contract](../native/README.md#jsonl-event-contract) and
[exit codes](../native/README.md#exit-codes). The same Rust harness asserts over native and browser
processes interchangeably. EPIPE handling matches too: a failed stdout write is logged to stderr once and
latches suppression; the run continues to its bounded exit. Usage errors exit `2` before the event stream
starts (no `exiting` event), matching the documented clap behavior.

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
| Pure-v2 browser on `/v2/ws` floors a mesh-preferring room (zero session traffic, full relay matrix) | `mixed_v2_browser_v3_native_relay_floor` |
| Mid-handshake server close → exactly one `error` (real close reason) + prompt exit `3` | `browser_cli_mid_handshake_close_single_error_exit_3` |
| SIGTERM (graceful teardown, exit 143) and SIGKILL (detached reaper) leave zero `headless_shell` survivors | `browser_cli_signal_teardown_reaps_chromium` |

## Dependency policy

Runtime: `playwright-core` only (no full `playwright`, no bundled test runner, no install-time browser
download). Dev: `typescript`, `esbuild`, `prettier`, `@types/node` — all exact-pinned with a committed
`package-lock.json`. No eslint: the surface is two small bundles enforced by strict `tsc`, prettier, and the
runner script's `console.log` grep (stdout purity); an eslint toolchain would outweigh the code it lints.

## Troubleshooting

- **`SIGNAL_FISH_BROWSER_CLI is not set` panic:** run `npm ci && npm run build` here and export
  `SIGNAL_FISH_BROWSER_CLI=<repo>/clients/browser/dist/cli.js`, or just use
  `bash scripts/run-browser-interop.sh`, which does everything.
- **Chromium fails to launch:** install the headless shell
  (`node_modules/.bin/playwright-core install chromium-headless-shell`; on bare CI runners add `--with-deps`
  for the apt libraries).
- **Leftover `headless_shell` processes:** should never happen (bounded teardown + the reaper); if observed,
  capture how the CLI exited — that is a bug in this client, not something to `pkill` away.
