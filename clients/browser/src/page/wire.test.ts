import { encode } from '@msgpack/msgpack';
import {
  bytesToBase64,
  classifyOpaqueNegotiatedServerInput,
  joinRoomFrameData,
  negotiatedGameDataFormat,
  parseV3BinaryGameDataFrame,
  sendOpaqueGameData,
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
  expectError(
    () =>
      classifyOpaqueNegotiatedServerInput(
        JSON.stringify({ type: 'GameData', data: { data: { relay_msg: 'hi' } } }),
      ),
    'text GameData while an opaque game_data_format was negotiated',
  );
  expectError(
    () => classifyOpaqueNegotiatedServerInput(Uint8Array.of(0x80).buffer),
    'invalid binary GameData frame while an opaque game_data_format was negotiated',
  );
  console.error(
    'ok - browser opaque-negotiated input routes binary and rejects text game data',
  );
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
