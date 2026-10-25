import { encode, decode, ExtData } from '@msgpack/msgpack';
import {
  bytesToBase64,
  classifyMessagePackNegotiatedServerInput,
  sendMessagePackGameData,
  connect,
  CONNECT_TIMEOUT_MS,
  classifyOpaqueNegotiatedServerInput,
  classifyJsonNegotiatedServerInput,
  joinRoomFrameData,
  negotiatedGameDataFormat,
  negotiatedProtocolVersion,
  parseV3BinaryGameDataFrame,
  sendOpaqueGameData,
  encodeOpaqueRelayProbe,
  decodeOpaqueRelayProbe,
  type ServerFrame,
} from './wire.js';

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
}

function expectError(run: () => void, detail: string): void {
  try {
    run();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (message.includes(detail)) {
      return;
    }
    throw new Error(`expected error containing ${detail}, got ${message}`);
  }
  throw new Error(`expected error containing ${detail}`);
}

const createConfig = {
  gameName: 'reference-browser',
  joinCode: null,
  playerName: 'RefBrowser',
  maxPlayers: null,
  peers: 3,
} as const;

const joinConfig = {
  ...createConfig,
  joinCode: 'ABC123',
} as const;

// Issue #630: `--join-code` adopts the collision-safe admission shape —
// `join_only: true` makes an unresolvable code refuse `ROOM_NOT_FOUND`
// instead of silently creating the room. A create-room run keeps the legacy
// create-by-omission contract: no `join_only` key at all, so the wire form
// stays byte-identical.
const joined = joinRoomFrameData(joinConfig);
assert(joined['join_only'] === true, 'a join-code run sends join_only: true');
assert(joined['room_code'] === 'ABC123', 'a join-code run carries the requested code');

const created = joinRoomFrameData(createConfig);
const createdKeys = Object.keys(created).sort();
assert(
  !('join_only' in created),
  'a create-room run omits join_only entirely (byte-identical legacy form)',
);
assert(
  JSON.stringify(createdKeys) ===
    JSON.stringify([
      'game_name',
      'max_players',
      'player_name',
      'room_code',
      'supports_authority',
    ]),
  `create-room keeps the exact legacy field set, got ${createdKeys.join(',')}`,
);

console.error('ok - browser JoinRoom frames join_only only for --join-code runs');

const SENDER = '00112233-4455-6677-8899-aabbccddeeff';
const SENDER_BYTES = Uint8Array.from([
  0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
  0xff,
]);

function exactArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return Uint8Array.from(bytes).buffer;
}

function binaryEnvelope(
  encoding: 'json' | 'rkyv' | 'protobuf',
  payload: Uint8Array,
): Uint8Array {
  return encode({
    from_player: SENDER_BYTES,
    encoding,
    payload,
    seq: 7,
    epoch: 3,
  });
}

// Issue #627: the strict v3 envelope validator accepts the protobuf wire
// token exactly like the other opaque encodings.
{
  const payload = Uint8Array.from([0, 1, 2, 255]);
  const frame = parseV3BinaryGameDataFrame(
    exactArrayBuffer(binaryEnvelope('protobuf', payload)),
  );
  assert(frame.type === 'GameDataBinary', 'a protobuf envelope decodes to GameDataBinary');
  assert(frame.data['from_player'] === SENDER, 'protobuf envelope decodes from_player');
  assert(frame.data['encoding'] === 'protobuf', 'protobuf envelope keeps its encoding token');
  assert(
    frame.data['seq'] === 7 && frame.data['epoch'] === 3,
    'protobuf envelope keeps stamps',
  );
  const decoded = frame.data['payload'];
  assert(
    decoded instanceof Uint8Array &&
      decoded.length === payload.length &&
      decoded.every((byte, index) => byte === payload[index]),
    'protobuf payload bytes are preserved exactly',
  );
  console.error('ok - browser v3 binary envelope accepts the protobuf encoding token');
}

