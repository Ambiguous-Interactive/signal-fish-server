import { decode } from '@msgpack/msgpack';
import { run } from './orchestrator.js';
import {
  EXIT_CRITERIA_UNMET,
  EXIT_SUCCESS,
  GAME_DATA_FORMATS,
  type RunConfig,
} from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #1037: a delayed relay timer cannot complete an expired run.
for (const format of GAME_DATA_FORMATS) {
  for (const [sendAt, held, preparationCrossesCutoff] of [
    [350, false, false],
    [1099, false, false],
    [1100, false, false],
    [1101, false, false],
    [350, true, false],
    [1099, true, false],
    [350, false, true],
  ] as const) {
    const originalSocket = globalThis.WebSocket;
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
    const originalNow = performance.now;
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const me = '00000000-0000-0000-0000-000000000001';
    const timers = new Map<number, { at: number; callback: () => void }>();
    const events: Record<string, unknown>[] = [];
    let now = 100;
    let timerId = 0;
    const relayFrames: (string | ArrayBuffer)[] = [];
    let released = false;
    let result: number | undefined;
    const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
    class Socket {
      static OPEN = 1;
      readyState = 1;
      bufferedAmount = 0;
      onopen: (() => void) | null = null;
      onclose: (() => void) | null = null;
      onmessage: ((event: { data: string }) => void) | null = null;
      constructor() {
        queueMicrotask(() => this.onopen?.());
      }
      deliver(type: string, data: Record<string, unknown> = {}) {
        this.onmessage?.({ data: JSON.stringify({ type, data }) });
      }
      send(frame: string | ArrayBuffer) {
        if (typeof frame !== 'string') {
          relayFrames.push(frame);
          return;
        }
        const message = JSON.parse(frame) as { type: string };
        if (message.type === 'GameData') relayFrames.push(frame);
        if (message.type === 'Authenticate') {
          queueMicrotask(() => {
            this.deliver('Authenticated');
            this.deliver('ProtocolInfo', {
              protocol_version: 3,
              game_data_formats: [...GAME_DATA_FORMATS],
            });
          });
        }
        if (message.type === 'JoinRoom') {
          queueMicrotask(() => {
            this.deliver('RoomJoined', {
              player_id: me,
              room_id: me,
              room_code: 'TEST',
              lobby_state: 'finalized',
              ready_players: [],
              current_players: [{ id: me, epoch: 1, seq: 0 }],
            });
            this.deliver('SessionPlan', {
              generation: me,
              topology: 'relay',
              transport: 'relay',
              fallback: 'relay',
              ice_servers: [],
              peers: [],
            });
          });
        }
      }
      close() {
        this.readyState = 3;
      }
    }
    try {
      globalThis.WebSocket = Socket as unknown as typeof WebSocket;
      Object.defineProperty(globalThis, 'window', {
        configurable: true,
        value: {
          __sf_emit: (line: string) => events.push(JSON.parse(line)),
          __sf_success_released: async () => released,
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
        peers: 1,
        maxPlayers: null,
        expectTotalPeers: null,
        leaveOnGameStart: false,
        gameName: 'relay-cutoff',
        playerName: 'test',
        appId: 'test',
        platform: 'test',
        exchange: false,
        relayPayload: 'probe',
        crippleIce: false,
        p2pTimeoutSecs: 1,
        p2pRetryCount: 0,
        runForSecs: 1,
        successReleaseEnabled: held,
        protocolVersion: 3,
        supportedTopologies: ['relay'],
        supportedTransports: ['relay'],
        gameDataFormat: format,
        sdkVersion: 'test',
        elapsedBeforeStartMs: 0,
      };
      const running = run(config);
      void running.then((code) => {
        result = code;
      });
      await turn();
      assert(
        events.some((event) => event['event'] === 'session_plan'),
        'relay plan must reach the real dispatcher',
      );
      const advance = async (next: number) => {
        now = next;
        for (const [id, timer] of [...timers]) {
          if (timer.at <= now) {
            timers.delete(id);
            timer.callback();
          }
        }
        await turn();
      };
      if (preparationCrossesCutoff) {
        // Model CPU time or preemption after timer admission and before the
        // encoded frame reaches WebSocket.send. All formats read this payload.
        Object.defineProperty(config, 'relayPayload', {
          get: () => {
            now = 1100;
            return 'probe';
          },
        });
      }
      await advance(sendAt);
      const expectedSends = sendAt < 1100 && !preparationCrossesCutoff ? 1 : 0;
      assert(
        relayFrames.length === expectedSends,
        `${format}/${sendAt}: expired relay timer sent ${relayFrames.length} probes; expected ${expectedSends}`,
      );
      if (expectedSends === 1) {
        const frame = relayFrames[0]!;
        if (format === 'json') {
          assert(
            frame === '{"type":"GameData","data":{"data":{"relay_msg":"probe"}}}',
            'JSON relay frame stays exact',
          );
        } else {
          assert(frame instanceof ArrayBuffer, 'binary relay sends an ArrayBuffer');
          const bytes = new Uint8Array(frame as ArrayBuffer);
          if (format === 'message_pack') {
            assert(
              JSON.stringify(decode(bytes)) === '{"relay_msg":"probe"}',
              'MessagePack carries the relay object',
            );
          } else {
            const expected = new Uint8Array(26);
            expected.set([83, 70, 82, 80, 1]);
            expected[20] = 1;
            expected.set(new TextEncoder().encode('probe'), 21);
            assert(
              JSON.stringify([...bytes]) === JSON.stringify([...expected]),
              'opaque relay preserves exact sender header and payload bytes',
            );
          }
        }
      }
      if (held) {
        assert(
          events.some((event) => event['event'] === 'success_criteria_met'),
          'the successful run must enter its requested hold',
        );
        await advance(1200);
        assert(
          result === undefined,
          'an established success hold survives the original run cutoff',
        );
        assert(relayFrames.length === 1, 'the hold never repeats its completed relay probe');
        released = true;
        await advance(1300);
        assert(result === undefined, 'release starts the full exit linger');
        await advance(1549);
        assert(
          result === undefined,
          'the active linger must survive beyond the original cutoff',
        );
        await advance(1550);
      } else {
        await advance((preparationCrossesCutoff ? 1100 : sendAt) + 249);
        if (expectedSends === 1 && sendAt + 249 < 1100)
          assert(
            result === undefined,
            'a permitted relay send retains its exit linger before the cutoff',
          );
        await advance((preparationCrossesCutoff ? 1100 : sendAt) + 250);
      }
      assert(
        relayFrames.length === expectedSends,
        'hold and linger expiry cannot repeat the relay probe',
      );
      assert(
        result === (expectedSends === 1 ? EXIT_SUCCESS : EXIT_CRITERIA_UNMET),
        `${format}/${sendAt}: unexpected exit ${result}`,
      );
      assert(
        events.filter((event) => event['event'] === 'game_data_sent').length === expectedSends,
        'receipt events must match actual permitted sends',
      );
      await running;
    } finally {
      globalThis.WebSocket = originalSocket;
      if (originalWindow === undefined) delete (globalThis as { window?: unknown }).window;
      else Object.defineProperty(globalThis, 'window', originalWindow);
      performance.now = originalNow;
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    }
  }
}
console.error('ok - relay probes respect the run cutoff in every encoding');
