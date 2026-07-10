// WebRTC engine: one browser `RTCPeerConnection` per remote peer, two data
// channels per pair, trickle ICE relayed through the server's opaque `Signal`
// envelope. Protocol semantics mirror the native client's engine
// (clients/native/src/engine.rs):
//
// - The initiator rule comes ONLY from the server (`SessionPlan.peers[].initiate`
//   / `NewPeer.you_initiate`) — never recomputed locally.
// - Initiator: create BOTH data channels (`reliable`: ordered; `unreliable`:
//   `{ordered: false, maxRetransmits: 0}`) before offering so they ride the
//   initial SDP; the answerer adopts them via `ondatachannel`.
// - Trickle ICE: every gathered local candidate is surfaced for relay as
//   `{"IceCandidate": <json>}` where `<json>` is `JSON.stringify` of the
//   browser's `RTCIceCandidate.toJSON()` (`candidate` / `sdpMid` /
//   `sdpMLineIndex` / `usernameFragment` — matchbox-compatible and
//   byte-equivalent to webrtc-rs's `RTCIceCandidateInit`). Remote candidates
//   arriving before the remote description are buffered and flushed after.
// - Crippled mode (`--cripple-ice`, browser flavor): the browser cannot filter
//   network interfaces, so determinism comes from dropping ALL outbound
//   candidates AND ignoring all inbound ones — with zero remote candidates on
//   both sides, no ICE check pair ever forms and the pair deterministically
//   never connects.

/** Label of the ordered, reliable data channel (commands / critical events). */
export const RELIABLE_LABEL = 'reliable';
/** Label of the unordered, no-retransmit channel (movement / state). */
export const UNRELIABLE_LABEL = 'unreliable';

/** Callbacks from engine-owned browser event handlers to the orchestrator. */
export interface EngineCallbacks {
  onLocalCandidate(peer: string, generation: number, candidateJson: string): void;
  onPcState(peer: string, generation: number, state: string): void;
  onChannelOpen(peer: string, generation: number, label: string): void;
  onChannelMessage(peer: string, generation: number, label: string, text: string): void;
}

/** An ICE server entry from a `SessionPlan` (wire shape, camel-cased here). */
export interface PlanIceServer {
  urls: string[];
  username?: string;
  credential?: string;
}

/** Per-remote-peer connection state. */
interface PeerLink {
  generation: number;
  pc: RTCPeerConnection;
  channels: Map<string, RTCDataChannel>;
  openLabels: Set<string>;
  pendingCandidates: RTCIceCandidateInit[];
  remoteDescriptionSet: boolean;
  pairConnected: boolean;
}

/** The per-client WebRTC engine (single-threaded; owned by the orchestrator). */
export class Engine {
  private readonly crippled: boolean;
  private readonly callbacks: EngineCallbacks;
  private readonly peers = new Map<string, PeerLink>();
  private nextGeneration = 0;

  constructor(crippled: boolean, callbacks: EngineCallbacks) {
    this.crippled = crippled;
    this.callbacks = callbacks;
  }

  /** Whether a peer connection toward `peer` already exists. */
  isPaired(peer: string): boolean {
    return this.peers.has(peer);
  }

  isCurrentGeneration(peer: string, generation: number): boolean {
    return this.peers.get(peer)?.generation === generation;
  }

  /** Close and forget a departed peer so a later directive can pair it anew. */
  removePeer(peer: string): void {
    const link = this.peers.get(peer);
    if (link === undefined) {
      return;
    }
    this.peers.delete(peer);
    link.pc.close();
  }

