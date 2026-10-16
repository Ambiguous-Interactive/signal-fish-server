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
      check(connections.length === 2, 'both peers negotiate while their offers are held');
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
      connections.length === (scenario === 'synchronous' ? 0 : 2),
      'failure must prevent any new peer connections after teardown',
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

// Regression #1011: relay input remains live during each suspended peer operation.
// Regressions #960/#969/#972/#979: negotiation and retries share absolute connection deadlines.
for (const deadlineKind of [
  'p2p',
  'p2p-late',
  'p2p-late-reject',
  'p2p-connected',
  'p2p-connected-timeout',
  'run',
  'zero',
  'send-failure',
  'retry',
  'plan',
  'plan-expiry',
] as const) {
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
    if (deadlineKind.startsWith('p2p-connected') && seam !== 'candidate') continue;
    if (
      (deadlineKind === 'retry' || deadlineKind.startsWith('plan')) &&
      seam !== 'offer' &&
      seam !== 'local'
    )
      continue;
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
    const secondPeer = '00000000-0000-0000-0000-000000000003';
    let socket: DeadlineSocket | undefined;
    let connection: DeadlineConnection | undefined;
    let release: (() => void) | undefined;
    let reject: ((error: Error) => void) | undefined;
    let running: Promise<number> | undefined;
    let runResult: number | undefined;
    let holdEnabled = deadlineKind !== 'retry' && deadlineKind !== 'plan';
    let connectionCount = 0;
    const p2pExpiresFirst =
      deadlineKind.startsWith('p2p') ||
      deadlineKind === 'send-failure' ||
      deadlineKind === 'retry' ||
      deadlineKind.startsWith('plan');
    const held = new Promise<void>((resolve, rejectHeld) => {
      release = resolve;
      reject = rejectHeld;
    });
    const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
    const check = (condition: boolean, message: string) => {
      if (!condition) failures.push(`${deadlineKind}/${seam}: ${message}`);
    };
    class DeadlineChannel {
      readyState = 'connecting';
      bufferedAmount = 0;
      maxPacketLifeTime = null;
      ordered: boolean;
      maxRetransmits: number | null;
      onopen: (() => void) | null = null;
      constructor(readonly label: string) {
        this.ordered = label === 'reliable';
        this.maxRetransmits = this.ordered ? null : 0;
      }
    }
    class DeadlineConnection {
      readonly index: number;
      closed = false;
      ondatachannel: ((event: { channel: DeadlineChannel }) => void) | null = null;
      constructor() {
        connection = this;
        connectionCount += 1;
        this.index = connectionCount;
      }
      createDataChannel(label: string) {
        return { label, readyState: 'connecting' };
      }
      async createOffer() {
        calls.push('offer');
        if ((holdEnabled || (deadlineKind === 'plan' && this.index === 2)) && seam === 'offer')
          await held;
        return { type: 'offer', sdp: 'test-offer' };
      }
      async setLocalDescription() {
        calls.push('local');
        if (deadlineKind === 'plan' && this.index === 1) {
          now += 700;
          holdEnabled = true;
        } else if (holdEnabled && (seam === 'local' || seam === 'answer-local')) await held;
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
        if (deadlineKind === 'send-failure' && frame.type === 'TransportStatus') {
          throw new Error('socket send failed');
        }
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
              current_players: (deadlineKind.startsWith('plan')
                ? [me, peer, secondPeer]
                : [me, peer]
              ).map((id) => ({ id, epoch: 1, seq: 0 })),
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
                ...(deadlineKind.startsWith('plan')
                  ? [{ player_id: secondPeer, initiate: true }]
                  : []),
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
        peers: deadlineKind.startsWith('plan') ? 3 : 2,
        relayPayload: deadlineKind.startsWith('p2p-connected') ? 'connected-control' : null,
        p2pTimeoutSecs: deadlineKind === 'zero' ? 0 : p2pExpiresFirst ? 1 : 10,
        runForSecs: p2pExpiresFirst ? 3 : 1,
        p2pRetryCount: deadlineKind === 'retry' ? 2 : 0,
      });
      void running.then((code) => {
        runResult = code;
      });
      await turn();
      if (deadlineKind === 'retry') {
        const first = connection;
        now += 700;
        holdEnabled = true;
        socket?.deliver('Signal', {
          from: peer,
          generation: me,
          signal: { PairRetry: 1 },
        });
        await turn();
        check(
          first?.closed === true && connection !== first,
          'retry replaces the initial link',
        );
      }
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
          if (deadlineKind.startsWith('p2p-connected')) {
            for (const label of ['reliable', 'unreliable']) {
              const channel = new DeadlineChannel(label);
              connection?.ondatachannel?.({ channel });
              channel.readyState = 'open';
              channel.onopen?.();
            }
            await turn();
            check(
              events.some((event) => event['event'] === 'p2p_pair_connected'),
              'both required channels must connect before the late candidate',
            );
            now += 900;
          }
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
      check(calls.includes(heldCall), 'the actual browser operation must be suspended');
      socket?.deliver('GameData', {
        from_player: peer,
        epoch: 1,
        seq: 1,
        data: { relay_msg: 'live' },
      });
      now += deadlineKind.startsWith('p2p-connected')
        ? 99
        : deadlineKind === 'retry' || deadlineKind === 'plan'
          ? 299
          : 900;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          timer.callback();
        }
      }
      await turn();
      check(connection?.closed === false, 'negotiation must remain live before its deadline');
      check(
        events.filter((event) => event['event'] === 'game_data_received').length === 1,
        'relay must arrive while peer negotiation is suspended before expiry',
      );
      now +=
        deadlineKind === 'retry' || deadlineKind === 'plan' || deadlineKind === 'p2p-connected'
          ? 1
          : deadlineKind === 'p2p-connected-timeout'
            ? 901
            : deadlineKind === 'p2p-late'
              ? 100
              : 200;
      if (deadlineKind === 'p2p-connected') {
        release?.();
        await turn();
        await turn();
        check(connection?.closed === false, 'late ICE must preserve the established link');
        check(
          !events.some(
            (event) =>
              event['event'] === 'transport_status_sent' && event['connected'] === false,
          ),
          'late ICE must not force a healthy pair onto relay',
        );
        check(
          events.filter((event) => event['event'] === 'game_data_received').length === 1,
          'established-peer ICE must release queued relay data once',
        );
        continue;
      }
      if (deadlineKind.startsWith('p2p-late')) {
        const signalsBeforeCompletion = sent.filter((frame) => frame.type === 'Signal').length;
        if (deadlineKind === 'p2p-late-reject') reject?.(new Error('late stack failure'));
        else release?.();
        await turn();
        await turn();
        check(
          sent.filter((frame) => frame.type === 'Signal').length === signalsBeforeCompletion,
          'expired completion must not signal before the due timer callback runs',
        );
        check(connection?.closed === true, 'expired completion must retire its physical link');
      }
      let timerFailure: unknown;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          try {
            timer.callback();
          } catch (error) {
            timerFailure = error;
          }
        }
      }
      check(timerFailure === undefined, 'deadline callback must not throw outside the chain');
      await turn();
      await turn();
      check(connection?.closed === true, 'deadline must close the stalled physical link');
      if (deadlineKind === 'plan-expiry') {
        check(
          connectionCount === 2,
          'each planned peer must start independently before the shared window expires',
        );
      }
      if (
        deadlineKind.startsWith('p2p') ||
        deadlineKind === 'retry' ||
        deadlineKind.startsWith('plan')
      ) {
        const relayEvents = events.filter((event) => event['event'] === 'game_data_received');
        check(
          relayEvents.length === 1 &&
            relayEvents[0]?.['from'] === peer &&
            JSON.stringify(relayEvents[0]?.['payload']) ===
              JSON.stringify({ relay_msg: 'live' }),
          'queued relay traffic must arrive exactly once and intact before releasing negotiation',
        );
      } else if (deadlineKind === 'send-failure') {
        check(
          runResult === 3,
          'fallback send failure must end the run without releasing negotiation',
        );
      } else {
        check(
          runResult === 0,
          'the run must end from its existing fallback criteria without releasing the browser operation',
        );
      }
      if (deadlineKind !== 'send-failure')
        check(
          events.some(
            (event) =>
              event['event'] === 'transport_status_sent' && event['connected'] === false,
          ),
          'deadline must report relay fallback',
        );
      if (deadlineKind === 'retry') {
        socket?.deliver('Signal', {
          from: peer,
          generation: me,
          signal: { PairRetry: 2 },
        });
        await turn();
        check(
          connectionCount === 2,
          'another retry cannot reopen an expired incomplete window',
        );
      }
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
      if (
        deadlineKind.startsWith('p2p') ||
        deadlineKind === 'retry' ||
        deadlineKind.startsWith('plan')
      ) {
        const oldConnection = connection;
        holdEnabled = false;
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
