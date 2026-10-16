import { run } from './orchestrator.js';
import { EXIT_CRITERIA_UNMET, type RunConfig } from '../shared/types.js';
function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}
// The room wake can reach the cutoff before an equally due peer-operation timer.
for (const [scenario, timerOrder] of (
  ['fallback', 'paid', 'missing-reliable', 'missing-unreliable'] as const
).flatMap((scenario) =>
  (['room-first', 'peer-first'] as const).map((timerOrder) => [scenario, timerOrder] as const),
)) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const events: Record<string, unknown>[] = [];
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
    send() {}
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
      const { type } = JSON.parse(line) as { type: string };
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
      p2pTimeoutSecs: 0,
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
        if (scenario === 'paid' || scenario !== `missing-${label}`)
          channel.onmessage?.({ data: JSON.stringify({ from: peer, channel: label, seq: 0 }) });
      }
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
    assert(wake !== undefined && wake.at === now, `${scenario}: room wake targets cutoff`);
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
      result === (scenario.startsWith('missing-') ? EXIT_CRITERIA_UNMET : 0),
      `${scenario}: cutoff uses logical debt, observed exit ${result}`,
    );
    assert(connection.closed, `${scenario}: cutoff retires the pending link`);
    assert(
      !scenario.startsWith('missing-') ||
        events.some((event) => String(event['message']).includes('exchange incomplete')),
      'unpaid receipts remain required',
    );
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
