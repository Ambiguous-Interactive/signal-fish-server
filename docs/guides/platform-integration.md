# Platform Integration Guide

This guide maps each target platform to a concrete WebRTC stack and walks through what a client must do to
integrate with Signal Fish Server's protocol v3 (and the v2 relay floor underneath it). It is the practical
companion to the wire-level [Protocol v3 additions](../protocol.md#protocol-v3-additions): the protocol reference
tells you _what_ the messages are; this guide tells you _which library_ to reach for on each engine and platform,
and the traps to avoid when two different stacks must interoperate.

It corresponds to PLAN Appendix H (per-platform integration notes). The browser and native rows are demonstrated
end to end by the in-repo reference clients ([`clients/browser/`](../../clients/browser/README.md)
and [`clients/native/`](../../clients/native/README.md)); the mobile,
Steam, and engine rows are integration notes for builds that live outside this repository.

For **runnable, copy-pasteable code**, start from the [Rust Client Guide](rust-client.md) (a complete relay-floor
client) or the two reference clients linked above. This guide deliberately stays at the level of _which WebRTC
choices_ each platform makes on top of that flow, rather than repeating a full client per language. The server's
development default endpoint is `ws://localhost:3536/v2/ws` (or `/v3/ws`); in production use `wss://`.

## Before you pick a stack: the universal contract

Every Signal Fish client, on every platform, speaks the same protocol. The platform only decides _which WebRTC
implementation_ carries the peer-to-peer data once the server hands off — it does not change the signaling
contract.

- **The relay floor is universal and mandatory-capable.** Every client connects over `wss://` (or `ws://` in
  development) and can send and receive `GameData` through the server's WebSocket relay. A client that does
  nothing else is a valid v2 client. See the [Transport Fallback Contract](../architecture/transport-fallback.md).
- **Peer-to-peer is an opt-in upgrade.** A v3 client advertises capabilities in its first `Authenticate`
  (`protocol_version`, `supported_transports`, `supported_topologies`), receives a per-recipient `SessionPlan` at
  lobby finalization, and relays WebRTC handshake traffic through the server's `Signal` message. If P2P never
  establishes, the client stays on the relay floor — the server never stops relaying.
- **The signal payload is opaque and matchbox-shaped.** The server never parses SDP or ICE; it routes by `to` /
  `from` and forwards the `signal` field verbatim. By convention the payload is one of `{"Offer": "<sdp>"}`,
  `{"Answer": "<sdp>"}`, or `{"IceCandidate": "<candidate>"}` (per
  [ADR-0002](../adr/0002-matchbox-compatibility.md)). Any WebRTC stack works as long as the client serializes its
  local description and candidates into that shape and applies the remote side's verbatim.
- **Two data channels per peer.** The recommended game-traffic layout is one **reliable + ordered** channel
  (commands, chat, critical events) and one **unreliable + unordered** channel (`{ordered: false,
  maxRetransmits: 0}`) for movement and frequently-overwritten state. This layout interoperates browser ↔ native.
- **Glare is resolved statelessly.** The `SessionPlan` (and late-join `NewPeer`) tells each side whether it
  `initiate`s the offer to a given peer, so there is no perfect-negotiation dance. The client just obeys the flag.

If your platform can open a `wss://` WebSocket and run a DTLS+SCTP WebRTC data channel, it can be a full v3 client.
If it can only open the WebSocket, it is a valid relay-floor client and still interoperates with everyone else.

## Platform support matrix

| Platform | WebRTC stack | Browser interop | Notes |
|---|---|---|---|
| Browser | native `RTCPeerConnection` / `RTCDataChannel` | n/a | Free and built in; the reason WebRTC is mandatory (no raw UDP; WebTransport is client-server only) |
| Linux / Windows / macOS native | `webrtc-rs`, libdatachannel, or Google libwebrtc | Yes | libdatachannel is lean and broad; `webrtc-rs` is pure Rust (used by the reference client) |
| Mobile (iOS / Android) | libdatachannel or Google libwebrtc | Yes | Both interoperate with browsers; libdatachannel is the lighter embed |
| Steam build | any native stack (embed WebRTC) | Via WebRTC only | Steam networking (GNS / SDR) does **not** interoperate with browsers — embed a WebRTC stack for cross-platform play |
| Godot | built-in WebRTC + `webrtc-native` (libdatachannel) | Yes | Covers the whole matrix and is the easiest engine path |
| Unity | `com.unity.webrtc` | Yes (native) | Native builds work; **WebGL is not supported** by the package — needs a JavaScript bridge |
| Unreal | Pixel Streaming WebRTC / embedded libdatachannel | Yes (embedded) | The Pixel Streaming plugin is P2P-weak; embed libdatachannel outside it for game data |

The remainder of this guide expands each row.

## Browser (TypeScript / JavaScript)

Browsers ship a native `RTCPeerConnection`, which is exactly why WebRTC is the mandatory P2P transport: a browser
cannot open a raw UDP socket, and WebTransport is client-server only. The browser is therefore the lowest common
denominator that every other platform must interoperate with.

- **Signaling.** Open a `WebSocket` to the server's `/v3/ws` endpoint (e.g. `wss://your-server/v3/ws`; the
  development default is `ws://localhost:3536/v3/ws`). Hitting `/v3/ws` defaults the negotiated `protocol_version`
  to 3, but advertising it explicitly is good practice. Send `Authenticate` with `protocol_version: 3`,
  `supported_transports: ["relay", "webrtc"]`, and `supported_topologies` matching your game (`["relay", "mesh"]`
  or `["relay", "host"]`).
