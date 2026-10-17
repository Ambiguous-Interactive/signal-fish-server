import { ExchangeLedger, run } from './orchestrator.js';
import { EXIT_CRITERIA_UNMET, type RunConfig } from '../shared/types.js';
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}
// The room wake can reach the cutoff before an equally due peer-operation timer.
for (const [scenario, timerOrder, p2pTimeoutSecs] of (
  [
    'fallback',
    'paid',
    'missing-reliable',
    'missing-unreliable',
    'live-sent-reliable',
    'live-sent-unreliable',
    'live-received-reliable',
    'live-received-unreliable',
    'answered',
  ] as const
).flatMap((scenario) =>
  (scenario === 'answered' || scenario.startsWith('live-')
    ? (['room-first'] as const)
    : (['room-first', 'peer-first'] as const)
  ).flatMap((timerOrder) =>
    [0, 15].map((p2pTimeoutSecs) => [scenario, timerOrder, p2pTimeoutSecs] as const),
  ),
)) {
  const originalError = console.error;
  const diagnostics: string[] = [];
  const live = scenario.startsWith('live-');
  const unpaid = scenario.startsWith('missing-') || live;
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const events: Record<string, unknown>[] = [];
  const transportStatuses: boolean[] = [];
  let cutoffOnAnswer = false;
  const channels: Channel[] = [];
  const timers = new Map<number, { at: number; callback: () => void }>();
  let timerId = 0;
  let now = 100;
  let socket!: Socket;
  let connection!: Connection;
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
  let running: Promise<number> | undefined;
  let result: number | undefined;
  class Channel {
    readyState = 'connecting';
    bufferedAmount = 0;
    maxPacketLifeTime = null;
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(readonly label: string) {
      channels.push(this);
      this.ordered = label === 'reliable';
      this.maxRetransmits = this.ordered ? null : 0;
    }
    send() {
      if (scenario === `live-sent-${this.label}`) throw new Error('not ready');
    }
    close() {
      this.readyState = 'closed';
    }
  }
  class Connection {
    closed = false;
    ondatachannel: ((event: { channel: Channel }) => void) | null = null;
    constructor() {
      connection = this;
    }
    createDataChannel(label: string) {
      return new Channel(label);
    }
    async createOffer() {
      await held;
      return { type: 'offer', sdp: 'offer' };
    }
    async setRemoteDescription() {}
    async createAnswer() {
      return { type: 'answer', sdp: 'answer' };
    }
    async setLocalDescription() {}
    async addIceCandidate() {
      await held;
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
    send(line: string) {
      const frame = JSON.parse(line) as {
        type: string;
        data: { connected?: boolean; signal?: { Answer?: string } };
      };
      const { type } = frame;
      if (type === 'TransportStatus') transportStatuses.push(frame.data.connected === true);
      // Observe the Answer after awaitPeerOperation has returned and cancelled
      // its timer, while the enclosing peer-task finalizer is still pending.
      if (type === 'Signal' && frame.data.signal?.Answer && cutoffOnAnswer) {
        now = 1100;
        [...timers.values()].at(-1)?.callback();
      }
      if (type === 'Authenticate')
        queueMicrotask(() => {
          this.deliver('Authenticated');
          this.deliver('ProtocolInfo', { protocol_version: 3 });
        });
      if (type === 'JoinRoom')
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
            peers: [{ player_id: peer, initiate: scenario === 'fallback' }],
          });
          this.deliver('PeerTransportStatus', {
            peer_id: peer,
            transport: 'webrtc',
            connected: true,
          });
        });
    }
    close() {
      this.readyState = 3;
    }
  }
  try {
    console.error = (...args: unknown[]) => diagnostics.push(args.map(String).join(' '));
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
      gameName: 'run-cutoff',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: true,
      relayPayload: null,
      crippleIce: false,
      p2pTimeoutSecs,
      p2pRetryCount: 0,
      runForSecs: 1,
      successReleaseEnabled: false,
      protocolVersion: 3,
      supportedTopologies: ['mesh'],
      supportedTransports: ['webrtc', 'relay'],
      gameDataFormat: 'json',
      sdkVersion: 'test',
      elapsedBeforeStartMs: 0,
    };
    running = run(config);
    void running.then((code) => {
      result = code;
    });
    await turn();
    if (scenario !== 'fallback') {
      socket.deliver('Signal', { from: peer, generation: me, signal: { Offer: 'offer' } });
      await turn();
      for (const label of ['reliable', 'unreliable']) {
        const channel = new Channel(label);
        connection.ondatachannel?.({ channel });
        channel.readyState = 'open';
        channel.onopen?.();
        if (scenario !== `missing-${label}` && scenario !== `live-received-${label}`)
          channel.onmessage?.({ data: JSON.stringify({ from: peer, channel: label, seq: 0 }) });
      }
      if (scenario === 'answered') {
        await turn();
        cutoffOnAnswer = true;
        socket.deliver('Signal', {
          from: peer,
          generation: me,
          signal: { Offer: 'renegotiate' },
        });
        await turn();
        assert(result === 0, 'a completed Answer permits success at cutoff');
        assert(
          JSON.stringify(transportStatuses) ===
            JSON.stringify(p2pTimeoutSecs === 0 ? [false, true] : [true]),
          'completed peer work must not publish transport loss at cutoff',
        );
        assert(
          events.filter((event) => event['event'] === 'fallback_engaged').length ===
            (p2pTimeoutSecs === 0 ? 1 : 0),
          'completed peer work must not announce fallback at cutoff',
        );
        continue;
      }
      if (!live)
        socket.deliver('Signal', {
          from: peer,
          generation: me,
          signal: { IceCandidate: 'held' },
        });
      await turn();
      assert(
        events.some((event) => event['event'] === 'p2p_pair_connected'),
        `${scenario}: formed pair is observed`,
      );
      // Drain any earlier linger wake while the candidate remains suspended.
      now = 500;
      for (const [id, timer] of [...timers])
        if (timer.at <= now) {
          timers.delete(id);
          timer.callback();
        }
      await turn();
    }
    assert(result === undefined, `${scenario}: pending work keeps the run live before cutoff`);
    now = 1100;
    // Deliver the latest room wake first. Leave the due peer timer uncalled.
    const wake = [...timers.values()].at(-1);
    // A blocked send keeps its 50 ms retry wake. Deliver it late at the run
    // cutoff, as a delayed browser task would, without extending the run.
    const expectedWake = scenario.startsWith('live-sent-') ? 550 : now;
    assert(
      wake !== undefined && wake.at === expectedWake,
      `${scenario}: room wake targets the next retry or cutoff; observed ${wake?.at}`,
    );
    if (timerOrder === 'peer-first') {
      const peerTimer = [...timers.values()].find(
        (timer) => timer !== wake && timer.at === now,
      );
      assert(peerTimer !== undefined, `${scenario}: peer timer also targets cutoff`);
      peerTimer?.callback();
      await turn();
    }
    wake?.callback();
    await turn();
    assert(
      result === (unpaid ? EXIT_CRITERIA_UNMET : 0),
      `${scenario}: cutoff uses logical debt, observed exit ${result}`,
    );
    assert(connection.closed, `${scenario}: cutoff retires the pending link`);
    const expectedStatuses = live
      ? p2pTimeoutSecs === 0
        ? [false, true]
        : [true]
      : scenario === 'fallback'
        ? [false]
        : p2pTimeoutSecs === 0
          ? [false, true, false]
          : [true, false];
    assert(
      JSON.stringify(transportStatuses) === JSON.stringify(expectedStatuses),
      `${scenario}: active expired work keeps the existing transport-loss report`,
    );
    assert(
      events.filter((event) => event['event'] === 'fallback_engaged').length ===
        expectedStatuses.filter((connected) => !connected).length,
      'active expired work preserves fallback event ordering',
    );
    assert(
      !unpaid ||
        events.some((event) => String(event['message']).includes('exchange incomplete')),
      'unpaid receipts remain required',
    );
    const snapshots = diagnostics.filter((line) =>
      line.startsWith(`Exchange timeout for ${peer}: `),
    );
    assert(
      snapshots.length === (unpaid ? 1 : 0),
      `${scenario}: snapshot only unpaid peers once`,
    );
    if (unpaid) {
      const snapshot = JSON.parse(snapshots[0]!.split(': ').slice(1).join(': '));
      assert(snapshot.connected === live, 'snapshot identifies current pair membership');
      const missingLabel = scenario.endsWith('unreliable') ? 'unreliable' : 'reliable';
      const missingSent = scenario.startsWith('live-sent-') ? [missingLabel] : [];
      const missingReceived = scenario.startsWith('live-sent-') ? [] : [missingLabel];
      assert(
        JSON.stringify(snapshot.missing_sent) === JSON.stringify(missingSent),
        'name missing sends',
      );
      assert(
        JSON.stringify(snapshot.missing_received) === JSON.stringify(missingReceived),
        'name missing receipts',
      );
      if (live) {
        for (const label of ['reliable', 'unreliable']) {
          assert(
            snapshot.link.channels[label].readyState === 'open',
            `${scenario}: capture live ${label} before teardown`,
          );
          assert(
            snapshot.link.channels[label].openObserved,
            `${scenario}: retain observed ${label} open`,
          );
        }
      } else
        assert(snapshot.link === null, 'retired unpaid peer is retained with no physical link');
    }
    const eventCount = events.length;
    release();
    for (const channel of channels)
      channel.onmessage?.({
        data: JSON.stringify({ from: peer, channel: channel.label, seq: 0 }),
      });
    await turn();
    assert(events.length === eventCount, `${scenario}: late completion cannot change cutoff`);
  } finally {
    release();
    socket?.onclose?.();
    await running;
    await turn();
    console.error = originalError;
    performance.now = originalNow;
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - run cutoff retires peer work and preserves logical exchange debt');

// Ignore completed obligations and traffic from a peer that never connected.
{
  const ledger = new ExchangeLedger();
  for (const peer of ['paid', 'unsent', 'unreceived']) ledger.noteConnected(peer);
  for (const label of ['reliable', 'unreliable']) {
    ledger.noteSent('paid', label);
    ledger.noteReceived('paid', label);
    ledger.noteReceived('unsent', label);
    ledger.noteSent('unreceived', label);
    ledger.noteSent('unplanned', label);
  }
  assert(
    ledger.unpaidPeers().join(',') === 'unsent,unreceived',
    'only unpaid obligations need snapshots',
  );
  ledger.resetIncarnation('paid');
  assert(
    ledger.unpaidPeers().join(',') === 'paid,unsent,unreceived',
    'a restored incarnation owes fresh exchange',
  );
}
