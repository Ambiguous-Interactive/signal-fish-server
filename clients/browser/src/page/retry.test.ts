import { run } from './orchestrator.js';
import { EXIT_CONNECTION_ERROR, EXIT_CRITERIA_UNMET, type RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Drive real dispatch and engine callbacks in both server-assigned roles.
// Clock steps must preserve run, retry, and replacement deadlines (#921, #925).
const retryScenarios = (
  [
    [false, false],
    [true, false],
    [false, true],
    [true, true],
  ] as const
).flatMap(([initiate, closeBeforeRetry]) =>
  (['open', 'timeout', 'removed'] as const).flatMap((replacementOutcome) =>
    ([60_000, -60_000, 0] as const).map(
      (clockStep) => [initiate, closeBeforeRetry, replacementOutcome, clockStep] as const,
    ),
  ),
);
const runTimeoutScenarios = ([false, true] as const).flatMap((initiate) =>
  ([60_000, -60_000, 0] as const).map(
    (clockStep) => [initiate, false, 'run-timeout', clockStep] as const,
  ),
);
// Regression #930: authoritative replacement reports recovery without PairRetry.
const authoritativeScenarios = ([false, true] as const).flatMap((initiate) =>
  ([false, true] as const).flatMap((closeBeforeRetry) =>
    (['open', 'late-open', 'removed'] as const).map(
      (outcome) => [initiate, closeBeforeRetry, outcome, 0, false] as const,
    ),
  ),
);
for (const [initiate, closeBeforeRetry, replacementOutcome, clockStep, retryAuthoritative] of [
  ...retryScenarios.map((scenario) => [...scenario, true] as const),
  ...runTimeoutScenarios.map((scenario) => [...scenario, true] as const),
  ...authoritativeScenarios,
  ...([false, true] as const).map(
    (initiate) => [initiate, false, 'send-failed', 0, false] as const,
  ),
  ...([false, true] as const).flatMap((initiate) =>
    (['first-open', 'late-first-open'] as const).map(
      (outcome) => [initiate, false, outcome, 0, false] as const,
    ),
  ),
]) {
  const originalSocket = globalThis.WebSocket;
  const originalPc = globalThis.RTCPeerConnection;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = Date.now;
  const originalPerformanceNow = performance.now;
  let wallOffset = 0;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const outsider = '00000000-0000-0000-0000-000000000003';
  let generation = me;
  let now = 123;
  const initialMonotonic = now;
  const initialWallTime = originalNow();
  let released = false;
  let settled = false;
  let failWrites = false;
  let socket!: Socket;
  let guard: ReturnType<typeof setTimeout> | undefined;
  const connections: Connection[] = [];
  const events: Record<string, unknown>[] = [];
  const sent: Record<string, unknown>[] = [];
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  class Channel {
    readyState = 'connecting';
    bufferedAmount = 0;
    maxPacketLifeTime = null;
    ordered: boolean;
    maxRetransmits: number | null;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(readonly label: string) {
      this.ordered = label === 'reliable';
      this.maxRetransmits = this.ordered ? null : 0;
    }
    send(): void {}
    close(): void {
      this.readyState = 'closed';
      this.onclose?.();
    }
    open(): void {
      this.readyState = 'open';
      this.onopen?.();
    }
    receive(): void {
      this.onmessage?.({ data: JSON.stringify({ from: peer, channel: this.label, seq: 0 }) });
    }
  }
  class Connection {
    channels: Channel[] = [];
    closed = false;
    offers = 0;
    answers = 0;
    ondatachannel: ((event: { channel: Channel }) => void) | null = null;
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string): Channel {
      const channel = new Channel(label);
      this.channels.push(channel);
      return channel;
    }
    async createOffer() {
      this.offers += 1;
      return { sdp: 'offer' };
    }
    async createAnswer() {
      this.answers += 1;
      return { sdp: 'answer' };
    }
    async setLocalDescription(): Promise<void> {}
    async setRemoteDescription(): Promise<void> {}
    close(): void {
      this.closed = true;
      for (const channel of this.channels) channel.close();
    }
    open(): void {
      if (!initiate) {
        for (const label of ['reliable', 'unreliable']) {
          const channel = this.createDataChannel(label);
          this.ondatachannel?.({ channel });
        }
      }
      for (const channel of this.channels) channel.open();
    }
  }
  class Socket {
    static OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(frame: unknown): void {
      this.onmessage?.({ data: JSON.stringify(frame) });
    }
    send(text: string): void {
      if (failWrites) throw new Error('write failed');
      const frame = JSON.parse(text) as Record<string, unknown>;
      sent.push(frame);
      if (frame['type'] === 'Authenticate') {
        queueMicrotask(() => {
          this.deliver({ type: 'Authenticated', data: {} });
          this.deliver({ type: 'ProtocolInfo', data: { protocol_version: 3 } });
        });
      } else if (frame['type'] === 'JoinRoom') {
        queueMicrotask(() => {
          this.deliver({
            type: 'RoomJoined',
            data: {
              player_id: me,
              room_id: me,
              room_code: 'TEST',
              lobby_state: 'finalized',
              ready_players: [],
              current_players: [
                { id: me, epoch: 1, seq: 0 },
                { id: peer, epoch: 1, seq: 0 },
              ],
            },
          });
          plan();
        });
      }
    }
    close(): void {}
  }
  const plan = () =>
    socket.deliver({
      type: 'SessionPlan',
      data: {
        generation,
        topology: 'mesh',
        transport: 'webrtc',
        fallback: 'relay',
        ice_servers: [],
        peers: [{ player_id: peer, initiate }],
      },
    });
  const signal = (payload: unknown, from = peer, wireGeneration = generation) =>
    socket.deliver({
      type: 'Signal',
      data: { from, generation: wireGeneration, signal: payload },
    });
  const config: RunConfig = {
    serverUrl: 'ws://mock.invalid/v3/ws',
    createRoom: true,
    joinCode: null,
    peers: 2,
    maxPlayers: null,
    expectTotalPeers: replacementOutcome === 'run-timeout' ? 3 : null,
    leaveOnGameStart: false,
    gameName: 'retry',
    playerName: 'test',
    appId: 'test',
    platform: 'test',
    exchange: true,
    relayPayload: null,
    crippleIce: false,
    p2pTimeoutSecs: 1,
    p2pRetryCount: 2,
    runForSecs: 10,
    successReleaseEnabled: replacementOutcome !== 'run-timeout',
    protocolVersion: 3,
    supportedTopologies: ['mesh'],
    supportedTransports: ['webrtc'],
    gameDataFormat: 'json',
    sdkVersion: 'test',
    // The page clock starts after the CLI; the derived run base is negative.
    elapsedBeforeStartMs: 1_000,
  };
  try {
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          events.push(JSON.parse(line));
        },
        __sf_success_released: async () => released,
      },
    });
    Date.now = () => initialWallTime + now - initialMonotonic + wallOffset;
    performance.now = () => now + 0.9;
    const running = run(config).then((code) => {
      settled = true;
      return code;
    });
    const history = async () => {
      await turn();
      const first = connections[0];
      assert(first !== undefined, 'install the initial planned link');
      wallOffset = clockStep;
      socket.deliver({ type: 'Pong' });
      await turn();
      assert(!settled, `clock step ${clockStep} cannot end the run early`);
      assert(
        !events.some((e) => e['event'] === 'transport_status_sent'),
        `clock step ${clockStep} cannot expire the P2P window early`,
      );
      if (replacementOutcome === 'send-failed') {
        failWrites = true;
        signal(initiate ? { PairRetry: 1 } : { Offer: 'incoming-offer' });
        await turn();
        assert(settled, `initiate=${initiate}: a failed signaling write must end the run`);
        assert((await running) === EXIT_CONNECTION_ERROR, 'preserve the connection error');
        assert(
          connections.every((connection) => connection.closed),
          'retire all peer links',
        );
        return;
      }
      if (replacementOutcome === 'run-timeout') {
        // Regression #929: wall-clock steps cannot shorten or extend the run.
        // Require a missing third member and omit the harness hold.
        now = initialMonotonic + config.runForSecs * 1000 - config.elapsedBeforeStartMs - 1;
        socket.deliver({ type: 'Pong' });
        await turn();
        assert(!settled, `clock step ${clockStep}: the run stays live before its deadline`);
        assert(
          !events.some((e) => e['event'] === 'success_criteria_met'),
          'a missing member keeps success criteria unmet',
        );
        now += 1;
        socket.deliver({ type: 'Pong' });
        const exitCode = await running;
        assert(
          exitCode === EXIT_CRITERIA_UNMET,
          `initiate=${initiate}, clock step ${clockStep}: expected unmet-criteria exit, got ${exitCode}`,
        );
        assert(
          events.some(
            (e) =>
              e['event'] === 'error' &&
              String(e['message']).includes(
                '--run-for-secs elapsed with unmet success criteria',
              ),
          ),
          'run expiry reports unmet success criteria',
        );
        return;
      }
      if (replacementOutcome === 'first-open' || replacementOutcome === 'late-first-open') {
        // Replacing a never-formed link still produces an initial connection.
        generation = outsider;
        plan();
        await turn();
        plan();
        await turn();
        assert(connections.length === 2, 'duplicate initial replacement plan keeps one link');
        if (!initiate) signal({ Offer: 'first-offer' });
        await turn();
        if (replacementOutcome === 'late-first-open') {
          now += 1001;
          socket.deliver({ type: 'Pong' });
          await turn();
        }
        first?.open();
        await turn();
        assert(
          !events.some((e) => e['event'] === 'p2p_pair_connected'),
          'retired callbacks cannot form the logical pair',
        );
        const current = connections[1];
        current?.open();
        await turn();
        for (const channel of current?.channels ?? []) {
          channel.open();
          channel.receive();
        }
        socket.deliver({
          type: 'PeerTransportStatus',
          data: { peer_id: peer, transport: 'webrtc', connected: true },
        });
        await turn();
        assert(
          events.filter((e) => e['event'] === 'p2p_pair_connected').length === 1,
          'first successful formation emits one logical pair',
        );
        assert(
          !events.some((e) => e['event'] === 'p2p_pair_reconnected'),
          'never-formed replacement cannot report recovery',
        );
        released = true;
        now += 100;
        socket.deliver({ type: 'Pong' });
        await turn();
        now += 300;
        socket.deliver({ type: 'Pong' });
        assert((await running) === 0, 'first replacement formation completes the exchange');
        return;
      }
      // Retry an incomplete link before any channel opens.
      now += 700;
      signal({ PairRetry: 1 });
      await turn();
      assert(
        connections.length === 2 && first?.closed === true,
        'retry must replace the incomplete physical link',
      );
      assert(
        !events.some((e) => e['event'] === 'transport_status_sent'),
        'an incomplete retry must not resolve fallback before its deadline',
      );
      now += 400;
      socket.deliver({ type: 'Pong' });
      await turn();
      assert(
        events.some((e) => e['event'] === 'transport_status_sent' && e['connected'] === false),
        'retry cannot extend the original incomplete-link deadline',
      );
      signal({ PairRetry: 2 });
      await turn();
      assert(connections.length === 2, 'an expired incomplete-link window cannot restart');
      const replacement = connections[1];
      assert(
        replacement?.offers === (initiate ? 1 : 0),
        'retry preserves the server-assigned offerer role',
      );
      if (!initiate)
        signal({ RetrySignal: { attempt: 1, signal: { Offer: 'replacement-offer' } } });
      await turn();
      replacement?.open();
      await turn();
      for (const channel of replacement?.channels ?? []) channel.receive();
      socket.deliver({
        type: 'PeerTransportStatus',
        data: { peer_id: peer, transport: 'webrtc', connected: true },
      });
      await turn();
      assert(
        events.filter((e) => e['event'] === 'channel_message').length === 2,
        'replacement exchanges both labels',
      );
      // UDP channel close can arrive before the retry marker on the signaling socket.
      if (closeBeforeRetry) {
        replacement?.channels[0]?.close();
        await turn();
      }
      // A completed link can join a peer's coordinated rebuild. Preserve logical exchange evidence.
      signal({ PairRetry: 2 });
      await turn();
      assert(
        connections.length === 3 && replacement?.closed === true,
        'retry must also replace a completed physical link',
      );
      const rebuilt = connections[2];
      released = true;
      now += 400;
      socket.deliver({ type: 'Pong' });
      await turn();
      assert(!settled, 'success must wait while a replacement link is still opening');
      released = false;
      for (const payload of [
        { PairRetry: 2 },
        { PairRetry: 1 },
        { PairRetry: 0 },
        { PairRetry: 3 },
        { PairRetry: 1.5 },
        { PairRetry: '1' },
      ])
        signal(payload);
      signal({ PairRetry: 1 }, outsider);
      signal({ PairRetry: 1 }, peer, outsider);
      await turn();
      assert(
        connections.length === 3,
        'duplicate, old, invalid, unplanned, and stale retries cannot replace the link',
      );
      first?.open();
      for (const channel of first?.channels ?? []) channel.receive();
      await turn();
      assert(
        events.filter((e) => e['event'] === 'channel_message').length === 2,
        'retired callbacks cannot satisfy receipts',
      );
      if (!initiate)
        signal({ RetrySignal: { attempt: 2, signal: { Offer: 'rebuilt-offer' } } });
      await turn();
      rebuilt?.open();
      await turn();
      assert(
        events.filter((e) => e['event'] === 'p2p_pair_connected').length === 1,
        'physical retries keep one logical pair event',
      );
      assert(
        events.filter((e) => e['event'] === 'channel_message_sent').length === 2,
        'physical retry preserves completed exchange debt',
      );
      assert(
        rebuilt?.offers === (initiate ? 1 : 0) && rebuilt?.answers === (initiate ? 0 : 1),
        'both roles use replacement SDP',
      );
      generation = outsider;
      plan();
      await turn();
      // Regression #925: completed old-link receipts cannot skip this new wait.
      released = true;
      now += 100;
      socket.deliver({ type: 'Pong' });
      await turn();
      now += 300;
      socket.deliver({ type: 'Pong' });
      await turn();
      assert(!settled, 'success must wait for an authoritative replacement link');
      released = false;
      const recoveriesBeforePlan = events.filter(
        (e) => e['event'] === 'p2p_pair_reconnected',
      ).length;
      plan();
      await turn();
      assert(connections.length === 4, 'a duplicate plan keeps the current physical link');
      if (retryAuthoritative) {
        signal({ PairRetry: 1 });
        await turn();
        assert(
          connections.length === 5,
          'a new authoritative generation starts a fresh retry budget',
        );
      }
      const finalLink = connections[retryAuthoritative ? 4 : 3];
      if (!initiate)
        signal(
          retryAuthoritative
            ? { RetrySignal: { attempt: 1, signal: { Offer: 'final-offer' } } }
            : { Offer: 'final-offer' },
        );
      await turn();
      if (replacementOutcome === 'open' || replacementOutcome === 'late-open') {
        if (replacementOutcome === 'late-open') {
          now += 1000;
          socket.deliver({ type: 'Pong' });
          await turn();
          assert(
            events.filter((e) => e['event'] === 'p2p_pair_reconnected').length ===
              recoveriesBeforePlan,
            'timeout alone cannot report recovery',
          );
        }
        finalLink?.open();
        await turn();
        for (const channel of finalLink?.channels ?? []) channel.open();
        rebuilt?.open();
        await turn();
        assert(
          events.filter((e) => e['event'] === 'p2p_pair_reconnected').length ===
            recoveriesBeforePlan + 1,
          `initiate=${initiate}, outcome=${replacementOutcome}: authoritative replacement reports exactly one recovery`,
        );
        assert(
          events.filter((e) => e['event'] === 'p2p_pair_connected').length === 1,
          'replacement retains one logical pair event',
        );
        assert(
          events.filter((e) => e['event'] === 'channel_message_sent').length === 2,
          'replacement retains the logical exchange',
        );
      } else if (replacementOutcome === 'timeout') {
        now += 1000;
        socket.deliver({ type: 'Pong' });
      } else {
        socket.deliver({
          type: 'PlayerLeft',
          data: { player_id: peer, epoch: 1, final_seq: 0 },
        });
      }
      await turn();
      if (replacementOutcome === 'removed') {
        finalLink?.open();
        await turn();
        assert(
          events.filter((e) => e['event'] === 'p2p_pair_reconnected').length ===
            recoveriesBeforePlan,
          'departure and retired callbacks cannot report recovery',
        );
      }
      released = true;
      now += 100;
      socket.deliver({ type: 'Pong' });
      await turn();
      now += 300;
      socket.deliver({ type: 'Pong' });
      assert((await running) === 0, `initiate=${initiate}: retry preserves success criteria`);
      assert(
        sent.filter((f) => f['type'] === 'Signal').length > 0,
        'replacement signaling uses the socket',
      );
    };
    await Promise.race([
      history(),
      new Promise<never>((_resolve, reject) => {
        guard = setTimeout(() => reject(new Error('retry history exceeded its bound')), 5000);
      }),
    ]);
  } finally {
    clearTimeout(guard);
    socket?.onclose?.();
    Date.now = originalNow;
    performance.now = originalPerformanceNow;
    globalThis.WebSocket = originalSocket;
    globalThis.RTCPeerConnection = originalPc;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error(
  'ok - browser retries replace physical links and preserve logical exchange in both roles',
);
