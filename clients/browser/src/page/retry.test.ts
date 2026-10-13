import { run } from './orchestrator.js';
import type { RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #921: drive real dispatch and engine callbacks in both server-assigned roles.
for (const [initiate, closeBeforeRetry] of [
  [false, false],
  [true, false],
  [false, true],
  [true, true],
] as const) {
  const originalSocket = globalThis.WebSocket;
  const originalPc = globalThis.RTCPeerConnection;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = Date.now;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const outsider = '00000000-0000-0000-0000-000000000003';
  let generation = me;
  let now = originalNow();
  let released = false;
  let settled = false;
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
    expectTotalPeers: null,
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
    successReleaseEnabled: true,
    protocolVersion: 3,
    supportedTopologies: ['mesh'],
    supportedTransports: ['webrtc'],
    gameDataFormat: 'json',
    sdkVersion: 'test',
    elapsedBeforeStartMs: 0,
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
    Date.now = () => now;
    const running = run(config).then((code) => {
      settled = true;
      return code;
    });
    const history = async () => {
      await turn();
      const first = connections[0];
      assert(first !== undefined, 'install the initial planned link');
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
      if (!initiate) signal({ Offer: 'replacement-offer' });
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
      if (!initiate) signal({ Offer: 'rebuilt-offer' });
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
      signal({ PairRetry: 1 });
      await turn();
      assert(
        connections.length === 5,
        'a new authoritative generation starts a fresh retry budget',
      );
      if (!initiate) signal({ Offer: 'final-offer' });
      await turn();
      connections[4]?.open();
      await turn();
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
    globalThis.WebSocket = originalSocket;
    globalThis.RTCPeerConnection = originalPc;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error(
  'ok - browser retries replace physical links and preserve logical exchange in both roles',
);
