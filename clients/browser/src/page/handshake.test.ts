import { run } from './orchestrator.js';
import { HANDSHAKE_TIMEOUT_MS } from './wire.js';
import type { RunConfig } from '../shared/types.js';

const config: RunConfig = {
  serverUrl: 'ws://mock.invalid/v3/ws',
  createRoom: true,
  joinCode: null,
  peers: 2,
  maxPlayers: null,
  expectTotalPeers: null,
  leaveOnGameStart: false,
  gameName: 'handshake',
  playerName: 'test',
  appId: 'test',
  platform: 'test',
  exchange: false,
  relayPayload: null,
  crippleIce: false,
  p2pTimeoutSecs: 10,
  p2pRetryCount: 0,
  runForSecs: 120,
  successReleaseEnabled: false,
  protocolVersion: 3,
  supportedTopologies: ['relay'],
  supportedTransports: ['relay'],
  gameDataFormat: 'json',
  sdkVersion: 'test',
  elapsedBeforeStartMs: 0,
};
const failures: string[] = [];
for (const version of [2, 3]) {
  for (const phase of [
    'authenticate',
    'join',
    'authenticate-output',
    'healthy',
    'late-buffered',
    'phase-window',
  ] as const) {
    const originalSocket = globalThis.WebSocket;
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
    const originalNow = performance.now;
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const originalError = console.error;
    let now = 0;
    let nextTimer = 0;
    const timers = new Map<number, { at: number; callback: () => void }>();
    const events: Record<string, unknown>[] = [];
    const sent: string[] = [];
    const me = '00000000-0000-0000-0000-000000000001';
    let socket!: Socket;
    let result: number | undefined;
    let running: Promise<number> | undefined;
    let releaseOutput!: () => void;
    const heldOutput = new Promise<void>((resolve) => {
      releaseOutput = resolve;
    });
    const check = (condition: boolean, message: string) => {
      if (!condition) failures.push(`v${version}/${phase}: ${message}`);
    };
    const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
    const advance = async (elapsed: number) => {
      now += elapsed;
      for (let pass = 0; pass < 10; pass += 1) {
        for (const [id, timer] of timers) {
          if (timer.at <= now) {
            timers.delete(id);
            timer.callback();
          }
        }
        await turn();
      }
    };
    class Socket {
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
        const { type } = JSON.parse(line) as { type: string };
        sent.push(type);
        if (
          type === 'Authenticate' &&
          !phase.startsWith('authenticate') &&
          phase !== 'phase-window'
        )
          queueMicrotask(() => {
            this.deliver('Authenticated');
            this.deliver('ProtocolInfo', { protocol_version: version });
          });
      }
      close() {
        this.readyState = 3;
      }
    }
    try {
      globalThis.WebSocket = Socket as unknown as typeof WebSocket;
      performance.now = () => now;
      globalThis.setTimeout = ((callback: () => void, delay = 0) => {
        const id = ++nextTimer;
        timers.set(id, { at: now + delay, callback });
        return id;
      }) as unknown as typeof setTimeout;
      globalThis.clearTimeout = ((id: number) => {
        timers.delete(id);
      }) as unknown as typeof clearTimeout;
      Object.defineProperty(globalThis, 'window', {
        configurable: true,
        value: {
          __sf_emit: (line: string) => {
            const event = JSON.parse(line) as Record<string, unknown>;
            events.push(event);
            if (phase === 'authenticate-output' && event['event'] === 'authenticated')
              return heldOutput;
          },
        },
      });
      console.error = () => {};
      running = run({ ...config, protocolVersion: version }).then((code) => {
        result = code;
        return code;
      });
      await turn();
      check(
        sent.includes(
          phase.startsWith('authenticate') || phase === 'phase-window'
            ? 'Authenticate'
            : 'JoinRoom',
        ),
        'start the real command',
      );
      await advance(HANDSHAKE_TIMEOUT_MS - 1);
      check(result === undefined, 'remain pending before the command deadline');
      if (phase.startsWith('authenticate') || phase === 'phase-window')
        socket.deliver('Authenticated');
      else if (phase !== 'authenticate-output' && version === 2)
        socket.deliver('PlayerJoined', {
          player: { id: '00000000-0000-0000-0000-000000000002', name: 'peer' },
        });
      else if (phase !== 'authenticate-output')
        socket.deliver('RelayStats', {
          interval_ms: 1_000,
          sent_to_you: 0,
          dropped_for_you: 0,
          backpressure_events: 0,
        });
      await turn();
      const deliverBaseline = () => {
        socket.deliver('RoomJoined', {
          player_id: me,
          room_id: me,
          room_code: 'TEST',
          lobby_state: 'lobby',
          current_players: [{ id: me, ...(version >= 3 ? { epoch: 1, seq: 0 } : {}) }],
          ready_players: [],
        });
      };
      if (phase === 'phase-window') {
        socket.deliver('ProtocolInfo', { protocol_version: version });
        await turn();
        check(
          sent.includes('JoinRoom'),
          'start a separate room command after late authentication',
        );
        await advance(HANDSHAKE_TIMEOUT_MS - 1);
        check(result === undefined, 'give the room command its own full window');
        deliverBaseline();
        await turn();
        check(
          events.some((event) => event['event'] === 'room_joined'),
          'complete both commands within their own windows',
        );
      } else if (phase === 'healthy') {
        deliverBaseline();
        await turn();
        check(
          events.some((event) => event['event'] === 'room_joined'),
          `accept an on-time baseline after a prelude: ${JSON.stringify(events)}`,
        );
        check(result === undefined, 'enter streaming with a fresh run wait');
      } else {
        if (phase === 'late-buffered') {
          // A ready reply can run before the due host timer's callback.
          now += 1;
          deliverBaseline();
          await turn();
          await advance(0);
        } else await advance(1);
        check(result === 3, 'expire at the original command deadline');
        check(socket.readyState === 3, 'close the socket after expiry');
        check(
          !events.some((event) => event['event'] === 'room_joined'),
          'publish no joined room after expiry',
        );
        releaseOutput();
        socket.deliver(phase.startsWith('authenticate') ? 'ProtocolInfo' : 'RoomJoined');
        await turn();
        check(!sent.includes('PlayerReady'), 'late replies cannot send readiness');
        check(timers.size === 0, 'clear every handshake timer');
      }
    } finally {
      releaseOutput();
      socket?.onclose?.();
      await running;
      performance.now = originalNow;
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
      globalThis.WebSocket = originalSocket;
      console.error = originalError;
      if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
      else Object.defineProperty(globalThis, 'window', originalWindow);
    }
  }
}
if (failures.length !== 0) throw new Error(failures.join('\n'));
console.error('ok - command handshakes retain one deadline across preludes and output waits');
