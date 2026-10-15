import {
  ExchangeLedger,
  advanceAuthenticateHandshake,
  nextKeepaliveWake,
  scheduleSuccessReleasePoll,
  shouldDeferSuccessAtRunDeadline,
  StartGameGate,
  run,
} from './orchestrator.js';
import type { RunConfig } from '../shared/types.js';
import { Engine, RELIABLE_LABEL, UNRELIABLE_LABEL } from './engine.js';

function assert(condition: boolean, message: string): void {
  if (!condition) {
    throw new Error(message);
  }
}

assert(
  scheduleSuccessReleasePoll(null, true, 123) === null,
  'an in-flight bridge Promise must not arm a zero-delay wake',
);
assert(
  scheduleSuccessReleasePoll(null, false, 123) === 123,
  'an idle barrier must schedule an immediate probe',
);
assert(
  scheduleSuccessReleasePoll(456, false, 123) === 456,
  'an existing bounded poll deadline must be retained',
);
assert(
  nextKeepaliveWake(100, 200) === 200,
  'an overdue ping must not create a zero-delay wake while Pong grace is active',
);
assert(
  nextKeepaliveWake(100, null) === 100,
  'the ping cadence must own keepalive scheduling when no Pong is outstanding',
);
assert(
  shouldDeferSuccessAtRunDeadline(true, true, false, null),
  'a held success barrier must defer an overdue soft deadline',
);
assert(
  shouldDeferSuccessAtRunDeadline(true, true, true, 500),
  'the post-release linger must remain authoritative after the barrier opens',
);
assert(
  !shouldDeferSuccessAtRunDeadline(false, false, false, 500),
  'an ordinary run must retain its bounded soft-deadline behavior',
);
assert(
  !shouldDeferSuccessAtRunDeadline(true, true, true, null),
  'a released barrier with no pending linger must not suppress the soft deadline',
);

const departedPeer = '00000000-0000-0000-0000-000000000002';
const exchangeLedger = new ExchangeLedger();
assert(
  exchangeLedger.unmetCriteria().length === 0,
  'a never-connected peer must not create exchange debt',
);
exchangeLedger.noteConnected(departedPeer);
exchangeLedger.noteSent(departedPeer, 'reliable');
exchangeLedger.noteReceived(departedPeer, 'reliable');
assert(
  exchangeLedger.unmetCriteria().length === 2,
  'a connected departed peer must retain both missing unreliable directions',
);
exchangeLedger.noteSent(departedPeer, 'unreliable');
exchangeLedger.noteReceived(departedPeer, 'unreliable');
assert(
  exchangeLedger.unmetCriteria().length === 0,
  'a completed exchange must remain satisfied after peer departure',
);

// Regression #889: report missing channels and their direction independently.
for (const direction of ['sent to', 'received from']) {
  for (const missing of [
    [RELIABLE_LABEL],
    [UNRELIABLE_LABEL],
    [RELIABLE_LABEL, UNRELIABLE_LABEL],
  ]) {
    const ledger = new ExchangeLedger();
    ledger.noteConnected(departedPeer);
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      if (direction !== 'sent to' || !missing.includes(label))
        ledger.noteSent(departedPeer, label);
      if (direction !== 'received from' || !missing.includes(label)) {
        ledger.noteReceived(departedPeer, label);
      }
    }
    assert(
      JSON.stringify(ledger.unmetCriteria()) ===
        JSON.stringify([
          `exchange incomplete (${direction} ${departedPeer}; missing ${missing.join(', ')})`,
        ]),
      `${direction}: missing ${missing.join(', ')} must name exactly the affected channels`,
    );
  }
}

// Regression #851: reset only the restored peer, including partial receipts.
{
  const restored = '00000000-0000-0000-0000-000000000002';
  const retained = '00000000-0000-0000-0000-000000000003';
  const ledger = new ExchangeLedger();
  ledger.resetIncarnation(restored);
  assert(ledger.unmetCriteria().length === 0, 'an unseen restore creates no exchange debt');
  for (const peer of [restored, retained]) {
    ledger.noteConnected(peer);
    for (const label of ['reliable', 'unreliable']) {
      ledger.noteSent(peer, label);
      ledger.noteReceived(peer, label);
    }
  }
  ledger.resetIncarnation(restored);
  ledger.resetIncarnation(restored);
  assert(
    ledger.unmetCriteria().length === 2,
    'a repeated restore retains both directions of debt',
  );
  for (const label of ['reliable', 'unreliable']) {
    assert(!ledger.hasSent(restored, label), 'a restored peer needs each label sent again');
    assert(ledger.hasSent(retained, label), 'other peers retain their completed exchange');
    ledger.noteSent(restored, label);
  }
  assert(
    ledger.unmetCriteria().length === 1,
    'fresh sends do not satisfy missing fresh receipts',
  );
  ledger.noteReceived(restored, 'reliable');
  assert(ledger.unmetCriteria().length === 1, 'both fresh labels are required');
  ledger.noteReceived(restored, 'unreliable');
  assert(
    ledger.unmetCriteria().length === 0,
    'fresh bidirectional traffic completes the exchange',
  );
}

// Pins the room creator's explicit-`StartGame` gate against the documented
// `all_ready` semantics (issue #447 F1 / issue #449) — the same scenarios as
// the native `start_game_gate_reissues_after_membership_invalidation` pin.
{
  const creator = '00000000-0000-0000-0000-000000000001';
  const joiner = '00000000-0000-0000-0000-000000000002';
  const present = (...players: string[]) => new Set(players);
  let gate = new StartGameGate([]);

  assert(
    !gate.shouldSend(true, false, present(creator)),
    'an empty readiness baseline must not send',
  );

  // Happy path: the all-ready toggle drives exactly one send, and a repeated
  // broadcast without an invalidation must not duplicate it.
  gate.snapshot([creator]);
  assert(gate.shouldSend(true, false, present(creator)), 'first all-ready snapshot must send');
  gate.noteSent();
  assert(
    !gate.shouldSend(true, false, present(creator)),
    'a repeated all-ready broadcast must not duplicate the send',
  );
  assert(!gate.shouldSend(false, false, present(creator)), 'non-creators never send');
  assert(!gate.shouldSend(true, true, present(creator)), 'a finalized room never sends');
  gate.snapshot([creator, joiner]);
  assert(
    !gate.shouldSend(true, false, present(creator)),
    'readiness additions must not duplicate a send',
  );

  // Join invalidation: the latecomer is unready with no corrective broadcast;
  // the latch re-arms but the room is provably not all-ready.
  gate.memberJoined(joiner);
  assert(!gate.shouldSend(true, false, present(creator, joiner)), 'right after the join');
  // The joiner's toggle restores an authoritative all-ready snapshot and the
  // creator re-issues.
  gate.snapshot([creator, joiner]);
  assert(
    gate.shouldSend(true, false, present(creator, joiner)),
    "after the joiner's toggle the creator must re-issue",
  );
  gate.noteSent();

  // Authoritative rejection re-arms: a NotReady between snapshot and send
  // means the cached snapshot was stale. Production only queries the gate on
  // the NEXT authoritative frame (toggle or membership change), which carries
  // the refreshed snapshot.
  gate.startRejected();
  gate.snapshot([creator, joiner]);
  assert(gate.shouldSend(true, false, present(creator, joiner)), 'recovery after a rejection');
  gate.noteSent();

  // Departure restoration: the unready member leaves with NO readiness
  // broadcast; membership recomputation alone must re-issue.
  gate.memberLeft(joiner);
  assert(
    gate.shouldSend(true, false, present(creator)),
    'departure restores all-ready without a broadcast',
  );

  // RoomLeft resets the whole baseline.
  gate.noteSent();
  gate.reset();
  assert(!gate.shouldSend(true, false, present(creator)), 'a reset gate must not send');

  // Regression #990: a ready player can toggle off and on before StartGame's
  // error arrives. Each new all-ready moment must issue a fresh request.
  for (const rejection of ['after', 'between', 'before', 'none']) {
    gate = new StartGameGate([creator, joiner]);
    gate.noteSent();
    if (rejection === 'before') gate.startRejected();
    gate.snapshot([creator]);
    assert(
      !gate.shouldSend(true, false, present(creator, joiner)),
      `${rejection}: an unready member must prevent a retry`,
    );
    if (rejection === 'between') gate.startRejected();
    gate.snapshot([creator, joiner]);
    assert(
      gate.shouldSend(true, false, present(creator, joiner)),
      `${rejection}: restored readiness must retry before a delayed rejection`,
    );
    assert(
      !gate.shouldSend(false, false, present(creator, joiner)),
      `${rejection}: restored readiness must not let a non-creator send`,
    );
    assert(
      !gate.shouldSend(true, true, present(creator, joiner)),
      `${rejection}: restored readiness must not restart a finalized room`,
    );
    gate.noteSent();
    gate.snapshot([joiner, creator]);
    assert(
      !gate.shouldSend(true, false, present(creator, joiner)),
      `${rejection}: an unchanged ready set must not duplicate the retry`,
    );
    if (rejection === 'after') gate.startRejected();
  }
}