// Issue #627: the opaque-negotiated classifier routes control frames as text,
// game data as v3 binary envelopes, and rejects text game data outright.
{
  const payload = Uint8Array.of(9, 8, 7);
  const frame = classifyOpaqueNegotiatedServerInput(
    exactArrayBuffer(binaryEnvelope('rkyv', payload)),
  );
  assert(
    frame.type === 'GameDataBinary' && frame.data['encoding'] === 'rkyv',
    'a binary frame decodes to its GameDataBinary envelope',
  );
  assert(
    classifyOpaqueNegotiatedServerInput(JSON.stringify({ type: 'Pong' })).type === 'Pong',
    'a text server control frame still classifies under opaque negotiation',
  );
  assert(
    classifyOpaqueNegotiatedServerInput(
      JSON.stringify({ type: 'GameData', data: { data: { relay_msg: 'hi' } } }),
    ).type === 'GameData',
    'opaque recipients must accept JSON-source and converted MessagePack data',
  );
  expectError(
    () => classifyOpaqueNegotiatedServerInput('{"type":"GameDataBinary"}'),
    'text GameDataBinary',
  );
  expectError(
    () => classifyOpaqueNegotiatedServerInput(Uint8Array.of(0x80).buffer),
    'invalid binary GameData frame while an opaque game_data_format was negotiated',
  );
  console.error('ok - browser opaque-negotiated input routes binary and accepts JSON fallback');
}

// Issue #627: the opaque relay payload is sent as exactly its payload bytes.
{
  const sent: ArrayBuffer[] = [];
  sendOpaqueGameData((frame) => sent.push(frame), 'hi');
  const utf8 =
    sent.length === 1 && sent[0] instanceof ArrayBuffer ? new Uint8Array(sent[0]) : null;
  assert(
    utf8 !== null && utf8.length === 2 && utf8[0] === 0x68 && utf8[1] === 0x69,
    'a string payload is sent as its UTF-8 bytes',
  );
  const payload = Uint8Array.from([0, 1, 254, 255]);
  sendOpaqueGameData((frame) => sent.push(frame), payload);
  payload[0] = 99;
  const frameBytes =
    sent.length === 2 && sent[1] instanceof ArrayBuffer ? new Uint8Array(sent[1]) : null;
  assert(
    frameBytes !== null &&
      frameBytes.length === 4 &&
      frameBytes[0] === 0 &&
      frameBytes[1] === 1 &&
      frameBytes[2] === 254 &&
      frameBytes[3] === 255,
    'a byte-array payload is sent as a copied, unmodified frame',
  );
  console.error('ok - browser opaque game data sends raw payload bytes as binary frames');
}

// Issue #627: the ProtocolInfo advertisement gate accepts advertised tokens
// and refuses an unadvertised opaque request by naming the server knobs.
{
  const advertised: ServerFrame = {
    type: 'ProtocolInfo',
    data: { game_data_formats: ['json', 'rkyv', 'protobuf'] },
  };
  assert(
    negotiatedGameDataFormat(advertised, 'rkyv') === 'rkyv' &&
      negotiatedGameDataFormat(advertised, 'protobuf') === 'protobuf',
    'an advertised opaque token is accepted unchanged',
  );
  const jsonOnly: ServerFrame = {
    type: 'ProtocolInfo',
    data: { game_data_formats: ['json'] },
  };
  expectError(
    () => negotiatedGameDataFormat(jsonOnly, 'rkyv'),
    'protocol.enable_rkyv_game_data / protocol.enable_protobuf_game_data',
  );
  expectError(() => negotiatedGameDataFormat(jsonOnly, 'protobuf'), 'does not advertise');
  expectError(
    () => negotiatedGameDataFormat(jsonOnly, 'message_pack'),
    'protocol.enable_message_pack_game_data',
  );
  expectError(
    () => negotiatedGameDataFormat({ type: 'Authenticated', data: {} }, 'rkyv'),
    'expected ProtocolInfo',
  );
  console.error('ok - browser ProtocolInfo game_data_formats gate names the server knobs');
}

