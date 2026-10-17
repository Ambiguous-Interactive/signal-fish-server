import { decode, encode } from '@msgpack/msgpack';
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
type Scenario = {
  sendAt: number;
  held?: boolean;
  preparationCrossesCutoff?: boolean;
  version?: number;
  receiveAt?: number;
  delayedOutput?: boolean;
  delayedDecode?: boolean;
  staleReceipt?: boolean;
};
const scenarios: Scenario[] = [
  { sendAt: 350 },
  { sendAt: 1099 },
  { sendAt: 1100 },
  { sendAt: 1101 },
  { sendAt: 350, held: true },
  { sendAt: 1099, held: true },
  { sendAt: 350, preparationCrossesCutoff: true },
];
// Regression #1039: receipt eligibility belongs to raw arrival, before decode
// and queued output. Accountable stale frames must never pay relay debt.
for (const version of [2, 3]) {
  for (const receiveAt of [1099, 1100, 1101])
    scenarios.push({ sendAt: 350, version, receiveAt });
  scenarios.push({ sendAt: 350, version, receiveAt: 1099, delayedOutput: true });
  scenarios.push({ sendAt: 350, version, receiveAt: 1099, delayedDecode: true });
}
scenarios.push({
  sendAt: 350,
  version: 3,
  receiveAt: 1099,
  delayedOutput: true,
  staleReceipt: true,
});
for (const format of GAME_DATA_FORMATS) {
  for (const {
    sendAt,
    held = false,
    preparationCrossesCutoff = false,
    version = 3,
    receiveAt,
    delayedOutput = false,
    delayedDecode = false,
    staleReceipt = false,
  } of scenarios) {
    const receiptScenario = receiveAt !== undefined;
    const originalSocket = globalThis.WebSocket;
    const originalParse = JSON.parse;
    const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
    const originalNow = performance.now;
    const originalSetTimeout = globalThis.setTimeout;
    const originalClearTimeout = globalThis.clearTimeout;
    const me = '00000000-0000-0000-0000-000000000001';
    const peer = '00000000-0000-0000-0000-000000000002';
    let socket!: Socket;
    let outputHeld = false;
    let releaseOutput!: () => void;
    const outputReady = new Promise<void>((resolve) => {
      releaseOutput = resolve;
    });
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
      onmessage: ((event: { data: unknown }) => void) | null = null;
      constructor() {
        socket = this;
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
              protocol_version: version,
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
              lobby_state: receiptScenario ? 'lobby' : 'finalized',
              ready_players: [],
              current_players: (receiptScenario ? [me, peer] : [me]).map((id) => ({
                id,
                ...(version >= 3 ? { epoch: 1, seq: 0 } : {}),
              })),
            });
            if (receiptScenario) this.deliver('GameStarting');
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
          __sf_emit: (line: string) => {
            const event = JSON.parse(line);
            events.push(event);
            if (delayedOutput && event.event === 'peer_transport_status') {
              outputHeld = true;
              return outputReady;
            }
          },
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
        peers: receiptScenario ? 2 : 1,
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
        protocolVersion: version,
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
      if (receiptScenario) {
        assert(result === undefined, 'outgoing relay leaves the peer receipt unpaid');
        if (staleReceipt) {
          socket.deliver('PlayerLeft', { player_id: peer, epoch: 1, final_seq: 1 });
          await turn();
        }
        if (delayedOutput) {
          socket.deliver('PeerTransportStatus', {
            peer_id: peer,
            transport: 'relay',
            connected: true,
          });
          await turn();
          assert(outputHeld, 'earlier output delays the receipt handler');
        }
        now = receiveAt;
        const stamps = version >= 3 ? { epoch: 1, seq: 1 } : {};
        if (format === 'json') {
          if (delayedDecode)
            JSON.parse = (...args: Parameters<typeof JSON.parse>) => {
              now = 1101;
              return originalParse(...args);
            };
          socket.deliver('GameData', {
            from_player: peer,
            ...stamps,
            data: { relay_msg: 'peer-probe' },
          });
        } else {
          const sender = new Uint8Array(16);
          sender[15] = 2;
          const opaque = new Uint8Array(26);
          opaque.set([83, 70, 82, 80, 1]);
          opaque.set(sender, 5);
          opaque.set(new TextEncoder().encode('probe'), 21);
          const payload =
            format === 'message_pack' ? encode({ relay_msg: 'peer-probe' }) : opaque;
          const frame =
            version < 3 && format !== 'message_pack'
              ? opaque
              : encode({ from_player: sender, encoding: format, payload, ...stamps });
          const view = new DataView(frame.buffer, frame.byteOffset, frame.byteLength);
          if (delayedDecode)
            Object.defineProperty(view, 'byteLength', {
              get: () => {
                now = 1101;
                return frame.byteLength;
              },
            });
          socket.onmessage?.({ data: view });
        }
        JSON.parse = originalParse;
        await turn();
        if (delayedOutput) {
          now = 1101;
          releaseOutput();
          await turn();
        }
        await advance(Math.max(now, receiveAt) + 250);
        assert(
          result === (receiveAt < 1100 && !staleReceipt ? EXIT_SUCCESS : EXIT_CRITERIA_UNMET),
          `${format}/v${version}/${receiveAt}/delayed=${delayedOutput}: relay receipt exit ${result}`,
        );
        assert(
          events.filter((event) => event.event === 'game_data_received').length ===
            (staleReceipt ? 0 : 1),
          'deadline admission preserves the valid receipt event',
        );
        await running;
        continue;
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
      JSON.parse = originalParse;
      if (originalWindow === undefined) delete (globalThis as { window?: unknown }).window;
      else Object.defineProperty(globalThis, 'window', originalWindow);
      performance.now = originalNow;
      globalThis.setTimeout = originalSetTimeout;
      globalThis.clearTimeout = originalClearTimeout;
    }
  }
}
console.error('ok - relay probes respect the run cutoff in every encoding');