console.error('ok - browser timer and latched-exchange state avoids false success');

// ---------------------------------------------------------------------------
// Authenticate handshake: the server's pinned downgrade notice (#627 family).
//
// The server answers an unsupported requested `game_data_format` with a
// budget-charged `Error` BEFORE `Authenticated` (pinned wire order, server
// `tests/e2e_tests.rs`) while downgrading the session to JSON. Before this
// pin the browser client aborted a viable session with
// "expected Authenticated, got Error" (audit 2026-10-04).
// ---------------------------------------------------------------------------
{
  const downgrade = {
    type: 'Error',
    data: {
      message: "Requested game data format 'rkyv' is not supported. Falling back to JSON.",
      error_code: 'UNSUPPORTED_GAME_DATA_FORMAT',
    },
  };

  let step = advanceAuthenticateHandshake(downgrade, 'rkyv');
  assert(
    !('fatal' in step) && !step.authenticated,
    'a downgrade notice continues the handshake',
  );
  assert(
    !('fatal' in step) && step.effectiveFormat === 'json',
    'a downgrade notice adopts JSON',
  );

  const authenticated = { type: 'Authenticated', data: { app_name: 'default' } };
  step = advanceAuthenticateHandshake(authenticated, 'json');
  assert(!('fatal' in step) && step.authenticated, 'Authenticated completes the handshake');

  // A notice for an already-JSON session cannot adopt JSON again: the loop
  // guard makes it fatal so a contract-violating server cannot spin it.
  step = advanceAuthenticateHandshake(downgrade, 'json');
  assert('fatal' in step, 'a repeat downgrade notice stays fatal');

  // Any other error code stays fatal at the handshake boundary.
  const roomFull = { type: 'Error', data: { message: 'full', error_code: 'ROOM_FULL' } };
  step = advanceAuthenticateHandshake(roomFull, 'rkyv');
  assert('fatal' in step, 'unrelated error codes stay fatal');

  const rejected = {
    type: 'AuthenticationError',
    data: { error: 'nope', error_code: 'UNAUTHORIZED' },
  };
  step = advanceAuthenticateHandshake(rejected, 'rkyv');
  assert('fatal' in step, 'AuthenticationError stays fatal');
}

console.error('ok - authenticate handshake adopts the pinned JSON downgrade notice');

// Regression #819: a bad message tag is untrusted input, not diagnostic text.
const credentialTag = advanceAuthenticateHandshake(
  { type: 'private-room-token', data: {} },
  'json',
);
assert(
  'fatal' in credentialTag && credentialTag.fatal === 'expected Authenticated',
  'unexpected authentication messages must not echo credential tags',
);

// Regression #819: drive the real JoinRoom handshake with a credential tag.
{
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalError = console.error;
  const events: Record<string, unknown>[] = [];
  const diagnostics: string[] = [];
  const sent: string[] = [];
  class HandshakeSocket {
    static readonly OPEN = 1;
    readonly readyState = 1;
    readonly bufferedAmount = 0;
    binaryType = 'arraybuffer';
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor(_url: string) {
      queueMicrotask(() => this.onopen?.());
    }
    send(text: string): void {
      const frame = JSON.parse(text) as { type: string };
      sent.push(frame.type);
      const replies =
        frame.type === 'Authenticate'
          ? [
              { type: 'Authenticated', data: {} },
              {
                type: 'ProtocolInfo',
                data: { protocol_version: 3, game_data_formats: ['json'] },
              },
            ]
          : [{ type: 'private-join-room-token', data: {} }];
      queueMicrotask(() => {
        for (const reply of replies) {
          this.onmessage?.({ data: JSON.stringify(reply) });
        }
      });
    }
    close(): void {}
  }
  const config: RunConfig = {
    serverUrl: 'ws://mock.invalid/v3/ws',
    createRoom: true,
    joinCode: null,
    peers: 2,
    maxPlayers: null,
    expectTotalPeers: null,
    leaveOnGameStart: false,
    gameName: 'join-privacy',
    playerName: 'test',
    appId: 'test',
    platform: 'test',
    exchange: false,
    relayPayload: null,
    crippleIce: false,
    p2pTimeoutSecs: 1,
    p2pRetryCount: 0,
    runForSecs: 1,
    successReleaseEnabled: false,
    protocolVersion: 3,
    supportedTopologies: ['relay'],
    supportedTransports: ['relay'],
    gameDataFormat: 'json',
    sdkVersion: 'test',
    elapsedBeforeStartMs: 0,
  };
  try {
    globalThis.WebSocket = HandshakeSocket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => events.push(JSON.parse(line) as Record<string, unknown>),
      },
    });
    console.error = (...args: unknown[]) => diagnostics.push(args.map(String).join(' '));
    const code = await run(config);
    assert(code === 2, 'an unexpected join reply must fail as a protocol error');
    assert(
      sent.join(',') === 'Authenticate,JoinRoom',
      'the real join handler must receive the reply',
    );
    const errors = events.filter((event) => event['event'] === 'error');
    assert(errors.length === 1, 'the join refusal must produce one terminal error');
    assert(
      !JSON.stringify(errors).includes('private-join-room-token'),
      'join error event must not expose the credential tag',
    );
    assert(
      !diagnostics.join(' ').includes('private-join-room-token'),
      'join stderr must not expose the credential tag',
    );
    assert(
      errors[0]?.['message'] === 'expected RoomJoined',
      'join diagnostic must identify the expected response',
    );
  } finally {
    globalThis.WebSocket = originalSocket;
    console.error = originalError;
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, 'window');
    } else {
      Object.defineProperty(globalThis, 'window', originalWindow);
    }
  }
}