// Opaque receipt events must render the payload losslessly (#627, native
// parity): base64 round-trips every byte, including non-UTF-8 ones.
{
  const bytes = Uint8Array.from([0x52, 0x4b, 0x59, 0x56, 0x00, 0xff]);
  assert(
    bytesToBase64(bytes) === 'UktZVgD/',
    'bytesToBase64 must render opaque bytes verbatim',
  );
  assert(bytesToBase64(new Uint8Array(0)) === '', 'an empty payload stays empty');
}

// Regression #819: neither malformed envelopes nor JSON parser errors may
// expose a room token through the shared handshake/runtime classifiers.
for (const text of [
  'private-room-token',
  JSON.stringify(['private-room-token']),
  JSON.stringify({ reconnection_token: 'private-room-token' }),
  JSON.stringify({ type: 'Reconnected', data: 'private-room-token' }),
]) {
  for (const classify of [
    classifyJsonNegotiatedServerInput,
    classifyOpaqueNegotiatedServerInput,
  ]) {
    let rejected = false;
    try {
      classify(text);
    } catch (error) {
      rejected = true;
      const message = error instanceof Error ? error.message : String(error);
      assert(!message.includes('private-room-token'), `token leaked: ${message}`);
      assert(message.includes(`${text.length} characters`), `missing size: ${message}`);
    }
    assert(rejected, 'malformed credential frame must fail');
  }
}

// An invalid message tag can contain the token too. Refusal diagnostics name
// the expected phase without echoing that untrusted tag.
for (const check of [
  () => negotiatedProtocolVersion({ type: 'private-room-token', data: {} }, 3),
  () => negotiatedGameDataFormat({ type: 'private-room-token', data: {} }, 'json'),
]) {
  try {
    check();
    assert(false, 'unexpected handshake message must fail');
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    assert(message === 'expected ProtocolInfo', `unsafe phase diagnostic: ${message}`);
  }
}

// Malformed binary metadata keys are also untrusted diagnostic input.
const privateBinaryKey = exactArrayBuffer(
  encode({
    from_player: SENDER_BYTES,
    encoding: 'rkyv',
    payload: Uint8Array.of(1),
    seq: 1,
    'private-room-token': 1,
  }),
);
try {
  parseV3BinaryGameDataFrame(privateBinaryKey);
  assert(false, 'unknown binary metadata key must fail');
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  assert(!message.includes('private-room-token'), `binary key leaked: ${message}`);
  assert(message.includes('unknown field'), `missing binary failure cause: ${message}`);
}

