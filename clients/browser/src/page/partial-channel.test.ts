import { run } from './orchestrator.js';
import { EXIT_CRITERIA_UNMET, EXIT_SUCCESS, type RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regressions #1046/#1051, audit #853/#889: an incoming probe can precede the second required channel.
const histories = [
  'live',
  'wrong-probe',
  'closed',
  'error',
  'queued-error-first',
  'queued-error-second',
  'queued-close-first',
  'queued-close-second',
  'queued-closing-first',
  'queued-closing-second',
  'queued-pc-failed',
  'queued-pc-closed',
  'queued-pc-disconnected',
].flatMap((scenario) => {
  const recoveries = ['live', 'wrong-probe', 'queued-pc-disconnected'].includes(scenario)
    ? [false]
    : [false, true];
  return recoveries.map((recover) => ({ scenario, recover }));
});
for (const firstLabel of ['reliable', 'unreliable']) {
  for (const { scenario, recover } of histories) {
    const description = `${firstLabel}/${scenario}/recover=${recover}`;
    const originalSocket = globalThis.WebSocket;
    const originalPc = globalThis.RTCPeerConnection;
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
    const originalNow = performance.now;
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const me = '00000000-0000-0000-0000-000000000001';
    const peer = '00000000-0000-0000-0000-000000000002';
    const events: Record<string, unknown>[] = [];
    const probes: string[] = [];
    let now = 100;
    let socket!: Socket;
    let connection!: Connection;
    let release!: () => void;
    let held = false;
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
      onmessage: ((event: { data: string }) => void) | null = null;
      onclose: (() => void) | null = null;
      onclosing: (() => void) | null = null;
      onerror: (() => void) | null = null;
      constructor(readonly label: string) {
        this.ordered = label === 'reliable';
        this.maxRetransmits = this.ordered ? null : 0;
      }
      send(text: string): void {
        assert(this.readyState === 'open', `${description}: send on an open channel`);
        probes.push(text);
      }
      receive(): void {
        this.onmessage?.({
          data: JSON.stringify({
            from: scenario === 'wrong-probe' ? me : peer,
            channel: this.label,
            seq: 0,
          }),
        });
      }
      close(): void {
        this.readyState = 'closed';
      }
    }
    class Connection {
      connectionState = 'new';
      onconnectionstatechange: (() => void) | null = null;
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
      deliver(type: string, data: Record<string, unknown> = {}): void {
        this.onmessage?.({ data: JSON.stringify({ type, data }) });
      }
      send(line: string): void {
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
            events.push(event);
            if (event['event'] === 'channel_message' && !held) {
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
        gameName: 'partial-channel',
        playerName: 'test',
        appId: 'test',
        platform: 'test',
        exchange: true,
        relayPayload: null,
        crippleIce: false,
        p2pTimeoutSecs: 1,
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
      await turn();
      const first = new Channel(firstLabel);
      const second = new Channel(firstLabel === 'reliable' ? 'unreliable' : 'reliable');
      connection.ondatachannel?.({ channel: first });
      await turn();
      first.receive();
      await turn();
      assert(held, `${description}: hold output after the early receipt`);
      assert(probes.length === 0, `${description}: one channel cannot start exchange`);
      if (scenario === 'closed') {
        first.close();
        first.onclose?.();
      }
      if (scenario === 'error') first.onerror?.();
      connection.ondatachannel?.({ channel: second });
      await turn();
      // Both open notifications are queued behind receipt output. An error
      // need not change readyState, so queue order alone cannot fence formation.
      if (scenario.startsWith('queued-pc-')) {
        connection.connectionState = scenario.slice('queued-pc-'.length);
        connection.onconnectionstatechange?.();
      } else if (scenario.startsWith('queued-')) {
        const failed = scenario.endsWith('first') ? first : second;
        if (scenario.startsWith('queued-closing')) {
          failed.readyState = 'closing';
          failed.onclosing?.();
        } else if (scenario.startsWith('queued-close')) {
          failed.close();
          failed.onclose?.();
        } else failed.onerror?.();
      }
      second.receive();
      release();
      await turn();
      const terminal =
        scenario === 'closed' ||
        scenario === 'error' ||
        (scenario.startsWith('queued-') && scenario !== 'queued-pc-disconnected');
      assert(
        probes.length === (terminal ? 0 : 2),
        `${description}: terminal partial pair cannot send probes`,
      );
      assert(
        events.filter((event) => event['event'] === 'p2p_pair_connected').length ===
          (terminal ? 0 : 1),
        `${description}: formation requires both live channels`,
      );
      assert(
        events.filter((event) => event['event'] === 'channel_message').length ===
          (terminal ? 1 : 2),
        `${description}: retired sibling messages cannot reach output`,
      );
      if (!terminal) {
        assert(
          JSON.stringify(probes.map((text) => JSON.parse(text))) ===
            JSON.stringify(
              ['reliable', 'unreliable'].map((channel) => ({ from: me, channel, seq: 0 })),
            ),
          `${description}: send one documented probe on each required channel`,
        );
      }
      if (recover) {
        socket.deliver('SessionPlan', {
          generation: peer,
          topology: 'mesh',
          transport: 'webrtc',
          fallback: 'relay',
          ice_servers: [],
          peers: [{ player_id: peer, initiate: false }],
        });
        await turn();
        // Old terminal callbacks must not retire the fresh generation.
        first.onclosing?.();
        second.onclosing?.();
        first.onerror?.();
        second.onclose?.();
        const fresh = [new Channel('reliable'), new Channel('unreliable')];
        for (const channel of fresh) connection.ondatachannel?.({ channel });
        await turn();
        for (const channel of fresh) channel.receive();
        await turn();
        assert(probes.length === 2, `${description}: fresh link sends both probes`);
        assert(
          events.filter((event) => event['event'] === 'p2p_pair_connected').length === 1 &&
            events.filter((event) => event['event'] === 'channel_message').length === 3,
          `${description}: only the fresh pair creates exchange debt and repays it`,
        );
        assert(
          events.some(
            (event) =>
              event['event'] === 'transport_status_sent' && event['connected'] === true,
          ),
          `${description}: fresh link restores connected transport`,
        );
      }
      now = 1100;
      socket.deliver('Pong');
      const code = await running;
      assert(
        code === (scenario === 'wrong-probe' ? EXIT_CRITERIA_UNMET : EXIT_SUCCESS),
        `${description}: early matching receipts pay debt; wrong-player receipts do not; exit=${code}`,
      );
      assert(
        events.some(
          (event) =>
            event['event'] === 'transport_status_sent' && event['connected'] === !terminal,
        ),
        `${description}: report the resulting transport state`,
      );
    } finally {
      release();
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
}
console.error(
  'ok - early probes and partial channel loss preserve exchange and transport criteria',
);