console.error('ok - join handshake excludes credential tags from event and stderr diagnostics');

// Regressions #851/#937: a restored peer needs fresh traffic from a fresh link.
// Drive the real dispatcher; a generation-only rebuild keeps prior receipts.
for (const [membershipEvent, departure] of [
  ['PlayerReconnected', true],
  ['PlayerJoined', true],
  ['PlayerReconnected', false],
  ['PlayerJoined', false],
  [null, false],
] as const) {
  const peerRestored = membershipEvent !== null;
  const originalSocket = globalThis.WebSocket;
  const originalPc = globalThis.RTCPeerConnection;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalError = console.error;
  const originalDebug = console.debug;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const events: Record<string, unknown>[] = [];
  const channelSends: string[] = [];
  const connections: PeerConnection[] = [];
  let socket!: RestoreSocket;
  let restoreSent = false;
  let invalidReceiptCount = 0;
  let released = false;
  let settled = false;
  const originalNow = performance.now;
  let virtualNow = Math.floor(originalNow.call(performance));
  let guardTimer: ReturnType<typeof setTimeout> | undefined;
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  const plan = (generation: number) => ({
    type: 'SessionPlan',
    data: {
      generation: `00000000-0000-0000-0000-${String(generation).padStart(12, '0')}`,
      topology: 'mesh',
      transport: 'webrtc',
      fallback: 'relay',
      ice_servers: [],
      peers: [{ player_id: peer, initiate: true }],
    },
  });
  class Channel {
    readonly readyState = 'open';
    readonly bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(
      readonly label: string,
      readonly generation: number,
    ) {}
    send(_text: string): void {
      channelSends.push(`${this.generation}:${this.label}`);
    }
    receive(text = JSON.stringify({ from: peer, channel: this.label, seq: 0 })): void {
      this.onmessage?.({ data: text });
    }
  }
  class PeerConnection {
    readonly channels: Channel[] = [];
    constructor(_config: unknown) {
      connections.push(this);
    }
    createDataChannel(label: string): Channel {
      const channel = new Channel(label, connections.length);
      this.channels.push(channel);
      return channel;
    }
    async createOffer(): Promise<{ type: string; sdp: string }> {
      return { type: 'offer', sdp: 'test-sdp' };
    }
    async setLocalDescription(_description: unknown): Promise<void> {}
    close(): void {}
  }
  class RestoreSocket {
    static readonly OPEN = 1;
    readonly readyState = 1;
    readonly bufferedAmount = 0;
    binaryType = 'arraybuffer';
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor(_url: string) {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(frame: unknown): void {
      this.onmessage?.({ data: JSON.stringify(frame) });
    }
    send(text: string): void {
      const frame = JSON.parse(text) as { type: string; data?: Record<string, unknown> };
      if (frame.type === 'Authenticate') {
        queueMicrotask(() => {
          this.deliver({ type: 'Authenticated', data: {} });
          this.deliver({ type: 'ProtocolInfo', data: { protocol_version: 3 } });
        });
      } else if (frame.type === 'JoinRoom') {
        queueMicrotask(() => {
          this.deliver({
            type: 'RoomJoined',
            data: {
              player_id: me,
              room_id: me,
              room_code: 'TEST',
              lobby_state: 'finalized',
              ready_players: [],
              current_players: [
                { id: me, epoch: 1, seq: 0 },
                { id: peer, epoch: 1, seq: 0 },
              ],
            },
          });
          this.deliver(plan(1));
        });
      } else if (frame.type === 'Signal') {
        const connection = connections[connections.length - 1];
        queueMicrotask(() => {
          for (const channel of connection?.channels ?? []) {
            channel.onopen?.();
          }
        });
      } else if (frame.type === 'TransportStatus' && frame.data?.['connected'] === true) {
        queueMicrotask(() =>
          this.deliver({
            type: 'PeerTransportStatus',
            data: {
              peer_id: peer,
              transport: 'webrtc',
              connected: true,
            },
          }),
        );
      }
    }
    close(): void {}
  }
  const config: RunConfig = {
    serverUrl: 'ws://mock.invalid/v3/ws',
    createRoom: true,
    joinCode: null,
    peers: 2,
    maxPlayers: null,
    expectTotalPeers: null,
    leaveOnGameStart: false,
    gameName: 'peer-restore',
    playerName: 'test',
    appId: 'test',
    platform: 'test',
    exchange: true,
    relayPayload: null,
    crippleIce: false,
    p2pTimeoutSecs: 1,
    p2pRetryCount: 2,
    runForSecs: 2,
    successReleaseEnabled: true,
    protocolVersion: 3,
    supportedTopologies: ['mesh'],
    supportedTransports: ['webrtc'],
    gameDataFormat: 'json',
    sdkVersion: 'test',
    elapsedBeforeStartMs: 0,
  };
  try {
    globalThis.WebSocket = RestoreSocket as unknown as typeof WebSocket;
    globalThis.RTCPeerConnection = PeerConnection as unknown as typeof RTCPeerConnection;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          const event = JSON.parse(line) as Record<string, unknown>;
          events.push(event);
          const receipts = events.filter((item) => item['event'] === 'channel_message');
          if (!restoreSent && receipts.length === invalidReceiptCount + 2) {
            restoreSent = true;
            queueMicrotask(() => {
              if (peerRestored) {
                if (departure) {
                  socket.deliver({
                    type: 'PlayerLeft',
                    data: { player_id: peer, epoch: 1, final_seq: 0 },
                  });
                }
                socket.deliver(
                  membershipEvent === 'PlayerReconnected'
                    ? { type: 'PlayerReconnected', data: { player_id: peer, epoch: 2 } }
                    : {
                        type: 'PlayerJoined',
                        data: { player: { id: peer, epoch: 2, seq: 0 } },
                      },
                );
              } else {
                // Duplicate live membership retains the completed exchange.
                socket.deliver({
                  type: 'PlayerJoined',
                  data: { player: { id: peer, epoch: 1, seq: 0 } },
                });
              }
              if (peerRestored) {
                for (const channel of connections[0]?.channels ?? []) channel.receive();
                for (const signal of [{ PairRetry: 1 }, { IceCandidate: 'old-candidate' }]) {
                  socket.deliver({
                    type: 'Signal',
                    data: { from: peer, generation: plan(1).data.generation, signal },
                  });
                }
                socket.deliver({
                  type: 'NewPeer',
                  data: { peer_id: peer, you_initiate: true },
                });
              }
              socket.deliver(plan(2));
            });
          }
        },
        __sf_success_released: async () => released,
      },
    });
    console.error = () => {};
    console.debug = () => {};
    performance.now = () => virtualNow;
    const running = run(config).then((code) => {
      settled = true;
      return code;
    });
    const history = (async () => {
      while (connections.length < 1 && !settled) {
        await turn();
      }
      await turn();
      for (const channel of connections[0]?.channels ?? []) {
        for (const text of [
          'not-json',
          'null',
          '[]',
          JSON.stringify({ from: me, channel: channel.label, seq: 0 }),
          JSON.stringify({ from: peer, channel: 'other', seq: 0 }),
          JSON.stringify({ from: peer, channel: channel.label, seq: 1 }),
          JSON.stringify({ from: peer, channel: channel.label, seq: '0' }),
          JSON.stringify({ from: peer, channel: channel.label, seq: false }),
          JSON.stringify({ from: peer, channel: channel.label }),
        ]) {
          invalidReceiptCount += 1;
          channel.receive(text);
        }
      }
      await turn();
      assert(
        !events.some((event) => event['event'] === 'success_criteria_met'),
        'invalid exchange payloads must not satisfy the real browser dispatcher',
      );
      assert(
        events.filter((event) => event['event'] === 'channel_message').length ===
          invalidReceiptCount,
        'invalid traffic must remain visible in channel_message events',
      );
      for (const channel of connections[0]?.channels ?? []) {
        channel.receive(
          `{ "seq": -0.0, "channel": "${channel.label}", "from": "${peer}", "extra": true }`,
        );
      }
      while (connections.length < 2 && !settled) {
        await turn();
      }
      await turn();
      assert(
        connections.length === 2,
        'the membership history must create replacement channels',
      );
      assert(
        channelSends.join(',') ===
          (peerRestored
            ? '1:reliable,1:unreliable,2:reliable,2:unreliable'
            : '1:reliable,1:unreliable'),
        `restore=${peerRestored}: only a peer restore must resend both labels; got ${channelSends.join(',')}`,
      );
      assert(
        events.filter((event) => event['event'] === 'channel_message').length ===
          invalidReceiptCount + 2,
        `${membershipEvent}, departure=${departure}: old-link callbacks cannot become fresh receipts`,
      );
      // A host task turn drains the input chain. The virtual clock owns all
      // deadlines, so host scheduling cannot race receipts against the linger.
      released = true;
      virtualNow += 100;
      socket.deliver({ type: 'Pong' });
      await turn();
      virtualNow += 300;
      socket.deliver({ type: 'Pong' });
      await turn();
      assert(
        settled === !peerRestored,
        'a restored peer must wait for both fresh receipt labels',
      );
      if (peerRestored) {
        for (const channel of connections[1]?.channels ?? []) {
          channel.receive();
        }
        await turn();
        // The same epoch repeats after fresh traffic, with no new plan.
        socket.deliver(
          membershipEvent === 'PlayerReconnected'
            ? { type: 'PlayerReconnected', data: { player_id: peer, epoch: 2 } }
            : { type: 'PlayerJoined', data: { player: { id: peer, epoch: 2, seq: 0 } } },
        );
        await turn();
        virtualNow += 300;
        socket.deliver({ type: 'Pong' });
        await turn();
      }
      assert(
        settled,
        'duplicate same-epoch membership must keep fresh exchange evidence and plan state',
      );
      return running;
    })();
    const bounded = new Promise<never>((_resolve, reject) => {
      guardTimer = setTimeout(
        () => reject(new Error('peer incarnation history exceeded its bound')),
        5000,
      );
    });
    const code = await Promise.race([history, bounded]);
    assert(
      code === 0,
      `a restored peer must complete a fresh bidirectional exchange; code=${code}, events=${JSON.stringify(events)}`,
    );
    assert(
      events.filter((event) => event['event'] === 'channel_message').length ===
        invalidReceiptCount + (peerRestored ? 4 : 2),
      'a restored peer must receive both fresh labels before success',
    );
  } finally {
    clearTimeout(guardTimer);
    socket?.onclose?.();
    performance.now = originalNow;
    globalThis.WebSocket = originalSocket;
    globalThis.RTCPeerConnection = originalPc;
    console.error = originalError;
    console.debug = originalDebug;
    if (originalWindow === undefined) {
      Reflect.deleteProperty(globalThis, 'window');
    } else {
      Object.defineProperty(globalThis, 'window', originalWindow);
    }
  }
}
console.error('ok - restored peers require fresh browser exchange traffic');

