import { run } from './orchestrator.js';
import { Engine } from './engine.js';
import type { RunConfig } from '../shared/types.js';

const config: RunConfig = {
  serverUrl: 'ws://mock.invalid/v3/ws',
  createRoom: true,
  joinCode: null,
  peers: 3,
  maxPlayers: null,
  expectTotalPeers: null,
  leaveOnGameStart: false,
  gameName: 'output-cancellation',
  playerName: 'test',
  appId: 'test',
  platform: 'test',
  exchange: false,
  relayPayload: null,
  crippleIce: false,
  p2pTimeoutSecs: 10,
  p2pRetryCount: 0,
  runForSecs: 3,
  successReleaseEnabled: false,
  protocolVersion: 3,
  supportedTopologies: ['mesh'],
  supportedTransports: ['webrtc', 'relay'],
  gameDataFormat: 'json',
  sdkVersion: 'test',
  elapsedBeforeStartMs: 0,
};

const failures: string[] = [];
for (const scenario of ['synchronous', 'delayed'] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalError = console.error;
  const connections: TestConnection[] = [];
  const sent: Array<{ type: string }> = [];
  const me = '00000000-0000-0000-0000-000000000001';
  const peers = [
    '00000000-0000-0000-0000-000000000002',
    '00000000-0000-0000-0000-000000000003',
  ];
  let socket: TestSocket | undefined;
  let rejectOutput: ((error: Error) => void) | undefined;
  let releaseOffer: (() => void) | undefined;
  let running: Promise<number> | undefined;
  const heldOutput = new Promise<void>((_resolve, reject) => {
    rejectOutput = reject;
  });
  // Only the delayed case returns this promise to the output writer.
  void heldOutput.catch(() => {});
  const heldOffer = new Promise<void>((resolve) => {
    releaseOffer = resolve;
  });
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  const check = (condition: boolean, message: string) => {
    if (!condition) failures.push(`${scenario}: ${message}`);
  };

  class TestChannel {
    readyState = 'connecting';
    bufferedAmount = 0;
    binaryType = 'arraybuffer';
    constructor(readonly label: string) {}
    close(): void {
      this.readyState = 'closed';
    }
  }
  class TestConnection {
    closed = false;
    localDescriptionAfterClose = 0;
    readonly channels: TestChannel[] = [];
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string): TestChannel {
      const channel = new TestChannel(label);
      this.channels.push(channel);
      return channel;
    }
    async createOffer(): Promise<{ type: 'offer'; sdp: string }> {
      if (scenario === 'delayed') await heldOffer;
      return { type: 'offer', sdp: 'test-offer' };
    }
    async setLocalDescription(): Promise<void> {
      if (this.closed) this.localDescriptionAfterClose += 1;
    }
    close(): void {
      this.closed = true;
      for (const channel of this.channels) channel.close();
    }
  }
  class TestSocket {
    static readonly OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(type: string, data: Record<string, unknown> = {}): void {
      this.onmessage?.({ data: JSON.stringify({ type, data }) });
    }
    send(line: string): void {
      const frame = JSON.parse(line) as { type: string };
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
            current_players: [me, ...peers].map((id) => ({ id, epoch: 1, seq: 0 })),
          });
          this.deliver('SessionPlan', {
            generation: me,
            topology: 'mesh',
            transport: 'webrtc',
            fallback: 'relay',
            ice_servers: [],
            peers: peers.map((player_id) => ({ player_id, initiate: true })),
          });
        });
    }
    close(): void {
      this.readyState = 3;
    }
  }

  try {
    globalThis.RTCPeerConnection = TestConnection as unknown as typeof RTCPeerConnection;
    globalThis.WebSocket = TestSocket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          if ((JSON.parse(line) as { event: string }).event !== 'session_plan') return;
          if (scenario === 'synchronous') throw new Error('consumer closed');
          return heldOutput;
        },
      },
    });
    console.error = () => {};
    running = run(config);
    await turn();
    if (scenario === 'delayed') {
      check(connections.length === 1, 'peer 1 must be awaiting its actual offer');
      rejectOutput?.(new Error('consumer closed'));
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const code = await Promise.race([
        running,
        new Promise<never>((_resolve, reject) => {
          timer = setTimeout(
            () => reject(new Error('output failure did not stop the run')),
            2000,
          );
        }),
      ]);
      check(code === 3, 'output failure must return connection exit 3');
    } finally {
      clearTimeout(timer);
    }
    check(socket?.readyState === 3, 'run completion must close the socket');
    check(
      connections.every((pc) => pc.closed),
      'run completion must close every existing peer',
    );
    releaseOffer?.();
    await turn();
    await turn();
    check(
      connections.length === (scenario === 'synchronous' ? 0 : 1),
      'failure must prevent new peer connections, including peer 2 after teardown',
    );
    check(
      connections.every((pc) => pc.localDescriptionAfterClose === 0),
      'an offer continuation must not set a local description after peer close',
    );
    check(
      connections.every((pc) => pc.closed),
      'no live peer may survive a late continuation',
    );
    check(!sent.some((frame) => frame.type === 'Signal'), 'failure must prevent signaling');
  } finally {
    releaseOffer?.();
    rejectOutput?.(new Error('test cleanup'));
    socket?.onclose?.();
    await running;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    console.error = originalError;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
// Retiring a responder or answerer must also stop native PC continuations.
for (const seam of [
  'offer-description',
  'answer-description',
  'create-answer',
  'candidates',
] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  const calls: string[] = [];
  const peer = '00000000-0000-0000-0000-000000000002';
  let release: (() => void) | undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  class RetiredConnection {
    async setRemoteDescription(): Promise<void> {
      calls.push('remote');
      if (seam === 'offer-description' || seam === 'answer-description') await held;
    }
    async createAnswer(): Promise<{ type: 'answer'; sdp: string }> {
      calls.push('answer');
      if (seam === 'create-answer') await held;
      return { type: 'answer', sdp: 'test-answer' };
    }
    async setLocalDescription(): Promise<void> {
      calls.push('local');
    }
    async addIceCandidate(): Promise<void> {
      calls.push('candidate');
      if (seam === 'candidates') await held;
    }
    close(): void {
      calls.push('close');
    }
  }
  let operation: Promise<boolean> | undefined;
  try {
    globalThis.RTCPeerConnection = RetiredConnection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    await engine.pairWith(peer, false, []);
    if (seam === 'answer-description' || seam === 'candidates') {
      await engine.handleRemoteCandidate(peer, 'candidate-1');
      await engine.handleRemoteCandidate(peer, 'candidate-2');
    }
    operation = (
      seam === 'answer-description'
        ? engine.handleAnswer(peer, 'remote-answer')
        : engine.handleOffer(peer, 'remote-offer')
    ).then(
      () => true,
      () => false,
    );
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
    const expectedHeld =
      seam === 'create-answer' ? 'answer' : seam === 'candidates' ? 'candidate' : 'remote';
    if (calls.at(-1) !== expectedHeld)
      failures.push(`${seam}: must reach the held native operation`);
    engine.removePeer(peer);
    const closedCalls = calls.length;
    release?.();
    if (await operation) failures.push(`${seam}: retired native work must reject`);
    if (calls.length !== closedCalls)
      failures.push(`${seam}: native PC work continued after close: ${calls.join(',')}`);
    if (engine.isPaired(peer))
      failures.push(`${seam}: a continuation retained the retired peer`);
  } finally {
    release?.();
    await operation;
    globalThis.RTCPeerConnection = originalPc;
  }
}

// Regression #960: browser negotiation must not block relay traffic or run deadlines.
for (const deadlineKind of ['p2p', 'run'] as const) {
  for (const seam of [
    'offer',
    'local',
    'remote-offer',
    'remote-answer',
    'answer',
    'answer-local',
    'candidate',
    'buffered-candidate',
  ] as const) {
    const originalPc = globalThis.RTCPeerConnection;
    const originalSocket = globalThis.WebSocket;
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
    const originalError = console.error;
    const originalNow = performance.now;
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    let now = Math.floor(originalNow.call(performance));
    const timers = new Map<number, { at: number; callback: () => void }>();
    let nextTimer = 0;
    const events: Record<string, unknown>[] = [];
    const sent: Array<{ type: string }> = [];
    const calls: string[] = [];
    const me = '00000000-0000-0000-0000-000000000001';
    const peer = '00000000-0000-0000-0000-000000000002';
    let socket: DeadlineSocket | undefined;
    let connection: DeadlineConnection | undefined;
    let release: (() => void) | undefined;
    let running: Promise<number> | undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
    const check = (condition: boolean, message: string) => {
      if (!condition) failures.push(`${deadlineKind}/${seam}: ${message}`);
    };
    class DeadlineConnection {
      closed = false;
      constructor() {
        connection = this;
      }
      createDataChannel(label: string) {
        return { label, readyState: 'connecting' };
      }
      async createOffer() {
        calls.push('offer');
        if (seam === 'offer') await held;
        return { type: 'offer', sdp: 'test-offer' };
      }
      async setLocalDescription() {
        calls.push('local');
        if (seam === 'local' || seam === 'answer-local') await held;
      }
      async setRemoteDescription() {
        calls.push('remote');
        if (seam.startsWith('remote-')) await held;
      }
      async createAnswer() {
        calls.push('answer');
        if (seam === 'answer') await held;
        return { type: 'answer', sdp: 'test-answer' };
      }
      async addIceCandidate() {
        calls.push('candidate');
        if (seam === 'candidate' || seam === 'buffered-candidate') await held;
      }
      close() {
        this.closed = true;
      }
    }
    class DeadlineSocket {
      static readonly OPEN = 1;
      readyState = 1;
      bufferedAmount = 0;
      onopen: (() => void) | null = null;
      onmessage: ((event: { data: string }) => void) | null = null;
      onclose: (() => void) | null = null;
      constructor() {
        socket = this;
        queueMicrotask(() => this.onopen?.());
      }
      deliver(type: string, data: Record<string, unknown> = {}) {
        this.onmessage?.({ data: JSON.stringify({ type, data }) });
      }
      send(line: string) {
        const frame = JSON.parse(line) as { type: string };
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
              peers: [
                {
                  player_id: peer,
                  initiate: seam === 'offer' || seam === 'local' || seam === 'remote-answer',
                },
              ],
            });
          });
      }
      close() {
        this.readyState = 3;
      }
    }
    try {
      globalThis.RTCPeerConnection = DeadlineConnection as unknown as typeof RTCPeerConnection;
      globalThis.WebSocket = DeadlineSocket as unknown as typeof WebSocket;
      Object.defineProperty(globalThis, 'window', {
        configurable: true,
        value: {
          __sf_emit: (line: string) => {
            events.push(JSON.parse(line));
          },
        },
      });
      performance.now = () => now;
      globalThis.setTimeout = ((callback: () => void, delay = 0) => {
        nextTimer += 1;
        timers.set(nextTimer, { at: now + delay, callback });
        return nextTimer;
      }) as unknown as typeof setTimeout;
      globalThis.clearTimeout = ((handle: number) => {
        timers.delete(handle);
      }) as unknown as typeof clearTimeout;
      console.error = () => {};
      running = run({
        ...config,
        peers: 2,
        p2pTimeoutSecs: deadlineKind === 'p2p' ? 1 : 10,
        runForSecs: deadlineKind === 'run' ? 1 : 3,
      });
      await turn();
      if (seam !== 'offer' && seam !== 'local') {
        if (seam === 'buffered-candidate') {
          socket?.deliver('Signal', {
            from: peer,
            generation: me,
            signal: { IceCandidate: 'candidate' },
          });
          await turn();
        }
        if (seam === 'candidate') {
          socket?.deliver('Signal', {
            from: peer,
            generation: me,
            signal: { Offer: 'test-offer' },
          });
          await turn();
        }
        socket?.deliver('Signal', {
          from: peer,
          generation: me,
          signal:
            seam === 'candidate'
              ? { IceCandidate: 'candidate' }
              : seam === 'remote-answer'
                ? { Answer: 'test-answer' }
                : { Offer: 'test-offer' },
        });
        await turn();
      }
      const heldCall = seam.startsWith('remote-')
        ? 'remote'
        : seam === 'buffered-candidate'
          ? 'candidate'
          : seam === 'answer-local'
            ? 'local'
            : seam;
      check(calls.at(-1) === heldCall, 'the actual browser operation must be suspended');
      socket?.deliver('GameData', {
        from_player: peer,
        epoch: 1,
        seq: 1,
        data: { relay_msg: 'live' },
      });
      now += 1100;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          timer.callback();
        }
      }
      await turn();
      await turn();
      check(connection?.closed === true, 'deadline must close the stalled physical link');
      if (deadlineKind === 'p2p') {
        check(
          events.some((event) => event['event'] === 'game_data_received'),
          'queued relay traffic must progress without releasing the browser operation',
        );
      } else {
        check(
          (await running) === 0,
          'the run must end from its existing fallback criteria without releasing the browser operation',
        );
      }
      check(
        events.some(
          (event) => event['event'] === 'transport_status_sent' && event['connected'] === false,
        ),
        'deadline must report relay fallback',
      );
      const callsAfterDeadline = calls.length;
      const signalsAfterDeadline = sent.filter((frame) => frame.type === 'Signal').length;
      release?.();
      await turn();
      check(
        calls.length === callsAfterDeadline,
        'late completion must not continue browser negotiation',
      );
      check(
        sent.filter((frame) => frame.type === 'Signal').length === signalsAfterDeadline,
        'late completion must not signal the retired link',
      );
      if (deadlineKind === 'p2p') {
        const oldConnection = connection;
        socket?.deliver('SessionPlan', {
          generation: peer,
          topology: 'mesh',
          transport: 'webrtc',
          fallback: 'relay',
          ice_servers: [],
          peers: [{ player_id: peer, initiate: true }],
        });
        await turn();
        check(
          connection !== oldConnection && connection?.closed === false,
          'a new generation must establish a fresh physical link',
        );
        check(
          sent.filter((frame) => frame.type === 'Signal').length === signalsAfterDeadline + 1,
          'fresh negotiation must send its offer',
        );
        check(
          timers.size === 1,
          'completed operations must cancel their deadline timers and leave only the session wake',
        );
      }
    } finally {
      release?.();
      socket?.onclose?.();
      await running;
      check(timers.size === 0, 'shutdown must cancel operation and session timers');
      performance.now = originalNow;
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
      globalThis.WebSocket = originalSocket;
      globalThis.RTCPeerConnection = originalPc;
      console.error = originalError;
      if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
      else Object.defineProperty(globalThis, 'window', originalWindow);
    }
  }
}
if (failures.length > 0) throw new Error(failures.join('\n'));
console.error('ok - output failure and negotiation deadlines fence late browser continuations');