// Regression #981: dispatching a ready open must not cancel an expired budget.
const connectFailures: string[] = [];
for (const scenario of [
  'on-time',
  'at-deadline',
  'late-open',
  'timeout',
  'closed',
  'on-time',
] as const) {
  const originalSocket = globalThis.WebSocket;
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  let now = 123;
  let socket!: Socket;
  let nextTimer = 0;
  const timers = new Map<number, () => void>();
  class Socket {
    binaryType = 'blob';
    onopen: (() => void) | null = null;
    onclose: ((event: { code: number }) => void) | null = null;
    closes = 0;
    constructor() {
      socket = this;
    }
    close(): void {
      this.closes += 1;
      this.onclose?.({ code: 1000 });
    }
  }
  const check = (condition: boolean, message: string) => {
    if (!condition) connectFailures.push(`${scenario}: ${message}`);
  };
  try {
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    performance.now = () => now;
    globalThis.setTimeout = ((callback: () => void) => {
      const id = ++nextTimer;
      timers.set(id, callback);
      return id;
    }) as unknown as typeof setTimeout;
    globalThis.clearTimeout = ((id: number) => {
      timers.delete(id);
    }) as unknown as typeof clearTimeout;
    let outcome: WebSocket | Error | undefined;
    let settlements = 0;
    const connecting = connect('ws://mock.invalid/v3/ws').then(
      (value) => {
        outcome = value;
        settlements += 1;
      },
      (error: Error) => {
        outcome = error;
        settlements += 1;
      },
    );
    check(socket.binaryType === 'arraybuffer', 'preserve synchronous binary input');
    now +=
      scenario === 'on-time' || scenario === 'closed'
        ? CONNECT_TIMEOUT_MS - 1
        : CONNECT_TIMEOUT_MS;
    if (scenario === 'late-open') now += 1;
    if (scenario === 'timeout') {
      const due = [...timers.values()];
      timers.clear();
      for (const callback of due) callback();
    } else if (scenario === 'closed') {
      socket.onclose?.({ code: 1006 });
    } else {
      socket.onopen?.();
    }
    await connecting;
    if (scenario === 'on-time') {
      check(outcome === (socket as unknown as WebSocket), 'return the on-time socket');
      check(socket.closes === 0, 'keep the successful socket open');
    } else {
      check(outcome instanceof Error, 'reject the failed connection');
      check(
        outcome instanceof Error &&
          outcome.message.includes(scenario === 'closed' ? 'close code 1006' : 'timed out'),
        'retain the correct failure cause',
      );
      check(
        socket.closes === (scenario === 'closed' ? 0 : 1),
        'close an expired connection exactly once',
      );
    }
    check(timers.size === 0, 'cancel the connection timer');
    socket.onopen?.();
    socket.onclose?.({ code: 1006 });
    await Promise.resolve();
    check(settlements === 1, 'ignore late duplicate callbacks');
  } finally {
    globalThis.WebSocket = originalSocket;
    performance.now = originalNow;
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
  }
}
assert(connectFailures.length === 0, connectFailures.join('\n'));
console.error('ok - WebSocket connection expiry rejects late opens and cleans up timers');

// #678: MessagePack payloads use the existing version-specific relay envelope.
for (const version of [2, 3]) {
  const payload = { relay_msg: 'hello 世界', nested: [null, true, 1.25, -17] };
  const fields = {
    from_player: SENDER_BYTES,
    encoding: 'message_pack',
    payload: encode(payload),
  };
  const envelope = encode({ ...fields, ...(version === 3 ? { seq: 7, epoch: 3 } : {}) });
  const padded = new Uint8Array(envelope.length + 8);
  padded.set(envelope, 4);
  const frame = classifyMessagePackNegotiatedServerInput(padded.subarray(4, -4), version);
  assert(frame.type === 'GameData', `v${version} MessagePack becomes application GameData`);
  expectError(
    () => classifyMessagePackNegotiatedServerInput('{"type":"GameDataBinary"}', version),
    'text GameDataBinary',
  );
  assert(frame.data['from_player'] === SENDER, 'MessagePack preserves sender attribution');
  assert(JSON.stringify(frame.data['data']) === JSON.stringify(payload), 'payload round-trips');
  assert(
    version === 3
      ? frame.data['seq'] === 7 &&
          frame.data['epoch'] === 3 &&
          frame.data['class'] === 'reliable'
      : !('seq' in frame.data) && !('epoch' in frame.data) && !('class' in frame.data),
    'delivery metadata follows the recipient protocol',
  );
  assert(
    classifyMessagePackNegotiatedServerInput('{"type":"Pong"}', version).type === 'Pong',
    'control remains text',
  );
  const fallback = { type: 'GameData', data: { from_player: SENDER, data: payload } };
  assert(
    JSON.stringify(
      classifyMessagePackNegotiatedServerInput(JSON.stringify(fallback), version),
    ) === JSON.stringify(fallback),
    'mixed JSON sources remain text',
  );
  expectError(
    () =>
      classifyMessagePackNegotiatedServerInput(
        encode({
          ...fields,
          encoding: 'protobuf',
          ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
        }),
        version,
      ),
    'expected MessagePack',
  );
  expectError(
    () =>
      classifyMessagePackNegotiatedServerInput(
        encode({ ...fields, ...(version === 2 ? { seq: 1, epoch: 1 } : {}) }),
        version,
      ),
    'fields',
  );
  expectError(
    () => classifyMessagePackNegotiatedServerInput(new Uint8Array([...envelope, 0]), version),
    'trailing',
  );
  for (const invalid of [
    Uint8Array.of(0xc1),
    encode(Uint8Array.of(1)),
    encode(9007199254740992n, { useBigInt64: true }),
    encode(Number.NaN),
    encode(new Date()),
  ]) {
    let rejected = false;
    try {
      classifyMessagePackNegotiatedServerInput(
        encode({ ...fields, payload: invalid, ...(version === 3 ? { seq: 1, epoch: 1 } : {}) }),
        version,
      );
    } catch {
      rejected = true;
    }
    assert(rejected, `v${version} must reject malformed or lossy MessagePack payloads`);
  }
  const numericKey = classifyMessagePackNegotiatedServerInput(
    encode({
      ...fields,
      payload: Uint8Array.of(0x81, 0x01, 0x02),
      ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
    }),
    version,
  );
  assert(
    JSON.stringify(numericKey.data['data']) === '{"1":2}',
    `v${version} must preserve the existing numeric map-key conversion`,
  );
  const keyedPayload = JSON.parse(
    '{"__proto__":{"score":3},"constructor":{"prototype":4},"nested":[{"__proto__":5}],"":6,"\\u0000score":7,"世界":8,"\\ufeffscore":9}',
  ) as Record<string, unknown>;
  keyedPayload['x'.repeat(300)] = 'long key';
  const keyed = classifyMessagePackNegotiatedServerInput(
    encode({
      ...fields,
      payload: encode(keyedPayload),
      ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
    }),
    version,
  );
  assert(
    JSON.stringify(keyed.data['data']) === JSON.stringify(keyedPayload),
    `v${version} must preserve every JSON object key`,
  );
  const safe64 = classifyMessagePackNegotiatedServerInput(
    encode({
      ...fields,
      payload: encode({ value: 42n }, { useBigInt64: true }),
      ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
    }),
    version,
  );
  assert(
    (safe64.data['data'] as Record<string, unknown>)['value'] === 42,
    'safe int64 remains exact',
  );
}