// Regression #853: retain local transport state when initial pairing expires.
for (const healthy of [false, true]) {
  const originalSocket = globalThis.WebSocket;
  const originalPc = globalThis.RTCPeerConnection;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalError = console.error;
  const errors: string[] = [];
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  let now = Math.floor(originalNow.call(performance));
  let socket!: DiagnosticSocket;
  let connection!: DiagnosticConnection;
  let guardTimer: ReturnType<typeof setTimeout> | undefined;
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  class DiagnosticChannel {
    readonly readyState = healthy ? 'open' : 'connecting';
    readonly bufferedAmount = 17;
    onopen: (() => void) | null = null;
    constructor(readonly label: string) {}
    send(_text: string): void {}
  }
  class DiagnosticConnection {
    readonly connectionState = 'connected';
    readonly iceConnectionState = 'completed';
    readonly sctp = { state: 'connected' };
    constructor() {
      connection = this;
    }
    createDataChannel(label: string): DiagnosticChannel {
      return new DiagnosticChannel(label);
    }
    async createOffer(): Promise<{ type: string; sdp: string }> {
      return { type: 'offer', sdp: 'private-sdp' };
    }
    async setLocalDescription(_description: unknown): Promise<void> {}
    close(): void {}
  }
  class DiagnosticSocket {
    static readonly OPEN = 1;
    readonly readyState = 1;
    readonly bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(frame: unknown): void {
      this.onmessage?.({ data: JSON.stringify(frame) });
    }
    send(text: string): void {
      const frame = JSON.parse(text) as { type: string };
      if (frame.type === 'Authenticate') {
        queueMicrotask(() => {
          this.deliver({ type: 'Authenticated', data: {} });
          this.deliver({ type: 'ProtocolInfo', data: { protocol_version: 3 } });
        });
      } else if (frame.type === 'JoinRoom') {
        queueMicrotask(() => {
          this.deliver({
            type: 'RoomJoined',
            data: {
              player_id: me,
              room_id: me,
              room_code: 'TEST',
              lobby_state: 'finalized',
              ready_players: [],
              current_players: [
                { id: me, epoch: 1, seq: 0 },
                { id: peer, epoch: 1, seq: 0 },
              ],
            },
          });
          this.deliver({
            type: 'SessionPlan',
            data: {
              generation: me,
              topology: 'mesh',
              transport: 'webrtc',
              fallback: 'relay',
              ice_servers: [],
              peers: [{ player_id: peer, initiate: true }],
            },
          });
        });
      }
    }
    close(): void {}
  }
  try {
    globalThis.WebSocket = DiagnosticSocket as unknown as typeof WebSocket;
    globalThis.RTCPeerConnection = DiagnosticConnection as unknown as typeof RTCPeerConnection;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { __sf_emit: () => {} },
    });
    performance.now = () => now;
    console.error = (...args: unknown[]) => errors.push(args.map(String).join(' '));
    const running = run({
      serverUrl: 'ws://mock.invalid/v3/ws',
      createRoom: true,
      joinCode: null,
      peers: 2,
      maxPlayers: null,
      expectTotalPeers: null,
      leaveOnGameStart: false,
      gameName: 'diagnostic',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: true,
      relayPayload: null,
      crippleIce: false,
      p2pTimeoutSecs: 1,
      p2pRetryCount: 0,
      runForSecs: 3,
      successReleaseEnabled: false,
      protocolVersion: 3,
      supportedTopologies: ['mesh'],
      supportedTransports: ['webrtc'],
      gameDataFormat: 'json',
      sdkVersion: 'test',
      elapsedBeforeStartMs: 0,
    });
    await turn();
    assert(connection !== undefined, 'the actual dispatcher must establish a peer');
    now += 1100;
    socket.deliver({ type: 'Pong' });
    await turn();
    const diagnostics = errors.filter((line) => line.startsWith('P2P timeout for '));
    now += 3000;
    socket.deliver({ type: 'Pong' });
    await Promise.race([
      running,
      new Promise<never>((_resolve, reject) => {
        guardTimer = setTimeout(
          () => reject(new Error('P2P diagnostic history exceeded its bound')),
          2000,
        );
      }),
    ]);
    assert(
      diagnostics.length === (healthy ? 0 : 1),
      `healthy=${healthy}: only unresolved pairs need timeout state; got ${errors.join('; ')}`,
    );
    if (!healthy) {
      const line = diagnostics[0] ?? '';
      assert(
        line.startsWith(`P2P timeout for ${peer}: `),
        'timeout state must identify the peer',
      );
      const snapshot = JSON.parse(line.slice(line.indexOf(': ') + 2));
      assert(
        snapshot.generation === 1 &&
          snapshot.connectionState === 'connected' &&
          snapshot.iceConnectionState === 'completed' &&
          snapshot.sctpState === 'connected',
        'timeout must retain actual peer and SCTP states',
      );
      for (const label of ['reliable', 'unreliable']) {
        assert(
          snapshot.channels[label].readyState === 'connecting' &&
            snapshot.channels[label].bufferedAmount === 17 &&
            snapshot.channels[label].openObserved === false,
          `timeout must retain ${label} state`,
        );
      }
      assert(
        !line.includes('private-sdp'),
        'timeout snapshots must exclude signaling payloads',
      );
    }
  } finally {
    clearTimeout(guardTimer);
    socket?.onclose?.();
    performance.now = originalNow;
    globalThis.WebSocket = originalSocket;
    globalThis.RTCPeerConnection = originalPc;
    console.error = originalError;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - initial P2P timeout reports unresolved local states only');

// Responder control for #853: observe either remote channel open order.
for (const alreadyOpen of [false, true]) {
  const originalPc = globalThis.RTCPeerConnection;
  const connections: ResponderConnection[] = [];
  const opens: string[] = [];
  const messages: string[] = [];
  let connected = 0;
  class RemoteChannel {
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    readonly maxPacketLifeTime = null;
    readyState = alreadyOpen ? 'open' : 'connecting';
    bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(readonly label: string) {
      this.ordered = label !== UNRELIABLE_LABEL;
      this.maxRetransmits = label === UNRELIABLE_LABEL ? 0 : null;
    }
  }
  class ResponderConnection {
    ondatachannel: ((event: { channel: RemoteChannel }) => void) | null = null;
    constructor() {
      connections.push(this);
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = ResponderConnection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelClosed: () => {},
      onChannelOpen: (peer, generation, label) => {
        assert(
          engine.isCurrentGeneration(peer, generation),
          'open callback must belong to the current generation',
        );
        assert(
          engine.channel(peer, label) !== undefined,
          'remote channel must be stored before its open callback',
        );
        opens.push(label);
        if (engine.noteChannelOpen(peer, label)) connected += 1;
      },
      onChannelMessage: (peer, generation, label, text) => {
        assert(
          engine.isCurrentGeneration(peer, generation),
          'message callback must belong to the current generation',
        );
        messages.push(`${label}:${text}`);
      },
    });
    await engine.pairWith('peer', false, []);
    const channels = [RELIABLE_LABEL, UNRELIABLE_LABEL].map(
      (label) => new RemoteChannel(label),
    );
    for (const channel of channels) {
      connections[0]?.ondatachannel?.({ channel });
      channel.onmessage?.({ data: 'initial-message' });
      if (!alreadyOpen) {
        channel.readyState = 'open';
        channel.onopen?.();
      }
    }
    await Promise.resolve();
    for (const channel of channels) channel.onopen?.();
    assert(
      opens.join(',') === 'reliable,unreliable' && connected === 1,
      `alreadyOpen=${alreadyOpen}: observe each open and pair exactly once`,
    );
    assert(
      messages.join(',') === 'reliable:initial-message,unreliable:initial-message',
      'initial messages must survive both responder open orders',
    );
    engine.removePeer('peer');
    await engine.pairWith('peer', false, []);
    for (const channel of channels) {
      channel.onopen?.();
      channel.onmessage?.({ data: 'stale-message' });
    }
    connections[0]?.ondatachannel?.({ channel: new RemoteChannel(RELIABLE_LABEL) });
    assert(
      messages.length === 2 && opens.length === 2 && connected === 1,
      'retired responder callbacks must not change replacement state',
    );
    assert(
      engine.channel('peer', RELIABLE_LABEL) === undefined,
      'retired announcements must not populate the new generation',
    );
    // Retire before either already-open channel's queued notification runs.
    // These fresh handlers have not consumed their one-shot open notification.
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      const pendingChannel = new RemoteChannel(label);
      pendingChannel.readyState = 'open';
      connections[1]?.ondatachannel?.({ channel: pendingChannel });
    }
    engine.removePeer('peer');
    await engine.pairWith('peer', false, []);
    await Promise.resolve();
    assert(
      opens.length === 2 && connected === 1,
      'queued open notifications from a retired link must not create open or pair evidence',
    );
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      assert(
        engine.channel('peer', label) === undefined,
        'queued retired opens must not populate replacement channels',
      );
    }
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error(
  'ok - responder channel announcements retain initial messages and suppress duplicate or stale opens',
);