- **Handshake.** On `SessionPlan` (or `NewPeer`), create an `RTCPeerConnection` per peer using the plan's
  `ice_servers`. For each peer where `initiate` is true, `createOffer()`, set it locally, and send
  `Signal {to, signal: {Offer: pc.localDescription.sdp}}`. On an incoming `Offer`, set it remotely, `createAnswer()`,
  and reply with `{Answer: ...}`. Forward each `onicecandidate` as `{IceCandidate: JSON.stringify(candidate)}` and
  apply incoming candidates verbatim (trickle ICE — RFC 8838 — comes free over the WebSocket).
- **Channels.** Create `pc.createDataChannel("reliable")` and
  `pc.createDataChannel("unreliable", {ordered: false, maxRetransmits: 0})`.
- **Fallback.** If the connection does not reach `connected` within your timeout, keep using `GameData` over the
  WebSocket and emit `TransportStatus {transport: "webrtc", connected: false}`.

The reference implementation is [`clients/browser/`](../../clients/browser/README.md),
a TypeScript client driving a real headless-Chromium `RTCPeerConnection`. See
[Interop traps](#interop-traps) at the end of this guide — Chrome obfuscates host candidates as `.local` mDNS names.

## Native desktop (Rust, C, C++)

Native desktop clients (Linux, Windows, macOS) have the widest choice of stack:

- **`webrtc-rs`** — pure Rust, no system dependencies. This is what the in-repo
  [`clients/native/`](../../clients/native/README.md) reference client
  uses (version 0.17), exercising real ICE gathering, DTLS handshakes, and SCTP data channels over loopback. The
  reference client is exercised in CI on Linux; the same pure-Rust stack is portable to Windows and macOS.
- **libdatachannel** — a lean C/C++ library (with bindings for many languages) that is the recommended embed when
  you need a small footprint and broad browser interop.
- **Google libwebrtc** — the full upstream stack; heaviest to build, maximum fidelity.

The signaling flow is identical to the browser; only the API names differ. One native-specific detail: when you
serialize an ICE candidate into the `{"IceCandidate": ...}` payload, serialize the full candidate init object (the
`RTCIceCandidateInit` shape — candidate string plus `sdpMid` / `sdpMLineIndex`), not the bare candidate string, so
the remote side can reconstruct it. The reference client documents the exact shape it emits in
[its signal-payload section](../../clients/native/README.md#signal-payload-convention-matchbox-peersignal).

Use the native reference client as a conformance oracle: run it against your build with the shared interop harness
to validate mesh, host-star, late-join, and crippled-ICE relay fallback before shipping.

## Mobile (iOS and Android)

Mobile is a native client with a mobile-friendly WebRTC embed:

- **libdatachannel** — small enough to embed comfortably on both platforms and interoperates with browsers.
- **Google libwebrtc** — the stack the official mobile SDKs are built from; larger but battle-tested on cellular
  networks.

Mobile networks make TURN especially relevant: carrier-grade NAT (CGNAT) and symmetric NAT are common, so plan for
the [TURN relay tier](../deployment-turn.md) (industry data puts ~15–20% of P2P connections on TURN, and mobile
skews higher). The client only ever receives ephemeral, server-minted TURN credentials inside its `SessionPlan` /
ICE list — it never sees the shared secret. Keep the WebSocket signaling on `wss://`; a mobile app on a hostile
network is precisely the on-path-attacker scenario the `wss://` requirement defends against.

## Steam

A Steam build is just a native build, so any native WebRTC stack from the desktop section applies. The one thing to
know: **Steam's own networking (GameNetworkingSockets / Steam Datagram Relay) does not interoperate with browsers.**
If your game is Steam-only and never needs to talk to a browser or another engine, you _can_ use Steam networking
for the actual game data and use Signal Fish purely as a lobby / matchmaking signaler over the relay floor. But the
moment you want cross-platform play with a browser or mobile client, embed a WebRTC stack (libdatachannel is the
usual choice) and follow the native flow above — the WebRTC data path is the common denominator across all
platforms.

## Game engines

### Godot

Godot has the smoothest path of any engine. It ships a built-in `WebRTCPeerConnection` API, and the
`webrtc-native` GDExtension (backed by libdatachannel) provides the implementation on desktop and mobile while the
web export uses the browser's native stack. The same project therefore covers the entire matrix. Implement the
signaling in GDScript or C#: open a `WebSocketPeer` to `/v3/ws`, drive `WebRTCPeerConnection` from the
`SessionPlan`, and pump candidates through `Signal`. Godot's `WebRTCMultiplayerPeer` maps naturally onto the mesh
and host topologies.

### Unity

Use the official `com.unity.webrtc` package for native builds (desktop, mobile, console). It exposes
`RTCPeerConnection` with an API close to the browser's, so the handshake code ports directly.

The critical limitation: **`com.unity.webrtc` does not support WebGL.** A Unity WebGL build has no native WebRTC,
so a browser-targeted Unity game needs a small JavaScript bridge (a `.jslib` plugin) that drives the browser's own
`RTCPeerConnection` and marshals the `Offer` / `Answer` / `IceCandidate` payloads across the C#/JS boundary. The
WebSocket signaling can stay in C# (Unity supports WebSocket on WebGL via a `.jslib` shim too) or move entirely
into the bridge. Plan for this bridge up front if WebGL is a target.

### Unreal

Unreal's WebRTC support is centered on Pixel Streaming, which is shaped for server-to-client video streaming and is
weak for symmetric peer-to-peer data channels. For game data, embed libdatachannel (or another standalone WebRTC
stack) outside the Pixel Streaming plugin and drive it from your networking layer, following the native flow. Treat
Pixel Streaming and your P2P data path as separate concerns.

## Interop traps

These bite when two _different_ stacks must connect (the cross-platform case). They are pinned empirically by the
browser ↔ native reference-client interop matrix.

- **Chrome / Safari `.local` mDNS candidates.** To protect users' local IP addresses, Chrome and Safari replace
  host ICE candidates with obfuscated `.local` mDNS hostnames in DataChannel-only apps. A non-browser peer that
  cannot resolve mDNS will see an unresolvable candidate. In practice P2P still establishes via peer-reflexive
  candidates once STUN runs, and `webrtc-rs` tolerates the unresolvable `.local` candidate — but if you control the
  native stack, make sure it does not hard-fail on a candidate it cannot resolve.
- **SCTP negotiation (`a=sctp-port` vs legacy `sctpmap`).** Modern stacks use the `a=sctp-port` form; very old
  ones emit the legacy `sctpmap`. Current browsers, `webrtc-rs`, and libdatachannel all use the modern form, so
  keep your stacks reasonably up to date and this is a non-issue.
- **DTLS and BUNDLE.** WebRTC negotiates DTLS-SRTP and bundles channels onto one transport automatically; you do
  not configure it, but it is why the `wss://` signaling integrity requirement matters — the DTLS fingerprints
  travel in the SDP, and an on-path attacker who can rewrite them defeats the encryption.
- **Browsers cannot do raw UDP.** There is no fallback to a custom UDP transport in a browser. WebRTC DataChannel
  is the only true browser P2P transport, so any platform that wants browser interop must speak WebRTC — not a
  proprietary networking layer.

## Security checklist (all platforms)

- Use `wss://` for signaling in production — it protects the DTLS fingerprints carried in the SDP.
- Send `Authenticate` before any room operation (`app_id` is your game's public identifier, matched against the
  server's `authorized_apps`; see the [Rust Client Guide](rust-client.md)). When authentication is enabled the
  server rejects unauthenticated traffic.
- Never embed a TURN shared secret in a client. Clients receive only ephemeral, server-minted TURN credentials in
  their `SessionPlan` / ICE list. See [TURN Deployment](../deployment-turn.md).
- Respect the server's signal payload size cap and rate limits; back off rather than hammering the `Signal` path.

## See also

- [Protocol v3 additions](../protocol.md#protocol-v3-additions) — the wire messages, the selection ladder, the
  glare rule, and ICE/TURN.
- [Transport Fallback Contract](../architecture/transport-fallback.md) — the client-side state machine and the
  relay-floor guarantee.
- [Handoff and Topologies](../architecture/handoff-and-topologies.md) — how the server chooses mesh / host / relay
  and fills per-recipient peer lists.
- [Rust Client Guide](rust-client.md) — a step-by-step relay-floor client in Rust.
- [ADR-0002: matchbox compatibility](../adr/0002-matchbox-compatibility.md) — why the signal payload is
  matchbox-shaped and opaque.
- [TURN Deployment](../deployment-turn.md) — operating the relay tier and the ephemeral-credential scheme.