// Regression #1215: long MessagePack strings must preserve their leading BOM.
for (const version of [2, 3]) {
  for (const length of [0, 190, 191, 192, 200, 1_000]) {
    for (const prefix of ['', '\ufeff', '\ufeff\ufeff']) {
      const text = `${prefix}${'a'.repeat(length)}世界`;
      for (const value of [text, [text], { nested: [{ dialogue: text }] }]) {
        const frame = classifyMessagePackNegotiatedServerInput(
          encode({
            from_player: SENDER_BYTES,
            encoding: 'message_pack',
            payload: encode(value),
            ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
          }),
          version,
        );
        assert(
          JSON.stringify(frame.data['data']) === JSON.stringify(value),
          `v${version}/${length}/${prefix.length}: MessagePack preserves complete string values`,
        );
      }
    }
  }
  for (const [invalid, error] of [
    [Uint8Array.of(0xef, 0xbb, 0xbf), 'not a plain object'],
    [new Date(0), 'not a plain object'],
    [new ExtData(42, Uint8Array.of(0xef, 0xbb, 0xbf)), 'not a plain object'],
    [9007199254740992n, 'not exactly representable'],
    [Number.NaN, 'not a finite number'],
  ] as const) {
    expectError(
      () =>
        classifyMessagePackNegotiatedServerInput(
          encode({
            from_player: SENDER_BYTES,
            encoding: 'message_pack',
            payload: encode({ text: '\ufeffvalue', invalid }, { useBigInt64: true }),
            ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
          }),
          version,
        ),
      error,
    );
  }
  const dialogue = { dialogue: `\ufeff${'a'.repeat(200)}` };
  const envelope = encode({
    from_player: SENDER_BYTES,
    encoding: 'message_pack',
    payload: encode(dialogue),
    ...(version === 3 ? { seq: 1, epoch: 1 } : {}),
  });
  const storage = Uint8Array.from([9, ...envelope, 8]);
  for (const view of [
    storage.subarray(1, -1),
    new DataView(storage.buffer, 1, envelope.length),
  ]) {
    assert(
      JSON.stringify(classifyMessagePackNegotiatedServerInput(view, version).data['data']) ===
        JSON.stringify(dialogue),
      `v${version}: preserve MessagePack dialogue inside an offset view`,
    );
  }
}
console.error('ok - MessagePack string values preserve leading BOMs across decoder sizes');

