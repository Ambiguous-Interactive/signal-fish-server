import { run } from './orchestrator.js';
import { signalForRetryAttempt } from './events.js';
import type { RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

assert(
  signalForRetryAttempt(null, 0) === null,
  'default raw null retains nonfatal classification',
);
assert(signalForRetryAttempt(null, 1) === undefined, 'retry raw null is discarded');

// Regression #1022: retry markers and fresh signals preserve sender FIFO. The stale inputs below
// are defensive contract controls; browsers do not request retries themselves.
// Regression #1520: publish local SDP before gathered ICE, without holding remote ICE.
for (const [
  initiate,
  retryCount,
  rejectEarlyCandidate,
  cancelHeldRetry,
  holdInboundCandidate,
  rejectLocalDescription,
  backlogCancellation,
] of [
  [false, 0, false],
  [true, 0, false],
  [false, 2, false],
  [true, 2, false],
  [false, 0, true],
  [true, 0, true],
  [false, 2, false, true],
  [true, 2, false, true],
  [false, 0, false, false, true],
  [true, 0, false, false, true],
  [false, 0, false, false, false, true],
  [true, 0, false, false, false, true],
  [false, 0, false, false, false, false, true],
  [true, 0, false, false, false, false, true],
] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const connections: Connection[] = [];
  const events: Record<string, unknown>[] = [];
  const sent: Array<{ type: string; data: Record<string, unknown> }> = [];
  let socket!: Socket;
  let running: Promise<number> | undefined;
  let blockReplacementOutput = false;
  let replacementOutputHeld = false;
  let releaseReplacementOutput!: () => void;
  const heldReplacementOutput = new Promise<void>((resolve) => {
    releaseReplacementOutput = resolve;
  });
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  class Connection {
    signalingState = 'stable';
    closed = false;
    descriptions: string[] = [];
    candidates: unknown[] = [];
    onicecandidate: ((event: { candidate: { toJSON(): object } }) => void) | null = null;
    readonly index = connections.length;
    localDescriptionPending = false;
    releaseLocalDescription!: () => void;
    readonly localDescriptionHeld = new Promise<void>((resolve) => {
      this.releaseLocalDescription = resolve;
    });
    inboundCandidatePending = false;
    releaseInboundCandidate!: () => void;
    readonly inboundCandidateHeld = new Promise<void>((resolve) => {
      this.releaseInboundCandidate = resolve;
    });
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string) {
      return { label, readyState: 'connecting' };
    }
    async createOffer() {
      return { type: 'offer', sdp: `offer-${connections.length}` };
    }
    async createAnswer() {
      return { type: 'answer', sdp: `answer-${connections.length}` };
    }
    async setLocalDescription(description: { type: string }) {
      this.signalingState = description.type === 'offer' ? 'have-local-offer' : 'stable';
      this.localDescriptionPending = true;
      for (let sequence = 1; sequence <= (backlogCancellation ? 300 : 2); sequence += 1) {
        this.onicecandidate?.({
          candidate: { toJSON: () => ({ candidate: `local-${this.index}-${sequence}` }) },
        });
      }
      await this.localDescriptionHeld;
      this.localDescriptionPending = false;
      if (rejectLocalDescription) throw new Error('local SDP failed after gathering ICE');
    }
    async setRemoteDescription(description: { type: string; sdp: string }) {
      if (description.type === 'answer' && this.signalingState !== 'have-local-offer')
        throw new Error(`answer requires have-local-offer, got ${this.signalingState}`);
      this.descriptions.push(description.sdp);
      this.signalingState = description.type === 'offer' ? 'have-remote-offer' : 'stable';
    }
    async addIceCandidate(candidate: unknown) {
      assert(this.descriptions.length > 0, 'early ICE waits for remote SDP');
      this.candidates.push(candidate);
      if (
        holdInboundCandidate &&
        (candidate as RTCIceCandidateInit).candidate === 'initial-candidate'
      ) {
        this.inboundCandidatePending = true;
        await this.inboundCandidateHeld;
        this.inboundCandidatePending = false;
      }
      if ((candidate as RTCIceCandidateInit).candidate === 'unusable-early-candidate')
        throw new Error('stack rejected one early candidate');
    }
    close() {
      this.closed = true;
    }
  }
  class Socket {
    static OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(type: string, data: Record<string, unknown> = {}) {
      this.onmessage?.({ data: JSON.stringify({ type, data }) });
    }
    signal(signal: object) {
      this.deliver('Signal', { from: peer, generation: me, signal });
    }
    send(line: string) {
      const frame = JSON.parse(line) as (typeof sent)[number];
      sent.push(frame);
      if (frame.type === 'Authenticate')
        queueMicrotask(() => {
          this.deliver('Authenticated');
          this.deliver('ProtocolInfo', { protocol_version: 3 });
        });
      if (frame.type === 'JoinRoom')
        queueMicrotask(() => {
          this.deliver('RoomJoined', {
            player_id: me,
            room_id: me,
            room_code: 'TEST',
            lobby_state: 'finalized',
            ready_players: [],
            current_players: [me, peer].map((id) => ({ id, epoch: 1, seq: 0 })),
          });
          this.deliver('SessionPlan', {
            generation: me,
            topology: 'mesh',
            transport: 'webrtc',
            fallback: 'relay',
            ice_servers: [],
            peers: [{ player_id: peer, initiate }],
          });
        });
    }
    close() {
      this.readyState = 3;
    }
  }
  const config: RunConfig = {
    serverUrl: 'ws://mock.invalid/v3/ws',
    createRoom: true,
    joinCode: null,
    peers: 3,
    maxPlayers: null,
    expectTotalPeers: null,
    leaveOnGameStart: false,
    gameName: 'retry-signals',
    playerName: 'test',
    appId: 'test',
    platform: 'test',
    exchange: false,
    relayPayload: null,
    crippleIce: false,
    p2pTimeoutSecs: 10,
    p2pRetryCount: retryCount,
    runForSecs: 30,
    successReleaseEnabled: false,
    protocolVersion: 3,
    supportedTopologies: ['mesh'],
    supportedTransports: ['webrtc'],
    gameDataFormat: 'json',
    sdkVersion: 'test',
    elapsedBeforeStartMs: 0,
  };
  const description = (sdp: string) => (initiate ? { Answer: sdp } : { Offer: sdp });
  const tagged = (attempt: unknown, signal: object) => ({ RetrySignal: { attempt, signal } });
  const releaseDescription = async (index: number, attempt: number) => {
    await turn();
    const connection = connections[index];
    assert(
      connection?.localDescriptionPending === true,
      'local SDP remains held after gathering ICE',
    );
    const attemptSignals = () =>
      sent
        .filter((frame) => frame.type === 'Signal')
        .map((frame) => frame.data['signal'] as Record<string, unknown>)
        .filter((signal) =>
          attempt === 0
            ? !Object.hasOwn(signal, 'RetrySignal')
            : (signal['RetrySignal'] as { attempt?: number } | undefined)?.attempt === attempt,
        )
        .map((signal) =>
          attempt === 0
            ? signal
            : (signal['RetrySignal'] as { signal: Record<string, unknown> }).signal,
        );
    assert(
      attemptSignals().length === 0,
      `initiate=${initiate}, attempt=${attempt}: local ICE waits until SDP is published`,
    );
    connection.releaseLocalDescription();
    await turn();
    const signals = attemptSignals();
    assert(
      JSON.stringify(signals) ===
        JSON.stringify(
          rejectLocalDescription
            ? []
            : [
                initiate ? { Offer: `offer-${index + 1}` } : { Answer: `answer-${index + 1}` },
                ...[1, 2].map((sequence) => ({
                  IceCandidate: JSON.stringify({ candidate: `local-${index}-${sequence}` }),
                })),
              ],
        ),
      `initiate=${initiate}, attempt=${attempt}: exact local SDP precedes each gathered candidate`,
    );
  };
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          const event = JSON.parse(line) as Record<string, unknown>;
          events.push(event);
          if (blockReplacementOutput && event['event'] === 'session_plan') {
            replacementOutputHeld = true;
            return heldReplacementOutput;
          }
        },
      },
    });
    running = run(config);
    await turn();
    if (rejectEarlyCandidate) {
      socket.signal({ IceCandidate: 'unusable-early-candidate' });
      socket.signal({ IceCandidate: 'usable-early-candidate' });
    }
    socket.signal(description('initial'));
    socket.signal({ IceCandidate: 'initial-candidate' });
    if (backlogCancellation) {
      await turn();
      const connection = connections[0];
      assert(connection?.localDescriptionPending === true, 'hold 300 gathered candidates');
      blockReplacementOutput = true;
      socket.deliver('SessionPlan', {
        generation: peer,
        topology: 'relay',
        transport: 'relay',
        fallback: 'relay',
        ice_servers: [],
        peers: [],
      });
      await turn();
      assert(
        connection.closed && replacementOutputHeld,
        'retire old SDP while room output is held',
      );
      for (let index = 0; index < 250; index += 1) socket.deliver('Pong');
      socket.deliver('PeerTransportStatus', {
        peer_id: peer,
        transport: 'relay',
        connected: true,
      });
      connection.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'retired' }) } });
      await turn();
      assert(
        !events.some(
          (event) => event['event'] === 'error' && String(event['message']).includes('backlog'),
        ),
        'retired candidate credits must not charge the held room chain or reject unrelated input',
      );
      releaseReplacementOutput();
      await turn();
      assert(
        events.some((event) => event['event'] === 'peer_transport_status'),
        'unrelated current-room input resumes after retired candidate credits are released',
      );
      assert(
        !sent.some((frame) => frame.type === 'Signal'),
        'the retired description and candidates never reach signaling',
      );
      continue;
    }
    await releaseDescription(0, 0);
    if (rejectLocalDescription) {
      assert(
        connections[0]?.closed === true,
        'failed local SDP retires the gathered candidate owner',
      );
      continue;
    }
    if (holdInboundCandidate) {
      const connection = connections[0];
      assert(
        connection?.inboundCandidatePending === true,
        'remote ICE stays held after local SDP',
      );
      connection.onicecandidate?.({
        candidate: { toJSON: () => ({ candidate: 'independent-local-candidate' }) },
      });
      await turn();
      assert(
        sent.some(
          (frame) =>
            frame.type === 'Signal' &&
            (frame.data['signal'] as { IceCandidate?: string }).IceCandidate ===
              JSON.stringify({ candidate: 'independent-local-candidate' }),
        ),
        'pending inbound ICE cannot hold gathered local ICE after SDP publication',
      );
      connection.releaseInboundCandidate();
      await turn();
    }
    assert(connections[0]?.descriptions.join() === 'initial', 'initial raw SDP is accepted');
    assert(
      connections[0]?.candidates.length === (rejectEarlyCandidate ? 3 : 1),
      'initial raw ICE is accepted, including candidates after an early rejection',
    );
    if (rejectEarlyCandidate) {
      assert(
        JSON.stringify(connections[0]?.candidates) ===
          JSON.stringify([
            { candidate: 'unusable-early-candidate', sdpMLineIndex: 0 },
            { candidate: 'usable-early-candidate', sdpMLineIndex: 0 },
            { candidate: 'initial-candidate', sdpMLineIndex: 0 },
          ]),
        'early and later candidates retain sender order after stack rejection',
      );
      assert(
        !events.some((event) => event['event'] === 'error'),
        'one rejected early candidate does not fail the peer negotiation',
      );
    }
    const initialSignals = sent.filter((frame) => frame.type === 'Signal');
    assert(
      initialSignals.some((frame) =>
        Object.hasOwn(frame.data['signal'] as object, initiate ? 'Offer' : 'Answer'),
      ),
      'initial outbound SDP keeps the Matchbox wire shape',
    );
    if (rejectEarlyCandidate) {
      assert(
        connections[0]?.signalingState === 'stable',
        'remote negotiation reaches stable after early ICE rejection',
      );
      if (!initiate)
        assert(
          initialSignals.some(
            (frame) =>
              frame.data['to'] === peer &&
              frame.data['generation'] === me &&
              (frame.data['signal'] as Record<string, unknown>)['Answer'] === 'answer-1',
          ),
          'the orchestrator publishes the answer to the correct peer after early ICE rejection',
        );
    }
    if (retryCount === 0) continue;
    socket.signal({ PairRetry: 1 });
    await turn();
    const replacement = connections[1];
    assert(
      replacement !== undefined && connections[0]?.closed === true,
      'marker replaces the link',
    );
    // A conforming sender puts its marker before these fresh tagged signals.
    // Untagged and wrong-attempt inputs must never poison the new description.
    socket.signal(description('old-raw'));
    for (const attempt of [0, 2, -1, 1.5, '1']) {
      socket.signal(tagged(attempt, description('old-tagged')));
      socket.signal(tagged(attempt, { IceCandidate: 'old-candidate' }));
    }
    socket.signal({
      RetrySignal: { attempt: 1, signal: description('extra-field'), extra: true },
    });
    socket.signal({ RetrySignal: { attempt: 1 } });
    socket.signal(tagged(1, { PairRetry: 2 }));
    socket.signal(tagged(1, tagged(1, description('nested'))));
    socket.signal(tagged(1, description('fresh')));
    socket.signal(tagged(1, { IceCandidate: 'fresh-candidate' }));
    if (cancelHeldRetry) {
      await turn();
      assert(replacement.localDescriptionPending, 'the old retry holds gathered candidates');
    } else await releaseDescription(1, 1);
    assert(
      replacement.descriptions.join() === (cancelHeldRetry && initiate ? '' : 'fresh'),
      `initiate=${initiate}: only the current retry description may apply; got ${replacement.descriptions.join()}`,
    );
    assert(
      JSON.stringify(replacement.candidates) ===
        JSON.stringify(
          cancelHeldRetry ? [] : [{ candidate: 'fresh-candidate', sdpMLineIndex: 0 }],
        ),
      'only current retry ICE may apply',
    );
    if (!cancelHeldRetry)
      assert(replacement.signalingState === 'stable', 'fresh negotiation completes');
    if (!cancelHeldRetry)
      replacement.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'local' }) } });
    await turn();
    const retrySignals = sent
      .filter((frame) => frame.type === 'Signal')
      .slice(initialSignals.length)
      .map(
        (frame) =>
          frame.data['signal'] as { RetrySignal?: { attempt: number; signal: object } },
      );
    assert(
      cancelHeldRetry
        ? retrySignals.length === 0
        : retrySignals.length >= 2 &&
            retrySignals.every((signal) => signal.RetrySignal?.attempt === 1),
      'replacement SDP and local ICE carry the current retry attempt',
    );
    socket.signal({ PairRetry: 2 });
    socket.signal(tagged(1, description('old-first-retry')));
    socket.signal(tagged(1, { IceCandidate: 'old-first-retry-candidate' }));
    socket.signal(tagged(2, description('second-retry')));
    socket.signal(tagged(2, { IceCandidate: 'second-retry-candidate' }));
    await releaseDescription(2, 2);
    if (cancelHeldRetry) {
      replacement.releaseLocalDescription();
      replacement.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'stale' }) } });
      await turn();
      assert(
        sent
          .filter((frame) => frame.type === 'Signal')
          .every(
            (frame) =>
              (frame.data['signal'] as { RetrySignal?: { attempt: number } }).RetrySignal
                ?.attempt !== 1,
          ),
        'retired local SDP and gathered ICE cannot escape after a fresh retry',
      );
    }
    assert(replacement.closed, 'a second retry retires the first replacement');
    assert(
      connections[2]?.descriptions.join() === 'second-retry' &&
        JSON.stringify(connections[2]?.candidates) ===
          JSON.stringify([{ candidate: 'second-retry-candidate', sdpMLineIndex: 0 }]),
      'delayed first-attempt SDP and ICE cannot affect the second retry',
    );
  } finally {
    releaseReplacementOutput();
    socket?.onclose?.();
    for (const connection of connections) {
      connection.releaseLocalDescription();
      connection.releaseInboundCandidate();
    }
    await running;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - browser retry signals keep physical attempts separate');
