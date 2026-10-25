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

import { monotonicNowMs } from '../shared/deadline.js';

/** Label of the ordered, reliable data channel (commands / critical events). */
export const RELIABLE_LABEL = 'reliable';
/** Label of the unordered, no-retransmit channel (movement / state). */
export const UNRELIABLE_LABEL = 'unreliable';

// Per physical link, including candidates already passed to the WebRTC stack.
// Keep aligned with the native engine; draining the queue must not renew it.
const MAX_REMOTE_CANDIDATES = 128;
const MAX_REMOTE_CANDIDATE_BYTES = 64 * 1024;
const MAX_CANDIDATE_PAYLOAD_BYTES = 4096;
const MAX_CHANNEL_MESSAGE_BYTES = 64 * 1024;
// Keep timeout diagnostics below the CLI's 16000-unit diagnostic limit.
const MAX_TRANSPORT_STATE_HISTORY = 32;

type TransportState = Pick<
  RTCPeerConnection,
  'connectionState' | 'signalingState' | 'iceConnectionState' | 'iceGatheringState'
>;

interface TransportObservation extends TransportState {
  elapsedMs: number;
}

interface ChannelDiagnostics {
  receipts: { raw: number; admitted: number; rejected: number; discarded: number };
  error: { errorDetail: RTCErrorDetailType; sctpCauseCode: number | null } | null;
}

interface DataChannelTotals {
  id: number;
  state: string;
  messagesSent: number;
  messagesReceived: number;
  bytesSent: number;
  bytesReceived: number;
}

interface DataChannelStats {
  status: 'not_requested' | 'ready' | 'failed' | 'unavailable';
  pending: boolean;
  sampledAt: number | null;
  requestedAt: number | null;
  channels: Record<string, DataChannelTotals>;
}

/** Callbacks from engine-owned browser event handlers to the orchestrator. */
export interface EngineCallbacks {
  onLocalCandidate(peer: string, generation: number, candidateJson: string): void;
  onPcState(peer: string, generation: number, state: string): void;
  onChannelOpen(peer: string, generation: number, label: string): void;
  onChannelClosed(peer: string, generation: number, label: string, reason?: string): void;
  onChannelMessage(
    peer: string,
    generation: number,
    label: string,
    text: string,
    textBytes: number,
    arrivedAt: number,
  ): boolean | void;
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
  startedAt: number;
  transportStateHistory: TransportObservation[];
  dataChannelStats: DataChannelStats;
  channels: Map<string, RTCDataChannel>;
  openLabels: Set<string>;
  pendingCandidates: RTCIceCandidateInit[];
  remoteCandidateCount: number;
  remoteCandidateBytes: number;
  remoteDescriptionSet: boolean;
  iceCandidateFailures: {
    count: number;
    last: { source: 'buffered' | 'trickle'; name: string | null } | null;
  };
  localIceCandidateErrors: { count: number; lastErrorCode: number | null };
  pairConnected: boolean;
  /** Observed failure fences queued work before the orchestrator retires this link. */
  terminal: boolean;
}

/** The per-client WebRTC engine (single-threaded; owned by the orchestrator). */
export class Engine {
  private readonly crippled: boolean;
  private readonly callbacks: EngineCallbacks;
  private readonly peers = new Map<string, PeerLink>();
  private nextGeneration = 0;
  private readonly channelDiagnostics = new WeakMap<RTCDataChannel, ChannelDiagnostics>();
  private readonly receiveEncoder = new TextEncoder();
  private receiveDecoder: TextDecoder | null = null;
  private receiveScratch: Uint8Array<ArrayBuffer> | null = null;

  constructor(crippled: boolean, callbacks: EngineCallbacks) {
    this.crippled = crippled;
    this.callbacks = callbacks;
  }

  /** Whether a peer connection toward `peer` already exists. */
  isPaired(peer: string): boolean {
    return this.peers.has(peer);
  }

