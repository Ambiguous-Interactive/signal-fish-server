import { run } from './orchestrator.js';
import type { RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #1013: a new physical link cannot wait behind the old link's work.
for (const phase of [
  'offer',
  'local-offer',
  'remote-answer',
  'remote-offer',
  'answer',
  'local-answer',
  'candidate',
] as const) {
  for (const oldOutcome of ['resolve', 'reject', 'pending'] as const) {
    const originalPc = globalThis.RTCPeerConnection;
    const originalSocket = globalThis.WebSocket;
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
    const originalNow = performance.now;
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const me = '00000000-0000-0000-0000-000000000001';
    const peer = '00000000-0000-0000-0000-000000000002';
    const stale = '00000000-0000-0000-0000-000000000003';
    const initiate = !['remote-offer', 'answer', 'local-answer'].includes(phase);
    const events: Record<string, unknown>[] = [];
    const sent: Record<string, unknown>[] = [];
    const connections: Connection[] = [];
    const calls: string[] = [];
    const timers = new Map<number, { at: number; callback: () => void }>();
    let timerId = 0;
    let now = 100;
    let socket!: Socket;
    let oldReached = false;
    let resolveOld!: () => void;
    let rejectOld!: (reason: Error) => void;
    let resolveReplacement!: () => void;
    const held = new Promise<void>((resolve, reject) => {
      resolveOld = resolve;
      rejectOld = reject;
    });
    const replacementHeld = new Promise<void>((resolve) => {
      resolveReplacement = resolve;
    });
    const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
    let running: Promise<number> | undefined;
    class Connection {
      readonly index = connections.length;
      closed = false;
      constructor() {
        connections.push(this);
      }
      createDataChannel(label: string) {
        return { label, readyState: 'connecting' };
      }
      async step(name: string) {
        calls.push(`${this.index}:${name}`);
        if (this.index === 0 && phase === name) {
          oldReached = true;
          await held;
        }
        if (this.index === 1 && name.startsWith('remote-')) await replacementHeld;
      }
      async createOffer() {
        await this.step('offer');
        return { type: 'offer', sdp: `offer-${this.index}` };
      }
      async createAnswer() {
        await this.step('answer');
        return { type: 'answer', sdp: `answer-${this.index}` };
      }
      async setLocalDescription(description: { type: string }) {
        await this.step(`local-${description.type}`);
      }
      async setRemoteDescription(description: { type: string }) {
        await this.step(`remote-${description.type}`);
      }
      async addIceCandidate() {
        await this.step('candidate');
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
      signal(signal: Record<string, unknown>, generation = me, from = peer) {
        this.deliver('Signal', { from, generation, signal });
      }
      send(line: string) {
        const frame = JSON.parse(line) as Record<string, unknown>;
        sent.push(frame);
        if (frame['type'] === 'Authenticate')
          queueMicrotask(() => {
            this.deliver('Authenticated');
            this.deliver('ProtocolInfo', { protocol_version: 3 });
          });
        if (frame['type'] === 'JoinRoom')
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
    try {
      globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
      globalThis.WebSocket = Socket as unknown as typeof WebSocket;
      Object.defineProperty(globalThis, 'window', {
        configurable: true,
        value: { __sf_emit: (line: string) => events.push(JSON.parse(line)) },
      });
      performance.now = () => now;
      globalThis.setTimeout = ((callback: () => void, delay = 0) => {
        const id = ++timerId;
        timers.set(id, { at: now + delay, callback });
        return id;
      }) as unknown as typeof setTimeout;
      globalThis.clearTimeout = ((id: number) =>
        timers.delete(id)) as unknown as typeof clearTimeout;
      const config: RunConfig = {
        serverUrl: 'ws://mock.invalid/v3/ws',
        createRoom: true,
        joinCode: null,
        peers: 2,
        maxPlayers: null,
        expectTotalPeers: null,
        leaveOnGameStart: false,
        gameName: 'queued-retry',
        playerName: 'test',
        appId: 'test',
        platform: 'test',
        exchange: false,
        relayPayload: null,
        crippleIce: false,
        p2pTimeoutSecs: 1,
        p2pRetryCount: 1,
        runForSecs: 3,
        successReleaseEnabled: false,
        protocolVersion: 3,
        supportedTopologies: ['mesh'],
        supportedTransports: ['webrtc', 'relay'],
        gameDataFormat: 'json',
        sdkVersion: 'test',
        elapsedBeforeStartMs: 0,
      };
      running = run(config);
      await turn();
      if (!initiate) socket.signal({ Offer: 'old-offer' });
      if (phase === 'remote-answer' || phase === 'candidate')
        socket.signal({ Answer: 'old-answer' });
      if (phase === 'candidate') socket.signal({ IceCandidate: 'old-candidate' });
      await turn();
      assert(oldReached, `${phase}: hold the selected browser operation`);
      const sentBeforeRetry = sent.filter((frame) => frame['type'] === 'Signal').length;
      socket.signal({ PairRetry: 1 }, stale);
      socket.signal({ PairRetry: 1 }, me, stale);
      for (const attempt of [0, 2, 1.5, '1']) socket.signal({ PairRetry: attempt });
      socket.signal({ IceCandidate: 'queued-old-candidate' });
      await turn();
      assert(connections.length === 1, `${phase}: invalid retries cannot replace old work`);
      const errorsBeforeRetry = events.filter((event) => event['event'] === 'error').length;
      now = 700;
      socket.signal({ PairRetry: 1 });
      socket.signal(
        initiate ? { Answer: 'replacement-answer' } : { Offer: 'replacement-offer' },
      );
      socket.signal({ IceCandidate: 'replacement-candidate' });
      await turn();
      assert(
        connections.length === 2 && connections[0]?.closed === true,
        `${phase}: valid retry replaces stalled work before the original deadline`,
      );
      socket.signal({ PairRetry: 1 });
      await turn();
      assert(
        connections.length === 2,
        `${phase}: duplicate retry cannot cancel its replacement`,
      );
      resolveReplacement();
      await turn();
      const remote = calls.indexOf(`1:remote-${initiate ? 'answer' : 'offer'}`);
      const candidate = calls.indexOf('1:candidate');
      assert(remote >= 0 && candidate > remote, `${phase}: replacement SDP precedes ICE`);
      assert(
        calls.filter((call) => call === '1:candidate').length === 1,
        `${phase}: only replacement ICE reaches the new link`,
      );
      if (oldOutcome === 'resolve') resolveOld();
      else if (oldOutcome === 'reject') rejectOld(new Error('retired operation failed'));
      await turn();
      now = 1100;
      for (const [id, timer] of [...timers])
        if (timer.at <= now) {
          timers.delete(id);
          timer.callback();
        }
      await turn();
      assert(
        connections[1]?.closed === false,
        `${phase}: old expiry cannot retire replacement`,
      );
      assert(
        events.some(
          (event) => event['event'] === 'transport_status_sent' && event['connected'] === false,
        ),
        `${phase}: retry preserves fallback at the original connection deadline`,
      );
      assert(
        sent.filter((frame) => frame['type'] === 'Signal').length === sentBeforeRetry + 1,
        `${phase}: only the replacement sends new SDP`,
      );
      assert(
        events.filter((event) => event['event'] === 'error').length === errorsBeforeRetry,
        `${phase}: late old results cannot report a current-link failure`,
      );
    } finally {
      resolveReplacement();
      resolveOld();
      socket?.onclose?.();
      await running;
      await turn();
      performance.now = originalNow;
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
      globalThis.RTCPeerConnection = originalPc;
      globalThis.WebSocket = originalSocket;
      if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
      else Object.defineProperty(globalThis, 'window', originalWindow);
    }
  }
}
console.error('ok - pair retries replace stalled work and preserve replacement SDP/ICE');
