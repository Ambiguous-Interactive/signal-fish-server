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
for (const [initiate, retryCount, rejectEarlyCandidate] of [
  [false, 0, false],
  [true, 0, false],
  [false, 2, false],
  [true, 2, false],
  [false, 0, true],
  [true, 0, true],
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
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  class Connection {
    signalingState = 'stable';
    closed = false;
    descriptions: string[] = [];
    candidates: unknown[] = [];
    onicecandidate: ((event: { candidate: { toJSON(): object } }) => void) | null = null;
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
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          events.push(JSON.parse(line));
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
    await turn();
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
    await turn();
    assert(
      replacement.descriptions.join() === 'fresh',
      `initiate=${initiate}: only the current retry description may apply; got ${replacement.descriptions.join()}`,
    );
    assert(
      JSON.stringify(replacement.candidates) ===
        JSON.stringify([{ candidate: 'fresh-candidate', sdpMLineIndex: 0 }]),
      'only current retry ICE may apply',
    );
    assert(replacement.signalingState === 'stable', 'fresh negotiation completes');
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
      retrySignals.length >= 2 &&
        retrySignals.every((signal) => signal.RetrySignal?.attempt === 1),
      'replacement SDP and local ICE carry the current retry attempt',
    );
    socket.signal({ PairRetry: 2 });
    socket.signal(tagged(1, description('old-first-retry')));
    socket.signal(tagged(1, { IceCandidate: 'old-first-retry-candidate' }));
    socket.signal(tagged(2, description('second-retry')));
    socket.signal(tagged(2, { IceCandidate: 'second-retry-candidate' }));
    await turn();
    assert(replacement.closed, 'a second retry retires the first replacement');
    assert(
      connections[2]?.descriptions.join() === 'second-retry' &&
        JSON.stringify(connections[2]?.candidates) ===
          JSON.stringify([{ candidate: 'second-retry-candidate', sdpMLineIndex: 0 }]),
      'delayed first-attempt SDP and ICE cannot affect the second retry',
    );
  } finally {
    socket?.onclose?.();
    await running;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - browser retry signals keep physical attempts separate');
