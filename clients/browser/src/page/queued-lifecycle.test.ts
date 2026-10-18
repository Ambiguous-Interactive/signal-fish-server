import { run } from './orchestrator.js';
import { EXIT_CRITERIA_UNMET, type RunConfig } from '../shared/types.js';

const failures: string[] = [];
// Regression #1041: arrived lifecycle work must invalidate stale success before exit.
for (const [scenario, boundary] of (
  ['joined', 'reconnected', 'plan', 'pair-open', 'benign', 'late'] as const
).flatMap((scenario) =>
  (['linger', 'run'] as const).map((boundary) => [scenario, boundary] as const),
)) {
  const label = `${scenario}/${boundary}`;
  const originalSocket = globalThis.WebSocket;
  const originalPc = globalThis.RTCPeerConnection;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const newcomer = '00000000-0000-0000-0000-000000000003';
  const events: Record<string, unknown>[] = [];
  let now = 100;
  let socket!: Socket;
  let connection!: Connection;
  let release!: () => void;
  let held = false;
  let result: number | undefined;
  let running: Promise<number> | undefined;
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
  class Channel {
    readyState = 'open';
    bufferedAmount = 0;
    maxPacketLifeTime = null;
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    constructor(readonly label: string) {
      this.ordered = label === 'reliable';
      this.maxRetransmits = this.ordered ? null : 0;
    }
    send(): void {}
    close(): void {
      this.readyState = 'closed';
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
    plan(transport: string, peers: string[], generation = me) {
      this.deliver('SessionPlan', {
        generation,
        topology: 'mesh',
        transport,
        fallback: 'relay',
        ice_servers: [],
        peers: peers.map((player_id) => ({ player_id, initiate: false })),
      });
    }
    send(line: string) {
      const frame = JSON.parse(line) as { type: string };
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
          this.plan(
            scenario === 'pair-open' ? 'webrtc' : 'relay',
            scenario === 'pair-open' ? [peer] : [],
          );
        });
    }
    close() {
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
          events.push(event);
          if (event['event'] === 'peer_transport_status') {
            held = true;
            return blocked;
          }
        },
      },
    });
    const config: RunConfig = {
      serverUrl: 'ws://mock.invalid/v3/ws',
      createRoom: true,
      joinCode: null,
      peers: 2,
      maxPlayers: null,
      expectTotalPeers: null,
      leaveOnGameStart: false,
      gameName: 'queued-lifecycle',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: scenario === 'pair-open',
      relayPayload: null,
      crippleIce: false,
      p2pTimeoutSecs: scenario === 'pair-open' ? 1 : 10,
      p2pRetryCount: 0,
      runForSecs: scenario === 'pair-open' ? 3 : 1,
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
    if (scenario === 'pair-open') {
      now = 1100;
      socket.deliver('Pong');
      await turn();
    }
    now = scenario === 'pair-open' ? 1200 : 200;
    socket.deliver('PeerTransportStatus', {
      peer_id: peer,
      transport: 'relay',
      connected: true,
    });
    await turn();
    if (!held) throw new Error(`${scenario}: hold output before the lifecycle frame`);
    now = scenario === 'late' ? 1100 : scenario === 'pair-open' ? 1300 : 300;
    if (scenario === 'joined')
      socket.deliver('PlayerJoined', { player: { id: newcomer, epoch: 1, seq: 0 } });
    else if (scenario === 'reconnected')
      socket.deliver('PlayerReconnected', { player_id: peer, epoch: 2 });
    else if (scenario === 'plan') socket.plan('webrtc', [peer], newcomer);
    else if (scenario === 'pair-open') {
      for (const name of ['reliable', 'unreliable'])
        connection.ondatachannel?.({ channel: new Channel(name) });
      await turn();
    } else
      // The held frame retains one of the 256 backlog credits.
      for (let seq = 1; seq <= 255; seq += 1)
        socket.deliver('GameData', {
          from_player: peer,
          epoch: 1,
          seq,
          data: { benign: true },
        });
    // Output resumes after the old linger expires, with admitted input still queued.
    now =
      scenario === 'pair-open'
        ? boundary === 'run'
          ? 3200
          : 1400
        : boundary === 'run' || scenario === 'late'
          ? 1200
          : 400;
    release();
    await turn();
    const addsDebt =
      scenario === 'joined' ||
      scenario === 'reconnected' ||
      scenario === 'plan' ||
      scenario === 'pair-open';
    const expected = addsDebt
      ? boundary === 'run'
        ? scenario === 'plan'
          ? 0
          : EXIT_CRITERIA_UNMET
        : undefined
      : 0;
    if (result !== expected)
      failures.push(`${label}: expected exit ${String(expected)}, got ${String(result)}`);
    const observed = events.some(
      (event) =>
        (scenario === 'plan' &&
          event['event'] === 'session_plan' &&
          event['transport'] === 'webrtc') ||
        ((scenario === 'joined' || scenario === 'reconnected') &&
          event['event'] === 'peer_joined' &&
          event['player_id'] === (scenario === 'joined' ? newcomer : peer)) ||
        (scenario === 'pair-open' && event['event'] === 'p2p_pair_connected') ||
        ((scenario === 'benign' || scenario === 'late') &&
          event['event'] === 'game_data_received'),
    );
    if (scenario === 'benign' || scenario === 'late') {
      const received = events.filter((event) => event['event'] === 'game_data_received').length;
      if (received !== (scenario === 'benign' ? 255 : 0))
        failures.push(`${label}: wrong bounded backlog drain count ${received}`);
    }
    if (observed !== (scenario !== 'late'))
      failures.push(`${label}: wrong queued frame dispatch: ${observed}`);
  } finally {
    release();
    socket?.onclose?.();
    let guard: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        running,
        new Promise<never>((_resolve, reject) => {
          guard = originalSetTimeout(
            () => reject(new Error(`${label}: cleanup did not stop the run`)),
            2000,
          );
        }),
      ]);
    } finally {
      originalClearTimeout(guard);
    }
    performance.now = originalNow;
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
    globalThis.WebSocket = originalSocket;
    globalThis.RTCPeerConnection = originalPc;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
if (failures.length > 0) throw new Error(failures.join('\n'));
console.error('ok - arrived lifecycle frames prevent stale success during output stalls');