{
  let output: ArrayBuffer | undefined;
  sendMessagePackGameData(
    (frame) => {
      output = frame;
    },
    { relay_msg: 'hello 世界' },
  );
  assert(
    output !== undefined && JSON.stringify(decode(output)) === '{"relay_msg":"hello 世界"}',
    'sender emits raw MessagePack with no relay envelope',
  );
  expectError(
    () =>
      sendMessagePackGameData(
        () => {
          throw new Error('must not send');
        },
        { invalid: Infinity },
      ),
    'not a finite number',
  );
}

// #678: both reference clients use these exact application bytes on v2/v3.
{
  for (const payload of [
    new Uint8Array(),
    Uint8Array.of(0, 255, 1),
    new TextEncoder().encode('雪\nSFRP\u0000'),
    new Uint8Array(65536).fill(42),
  ]) {
    const expected = Uint8Array.from([83, 70, 82, 80, 1, ...SENDER_BYTES, ...payload]);
    const bytes = encodeOpaqueRelayProbe(SENDER, payload);
    assert(
      bytes.length === expected.length &&
        bytes.every((byte, index) => byte === expected[index]),
      'exact shared header vector',
    );
    for (const serverSender of [undefined, SENDER]) {
      const probe = decodeOpaqueRelayProbe(bytes, serverSender);
      assert(
        probe.from === SENDER && bytesToBase64(probe.payload) === bytesToBase64(payload),
        'lossless opaque probe',
      );
    }
    for (const format of ['rkyv', 'protobuf'] as const) {
      for (const version of [2, 3]) {
        const input = version === 2 ? bytes : binaryEnvelope(format, bytes);
        const storage = Uint8Array.from([9, ...input, 8]);
        const frame = classifyOpaqueNegotiatedServerInput(
          new DataView(storage.buffer, 1, input.length),
          version,
          format,
        );
        assert(
          frame.data['from_player'] === SENDER && frame.data['encoding'] === format,
          'version-specific probe attribution',
        );
        assert(
          bytesToBase64(frame.data['payload'] as Uint8Array) === bytesToBase64(payload),
          'header is not application payload',
        );
        assert(
          'seq' in frame.data === (version === 3) && 'epoch' in frame.data === (version === 3),
          'v2 never invents delivery metadata',
        );
      }
    }
  }
  const valid = encodeOpaqueRelayProbe(SENDER, Uint8Array.of(7));
  for (let length = 0; length < 21; length++) {
    expectError(
      () => decodeOpaqueRelayProbe(valid.subarray(0, length)),
      length < 4
        ? 'requires a reference probe header'
        : 'invalid opaque reference probe header',
    );
  }
  for (const version of [0, 2, 255]) {
    const invalid = valid.slice();
    invalid[4] = version;
    expectError(
      () => decodeOpaqueRelayProbe(invalid, SENDER),
      'invalid opaque reference probe header',
    );
  }
  expectError(
    () => decodeOpaqueRelayProbe(valid, '11111111-1111-1111-1111-111111111111'),
    'sender differs',
  );
  expectError(
    () => encodeOpaqueRelayProbe('not-a-uuid', new Uint8Array()),
    'invalid opaque reference probe sender',
  );
  const legacy = Uint8Array.of(0, 255);
  assert(
    decodeOpaqueRelayProbe(legacy, SENDER).payload === legacy,
    'v3 keeps legacy raw bytes',
  );
  expectError(() => decodeOpaqueRelayProbe(legacy), 'requires a reference probe header');
  console.error('ok - opaque reference probes attribute v2 raw passthrough and v3 envelopes');
}