  generationOf(peer: string): number | undefined {
    return this.peers.get(peer)?.generation;
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
      startedAt: performance.now(),
      transportStateHistory: [],
      dataChannelStats: {
        requestedAt: null,
        status: 'not_requested',
        pending: false,
        sampledAt: null,
        channels: {},
      },
      channels: new Map(),
      openLabels: new Set(),
      pendingCandidates: [],
      remoteCandidateCount: 0,
      remoteCandidateBytes: 0,
      remoteDescriptionSet: false,
      iceCandidateFailures: { count: 0, last: null },
      localIceCandidateErrors: { count: 0, lastErrorCode: null },
      pairConnected: false,
      terminal: false,
    };
    this.peers.set(peer, link);
    this.recordTransportState(peer, link);
    this.registerPcHandlers(peer, link);

    if (!initiate) {
      return null;
    }
    let phase = 'create_reliable_channel';
    try {
      // Both channels must exist before the offer so the SDP negotiates SCTP
      // and the channels open as soon as DTLS completes.
      const reliable = pc.createDataChannel(RELIABLE_LABEL);
      link.channels.set(reliable.label, reliable);
      this.registerChannelHandlers(peer, link, reliable);
      phase = 'create_unreliable_channel';
      const unreliable = pc.createDataChannel(UNRELIABLE_LABEL, {
        ordered: false,
        maxRetransmits: 0,
      });
      link.channels.set(unreliable.label, unreliable);
      this.registerChannelHandlers(peer, link, unreliable);
      phase = 'create_offer';
      const offer = await pc.createOffer();
      this.assertCurrentLink(peer, link);
      phase = 'set_local_description';
      await pc.setLocalDescription(offer);
      this.assertCurrentLink(peer, link);
      phase = 'validate_offer';
      if (typeof offer.sdp !== 'string') {
        throw new Error(`createOffer for ${peer} produced no SDP`);
      }
      return offer.sdp;
    } catch (error) {
      if (this.peers.get(peer) === link) {
        try {
          console.error(
            `P2P initial negotiation failed for ${peer}: ${JSON.stringify({ phase, link: this.diagnosticSnapshot(peer) })}`,
          );
        } catch {
          /* Diagnostics must not replace the stack error or prevent cleanup. */
        }
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
    await link.pc.setRemoteDescription({ type: 'offer', sdp: this.remoteSdp(sdp) });
    this.assertCurrentLink(peer, link);
    link.remoteDescriptionSet = true;
    await this.flushPendingCandidates(peer, link);
    this.assertCurrentLink(peer, link);
    const answer = await link.pc.createAnswer();
    this.assertCurrentLink(peer, link);
    await link.pc.setLocalDescription(answer);
    this.assertCurrentLink(peer, link);
    if (typeof answer.sdp !== 'string') {
      throw new Error(`createAnswer for ${peer} produced no SDP`);
    }
    return answer.sdp;
  }

  /** Initiator path: apply the remote answer; flush buffered candidates. */
  async handleAnswer(peer: string, sdp: string): Promise<void> {
    const link = this.linkOf(peer, 'answer');
    await link.pc.setRemoteDescription({ type: 'answer', sdp: this.remoteSdp(sdp) });
    this.assertCurrentLink(peer, link);
    link.remoteDescriptionSet = true;
    await this.flushPendingCandidates(peer, link);
    this.assertCurrentLink(peer, link);
  }

  /**
   * Apply (or buffer) a relayed remote ICE candidate. The payload is normally
   * the JSON serialization of an `RTCIceCandidateInit`; a payload that is not
   * valid JSON is tolerated as a bare candidate string (interop with minimal
   * clients, mirroring the native engine).
   */
  async handleRemoteCandidate(peer: string, candidatePayload: string): Promise<void> {
    if (this.crippled) return;
    // UTF-16 length is a cheap lower bound on UTF-8 bytes. Reject oversized
    // strings before encoding, so even the size check has bounded allocation.
    if (candidatePayload.length > MAX_CANDIDATE_PAYLOAD_BYTES) {
      throw new Error('remote ICE candidate payload exceeds 4096 bytes');
    }
    const payloadBytes = new TextEncoder().encode(candidatePayload).byteLength;
    if (payloadBytes > MAX_CANDIDATE_PAYLOAD_BYTES) {
      throw new Error('remote ICE candidate payload exceeds 4096 bytes');
    }
    const link = this.linkOf(peer, 'candidate');
    if (
      link.remoteCandidateCount >= MAX_REMOTE_CANDIDATES ||
      payloadBytes > MAX_REMOTE_CANDIDATE_BYTES - link.remoteCandidateBytes
    ) {
      throw new Error('remote ICE candidate budget exhausted');
    }
    let init: RTCIceCandidateInit;
    try {
      init = JSON.parse(candidatePayload) as RTCIceCandidateInit;
    } catch {
      // Bare candidate strings carry no media-line binding; sdpMLineIndex 0
      // targets the lone SCTP m-line (a candidate-init with NEITHER sdpMid
      // nor sdpMLineIndex is rejected by the browser with a TypeError).
      init = { candidate: candidatePayload, sdpMLineIndex: 0 };
    }
    link.remoteCandidateCount += 1;
    link.remoteCandidateBytes += payloadBytes;
    if (link.remoteDescriptionSet) {
      try {
        await link.pc.addIceCandidate(init);
      } catch (error) {
        this.noteIceCandidateFailure(peer, link, 'trickle', error);
        throw error;
      }
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
    if (!this.usableLink(peer, link)) return false;
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

  /** Look up a usable channel by peer and label. */
  channel(peer: string, label: string): RTCDataChannel | undefined {
    const link = this.peers.get(peer);
    return link !== undefined && this.usableLink(peer, link)
      ? link.channels.get(label)
      : undefined;
  }

  /** Sample stack counters without delaying gameplay, teardown, or run deadlines. */
  sampleDataChannelStats(peer: string): void {
    const link = this.peers.get(peer);
    if (link === undefined || link.dataChannelStats.pending) return;
    const stats = link.dataChannelStats;
    if (typeof link.pc.getStats !== 'function') {
      stats.status = 'unavailable';
      return;
    }
    stats.pending = true;
    const sampledAt = monotonicNowMs();
    stats.requestedAt = sampledAt;
    // Bind each report to the actual retained SCTP stream, not just its label.
    const channelIds = new Map(
      [...link.channels].map(([label, channel]) => [label, channel.id]),
    );
    const failed = () => {
      if (this.peers.get(peer) !== link) return;
      stats.pending = false;
      stats.status = 'failed';
    };
    try {
      void link.pc
        .getStats()
        .then((report) => {
          if (this.peers.get(peer) !== link) return;
          const channels: Record<string, DataChannelTotals> = {};
          report.forEach((row: Record<string, unknown>) => {
            const label = row['label'];
            const id = row['dataChannelIdentifier'];
            if (
              row['type'] !== 'data-channel' ||
              (label !== RELIABLE_LABEL && label !== UNRELIABLE_LABEL) ||
              typeof id !== 'number' ||
              channelIds.get(label) !== id ||
              typeof row['state'] !== 'string' ||
              !['messagesSent', 'messagesReceived', 'bytesSent', 'bytesReceived'].every(
                (field) =>
                  typeof row[field] === 'number' &&
                  Number.isSafeInteger(row[field]) &&
                  row[field] >= 0,
              )
            )
              return;
            channels[label] = {
              id,
              state: row['state'],
              messagesSent: row['messagesSent'] as number,
              messagesReceived: row['messagesReceived'] as number,
              bytesSent: row['bytesSent'] as number,
              bytesReceived: row['bytesReceived'] as number,
            };
          });
          stats.pending = false;
          stats.status = Object.keys(channels).length === 0 ? 'unavailable' : 'ready';
          stats.sampledAt = sampledAt;
          stats.channels = channels;
        })
        .catch(failed);
    } catch {
      failed();
    }
  }

  /** Local transport state only; excludes signaling payloads and ICE addresses. */
  diagnosticSnapshot(peer: string): object | null {
    const link = this.peers.get(peer);
    if (link === undefined) {
      return null;
    }
    this.recordTransportState(peer, link);
    return {
      generation: link.generation,
      dataChannelStats: {
        status: link.dataChannelStats.status,
        pending: link.dataChannelStats.pending,
        pendingAgeMs:
          link.dataChannelStats.pending && link.dataChannelStats.requestedAt !== null
            ? Math.max(0, monotonicNowMs() - link.dataChannelStats.requestedAt)
            : null,
        ageMs:
          link.dataChannelStats.sampledAt === null
            ? null
            : Math.max(0, monotonicNowMs() - link.dataChannelStats.sampledAt),
        channels: Object.fromEntries(
          Object.entries(link.dataChannelStats.channels).map(([label, totals]) => [
            label,
            { ...totals },
          ]),
        ),
      },
      transportStateHistory: link.transportStateHistory.map((entry) => ({ ...entry })),
      connectionState: link.pc.connectionState,
      signalingState: link.pc.signalingState,
      iceConnectionState: link.pc.iceConnectionState,
      iceGatheringState: link.pc.iceGatheringState,
      dtlsState: link.pc.sctp?.transport?.state ?? null,
      sctpState: link.pc.sctp?.state ?? null,
      remoteDescriptionSet: link.remoteDescriptionSet,
      iceCandidateFailures: {
        count: link.iceCandidateFailures.count,
        last:
          link.iceCandidateFailures.last === null
            ? null
            : { ...link.iceCandidateFailures.last },
      },
      localIceCandidateErrors: { ...link.localIceCandidateErrors },
      remoteCandidateCount: link.remoteCandidateCount,
      pendingRemoteCandidates: link.pendingCandidates.length,
      channels: Object.fromEntries(
        [RELIABLE_LABEL, UNRELIABLE_LABEL].map((label) => {
          const channel = link.channels.get(label);
          return [
            label,
            channel === undefined
              ? null
              : {
                  id: channel.id,
                  readyState: channel.readyState,
                  bufferedAmount: channel.bufferedAmount,
                  openObserved: link.openLabels.has(label),
                  receipts: { ...this.channelDiagnostics.get(channel)?.receipts },
                  error:
                    this.channelDiagnostics.get(channel)?.error === null
                      ? null
                      : { ...this.channelDiagnostics.get(channel)?.error },
                },
          ];
        }),
      ),
    };
  }

  private remoteSdp(sdp: string): string {
    // Complete SDP bypasses trickle-ICE filtering.
    return this.crippled ? sdp.replace(/^a=candidate:.*(?:\r?\n|$)/gm, '') : sdp;
  }

  private linkOf(peer: string, what: string): PeerLink {
    const link = this.peers.get(peer);
    if (link === undefined || !this.usableLink(peer, link)) {
      throw new Error(`${what} from unpaired or terminal peer ${peer}`);
    }
    return link;
  }

  private assertCurrentLink(peer: string, link: PeerLink): void {
    if (!this.usableLink(peer, link))
      throw new Error('the peer link was retired or terminated');
  }

  /** Observe synchronous state changes before their browser events are delivered. */
  private usableLink(peer: string, link: PeerLink): boolean {
    if (this.peers.get(peer) !== link || link.terminal) return false;
    const state = link.pc.connectionState;
    if (state === 'failed' || state === 'closed') {
      // Fence first: callbacks may synchronously inspect or replace this link.
      this.recordTransportState(peer, link);
      link.terminal = true;
      this.callbacks.onPcState(peer, link.generation, state);
      return false;
    }
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      const channel = link.channels.get(label);
      if (channel?.readyState === 'closing' || channel?.readyState === 'closed') {
        link.terminal = true;
        this.callbacks.onChannelClosed(peer, link.generation, label);
        return false;
      }
    }
    return true;
  }

  private async flushPendingCandidates(peer: string, link: PeerLink): Promise<void> {
    const pending = link.pendingCandidates.splice(0);
    for (const candidate of pending) {
      this.assertCurrentLink(peer, link);
      try {
        await link.pc.addIceCandidate(candidate);
      } catch (error) {
        this.noteIceCandidateFailure(peer, link, 'buffered', error);
        console.error('The browser rejected a buffered ICE candidate.');
      }
    }
  }

  private noteIceCandidateFailure(
    peer: string,
    link: PeerLink,
    source: 'buffered' | 'trickle',
    error: unknown,
  ): void {
    if (this.peers.get(peer) !== link) return;
    link.iceCandidateFailures.count = Math.min(
      Number.MAX_SAFE_INTEGER,
      link.iceCandidateFailures.count + 1,
    );
    link.iceCandidateFailures.last = {
      source,
      name:
        error instanceof Error && typeof error.name === 'string'
          ? error.name.slice(0, 64)
          : null,
    };
  }

  /** Record observed states only; never enqueue orchestrator work. */
  private recordTransportState(peer: string, link: PeerLink): void {
    if (this.peers.get(peer) !== link) return;
    const state: TransportState = {
      connectionState: link.pc.connectionState,
      signalingState: link.pc.signalingState,
      iceConnectionState: link.pc.iceConnectionState,
      iceGatheringState: link.pc.iceGatheringState,
    };
    const previous = link.transportStateHistory.at(-1);
    if (
      previous !== undefined &&
      previous.connectionState === state.connectionState &&
      previous.signalingState === state.signalingState &&
      previous.iceConnectionState === state.iceConnectionState &&
      previous.iceGatheringState === state.iceGatheringState
    ) {
      return;
    }
    link.transportStateHistory.push({
      elapsedMs: Math.max(
        previous?.elapsedMs ?? 0,
        Math.round(performance.now() - link.startedAt),
      ),
      ...state,
    });
    if (link.transportStateHistory.length > MAX_TRANSPORT_STATE_HISTORY) {
      link.transportStateHistory.shift();
    }
  }

  private registerPcHandlers(peer: string, link: PeerLink): void {
    const pc = link.pc;
    pc.onicecandidateerror = (event) => {
      if (this.peers.get(peer) !== link || link.terminal) return;
      link.localIceCandidateErrors.count = Math.min(
        Number.MAX_SAFE_INTEGER,
        link.localIceCandidateErrors.count + 1,
      );
      link.localIceCandidateErrors.lastErrorCode = event.errorCode;
    };
    pc.oniceconnectionstatechange = () => this.recordTransportState(peer, link);
    pc.onicegatheringstatechange = () => this.recordTransportState(peer, link);
    pc.onsignalingstatechange = () => this.recordTransportState(peer, link);
    pc.onconnectionstatechange = () => {
      this.recordTransportState(peer, link);
      if (!this.usableLink(peer, link)) return;
      this.callbacks.onPcState(peer, link.generation, pc.connectionState);
    };
    pc.onicecandidate = (event) => {
      // `null` marks end-of-gathering; crippled mode drops everything.
      if (!this.usableLink(peer, link) || this.crippled || event.candidate === null) {
        return;
      }
      this.callbacks.onLocalCandidate(
        peer,
        link.generation,
        JSON.stringify(event.candidate.toJSON()),
      );
    };
    pc.ondatachannel = (event) => {
      // Require the same delivery modes as our locally created channels.
      // Reject invalid announcements before retaining labels or callbacks.
      const channel = event.channel;
      const requiredMode =
        channel.maxPacketLifeTime === null &&
        ((channel.label === RELIABLE_LABEL &&
          channel.ordered === true &&
          channel.maxRetransmits === null) ||
          (channel.label === UNRELIABLE_LABEL &&
            channel.ordered === false &&
            channel.maxRetransmits === 0));
      if (!requiredMode) {
        event.channel.close();
        return;
      }
      if (this.peers.get(peer) !== link) {
        channel.close();
        return;
      }
      if (!this.usableLink(peer, link)) {
        channel.close();
        return;
      }
      // Labels are application names, not unique WebRTC channel identifiers.
      // Preserve the first channel, including locally created initiator channels.
      const existing = link.channels.get(event.channel.label);
      if (existing !== undefined) {
        if (existing !== event.channel) event.channel.close();
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
    const diagnostics: ChannelDiagnostics = {
      receipts: { raw: 0, admitted: 0, rejected: 0, discarded: 0 },
      error: null,
    };
    this.channelDiagnostics.set(channel, diagnostics);
    const count = (kind: keyof ChannelDiagnostics['receipts']) => {
      diagnostics.receipts[kind] = Math.min(
        Number.MAX_SAFE_INTEGER,
        diagnostics.receipts[kind] + 1,
      );
    };
    channel.binaryType = 'arraybuffer';
    // A remotely announced channel may already be open at registration time
    // (the announce task and the open transition race); notify exactly once
    // either way so the open edge can never be missed.
    let openNotified = false;
    const notifyOpen = () => {
      const link = this.peers.get(peer);
      if (
        !openNotified &&
        this.usableLink(peer, callbackLink) &&
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
      // Receipt timing belongs to raw arrival, before UTF-8 normalization.
      const arrivedAt = monotonicNowMs();
      const link = this.peers.get(peer);
      if (link !== callbackLink || link.channels.get(label) !== channel) {
        return;
      }
      count('raw');
      // Publish the discarded classification before a terminal observer can
      // snapshot and retire this link. A usable link has no callback here.
      const previousDiscarded = diagnostics.receipts.discarded;
      count('discarded');
      if (!this.usableLink(peer, link)) return;
      diagnostics.receipts.discarded = previousDiscarded;
      // Preflight allocations; UTF-16 length bounds UTF-8 length from below.
      const data = event.data;
      if (
        (typeof data === 'string' && data.length > MAX_CHANNEL_MESSAGE_BYTES) ||
        (data instanceof ArrayBuffer && data.byteLength > MAX_CHANNEL_MESSAGE_BYTES)
      ) {
        count('rejected');
        rejectMessage('incoming data-channel message exceeds 65536 bytes');
        return;
      }
      if (typeof data !== 'string' && !(data instanceof ArrayBuffer)) {
        count('rejected');
        rejectMessage('incoming data-channel message has an unsupported type');
        return;
      }
      // Each non-streaming decode resets UTF-8 state at the message boundary.
      const text =
        typeof data === 'string'
          ? data
          : (this.receiveDecoder ??= new TextDecoder('utf-8', { ignoreBOM: true })).decode(
              data,
            );
      /* Sizing matches normalized UTF-8, including replacement characters,
       * without allocating another payload buffer for every receipt. */
      let textBytes = 0;
      if (text.length !== 0) {
        /* Three bytes per UTF-16 unit bounds replacement and surrogate encoding.
         * Grow geometrically so small gameplay does not reserve the maximum. */
        const needed = Math.min(MAX_CHANNEL_MESSAGE_BYTES, text.length * 3);
        if (this.receiveScratch === null || this.receiveScratch.length < needed) {
          let capacity = this.receiveScratch?.length ?? 256;
          while (capacity < needed) capacity *= 2;
          this.receiveScratch = new Uint8Array(capacity);
        }
        const { read, written } = this.receiveEncoder.encodeInto(text, this.receiveScratch);
        if (read !== text.length) {
          count('rejected');
          rejectMessage('incoming data-channel message exceeds 65536 bytes');
          return;
        }
        textBytes = written;
      }
      if (
        this.callbacks.onChannelMessage(
          peer,
          callbackLink.generation,
          label,
          text,
          textBytes,
          arrivedAt,
        ) === false
      ) {
        count('rejected');
        rejectMessage('incoming data-channel message backlog exhausted');
      } else {
        count('admitted');
      }
    };
    // A required data channel can fail while the peer connection itself stays
    // `connected`. Treat close/error as a terminal pair signal so the
    // orchestrator reports relay fallback and permits a later plan to rebuild.
    const rejectMessage = (reason: string) => {
      if (callbackLink.terminal) return;
      callbackLink.terminal = true;
      this.callbacks.onChannelClosed(peer, callbackLink.generation, label, reason);
      for (const required of callbackLink.channels.values()) required.close();
    };
    const notifyClosed = () => {
      const link = this.peers.get(peer);
      if (link === callbackLink && !link.terminal && link.channels.get(label) === channel) {
        callbackLink.terminal = true;
        this.callbacks.onChannelClosed(peer, callbackLink.generation, label);
      }
    };
    channel.onclosing = notifyClosed;
    channel.onclose = notifyClosed;
    channel.onerror = (event) => {
      if (
        this.peers.get(peer) !== callbackLink ||
        callbackLink.channels.get(label) !== channel ||
        callbackLink.terminal
      )
        return;
      const error = (event as RTCErrorEvent | undefined)?.error;
      if (error !== undefined) {
        diagnostics.error = {
          errorDetail: error.errorDetail,
          sctpCauseCode: error.sctpCauseCode,
        };
      }
      notifyClosed();
    };
    if (channel.readyState === 'closing' || channel.readyState === 'closed') {
      queueMicrotask(notifyClosed);
    }
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
