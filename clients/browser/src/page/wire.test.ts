import { joinRoomFrameData } from './wire.js';

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
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