  /**
   * Create the peer connection toward `peer` per the server's pairing
   * directive. Idempotent: re-pairing an existing peer is a no-op.
   * Returns the offer SDP when this side initiates, `null` when it answers.
   */
  async pairWith(
    peer: string,
    initiate: boolean,
    iceServers: PlanIceServer[],
  ): Promise<string | null> {
    if (this.isPaired(peer)) {
      console.error(`already paired with ${peer}; ignoring duplicate pairing directive`);
      return null;
    }

    if (this.nextGeneration >= Number.MAX_SAFE_INTEGER) {
      throw new Error('peer-link generation overflow');
    }
    this.nextGeneration += 1;

    const pc = new RTCPeerConnection({ iceServers: convertIceServers(iceServers) });
    const link: PeerLink = {
      generation: this.nextGeneration,
      pc,
      channels: new Map(),
      openLabels: new Set(),
      pendingCandidates: [],
      remoteDescriptionSet: false,
      pairConnected: false,
    };
    this.peers.set(peer, link);
    this.registerPcHandlers(peer, link);

    if (!initiate) {
      return null;
    }
    try {
      // Both channels must exist before the offer so the SDP negotiates SCTP
      // and the channels open as soon as DTLS completes.
      const reliable = pc.createDataChannel(RELIABLE_LABEL);
      const unreliable = pc.createDataChannel(UNRELIABLE_LABEL, {
        ordered: false,
        maxRetransmits: 0,
      });
      for (const channel of [reliable, unreliable]) {
        link.channels.set(channel.label, channel);
        this.registerChannelHandlers(peer, link, channel);
      }
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      if (typeof offer.sdp !== 'string') {
        throw new Error(`createOffer for ${peer} produced no SDP`);
      }
      return offer.sdp;
    } catch (error) {
      if (this.peers.get(peer) === link) {
        this.peers.delete(peer);
        pc.close();
      }
      throw error;
    }
  }

  /**
   * Responder path: apply a remote offer and produce the answer SDP. Buffered
   * remote candidates are flushed once the remote description is set.
   */
  async handleOffer(peer: string, sdp: string): Promise<string> {
    const link = this.linkOf(peer, 'offer');
    await link.pc.setRemoteDescription({ type: 'offer', sdp });
    link.remoteDescriptionSet = true;
    await this.flushPendingCandidates(link);
    const answer = await link.pc.createAnswer();
    await link.pc.setLocalDescription(answer);
    if (typeof answer.sdp !== 'string') {
      throw new Error(`createAnswer for ${peer} produced no SDP`);
    }
    return answer.sdp;
  }

  /** Initiator path: apply the remote answer; flush buffered candidates. */
  async handleAnswer(peer: string, sdp: string): Promise<void> {
    const link = this.linkOf(peer, 'answer');
    await link.pc.setRemoteDescription({ type: 'answer', sdp });
    link.remoteDescriptionSet = true;
    await this.flushPendingCandidates(link);
  }

  /**
   * Apply (or buffer) a relayed remote ICE candidate. The payload is normally
   * the JSON serialization of an `RTCIceCandidateInit`; a payload that is not
   * valid JSON is tolerated as a bare candidate string (interop with minimal
   * clients, mirroring the native engine).
   */
  async handleRemoteCandidate(peer: string, candidatePayload: string): Promise<void> {
    let init: RTCIceCandidateInit;
    try {
      init = JSON.parse(candidatePayload) as RTCIceCandidateInit;
    } catch {
      // Bare candidate strings carry no media-line binding; sdpMLineIndex 0
      // targets the lone SCTP m-line (a candidate-init with NEITHER sdpMid
      // nor sdpMLineIndex is rejected by the browser with a TypeError).
      init = { candidate: candidatePayload, sdpMLineIndex: 0 };
    }
    const link = this.linkOf(peer, 'candidate');
    if (link.remoteDescriptionSet) {
      await link.pc.addIceCandidate(init);
    } else {
      link.pendingCandidates.push(init);
    }
  }