// Regression #869: a duplicate label must not replace a live protocol channel.
for (const [initiate, alreadyOpen] of [
  [false, false],
  [false, true],
  [true, false],
  [true, true],
] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  let connection: DuplicateConnection | undefined;
  const observed: string[] = [];
  class DuplicateChannel {
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    readonly maxPacketLifeTime = null;
    readyState = alreadyOpen ? 'open' : 'connecting';
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(readonly label: string) {
      this.ordered = label !== UNRELIABLE_LABEL;
      this.maxRetransmits = label === UNRELIABLE_LABEL ? 0 : null;
    }
    close(): void {
      this.readyState = 'closed';
      this.onclose?.();
    }
  }
  class DuplicateConnection {
    ondatachannel: ((event: { channel: DuplicateChannel }) => void) | null = null;
    constructor() {
      connection = this;
    }
    createDataChannel(label: string): DuplicateChannel {
      return new DuplicateChannel(label);
    }
    async createOffer(): Promise<{ sdp: string }> {
      return { sdp: 'offer' };
    }
    async setLocalDescription(): Promise<void> {}
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = DuplicateConnection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: (_peer, _generation, label) => {
        observed.push(`open:${label}`);
        engine.noteChannelOpen('peer', label);
      },
      onChannelClosed: (_peer, _generation, label) => observed.push(`close:${label}`),
      onChannelMessage: (_peer, _generation, label, text) => {
        observed.push(`${label}:${text}`);
      },
    });
    await engine.pairWith('peer', initiate, []);
    // Regression #870: unsupported labels cannot consume channel slots or callbacks.
    for (const phase of ['before-required', 'after-required']) {
      if (phase === 'after-required') {
        for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
          if (engine.channel('peer', label) === undefined) {
            connection?.ondatachannel?.({ channel: new DuplicateChannel(label) });
          }
        }
      }
      const before = observed.length;
      const labels = [
        '',
        'Reliable',
        'reliable\0',
        '界'.repeat(4096),
        ...Array.from({ length: 128 }, (_, index) => `optional-${index}`),
      ];
      for (const label of labels) {
        const unsupported = new DuplicateChannel(label);
        connection?.ondatachannel?.({ channel: unsupported });
        assert(unsupported.readyState === 'closed', `${phase}: unsupported channel must close`);
        assert(
          engine.channel('peer', label) === undefined,
          'unsupported label must not be retained',
        );
        assert(
          unsupported.onopen === null &&
            unsupported.onmessage === null &&
            unsupported.onclose === null &&
            unsupported.onerror === null,
          'unsupported channel must have no engine callbacks',
        );
      }
      await Promise.resolve();
      // Required channels can notify opens in the second phase.
      assert(
        observed
          .slice(before)
          .every((event) => event === 'open:reliable' || event === 'open:unreliable'),
        'unsupported channels must not publish events',
      );
    }
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      if (engine.channel('peer', label) === undefined) {
        connection?.ondatachannel?.({ channel: new DuplicateChannel(label) });
      }
    }
    await Promise.resolve();
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      const first = engine.channel('peer', label) as unknown as DuplicateChannel;
      assert(first !== undefined, 'first channel must exist');
      connection?.ondatachannel?.({ channel: first });
      assert(first.readyState !== 'closed', 'repeat announcement must keep the original alive');
      const duplicate = new DuplicateChannel(label);
      connection?.ondatachannel?.({ channel: duplicate });
      assert(
        engine.channel('peer', label) === first,
        `initiate=${initiate}: preserve first ${label}`,
      );
      assert(duplicate.readyState === 'closed', 'duplicate channel must close');
      const before = observed.length;
      duplicate.onopen?.();
      duplicate.onmessage?.({ data: 'duplicate' });
      duplicate.onclose?.();
      duplicate.onerror?.();
      await Promise.resolve();
      assert(observed.length === before, 'duplicate events must not reach the orchestrator');
      first.onmessage?.({ data: 'original' });
      first.readyState = 'open';
      first.onopen?.();
      assert(observed.includes(`${label}:original`), 'original messages must remain visible');
    }
    const required = engine.channel('peer', RELIABLE_LABEL) as unknown as DuplicateChannel;
    required?.onclose?.();
    assert(observed.at(-1) === 'close:reliable', 'original required close must remain visible');
    required.readyState = 'closed';
    const afterClose = new DuplicateChannel(RELIABLE_LABEL);
    connection?.ondatachannel?.({ channel: afterClose });
    assert(afterClose.readyState === 'closed', 'closed original keeps its label reserved');
    assert(
      engine.channel('peer', RELIABLE_LABEL) === required,
      'closed original stays selected',
    );
    engine.removePeer('peer');
    await engine.pairWith('peer', false, []);
    const replacement = new DuplicateChannel(RELIABLE_LABEL);
    connection?.ondatachannel?.({ channel: replacement });
    assert(
      engine.channel('peer', RELIABLE_LABEL) === replacement,
      'fresh link accepts the same label',
    );
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error(
  'ok - duplicate remote labels preserve original initiator and responder channels',
);

