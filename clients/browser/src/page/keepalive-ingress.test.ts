import { run } from './orchestrator.js';
import { EXIT_CONNECTION_ERROR, EXIT_PROTOCOL_ERROR, type RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #1062: browser ingress can precede slow stdout processing.
for (const scenario of [
  'timely',
  'traffic',
  'stale',
  'missing',
  'malformed',
  'closed',
] as const) {
  const originalSocket = globalThis.WebSocket;
  const originalPc = globalThis.RTCPeerConnection;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const releases: Array<() => void> = [];
  let now = 100;
  let socket!: Socket;
  let connection!: Connection;
  let result: number | undefined;
  let running: Promise<number> | undefined;
  const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
  class Channel {
    readyState = 'open';
    bufferedAmount = 0;
    maxPacketLifeTime = null;
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(readonly label: string) {
      this.ordered = label === 'reliable';
      this.maxRetransmits = this.ordered ? null : 0;
    }
    send(): void {}
    close(): void {
      this.readyState = 'closed';
    }
    receive(): void {
      this.onmessage?.({ data: 'gameplay' });
    }
  }
  class Connection {
    ondatachannel: ((event: { channel: Channel }) => void) | null = null;
    constructor() {
      connection = this;
    }
    close(): void {}
  }
  class Socket {
    static OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    pings = 0;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(type: string, data: Record<string, unknown> = {}): void {
      this.onmessage?.({ data: JSON.stringify({ type, data }) });
    }
    send(line: string): void {
      const frame = JSON.parse(line) as { type: string };
      if (frame.type === 'Ping') this.pings += 1;
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
            peers: [{ player_id: peer, initiate: false }],
          });
        });
    }
    close(): void {
      this.readyState = 3;
    }
  }
  try {
    performance.now = () => now;
    globalThis.setTimeout = (() => 1) as unknown as typeof setTimeout;
    globalThis.clearTimeout = (() => {}) as typeof clearTimeout;
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          const event = JSON.parse(line) as Record<string, unknown>;
          if (event['event'] === 'channel_message' && releases.length < 2)
            return new Promise<void>((resolve) => releases.push(resolve));
        },
      },
    });
    const config: RunConfig = {
      serverUrl: 'ws://mock.invalid/v3/ws',
      createRoom: true,
      joinCode: null,
      peers: 3,
      maxPlayers: null,
      expectTotalPeers: null,
      leaveOnGameStart: false,
      gameName: 'keepalive-ingress',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: false,
      relayPayload: null,
      crippleIce: false,
      p2pTimeoutSecs: 1,
      p2pRetryCount: 0,
      runForSecs: 60,
      successReleaseEnabled: false,
      protocolVersion: 3,
      supportedTopologies: ['mesh'],
      supportedTransports: ['webrtc', 'relay'],
      gameDataFormat: 'json',
      sdkVersion: 'test',
      elapsedBeforeStartMs: 0,
    };
    running = run(config).then((code) => {
      result = code;
      return code;
    });
    await turn();
    const channel = new Channel('reliable');
    connection.ondatachannel?.({ channel });
    connection.ondatachannel?.({ channel: new Channel('unreliable') });
    await turn();
    now = 10100;
    socket.deliver('Pong');
    await turn();
    assert(socket.pings === 1, `${scenario}: first Ping is outstanding`);
    now = 11000;
    channel.receive();
    await turn();
    assert(releases.length === 1, `${scenario}: hold first gameplay output`);
    channel.receive();
    now = 12000;
    if (scenario === 'malformed') socket.onmessage?.({ data: '{' });
    else if (scenario === 'closed') socket.onclose?.();
    else if (scenario === 'traffic')
      socket.deliver('PeerTransportStatus', {
        peer_id: peer,
        transport: 'webrtc',
        connected: true,
      });
    else if (scenario !== 'missing') socket.deliver('Pong');
    if (scenario === 'malformed' || scenario === 'closed') {
      await turn();
      assert(
        result === (scenario === 'malformed' ? EXIT_PROTOCOL_ERROR : EXIT_CONNECTION_ERROR),
        `${scenario}: terminal ingress fails while gameplay output is held`,
      );
      continue;
    }
    now = 20100;
    releases[0]?.();
    await turn();
    assert(releases.length === 2, `${scenario}: second gameplay output precedes queued Pong`);
    now = 21101;
    releases[1]?.();
    await turn();
    if (scenario === 'missing') {
      assert(result === EXIT_CONNECTION_ERROR, 'an unanswered Ping still fails after grace');
    } else {
      assert(
        result === undefined,
        `${scenario}: timely Pong must survive slow gameplay output`,
      );
      assert(
        socket.pings === 2,
        `${scenario}: timely ingress permits the next Ping on cadence`,
      );
      if (scenario === 'stale') {
        now = 30100;
        channel.receive();
        await turn();
        assert(result === undefined, 'the newer unanswered Ping receives one drain grace');
        now = 31100;
        channel.receive();
        await turn();
        assert(
          result === EXIT_CONNECTION_ERROR,
          'dispatch of the old queued Pong cannot pay the newer Ping',
        );
      }
    }
  } finally {
    for (const release of releases) release();
    socket?.onclose?.();
    await running;
    performance.now = originalNow;
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
    globalThis.WebSocket = originalSocket;
    globalThis.RTCPeerConnection = originalPc;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - keepalive uses server ingress across queued gameplay output');