  /** Record an open channel; `true` exactly once, when both labels are open. */
  noteChannelOpen(peer: string, label: string): boolean {
    const link = this.peers.get(peer);
    if (link === undefined) {
      console.error(`channel ${label} open for unknown peer ${peer}`);
      return false;
    }
    link.openLabels.add(label);
    const reliable = link.channels.get(RELIABLE_LABEL);
    const unreliable = link.channels.get(UNRELIABLE_LABEL);
    const fullyOpen =
      link.openLabels.has(RELIABLE_LABEL) &&
      link.openLabels.has(UNRELIABLE_LABEL) &&
      reliable?.readyState === 'open' &&
      unreliable?.readyState === 'open';
    if (fullyOpen && !link.pairConnected) {
      link.pairConnected = true;
      return true;
    }
    return false;
  }

  /** Look up a stored channel by peer and label. */
  channel(peer: string, label: string): RTCDataChannel | undefined {
    return this.peers.get(peer)?.channels.get(label);
  }

  private linkOf(peer: string, what: string): PeerLink {
    const link = this.peers.get(peer);
    if (link === undefined) {
      throw new Error(`${what} from unpaired peer ${peer}`);
    }
    return link;
  }

  private async flushPendingCandidates(link: PeerLink): Promise<void> {
    const pending = link.pendingCandidates.splice(0);
    for (const candidate of pending) {
      await link.pc.addIceCandidate(candidate);
    }
  }

  private registerPcHandlers(peer: string, link: PeerLink): void {
    const pc = link.pc;
    pc.onconnectionstatechange = () => {
      if (this.peers.get(peer) !== link) {
        return;
      }
      this.callbacks.onPcState(peer, link.generation, pc.connectionState);
    };
    pc.onicecandidate = (event) => {
      // `null` marks end-of-gathering; crippled mode drops everything.
      if (this.peers.get(peer) !== link || this.crippled || event.candidate === null) {
        return;
      }
      this.callbacks.onLocalCandidate(
        peer,
        link.generation,
        JSON.stringify(event.candidate.toJSON()),
      );
    };
    pc.ondatachannel = (event) => {
      if (this.peers.get(peer) !== link) {
        return;
      }
      // Responder path: store the channel BEFORE wiring its handlers so the
      // orchestrator's bookkeeping exists before any open/message callback.
      link.channels.set(event.channel.label, event.channel);
      this.registerChannelHandlers(peer, link, event.channel);
    };
  }

  private registerChannelHandlers(
    peer: string,
    callbackLink: PeerLink,
    channel: RTCDataChannel,
  ): void {
    const label = channel.label;
    // A remotely announced channel may already be open at registration time
    // (the announce task and the open transition race); notify exactly once
    // either way so the open edge can never be missed.
    let openNotified = false;
    const notifyOpen = () => {
      const link = this.peers.get(peer);
      if (
        !openNotified &&
        channel.readyState === 'open' &&
        link === callbackLink &&
        link.channels.get(label) === channel
      ) {
        openNotified = true;
        this.callbacks.onChannelOpen(peer, callbackLink.generation, label);
      }
    };
    channel.onopen = notifyOpen;
    if (channel.readyState === 'open') {
      queueMicrotask(notifyOpen);
    }
    channel.onmessage = (event: MessageEvent<unknown>) => {
      const link = this.peers.get(peer);
      if (link !== callbackLink || link.channels.get(label) !== channel) {
        return;
      }
      const text =
        typeof event.data === 'string'
          ? event.data
          : new TextDecoder().decode(event.data as ArrayBuffer);
      this.callbacks.onChannelMessage(peer, callbackLink.generation, label, text);
    };
  }
}

/** Convert the plan's ICE servers into browser configuration entries. */
function convertIceServers(iceServers: PlanIceServer[]): RTCIceServer[] {
  return iceServers.map((server) => {
    const entry: RTCIceServer = { urls: server.urls };
    if (server.username !== undefined) {
      entry.username = server.username;
    }
    if (server.credential !== undefined) {
      entry.credential = server.credential;
    }
    return entry;
  });
}