// Regression #861: bound pre-description storage and post-description admission.
{
  const originalPc = globalThis.RTCPeerConnection;
  const connections: CandidateConnection[] = [];
  class CandidateConnection {
    readonly applied: RTCIceCandidateInit[] = [];
    constructor() {
      connections.push(this);
    }
    async setRemoteDescription(_description: unknown): Promise<void> {}
    async addIceCandidate(candidate: RTCIceCandidateInit): Promise<void> {
      this.applied.push(candidate);
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = CandidateConnection as unknown as typeof RTCPeerConnection;
    const jsonMetadataBase = JSON.stringify({ candidate: '', usernameFragment: '' });
    const jsonMetadata = JSON.stringify({
      candidate: '',
      usernameFragment: 'x'.repeat(4096 - jsonMetadataBase.length),
    });
    for (const [name, payload, accepted] of [
      ['count', 'candidate:1 1 udp 2130706431 127.0.0.1 9 typ host', 128],
      ['bytes', 'x'.repeat(4096), 16],
      ['utf8', 'é'.repeat(2048), 16],
      ['json-metadata', jsonMetadata, 16],
    ] as const) {
      const engine = new Engine(false, {
        onLocalCandidate: () => {},
        onPcState: () => {},
        onChannelOpen: () => {},
        onChannelClosed: () => {},
        onChannelMessage: () => {},
      });
      await engine.pairWith('peer', false, []);
      const connection = connections[connections.length - 1]!;
      async function rejected(payload: string): Promise<void> {
        let failed = false;
        try {
          await engine.handleRemoteCandidate('peer', payload);
        } catch {
          failed = true;
        }
        assert(failed, `${name}: excess candidate must fail`);
      }
      await rejected('x'.repeat(4097));
      await rejected('é'.repeat(2049));
      for (let index = 0; index < accepted; index += 1) {
        await engine.handleRemoteCandidate('peer', payload);
      }
      for (let index = 0; index < 3; index += 1) await rejected(payload);
      assert(connection.applied.length === 0, `${name}: defer until remote description`);
      await engine.handleAnswer('peer', 'test-answer');
      assert(connection.applied.length === accepted, `${name}: flush only accepted candidates`);
      let expectedCandidate = payload;
      if (name === 'json-metadata') expectedCandidate = '';
      assert(
        connection.applied.every((candidate) => candidate.candidate === expectedCandidate),
        `${name}: preserve payloads`,
      );
      await rejected(payload);
      assert(
        connection.applied.length === accepted,
        `${name}: drain must not renew the budget`,
      );
      await engine.pairWith('other', false, []);
      await engine.handleRemoteCandidate('other', payload);
      await engine.handleAnswer('other', 'other-answer');
      assert(
        connections[connections.length - 1]!.applied.length === 1,
        `${name}: peers have independent budgets`,
      );
      engine.removePeer('other');
      engine.removePeer('peer');
      await engine.pairWith('peer', false, []);
      await engine.handleRemoteCandidate('peer', payload);
      await engine.handleAnswer('peer', 'replacement-answer');
      assert(
        connections[connections.length - 1]!.applied.length === 1,
        `${name}: replacement gets fresh budget`,
      );
      engine.removePeer('peer');
    }
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    await engine.pairWith('json-peer', false, []);
    const connection = connections[connections.length - 1]!;
    const candidate = {
      candidate: 'candidate:1 1 udp 2130706431 127.0.0.1 9 typ host',
      sdpMid: '0',
      sdpMLineIndex: 0,
      usernameFragment: null,
    };
    const payload = JSON.stringify(candidate);
    for (let index = 0; index < 127; index += 1)
      await engine.handleRemoteCandidate('json-peer', payload);
    await engine.handleAnswer('json-peer', 'json-answer');
    await engine.handleRemoteCandidate('json-peer', payload);
    assert(
      connection.applied.length === 128,
      'JSON candidates apply before and after description',
    );
    assert(
      connection.applied.every((value) => JSON.stringify(value) === payload),
      'JSON candidate metadata survives queue drain',
    );
    let rejected = false;
    try {
      await engine.handleRemoteCandidate('json-peer', payload);
    } catch {
      rejected = true;
    }
    assert(
      rejected && connection.applied.length === 128,
      'JSON admission remains bounded after queue drain',
    );
    engine.removePeer('json-peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error(
  'ok - remote ICE candidate admission bounds storage and preserves accepted signaling',
);

// A rejected buffered candidate must not stop SDP negotiation or discard later input.
{
  const originalPc = globalThis.RTCPeerConnection;
  const connections: RecoveringCandidateConnection[] = [];
  class RecoveringCandidateConnection {
    readonly attempted: string[] = [];
    localDescription: unknown = null;
    constructor() {
      connections.push(this);
    }
    async setRemoteDescription(_description: unknown): Promise<void> {}
    async addIceCandidate(candidate: RTCIceCandidateInit): Promise<void> {
      this.attempted.push(candidate.candidate ?? '');
      if (candidate.candidate === 'rejected-secret-candidate') {
        throw new Error('stack rejected-secret-candidate');
      }
    }
    async createAnswer(): Promise<RTCSessionDescriptionInit> {
      return { type: 'answer', sdp: 'healthy-answer' };
    }
    async setLocalDescription(description: unknown): Promise<void> {
      this.localDescription = description;
    }
    close(): void {}
  }
  const originalError = console.error;
  const diagnostics: string[] = [];
  try {
    globalThis.RTCPeerConnection =
      RecoveringCandidateConnection as unknown as typeof RTCPeerConnection;
    console.error = (...args: unknown[]) => {
      diagnostics.push(args.join(' '));
    };
    for (const path of ['offer', 'answer'] as const) {
      for (const candidates of [
        [],
        ['healthy-first', 'healthy-last'],
        ['rejected-secret-candidate', 'healthy-last'],
        ['healthy-first', 'rejected-secret-candidate', 'healthy-last'],
        ['rejected-secret-candidate', 'rejected-secret-candidate', 'healthy-last'],
      ]) {
        const engine = new Engine(false, {
          onLocalCandidate: () => {},
          onPcState: () => {},
          onChannelOpen: () => {},
          onChannelClosed: () => {},
          onChannelMessage: () => {},
        });
        await engine.pairWith('peer', false, []);
        const connection = connections[connections.length - 1]!;
        for (const candidate of candidates)
          await engine.handleRemoteCandidate('peer', candidate);
        if (path === 'offer') {
          const answer = await engine.handleOffer('peer', 'offer');
          assert(
            answer === 'healthy-answer' && connection.localDescription !== null,
            'a rejected candidate must not prevent the answer',
          );
        } else {
          await engine.handleAnswer('peer', 'answer');
        }
        assert(
          connection.attempted.join(',') === candidates.join(','),
          `${path}: attempt each buffered candidate once and preserve order`,
        );
        await engine.handleRemoteCandidate('peer', 'healthy-after-description');
        assert(
          connection.attempted.at(-1) === 'healthy-after-description',
          'healthy candidates must still apply after description',
        );
        const attempts = connection.attempted.length;
        let rejected = false;
        try {
          await engine.handleRemoteCandidate('peer', 'rejected-secret-candidate');
        } catch {
          rejected = true;
        }
        assert(rejected, 'direct stack rejection must still reach the caller');
        await engine.handleAnswer('peer', 'repeat-answer');
        assert(
          connection.attempted.length === attempts + 1,
          'a repeated description must not reapply buffered candidates',
        );
        engine.removePeer('peer');
      }
    }
    assert(diagnostics.length === 8, 'report each buffered rejection once');
    assert(
      diagnostics.every((message) => !message.includes('secret')),
      'buffered rejection diagnostics must omit candidate and stack error contents',
    );
  } finally {
    globalThis.RTCPeerConnection = originalPc;
    console.error = originalError;
  }
}
console.error('ok - buffered ICE rejection preserves SDP negotiation and later candidates');

// Regression #870: remote message bursts must not retain unbounded promise work.
for (const scenario of [
  { name: 'count', data: '', count: 257, accepted: 256 },
  { name: 'failed output', data: '', count: 2, accepted: 1, drains: 1, outputFailure: true },
  { name: 'pending output count', data: '', count: 129, accepted: 256, drains: 2, held: true },
  {
    name: 'pending output bytes',
    data: 'a'.repeat(65536),
    count: 9,
    accepted: 16,
    drains: 2,
    held: true,
  },
  { name: 'global peers', data: '', count: 257, accepted: 256, links: 2 },
  { name: 'bytes', data: 'a'.repeat(65536), count: 17, accepted: 16 },
  { name: 'text size', data: 'a'.repeat(65537), count: 2, accepted: 0 },
  { name: 'Unicode size', data: '€'.repeat(21846), count: 2, accepted: 0 },
  { name: 'binary size', data: new ArrayBuffer(65537), count: 2, accepted: 0 },
  {
    name: 'lossy binary size',
    data: new Uint8Array(21846).fill(255).buffer,
    count: 2,
    accepted: 0,
  },
  { name: 'drained credits', data: '', count: 256, accepted: 768, drains: 3 },
  {
    name: 'retired generation credits',
    data: '',
    count: 256,
    accepted: 256,
    drains: 2,
    replace: true,
  },
  {
    name: 'binary boundary',
    data: new Uint8Array(65536).fill(97).buffer,
    count: 1,
    accepted: 1,
    drains: 1,
  },
]) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalError = console.error;
  const events: Record<string, unknown>[] = [];
  const connections: BoundedConnection[] = [];
  let socket: BoundedSocket | undefined;
  let running: Promise<number> | undefined;
  let releaseOutput: (() => void) | undefined;
  let outputHeld = scenario.held === true;
  const outputAck = new Promise<void>((resolve) => {
    releaseOutput = resolve;
  });
  const peer = '00000000-0000-0000-0000-000000000002';
  const me = '00000000-0000-0000-0000-000000000001';
  const peerIds = [peer, '00000000-0000-0000-0000-000000000003'].slice(0, scenario.links ?? 1);
  const turn = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
  class BoundedChannel {
    readyState = 'open';
    bufferedAmount = 0;
    binaryType = 'blob';
    onmessage: ((event: { data: unknown }) => void) | null = null;
    onclose: (() => void) | null = null;
    constructor(readonly label: string) {}
    close(): void {
      this.readyState = 'closed';
      this.onclose?.();
    }
    send(): void {}
  }
  class BoundedConnection {
    readonly channels: BoundedChannel[] = [];
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string): BoundedChannel {
      const channel = new BoundedChannel(label);
      this.channels.push(channel);
      return channel;
    }
    async createOffer(): Promise<{ type: 'offer'; sdp: string }> {
      return { type: 'offer', sdp: 'test' };
    }
    async setLocalDescription(): Promise<void> {}
    close(): void {
      for (const channel of this.channels) channel.close();
    }
  }
  class BoundedSocket {
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
    deliver(frame: unknown): void {
      this.onmessage?.({ data: JSON.stringify(frame) });
    }
    send(text: string): void {
      const frame = JSON.parse(text) as { type: string };
      if (frame.type === 'Authenticate')
        queueMicrotask(() => {
          this.deliver({ type: 'Authenticated', data: {} });
          this.deliver({ type: 'ProtocolInfo', data: { protocol_version: 3 } });
        });
      if (frame.type === 'JoinRoom')
        queueMicrotask(() => {
          this.deliver({
            type: 'RoomJoined',
            data: {
              player_id: me,
              room_id: me,
              room_code: 'TEST',
              lobby_state: 'finalized',
              ready_players: [],
              current_players: [
                { id: me, epoch: 1, seq: 0 },
                ...peerIds.map((id) => ({ id, epoch: 1, seq: 0 })),
              ],
            },
          });
          this.deliver({
            type: 'SessionPlan',
            data: {
              generation: me,
              topology: 'mesh',
              transport: 'webrtc',
              fallback: 'relay',
              ice_servers: [],
              peers: peerIds.map((player_id) => ({ player_id, initiate: true })),
            },
          });
        });
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = BoundedConnection as unknown as typeof RTCPeerConnection;
    globalThis.WebSocket = BoundedSocket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          const event = JSON.parse(line) as Record<string, unknown>;
          events.push(event);
          if (outputHeld && event['event'] === 'channel_message') return outputAck;
          if (scenario.outputFailure && event['event'] === 'channel_message')
            return Promise.reject(new Error('consumer closed'));
        },
      },
    });
    console.error = () => {};
    running = run({
      serverUrl: 'ws://mock.invalid/v3/ws',
      createRoom: true,
      joinCode: null,
      peers: peerIds.length + 1,
      maxPlayers: null,
      expectTotalPeers: null,
      leaveOnGameStart: false,
      gameName: 'bounded',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: scenario.outputFailure === true,
      relayPayload: null,
      crippleIce: false,
      p2pTimeoutSecs: 10,
      p2pRetryCount: 0,
      runForSecs: 120,
      successReleaseEnabled: false,
      protocolVersion: 3,
      supportedTopologies: ['mesh'],
      supportedTransports: ['webrtc'],
      gameDataFormat: 'json',
      sdkVersion: 'test',
      elapsedBeforeStartMs: 0,
    });
    await turn();
    let channels = connections.flatMap((connection) => connection.channels);
    assert(
      channels.length === peerIds.length * 2,
      `${scenario.name}: the dispatcher creates both channels`,
    );
    for (let drain = 0; drain < (scenario.drains ?? 1); drain += 1) {
      if (scenario.replace && drain === 0)
        socket?.deliver({
          type: 'SessionPlan',
          data: {
            generation: peer,
            topology: 'mesh',
            transport: 'webrtc',
            fallback: 'relay',
            ice_servers: [],
            peers: [{ player_id: peer, initiate: true }],
          },
        });
      for (let index = 0; index < scenario.count; index += 1) {
        const channel = channels[index % channels.length];
        channel?.onmessage?.({
          data: scenario.outputFailure
            ? JSON.stringify({ from: peer, channel: channel.label, seq: 0 })
            : scenario.data,
        });
      }
      await turn();
      if (scenario.replace) channels = connections[connections.length - 1]?.channels ?? [];
    }
    if (scenario.held) {
      assert(
        events.filter((event) => event['event'] === 'channel_message').length === 1,
        `${scenario.name}: only one message may enter an unacknowledged output bridge`,
      );
      assert(
        channels.filter((channel) => channel.readyState === 'closed').length === 2,
        `${scenario.name}: pending output must remain charged across separate browser tasks`,
      );
      outputHeld = false;
      releaseOutput?.();
      await turn();
    }
    const messages = events.filter((event) => event['event'] === 'channel_message');
    assert(
      messages.length === scenario.accepted,
      `${scenario.name}: expected ${scenario.accepted} admitted messages, got ${messages.length}`,
    );
    const rejected = scenario.drains === undefined || scenario.held === true;
    assert(
      events.filter((event) => event['event'] === 'channel_closed').length ===
        (rejected ? 1 : 0),
      `${scenario.name}: overload must produce one terminal event despite both channel close callbacks`,
    );
    assert(
      events.filter((event) => event['event'] === 'error').length ===
        (rejected || scenario.outputFailure ? 1 : 0),
      `${scenario.name}: overload must produce one explicit error`,
    );
    assert(
      channels?.every((channel) => channel.binaryType === 'arraybuffer') === true,
      `${scenario.name}: binary messages must be delivered as bounded array buffers`,
    );
    if (scenario.outputFailure) {
      assert(
        (await running) === 3,
        'critical output failure must stop the run with a connection error',
      );
      assert(
        events.some(
          (event) =>
            event['event'] === 'error' &&
            String(event['message']).includes('stdout event delivery failed'),
        ),
        'critical output failure must name the failed delivery',
      );
      assert(
        !events.some((event) => event['event'] === 'success_criteria_met'),
        'an exchange receipt with failed output cannot produce success',
      );
    }
    if (rejected) {
      if (peerIds.length === 1) {
        assert(
          events.some(
            (event) =>
              event['event'] === 'transport_status_sent' && event['connected'] === true,
          ),
          `${scenario.name}: overload must start from a connected pair`,
        );
        assert(
          events.filter(
            (event) =>
              event['event'] === 'transport_status_sent' && event['connected'] === false,
          ).length === 1,
          `${scenario.name}: overload must report disconnected transport once`,
        );
        assert(
          events.filter((event) => event['event'] === 'fallback_engaged').length === 1,
          `${scenario.name}: overload must engage relay fallback once`,
        );
      }
      assert(
        channels.filter((channel) => channel.readyState === 'closed').length === 2,
        `${scenario.name}: close both required channels`,
      );
      channels
        .find((channel) => channel.readyState === 'closed')
        ?.onmessage?.({ data: 'late' });
      await turn();
      assert(
        events.filter((event) => event['event'] === 'channel_message').length ===
          scenario.accepted,
        `${scenario.name}: failed-link callbacks must stay fenced`,
      );
      if (scenario.links === 2) {
        channels
          .find((channel) => channel.readyState === 'open')
          ?.onmessage?.({ data: 'healthy' });
        await turn();
        assert(
          events.filter((event) => event['event'] === 'channel_message').length ===
            scenario.accepted + 1,
          'global overload must preserve healthy peer traffic after the backlog drains',
        );
      }
    }
  } finally {
    outputHeld = false;
    releaseOutput?.();
    socket?.onclose?.();
    await running;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    console.error = originalError;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error('ok - incoming channel messages have bounded frames and pending count/bytes');
