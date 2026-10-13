import { run } from './orchestrator.js';
import { encode } from '@msgpack/msgpack';
import type { RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

const config: RunConfig = {
  serverUrl: 'ws://mock.invalid/v3/ws',
  createRoom: true,
  joinCode: null,
  peers: 2,
  maxPlayers: null,
  expectTotalPeers: null,
  leaveOnGameStart: false,
  gameName: 'ingress',
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

for (const scenario of [
  ...[
    'authenticate',
    'join',
    'stream',
    'authenticate-held',
    'join-held',
    'join-sequential',
    'transition-held',
    'frame-overflow',
    'byte-overflow',
  ].map((phase) => ({ phase, version: 3, format: 'json' as const })),
  { phase: 'backing-overflow', version: 3, format: 'protobuf' as const },
  ...([2, 3] as const).map((version) => ({
    phase: 'healthy',
    version,
    format: 'json' as const,
  })),
  ...([2, 3] as const).map((version) => ({
    phase: 'healthy',
    version,
    format: 'json' as const,
    large: true,
  })),
  ...(['rkyv', 'protobuf'] as const).map((format) => ({
    phase: 'healthy',
    version: 3,
    format,
  })),
]) {
  const { phase, version, format } = scenario;
  const large = 'large' in scenario && scenario.large === true;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalError = console.error;
  const events: Record<string, unknown>[] = [];
  const peer = '00000000-0000-0000-0000-000000000002';
  const me = '00000000-0000-0000-0000-000000000001';
  const players = Array.from({ length: large ? 255 : 2 }, (_value, index) => ({
    id: `00000000-0000-0000-0000-${(index + 1).toString(16).padStart(12, '0')}`,
    name: large ? 'p'.repeat(256) : 'player',
    is_authority: index === 0,
    is_ready: false,
    ...(version >= 3 ? { epoch: 1, seq: 0 } : {}),
  }));
  let socket: TestSocket | undefined;
  let release: (() => void) | undefined;
  let held = [
    'stream',
    'healthy',
    'byte-overflow',
    'authenticate-held',
    'join-held',
    'transition-held',
  ].includes(phase);
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  let result: number | undefined;
  let running: Promise<number> | undefined;
  class TestSocket {
    static readonly OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    binaryType = 'arraybuffer';
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: unknown }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(type: string, data: Record<string, unknown> = {}): void {
      this.onmessage?.({ data: JSON.stringify({ type, data }) });
    }
    send(text: string): void {
      const frame = JSON.parse(text) as { type: string };
      if (frame.type === 'Authenticate')
        queueMicrotask(() => {
          this.deliver('Authenticated');
          this.deliver('ProtocolInfo', {
            protocol_version: version,
            game_data_formats: ['json', 'rkyv', 'protobuf'],
          });
          if (phase === 'authenticate' || phase === 'authenticate-held')
            for (let i = 0; i < 257; i += 1) this.deliver('Pong');
        });
      if (frame.type === 'JoinRoom')
        queueMicrotask(() => {
          if (phase === 'join' || phase === 'join-held' || phase === 'join-sequential') {
            if (phase === 'join-sequential') return;
            for (let i = 0; i < 257; i += 1)
              this.deliver('PlayerJoined', { player: { id: peer, epoch: 1, seq: 0 } });
            return;
          }
          this.deliver('RoomJoined', {
            player_id: me,
            room_id: me,
            room_code: 'TEST',
            lobby_state: 'lobby',
            current_players: players,
            ready_players: [],
          });
        });
    }
    close(): void {
      this.readyState = 3;
    }
  }
  try {
    globalThis.WebSocket = TestSocket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          const event = JSON.parse(line) as Record<string, unknown>;
          events.push(event);
          if (
            held &&
            (event['event'] === 'game_data_received' ||
              (phase === 'authenticate-held' && event['event'] === 'connected') ||
              (phase === 'join-held' && event['event'] === 'protocol_info') ||
              (phase === 'transition-held' && event['event'] === 'room_joined'))
          )
            return blocked;
        },
      },
    });
    console.error = () => {};
    running = run({ ...config, protocolVersion: version, gameDataFormat: format }).then(
      (code) => {
        result = code;
        return code;
      },
    );
    await turn();
    const deliverRelay = (seq: number) => {
      if (format === 'json')
        socket?.deliver('GameData', {
          from_player: peer,
          ...(version >= 3 ? { epoch: 1, seq } : {}),
          data: { relay_msg: `relay-${seq}` },
        });
      else
        socket?.onmessage?.({
          data: encode({
            from_player: new Uint8Array(16)
              .fill(0)
              .map((_byte, index) => (index === 15 ? 2 : 0)),
            encoding: format,
            payload: new Uint8Array([seq]),
            seq,
            epoch: 1,
          }),
        });
    };
    if (['stream', 'healthy', 'byte-overflow'].includes(phase)) {
      deliverRelay(1);
      await turn();
      assert(
        events.filter((event) => event['event'] === 'game_data_received').length === 1,
        `${phase}/v${version}/${format}: hold an actual relay event; observed ${JSON.stringify(events)}`,
      );
      if (phase === 'byte-overflow') {
        const envelope = JSON.stringify({ type: 'Pong', data: { padding: '' } });
        const maximum = envelope.replace(
          '"padding":""',
          `"padding":"${'a'.repeat(64 * 1024 * 1024 - envelope.length)}"`,
        );
        socket?.onmessage?.({ data: maximum });
        await turn();
        assert(
          result === undefined,
          'accept one exact portable 64 MiB frame while output is held',
        );
        socket?.onmessage?.({ data: maximum });
        await turn();
      } else {
        for (let i = 0; i < 255; i += 1) socket?.deliver('Pong');
        await turn();
        assert(
          result === undefined,
          'the held relay plus 255 frames fit the exact count boundary',
        );
        if (phase === 'stream') {
          socket?.deliver('Pong');
          await turn();
        } else {
          held = false;
          release?.();
          await turn();
          for (let i = 0; i < 256; i += 1) socket?.deliver('Pong');
          await turn();
          deliverRelay(2);
          await turn();
          const receipts = events.filter((event) => event['event'] === 'game_data_received');
          if (large) {
            const current = players.map((player) => ({
              ...player,
              ...(version >= 3 && player.id === peer ? { seq: 2 } : {}),
            }));
            const baseline = {
              current_players: current,
              lobby_state: 'lobby',
              ready_players: [],
              ...(version >= 3
                ? {
                    sender_watermarks: current.map((player) => ({
                      player_id: player.id,
                      epoch: 1,
                      seq: player.id === peer ? 2 : 0,
                    })),
                  }
                : {}),
            };
            assert(
              new TextEncoder().encode(JSON.stringify(baseline)).byteLength > 65536,
              'exercise a snapshot larger than the inbound default',
            );
            socket?.deliver('Reconnected', baseline);
            await turn();
            deliverRelay(3);
            await turn();
            assert(
              events.filter((event) => event['event'] === 'game_data_received').length === 3,
              'large reconnect baseline preserves later relay delivery',
            );
          }

          assert(
            receipts.length === 2,
            'drained capacity must admit exact later relay receipts',
          );
          if (format === 'json')
            assert(
              (receipts[1]?.['payload'] as Record<string, unknown>)['relay_msg'] === 'relay-2',
              'keep JSON relay order and content',
            );
          else
            assert(
              (receipts[1]?.['payload'] as Record<string, unknown>)['payload'] === 'Ag==',
              'keep exact opaque relay bytes',
            );
          assert(
            result === undefined && !events.some((event) => event['event'] === 'error'),
            'healthy v2/v3 and opaque traffic stays live without errors',
          );
        }
      }
    }
    if (phase === 'backing-overflow') {
      const wire = encode({
        from_player: new Uint8Array(16),
        encoding: 'protobuf',
        payload: new Uint8Array([1]),
        seq: 1,
        epoch: 1,
      });
      const backing = new ArrayBuffer(128 * 1024 * 1024 + 1);
      const view = new Uint8Array(backing, 1024, wire.byteLength);
      view.set(wire);
      socket?.onmessage?.({ data: view });
      await turn();
    }
    if (phase === 'frame-overflow') {
      socket?.onmessage?.({ data: new ArrayBuffer(64 * 1024 * 1024 + 1) });
      await turn();
    }
    if (phase === 'transition-held') {
      for (let i = 0; i < 256; i += 1) socket?.deliver('Pong');
      await turn();
    }
    if (phase === 'join-sequential') {
      for (let i = 0; i < 257; i += 1) {
        socket?.deliver('PlayerJoined', { player: { id: peer, epoch: 1, seq: 0 } });
        await turn();
      }
    }
    if (phase !== 'healthy') {
      assert(
        result === 3,
        `${phase}: unconsumed WebSocket backlog must fail promptly with code 3, got ${result} (#879)`,
      );
      const message =
        phase === 'frame-overflow' ? 'WebSocket receive frame' : 'WebSocket receive backlog';
      assert(
        events.some(
          (event) => event['event'] === 'error' && String(event['message']).includes(message),
        ),
        `${phase}: identify ingress overload`,
      );
      assert(
        !events.some((event) => event['event'] === 'success_criteria_met'),
        `${phase}: overload cannot report success`,
      );
      const priorEvents = events.length;
      socket?.deliver('GameData', { from_player: peer, epoch: 1, seq: 999, data: 'late' });
      await turn();
      assert(
        events.length === priorEvents && socket?.readyState === 3,
        `${phase}: stop input and close the socket after terminal failure`,
      );
    }
  } finally {
    held = false;
    release?.();
    socket?.onclose?.();
    await running;
    globalThis.WebSocket = originalSocket;
    console.error = originalError;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - WebSocket retention stays bounded across handshake and held relay output');
