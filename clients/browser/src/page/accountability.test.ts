import { DELIVERY_REPORT_MAX_GAPS, DeliveryAccountability } from './accountability.js';
import { encode } from '@msgpack/msgpack';
import { Engine, RELIABLE_LABEL } from './engine.js';
import {
  applyJoinAccountabilityBaseline,
  authoritativePeerDelta,
  changedTransportStatus,
  clearDepartedMembershipPlan,
  isTerminalPeerConnectionState,
  observeJoinHandshakeFrame,
  requireFinalizedMembershipPlan,
  requiresAuthoritativeFinalizationPlan,
  restoreReconnectedMember,
  shouldBufferSignalForUnpairedPeer,
  shouldResolveConnectedPair,
} from './orchestrator.js';
import {
  NON_TEXT_APPLICATION_FRAME,
  classifyBrowserServerInput,
  classifyJsonNegotiatedServerInput,
  negotiatedProtocolVersion,
  sendGameData,
  sendGameDataWithDelivery,
  type ServerFrame,
} from './wire.js';

type Test = { name: string; run: () => void | Promise<void> };
const tests: Test[] = [];
const SENDER = '00000000-0000-0000-0000-00000000000a';
const BINARY_SENDER = '00112233-4455-6677-8899-aabbccddeeff';
const BINARY_SENDER_BYTES = Uint8Array.from([
  0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
  0xff,
]);

function test(name: string, run: () => void | Promise<void>): void {
  tests.push({ name, run });
}

function expectViolation(run: () => void, detail: string): void {
  try {
    run();
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (message.includes('delivery accountability violation:') && message.includes(detail)) {
      return;
    }
    throw new Error(`expected accountability violation containing ${detail}, got ${message}`);
  }
  throw new Error(`expected accountability violation containing ${detail}`);
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

function exactArrayBuffer(bytes: Uint8Array): ArrayBuffer {
  return Uint8Array.from(bytes).buffer;
}

function playerId(value: number): string {
  const hex = value.toString(16).padStart(32, '0');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(
    16,
    20,
  )}-${hex.slice(20)}`;
}

type MessagePackEntry = [Uint8Array, Uint8Array];

function encodedMessagePack(value: unknown): Uint8Array {
  return encode(value, { useBigInt64: true });
}

function concatenateBytes(...parts: readonly Uint8Array[]): Uint8Array {
  const result = new Uint8Array(parts.reduce((length, part) => length + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    result.set(part, offset);
    offset += part.length;
  }
  return result;
}

function encodeMessagePackMap(entries: readonly MessagePackEntry[]): Uint8Array {
  if (entries.length > 15) {
    throw new Error('test MessagePack map exceeds fixmap capacity');
  }
  return concatenateBytes(
    Uint8Array.of(0x80 | entries.length),
    ...entries.flatMap(([key, value]) => [key, value]),
  );
}

function binaryEnvelopeEntries(): MessagePackEntry[] {
  return [
    [encodedMessagePack('from_player'), encodedMessagePack(BINARY_SENDER_BYTES)],
    [encodedMessagePack('encoding'), encodedMessagePack('json')],
    [encodedMessagePack('payload'), encodedMessagePack(Uint8Array.of(1, 2, 3))],
    [encodedMessagePack('seq'), encodedMessagePack(1)],
    [encodedMessagePack('epoch'), encodedMessagePack(1)],
  ];
}

function encodedFloat64(value: number): Uint8Array {
  const wire = new Uint8Array(9);
  wire[0] = 0xcb;
  new DataView(wire.buffer).setFloat64(1, value);
  return wire;
}

function counters(seed: number): Record<string, unknown> {
  return {
    reliable: { delivered: seed, abandoned: 0, unsupported_format: 0 },
    latest: {
      delivered: seed,
      superseded: 0,
      dropped_full: 0,
      abandoned: 0,
      unsupported_format: 0,
    },
    volatile: {
      delivered: seed,
      dropped: 0,
      abandoned: 0,
      unsupported_format: 0,
    },
  };
}

function countersWithSuperseded(count: number): Record<string, unknown> {
  const value = counters(0);
  (value['latest'] as Record<string, unknown>)['superseded'] = count;
  return value;
}

function countersWithUnsupported(count: number): Record<string, unknown> {
  const value = counters(0);
  (value['reliable'] as Record<string, unknown>)['unsupported_format'] = count;
  return value;
}

function relayStats(
  intervalMs = 1_000,
  sentToYou = 0,
  droppedForYou = 0,
  backpressureEvents = 0,
): Record<string, unknown> {
  return {
    interval_ms: intervalMs,
    sent_to_you: sentToYou,
    dropped_for_you: droppedForYou,
    backpressure_events: backpressureEvents,
  };
}

function gap(fromSeq: number, toSeq: number): Record<string, unknown> {
  return {
    from_player: SENDER,
    epoch: 1,
    from_seq: fromSeq,
    to_seq: toSeq,
    reason: 'latest_superseded',
  };
}

function unsupportedGap(seq: number): Record<string, unknown> {
  return { ...gap(seq, seq), reason: 'unsupported_format' };
}

function joinedState(): DeliveryAccountability {
  const state = new DeliveryAccountability();
  state.notePlayerJoined({ id: SENDER, epoch: 1, seq: 0 });
  return state;
}

function dispatchClassifiedFrame(state: DeliveryAccountability, frame: ServerFrame): void {
  const isUnsupportedError =
    frame.type === 'Error' && frame.data['error_code'] === 'UNSUPPORTED_GAME_DATA_FORMAT';
  state.observeServerMessage(isUnsupportedError);
  if (frame.type === 'DeliveryReport') {
    state.recordReport(frame.data);
  }
}

test('only a causally prior exact range authorizes a sequence gap', () => {
  const valid = joinedState();
  valid.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 });
  valid.recordReport({ per_class: countersWithSuperseded(2), gaps: [gap(2, 3)] });
  valid.recordGameData({
    from_player: SENDER,
    epoch: 1,
    seq: 4,
    class: 'latest',
    key: 7,
  });

  for (const [name, gaps] of [
    ['missing', []],
    ['incomplete', [gap(2, 2)]],
    ['overreaching', [gap(2, 4)]],
  ] as const) {
    const state = joinedState();
    state.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 });
    if (gaps.length !== 0) {
      const count = gaps.reduce(
        (sum, item) => sum + Number(item['to_seq']) - Number(item['from_seq']) + 1,
        0,
      );
      state.recordReport({ per_class: countersWithSuperseded(count), gaps });
    }
    expectViolation(
      () => state.recordGameData({ from_player: SENDER, epoch: 1, seq: 4 }),
      name === 'missing' ? 'unexplained gap' : 'prior exact reports do not cover',
    );
  }
});

test('class/key combinations and cumulative counters are enforced', () => {
  const validCases: Array<[unknown, unknown]> = [
    [undefined, undefined],
    ['reliable', undefined],
    ['latest', 0],
    ['volatile', undefined],
  ];
  for (const [className, key] of validCases) {
    const state = joinedState();
    state.recordGameData({
      from_player: SENDER,
      epoch: 1,
      seq: 1,
      class: className,
      key,
    });
  }
  for (const [className, key] of [
    [undefined, 1],
    ['reliable', 1],
    ['latest', undefined],
    ['volatile', 1],
    ['unknown', undefined],
  ] as Array<[unknown, unknown]>) {
    const state = joinedState();
    expectViolation(
      () =>
        state.recordGameData({
          from_player: SENDER,
          epoch: 1,
          seq: 1,
          class: className,
          key,
        }),
      className === 'unknown' ? 'invalid delivery class' : 'invalid received class/key',
    );
  }

  const state = joinedState();
  state.recordReport({ per_class: counters(2) });
  expectViolation(
    () => state.recordReport({ per_class: counters(1) }),
    'counters moved backward',
  );
});

test('room snapshots, reconnect watermarks, and resets rebaseline cursors', () => {
  const state = new DeliveryAccountability();
  state.rebaselineSnapshot([{ id: SENDER, epoch: 4, seq: 89 }]);
  state.recordGameData({ from_player: SENDER, epoch: 4, seq: 90 });
  state.resetRoom();
  expectViolation(
    () => state.recordGameData({ from_player: SENDER, epoch: 4, seq: 91 }),
    'before a room/lifecycle baseline',
  );

  state.rebaselineReconnected(
    [{ id: SENDER, epoch: 4, seq: 100 }],
    [{ player_id: SENDER, epoch: 4, seq: 100 }],
  );
  state.recordGameData({ from_player: SENDER, epoch: 4, seq: 101 });
  expectViolation(
    () => state.recordGameData({ from_player: SENDER, epoch: 4, seq: 103 }),
    'unexplained gap',
  );

  state.recordReport({ per_class: counters(2) });
  state.rebaselineSnapshot([]);
  expectViolation(
    () => state.recordReport({ per_class: counters(1) }),
    'counters moved backward',
  );
});

test('Reconnected preserves same-socket delivery and RelayStats frontiers', () => {
  const state = joinedState();
  state.recordReport({ per_class: counters(2) });
  state.recordRelayStats(relayStats(1_000, 4, 2, 1));
  state.rebaselineReconnected(
    [{ id: SENDER, epoch: 1, seq: 9 }],
    [{ player_id: SENDER, epoch: 1, seq: 9 }],
  );

  expectViolation(
    () => state.recordReport({ per_class: counters(1) }),
    'counters moved backward',
  );
  expectViolation(
    () => state.recordRelayStats(relayStats(1_000, 3, 2, 1)),
    'counters moved backward',
  );
  state.recordReport({ per_class: counters(3) });
  state.recordRelayStats(relayStats(1_000, 5, 3, 2));
});

test('same-epoch lifecycle is idempotent only while sender is present', () => {
  for (const seq of [0, 7]) {
    const state = new DeliveryAccountability();
    state.rebaselineReconnected(
      [{ id: SENDER, epoch: 4, seq }],
      [{ player_id: SENDER, epoch: 4, seq }],
    );

    state.notePlayerJoined({ id: SENDER, epoch: 4, seq: 0 });
    state.notePlayerReconnected(SENDER, 4);

    state.notePlayerLeft(SENDER, 4, seq + 1);
    expectViolation(
      () => state.notePlayerJoined({ id: SENDER, epoch: 4, seq: 0 }),
      'is not newer',
    );
    expectViolation(() => state.notePlayerReconnected(SENDER, 4), 'is not newer');
  }
});

test('priority lifecycle control may overtake queued old-epoch data', () => {
  const state = joinedState();
  state.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 });
  state.notePlayerLeft(SENDER, 1, 2);
  state.notePlayerReconnected(SENDER, 2);
  state.recordReport({
    per_class: countersWithSuperseded(2),
    gaps: [
      {
        from_player: SENDER,
        epoch: 2,
        from_seq: 1,
        to_seq: 2,
        reason: 'latest_superseded',
      },
    ],
  });

  if (state.recordGameData({ from_player: SENDER, epoch: 1, seq: 2 }) !== 'stale') {
    throw new Error('old epoch data must be validated but discarded');
  }
  if (state.recordGameData({ from_player: SENDER, epoch: 2, seq: 3 }) !== 'apply') {
    throw new Error('announced new epoch data must be application-visible');
  }
  expectViolation(
    () => state.recordGameData({ from_player: SENDER, epoch: 1, seq: 3 }),
    'moved backward to epoch',
  );
  expectViolation(
    () => state.recordGameData({ from_player: SENDER, epoch: 99, seq: 1 }),
    'unannounced epoch',
  );
});

test('multiple overtaking PlayerLeft epochs retire independently', () => {
  const state = joinedState();
  for (const epoch of [1, 2, 3]) {
    if (epoch > 1) {
      state.notePlayerReconnected(SENDER, epoch);
    }
    state.notePlayerLeft(SENDER, epoch, 2);
  }

  // A newer epoch can be fully omitted before the oldest data tail drains.
  state.recordReport({
    per_class: countersWithSuperseded(2),
    gaps: [{ ...gap(1, 2), epoch: 2 }],
  });
  if (state.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 }) !== 'stale') {
    throw new Error('epoch 1 tail must remain stale after later terminal controls');
  }
  state.recordReport({
    per_class: countersWithSuperseded(3),
    gaps: [{ ...gap(2, 2), epoch: 1 }],
  });
  state.recordReport({
    per_class: countersWithSuperseded(4),
    gaps: [{ ...gap(1, 1), epoch: 3 }],
  });
  if (state.recordGameData({ from_player: SENDER, epoch: 3, seq: 2 }) !== 'stale') {
    throw new Error('latest departed epoch tail must remain stale');
  }
  expectViolation(
    () => state.recordGameData({ from_player: SENDER, epoch: 3, seq: 3 }),
    'before a room/lifecycle baseline',
  );
});

test('PlayerLeft terminal watermark retires delivered and exactly omitted tails', () => {
  const snapshotTail = new DeliveryAccountability();
  snapshotTail.rebaselineSnapshot([{ id: SENDER, epoch: 1, seq: 41 }]);
  snapshotTail.notePlayerLeft(SENDER, 1, 43);
  for (const seq of [42, 43]) {
    if (snapshotTail.recordGameData({ from_player: SENDER, epoch: 1, seq }) !== 'stale') {
      throw new Error(`post-snapshot terminal seq ${seq} must be validated but discarded`);
    }
  }
  expectViolation(
    () => snapshotTail.recordGameData({ from_player: SENDER, epoch: 1, seq: 44 }),
    'before a room/lifecycle baseline',
  );

  const deliveredTail = joinedState();
  deliveredTail.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 });
  deliveredTail.notePlayerLeft(SENDER, 1, 4);
  deliveredTail.recordReport({
    per_class: countersWithSuperseded(2),
    gaps: [gap(2, 3)],
  });
  if (deliveredTail.recordGameData({ from_player: SENDER, epoch: 1, seq: 4 }) !== 'stale') {
    throw new Error('the terminal trailing delivery must be discarded as stale');
  }
  expectViolation(
    () => deliveredTail.recordGameData({ from_player: SENDER, epoch: 1, seq: 5 }),
    'before a room/lifecycle baseline',
  );

  const omittedTail = joinedState();
  omittedTail.notePlayerLeft(SENDER, 1, 2);
  omittedTail.recordReport({
    per_class: countersWithSuperseded(2),
    gaps: [gap(1, 2)],
  });
  expectViolation(
    () => omittedTail.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 }),
    'before a room/lifecycle baseline',
  );

  const beyond = joinedState();
  beyond.notePlayerLeft(SENDER, 1, 2);
  expectViolation(
    () =>
      beyond.recordReport({
        per_class: countersWithSuperseded(3),
        gaps: [gap(1, 3)],
      }),
    'extends beyond PlayerLeft terminal',
  );
});

test('PlayerLeft terminal watermarks keep seat churn state bounded and preserve v2 shape', () => {
  const state = new DeliveryAccountability();
  for (let index = 1; index <= 1_024; index += 1) {
    const id = playerId(index);
    state.notePlayerJoined({ id, epoch: 1, seq: 0 });
    state.notePlayerLeft(id, 1, 0);
  }
  const internals = state as unknown as {
    senders: Map<string, unknown>;
    announcedEpochs: Map<string, unknown>;
    staleSenders: Set<string>;
    departedSenders: Map<string, unknown>;
    pendingGaps: Map<string, unknown>;
  };
  for (const [name, size] of [
    ['senders', internals.senders.size],
    ['announced epochs', internals.announcedEpochs.size],
    ['stale senders', internals.staleSenders.size],
    ['departed senders', internals.departedSenders.size],
    ['pending gaps', internals.pendingGaps.size],
  ] as const) {
    if (size !== 0) {
      throw new Error(`${name} retained ${size} entries after terminal seat churn`);
    }
  }

  const v2 = new DeliveryAccountability(false);
  v2.notePlayerLeft(SENDER);
  expectViolation(
    () => v2.notePlayerLeft(SENDER, 1, 0),
    'v2 PlayerLeft exposed terminal delivery watermark fields',
  );
});

test('PlayerReconnected restores lobby and active-session membership without guessing a pair', () => {
  for (const phase of ['lobby', 'active'] as const) {
    const self = playerId(99);
    const present = new Set([self, SENDER]);
    const membersSeen = new Set([self, SENDER]);
    const expectedPeers = new Set([SENDER]);
    const pendingSignals = new Map([[SENDER, [{}]]]);

    // PlayerLeft clears live membership and every old pairing obligation.
    present.delete(SENDER);
    expectedPeers.delete(SENDER);
    pendingSignals.delete(SENDER);
    restoreReconnectedMember(present, membersSeen, SENDER);

    if (!present.has(SENDER) || !membersSeen.has(SENDER)) {
      throw new Error(`${phase}: PlayerReconnected did not restore application membership`);
    }
    if (phase === 'lobby' && present.size < 2) {
      throw new Error('lobby: restored member did not satisfy the ready population barrier');
    }
    if (expectedPeers.has(SENDER) || pendingSignals.has(SENDER)) {
      throw new Error(`${phase}: reconnect guessed stale pairing state before a new directive`);
    }
    const freshPlan = authoritativePeerDelta(expectedPeers, [SENDER]);
    if (freshPlan.added.length !== 1 || freshPlan.added[0] !== SENDER) {
      throw new Error(
        `${phase}: fresh post-reconnect plan did not restore the pair obligation`,
      );
    }
  }

  if (changedTransportStatus(true, 0) !== false) {
    throw new Error('active: losing the last connected pair must report relay fallback');
  }
  if (changedTransportStatus(false, 0) !== null) {
    throw new Error('active: an unchanged fallback state must not be reported twice');
  }
  if (changedTransportStatus(false, 1) !== true) {
    throw new Error('active: the replacement pair must report WebRTC restored');
  }
  if (!shouldResolveConnectedPair(false, false)) {
    throw new Error('active: one late pair after fallback must re-evaluate transport status');
  }
  if (changedTransportStatus(true, 2) !== null) {
    throw new Error('active: a second late pair must not duplicate the restored status');
  }

  for (const scenario of ['solo finalization', 'incapable late join']) {
    if (changedTransportStatus(null, 0) !== false) {
      throw new Error(`${scenario}: a zero-peer WebRTC plan must resolve to fallback`);
    }
  }
});

test('authoritative SessionPlan replaces topology and supports an empty no-pair plan', () => {
  const retained = playerId(11);
  const added = playerId(12);
  const current = new Set([SENDER, retained]);
  const replacement = authoritativePeerDelta(current, [retained, added, retained]);
  if (
    replacement.removed.join() !== SENDER ||
    replacement.retained.join() !== retained ||
    replacement.added.join() !== added
  ) {
    throw new Error(`unexpected replacement delta ${JSON.stringify(replacement)}`);
  }

  const empty = authoritativePeerDelta(new Set([retained, added]), []);
  if (empty.removed.length !== 2 || empty.retained.length !== 0 || empty.added.length !== 0) {
    throw new Error(`empty plan did not remove every pair ${JSON.stringify(empty)}`);
  }
});

test('authoritative plan obligations are finalized-v3 and epoch scoped', () => {
  for (const [scenario, version, expected] of [
    ['v2 relay finalization', 2, false],
    ['v3 WebRTC finalization', 3, true],
    ['v3 relay finalization', 3, true],
  ] as const) {
    let planPending = requiresAuthoritativeFinalizationPlan(version);
    if (planPending !== expected) {
      throw new Error(`${scenario}: initial plan obligation was not ${expected}`);
    }
    const simulatedPlanDelayMs = 251;
    if (expected && (simulatedPlanDelayMs <= 250 || !planPending)) {
      throw new Error(`${scenario}: plan obligation did not outlive exit linger`);
    }
    planPending = false; // Either an explicit WebRTC or Relay/Relay plan clears it.
    if (planPending) {
      throw new Error(`${scenario}: authoritative plan did not clear its obligation`);
    }
  }

  const pending = new Map<string, number>();
  if (requireFinalizedMembershipPlan(pending, 3, 'lobby', SENDER, 1)) {
    throw new Error('lobby membership incorrectly required a SessionPlan');
  }
  if (requireFinalizedMembershipPlan(pending, 2, 'finalized', SENDER, 1)) {
    throw new Error('v2 membership incorrectly required a SessionPlan');
  }
  if (!requireFinalizedMembershipPlan(pending, 3, 'finalized', SENDER, 1)) {
    throw new Error('finalized v3 membership did not require a SessionPlan');
  }
  requireFinalizedMembershipPlan(pending, 3, 'finalized', SENDER, 2);
  clearDepartedMembershipPlan(pending, SENDER, 1);
  if (pending.get(SENDER) !== 2) {
    throw new Error('overtaken old PlayerLeft cleared a newer plan obligation');
  }
  clearDepartedMembershipPlan(pending, SENDER, 2);
  if (pending.size !== 0) {
    throw new Error('matching PlayerLeft did not clear its plan obligation');
  }
});

test('only failed and closed peer-connection states are terminal', () => {
  const cases: Array<[string, boolean]> = [
    ['new', false],
    ['connecting', false],
    ['connected', false],
    ['disconnected', false],
    ['failed', true],
    ['closed', true],
  ];
  for (const [state, expected] of cases) {
    if (isTerminalPeerConnectionState(state) !== expected) {
      throw new Error(`${state} terminal classification was not ${expected}`);
    }
  }
  if (changedTransportStatus(true, 0) !== false) {
    throw new Error('terminal loss of the final connected pair did not resolve fallback');
  }
  if (!shouldBufferSignalForUnpairedPeer(new Set(), SENDER)) {
    throw new Error('pre-plan signal was not buffered defensively');
  }
  if (shouldBufferSignalForUnpairedPeer(new Set([SENDER]), SENDER)) {
    throw new Error('stale post-terminal signal would contaminate a replacement link');
  }
});

test('removed browser peer links cannot emit callbacks after the same peer is re-paired', async () => {
  class FakeDataChannel {
    readonly label: string;
    readyState = 'connecting';
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: unknown }) => void) | null = null;

    constructor(label: string) {
      this.label = label;
    }
  }

  class FakePeerConnection {
    static readonly instances: FakePeerConnection[] = [];
    static failNextOffer = false;
    connectionState = 'new';
    onconnectionstatechange: (() => void) | null = null;
    onicecandidate:
      | ((event: { candidate: { toJSON(): Record<string, unknown> } | null }) => void)
      | null = null;
    ondatachannel: ((event: { channel: RTCDataChannel }) => void) | null = null;

    constructor() {
      FakePeerConnection.instances.push(this);
    }

    close(): void {}

    createDataChannel(label: string): RTCDataChannel {
      return new FakeDataChannel(label) as unknown as RTCDataChannel;
    }

    async createOffer(): Promise<{ type: 'offer'; sdp: string }> {
      if (FakePeerConnection.failNextOffer) {
        FakePeerConnection.failNextOffer = false;
        throw new Error('injected offer failure');
      }
      return { type: 'offer', sdp: 'offer' };
    }

    async setLocalDescription(): Promise<void> {}
  }

  const originalPeerConnection = Reflect.get(globalThis, 'RTCPeerConnection');
  Reflect.set(globalThis, 'RTCPeerConnection', FakePeerConnection);
  try {
    const observed: string[] = [];
    const queued: Array<() => void> = [];
    let engine: Engine;
    const queueIfCurrent = (peer: string, generation: number, event: string): void => {
      queued.push(() => {
        if (engine.isCurrentGeneration(peer, generation)) {
          observed.push(event);
        }
      });
    };
    engine = new Engine(false, {
      onLocalCandidate: (peer, generation, candidate) =>
        queueIfCurrent(peer, generation, `candidate:${candidate}`),
      onPcState: (peer, generation, state) => queueIfCurrent(peer, generation, `pc:${state}`),
      onChannelOpen: (peer, generation, label) =>
        queueIfCurrent(peer, generation, `open:${label}`),
      onChannelMessage: (peer, generation, label, text) =>
        queueIfCurrent(peer, generation, `message:${label}:${text}`),
    });

    await engine.pairWith(SENDER, false, []);
    const stalePc = FakePeerConnection.instances[0];
    if (stalePc === undefined) {
      throw new Error('first fake peer connection was not constructed');
    }
    const staleChannel = new FakeDataChannel(RELIABLE_LABEL);
    stalePc.ondatachannel?.({ channel: staleChannel as unknown as RTCDataChannel });

    // These callbacks are valid when accepted by the engine, but their queued
    // orchestrator work is deliberately held until after replacement.
    stalePc.connectionState = 'failed';
    stalePc.onconnectionstatechange?.();
    stalePc.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'queued-stale' }) } });
    staleChannel.readyState = 'open';
    staleChannel.onopen?.();
    staleChannel.onmessage?.({ data: 'queued-stale' });
    if (queued.length < 4 || queued.length > 4 || observed.length !== 0) {
      throw new Error('old-link callbacks were not held at the queue boundary');
    }

    engine.removePeer(SENDER);
    await engine.pairWith(SENDER, false, []);
    const currentPc = FakePeerConnection.instances[1];
    if (currentPc === undefined) {
      throw new Error('replacement fake peer connection was not constructed');
    }

    for (const dispatch of queued.splice(0)) {
      dispatch();
    }
    if (observed.length !== 0) {
      throw new Error(
        `queued stale callbacks escaped replacement guard: ${observed.join(', ')}`,
      );
    }

    // Events originating after replacement are rejected at the engine edge.
    stalePc.connectionState = 'closed';
    stalePc.onconnectionstatechange?.();
    stalePc.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'stale' }) } });
    stalePc.ondatachannel?.({
      channel: new FakeDataChannel('stale') as unknown as RTCDataChannel,
    });
    staleChannel.onmessage?.({ data: 'stale' });
    if (queued.length !== 0 || observed.length !== 0) {
      throw new Error(`stale callbacks escaped replacement guard: ${observed.join(', ')}`);
    }

    currentPc.connectionState = 'connected';
    currentPc.onconnectionstatechange?.();
    currentPc.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'fresh' }) } });
    const currentChannel = new FakeDataChannel(RELIABLE_LABEL);
    currentPc.ondatachannel?.({ channel: currentChannel as unknown as RTCDataChannel });
    currentChannel.readyState = 'open';
    currentChannel.onopen?.();
    currentChannel.onmessage?.({ data: 'fresh' });
    for (const dispatch of queued.splice(0)) {
      dispatch();
    }
    const expected = [
      'pc:connected',
      'candidate:{"candidate":"fresh"}',
      `open:${RELIABLE_LABEL}`,
      `message:${RELIABLE_LABEL}:fresh`,
    ];
    if (observed.join('\n') !== expected.join('\n')) {
      throw new Error(`current callbacks were lost or reordered: ${observed.join(', ')}`);
    }

    const retryPeer = playerId(77);
    FakePeerConnection.failNextOffer = true;
    try {
      await engine.pairWith(retryPeer, true, []);
      throw new Error('injected initiator setup failure was not surfaced');
    } catch (error) {
      if (!(error instanceof Error) || !error.message.includes('injected offer failure')) {
        throw error;
      }
    }
    if (engine.isPaired(retryPeer)) {
      throw new Error('failed initiator setup left a dead peer link registered');
    }
    const retryDelta = authoritativePeerDelta(new Set([retryPeer]), [retryPeer]);
    if (retryDelta.retained[0] !== retryPeer || retryDelta.added.length !== 0) {
      throw new Error('fresh plan did not retain the failed expected peer');
    }
    await engine.pairWith(retryPeer, false, []);
    if (!engine.isPaired(retryPeer)) {
      throw new Error('fresh authoritative plan could not retry the failed retained peer');
    }
  } finally {
    if (originalPeerConnection === undefined) {
      Reflect.deleteProperty(globalThis, 'RTCPeerConnection');
    } else {
      Reflect.set(globalThis, 'RTCPeerConnection', originalPeerConnection);
    }
  }
});

test('negotiated mode requires exact snapshot and metadata shapes', () => {
  expectViolation(
    () => new DeliveryAccountability(true).rebaselineSnapshot([{ id: SENDER }]),
    'v3 snapshot omitted epoch',
  );
  expectViolation(
    () =>
      new DeliveryAccountability(true).rebaselineReconnected(
        [{ id: SENDER, epoch: 1, seq: 0 }],
        [],
      ),
    'watermarks do not cover',
  );
  expectViolation(
    () => new DeliveryAccountability(true).rebaselineSnapshot([{ id: SENDER, epoch: 1 }]),
    'must carry epoch and seq together',
  );

  const v2 = new DeliveryAccountability(false);
  v2.rebaselineSnapshot([{ id: SENDER }]);
  v2.recordGameData({ from_player: SENDER });
  v2.observeServerMessage(true);
  expectViolation(
    () => v2.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 }),
    'v2 GameData',
  );
  expectViolation(
    () => v2.recordReport({ per_class: counters(0) }),
    'v2 connection received DeliveryReport',
  );
  expectViolation(
    () => v2.rebaselineSnapshot([{ id: SENDER, seq: 0 }]),
    'must carry epoch and seq together',
  );
});

test('accountability identifiers are canonical UUIDs and null gaps are malformed', () => {
  const invalidIds: unknown[] = [
    'sender',
    SENDER.toUpperCase(),
    SENDER.replaceAll('-', ''),
    `{${SENDER}}`,
    `${SENDER}0`,
    null,
  ];
  for (const id of invalidIds) {
    expectViolation(
      () => new DeliveryAccountability().notePlayerJoined({ id, epoch: 1, seq: 0 }),
      'canonical lowercase UUID',
    );
  }

  const idConsumers: Array<() => void> = [
    () =>
      new DeliveryAccountability().rebaselineSnapshot([{ id: 'invalid', epoch: 1, seq: 0 }]),
    () =>
      new DeliveryAccountability().rebaselineReconnected(
        [{ id: SENDER, epoch: 1, seq: 0 }],
        [{ player_id: 'invalid', epoch: 1, seq: 0 }],
      ),
    () => new DeliveryAccountability().notePlayerReconnected('invalid', 1),
    () => new DeliveryAccountability().notePlayerLeft('invalid'),
    () =>
      new DeliveryAccountability().recordGameData({
        from_player: 'invalid',
        epoch: 1,
        seq: 1,
      }),
    () =>
      joinedState().recordReport({
        per_class: countersWithSuperseded(1),
        gaps: [{ ...gap(1, 1), from_player: 'invalid' }],
      }),
  ];
  for (const consume of idConsumers) {
    expectViolation(consume, 'canonical lowercase UUID');
  }

  joinedState().recordReport({ per_class: counters(0) });
  expectViolation(
    () => joinedState().recordReport({ per_class: counters(0), gaps: null }),
    'DeliveryReport.gaps must be an array',
  );
});

test('RelayStats interval and cumulative counters are validated per connection', () => {
  const valid = new DeliveryAccountability();
  valid.recordRelayStats(relayStats(1_000, 4, 2, 1));
  valid.recordRelayStats(relayStats(1_000, 5, 2, 3));
  valid.resetRoom();
  valid.recordRelayStats(relayStats(1_000, 5, 3, 3));

  const invalid: Array<
    [string, Record<string, unknown> | null, Record<string, unknown>, string]
  > = [
    ['zero interval', null, relayStats(0), 'interval_ms must be a safe integer'],
    [
      'changed interval',
      relayStats(1_000, 4, 2, 1),
      relayStats(2_000, 4, 2, 1),
      'interval_ms changed',
    ],
    [
      'sent moved backward',
      relayStats(1_000, 4, 2, 1),
      relayStats(1_000, 3, 2, 1),
      'counters moved backward',
    ],
    [
      'dropped moved backward',
      relayStats(1_000, 4, 2, 1),
      relayStats(1_000, 4, 1, 1),
      'counters moved backward',
    ],
    [
      'backpressure moved backward',
      relayStats(1_000, 4, 2, 1),
      relayStats(1_000, 4, 2, 0),
      'counters moved backward',
    ],
  ];
  for (const [name, first, next, expected] of invalid) {
    const state = new DeliveryAccountability();
    if (first !== null) {
      state.recordRelayStats(first);
    }
    try {
      expectViolation(() => state.recordRelayStats(next), expected);
    } catch (error) {
      throw new Error(`${name}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  for (const field of [
    'interval_ms',
    'sent_to_you',
    'dropped_for_you',
    'backpressure_events',
  ]) {
    const payload = relayStats();
    payload[field] = Number.MAX_SAFE_INTEGER + 1;
    expectViolation(
      () => new DeliveryAccountability().recordRelayStats(payload),
      `${field} must be a safe integer`,
    );
  }

  expectViolation(
    () => new DeliveryAccountability(false).recordRelayStats(relayStats()),
    'v2 connection received RelayStats',
  );
  valid.resetConnection();
  valid.recordRelayStats(relayStats(2_000));
});

test('unsafe JSON integers and overlapping reports are rejected atomically', () => {
  const unsafe = Number.MAX_SAFE_INTEGER + 1;
  expectViolation(
    () => joinedState().recordGameData({ from_player: SENDER, epoch: 1, seq: unsafe }),
    'GameData.seq must be a safe integer',
  );

  const unsafeCounters = counters(0);
  (unsafeCounters['reliable'] as Record<string, unknown>)['delivered'] = unsafe;
  expectViolation(
    () => joinedState().recordReport({ per_class: unsafeCounters }),
    'reliable.delivered must be a safe integer',
  );

  expectViolation(
    () =>
      joinedState().recordReport({
        per_class: counters(0),
        gaps: [gap(unsafe, unsafe)],
      }),
    'from_seq must be a safe integer',
  );

  expectViolation(
    () =>
      new DeliveryAccountability().rebaselineReconnected(
        [{ id: SENDER, epoch: 1, seq: 0 }],
        [{ player_id: SENDER, epoch: 1, seq: unsafe }],
      ),
    'sender_watermarks[0].seq must be a safe integer',
  );

  const state = joinedState();
  state.recordGameData({ from_player: SENDER, epoch: 1, seq: 1 });
  expectViolation(
    () =>
      state.recordReport({
        per_class: counters(2),
        gaps: [gap(2, 3), gap(3, 4)],
      }),
    'in one report',
  );
  state.recordReport({ per_class: countersWithSuperseded(1), gaps: [gap(2, 2)] });
  state.recordGameData({ from_player: SENDER, epoch: 1, seq: 3 });
});

test('gap units match cumulative loss deltas across a 256 plus one frontier', () => {
  const state = joinedState();
  const firstFrontier = Array.from({ length: DELIVERY_REPORT_MAX_GAPS }, (_, index) =>
    gap(index * 2 + 1, index * 2 + 1),
  );
  state.recordReport({
    per_class: countersWithSuperseded(DELIVERY_REPORT_MAX_GAPS),
    gaps: firstFrontier,
  });
  state.recordReport({
    per_class: countersWithSuperseded(DELIVERY_REPORT_MAX_GAPS + 1),
    gaps: [gap(DELIVERY_REPORT_MAX_GAPS * 2 + 1, DELIVERY_REPORT_MAX_GAPS * 2 + 1)],
  });
  const tooMany = Array.from({ length: DELIVERY_REPORT_MAX_GAPS + 1 }, (_, index) =>
    gap(index * 2 + 1_001, index * 2 + 1_001),
  );
  expectViolation(
    () =>
      state.recordReport({
        per_class: countersWithSuperseded(DELIVERY_REPORT_MAX_GAPS * 2 + 2),
        gaps: tooMany,
      }),
    'gap ranges, limit is',
  );
  expectViolation(
    () => joinedState().recordReport({ per_class: countersWithSuperseded(1) }),
    'do not match exact gap units',
  );
});

test('snapshot baseline validates only the recipient-visible tail', () => {
  const state = new DeliveryAccountability();
  state.rebaselineSnapshot([{ id: SENDER, epoch: 1, seq: 90 }]);
  state.recordReport({
    per_class: countersWithSuperseded(1),
    gaps: [gap(92, 92)],
  });
  state.recordGameData({ from_player: SENDER, epoch: 1, seq: 91 });
  state.recordGameData({ from_player: SENDER, epoch: 1, seq: 93 });

  const preBaseline = new DeliveryAccountability();
  preBaseline.rebaselineSnapshot([{ id: SENDER, epoch: 1, seq: 90 }]);
  expectViolation(
    () =>
      preBaseline.recordReport({
        per_class: countersWithSuperseded(1),
        gaps: [gap(89, 89)],
      }),
    'reported after data at or beyond its start',
  );
});

test('unsupported advisory requires a prior report but not adjacency', () => {
  const report = { per_class: countersWithUnsupported(1), gaps: [unsupportedGap(1)] };

  const paired = joinedState();
  paired.recordReport(report);
  paired.observeServerMessage(false);
  paired.observeServerMessage(true);
  paired.observeServerMessage(false);
  expectViolation(() => paired.observeServerMessage(true), 'lacked a prior causal');

  const rollover = joinedState();
  rollover.recordReport(report);
  rollover.recordReport({
    per_class: countersWithUnsupported(2),
    gaps: [unsupportedGap(2)],
  });
  rollover.observeServerMessage(true);

  const roomReset = joinedState();
  roomReset.recordReport(report);
  roomReset.resetRoom();
  expectViolation(() => roomReset.observeServerMessage(true), 'lacked a prior causal');

  const terminal = joinedState();
  terminal.recordReport(report);
  terminal.observeTerminal();

  const mixedCounters = countersWithUnsupported(1);
  (mixedCounters['latest'] as Record<string, unknown>)['superseded'] = 1;
  expectViolation(
    () =>
      joinedState().recordReport({
        per_class: mixedCounters,
        gaps: [unsupportedGap(1), gap(2, 2)],
      }),
    'unsupported-format report must name exactly one sequence',
  );
});

test('browser GameData send API validates before invoking the wire callback', () => {
  const sent: string[] = [];
  const send = (frame: string): void => {
    sent.push(frame);
  };
  sendGameData(send, { value: 1 });
  sendGameDataWithDelivery(send, { value: 2 }, 'latest', 7);
  sendGameDataWithDelivery(send, { value: 3 }, 'volatile');
  sendGameDataWithDelivery(send, { value: 4 }, 'reliable');
  sendGameDataWithDelivery(send, { value: 5 }, 'latest', 0xffff_ffff);

  const decoded = sent.map((frame) => JSON.parse(frame) as Record<string, unknown>);
  if (decoded[0]?.['data'] === undefined || sent[0]?.includes('class')) {
    throw new Error('reliable send must preserve the omitted class/key wire shape');
  }
  if (!sent[1]?.includes('"class":"latest"') || !sent[1]?.includes('"key":7')) {
    throw new Error('latest send must carry its validated class/key');
  }
  if (!sent[2]?.includes('"class":"volatile"') || sent[2]?.includes('"key"')) {
    throw new Error('volatile send must carry class without key');
  }
  if (!sent[3]?.includes('"class":"reliable"') || sent[3]?.includes('"key"')) {
    throw new Error('explicit reliable send must omit key');
  }
  if (!sent[4]?.includes('"key":4294967295')) {
    throw new Error('latest send must accept the inclusive u32 key maximum');
  }

  const invalidCases: Array<[unknown, unknown]> = [
    ['latest', undefined],
    ['reliable', 1],
    ['volatile', 1],
    ['latest', null],
    ['latest', -1],
    ['latest', 1.5],
    ['latest', 0x1_0000_0000],
    ['latest', Number.MAX_SAFE_INTEGER + 1],
    ['latest', Number.NaN],
    ['latest', Number.POSITIVE_INFINITY],
    ['latest', '7'],
    [null, undefined],
    [undefined, undefined],
    ['unknown', undefined],
  ];
  for (const [className, key] of invalidCases) {
    const before = sent.length;
    expectError(
      () => sendGameDataWithDelivery(send, null, className as never, key as never),
      'invalid outgoing GameData delivery:',
    );
    if (sent.length !== before) {
      throw new Error('invalid class/key reached the wire callback');
    }
  }
});

test('accountability mode follows ProtocolInfo rather than advertised max', () => {
  const cases = [
    { offered: 3, payload: {}, expected: 2 },
    { offered: 3, payload: { protocol_version: 2 }, expected: 2 },
    { offered: 3, payload: { protocol_version: 3 }, expected: 3 },
  ];
  for (const { offered, payload, expected } of cases) {
    const frame = classifyBrowserServerInput(
      JSON.stringify({ type: 'ProtocolInfo', data: payload }),
    );
    const negotiated = negotiatedProtocolVersion(frame, offered);
    if (negotiated !== expected) {
      throw new Error(`offered ${offered} incorrectly selected negotiated ${negotiated}`);
    }
  }
  for (const [offered, protocolVersion] of [
    [2, 3],
    [3, 1],
    [3, 4],
    [3, null],
    [3, 2.5],
  ] as const) {
    const frame = classifyBrowserServerInput(
      JSON.stringify({ type: 'ProtocolInfo', data: { protocol_version: protocolVersion } }),
    );
    expectError(
      () => negotiatedProtocolVersion(frame, offered),
      'must be an integer in 2..=3 no greater than the offered version',
    );
  }
  const applicationFrame = classifyBrowserServerInput(
    JSON.stringify({ type: 'GameData', data: { from_player: SENDER } }),
  );
  expectError(
    () => negotiatedProtocolVersion(applicationFrame, 3),
    'expected ProtocolInfo, got GameData',
  );
});

test('join success effects follow negotiated-v3 snapshot validation', () => {
  let successEffects = 0;
  expectViolation(() => {
    applyJoinAccountabilityBaseline(new DeliveryAccountability(true), [{ id: SENDER }], [], []);
    successEffects += 1;
  }, 'v3 snapshot omitted epoch');
  if (successEffects !== 0) {
    throw new Error('malformed v3 snapshot reached observable join success effects');
  }
});

test('join handshake consumes stateful accountability prefaces', () => {
  const state = joinedState();
  const report: ServerFrame = {
    type: 'DeliveryReport',
    data: { per_class: countersWithSuperseded(1), gaps: [gap(1, 1)] },
  };
  const stats: ServerFrame = { type: 'RelayStats', data: relayStats() };
  if (!observeJoinHandshakeFrame(state, report) || !observeJoinHandshakeFrame(state, stats)) {
    throw new Error('valid accountability preface was not consumed');
  }
  const roomJoined: ServerFrame = { type: 'RoomJoined', data: {} };
  if (observeJoinHandshakeFrame(state, roomJoined)) {
    throw new Error('RoomJoined was incorrectly consumed as an accountability preface');
  }

  expectViolation(
    () => observeJoinHandshakeFrame(new DeliveryAccountability(false), stats),
    'v2 connection received RelayStats',
  );
});

test('v3 binary envelopes decode stamps and opaque bytes for every encoding', () => {
  for (const encoding of ['json', 'message_pack', 'rkyv'] as const) {
    const payload = Uint8Array.from([0, 1, 2, 255]);
    const wire = encode({
      from_player: BINARY_SENDER_BYTES,
      encoding,
      payload,
      seq: 7,
      epoch: 3,
    });
    const frame = classifyBrowserServerInput(exactArrayBuffer(wire));
    if (
      frame.type !== 'GameDataBinary' ||
      frame.data['from_player'] !== BINARY_SENDER ||
      frame.data['encoding'] !== encoding ||
      frame.data['seq'] !== 7 ||
      frame.data['epoch'] !== 3
    ) {
      throw new Error(`incorrect decoded ${encoding} binary metadata`);
    }
    const decodedPayload = frame.data['payload'];
    if (
      !(decodedPayload instanceof Uint8Array) ||
      decodedPayload.length !== payload.length ||
      decodedPayload.some((byte, index) => byte !== payload[index])
    ) {
      throw new Error(`${encoding} payload bytes were not preserved exactly`);
    }
  }
});

test('the JSON-negotiated runtime rejects physical and text GameDataBinary frames', () => {
  const ordinary = classifyJsonNegotiatedServerInput(JSON.stringify({ type: 'Pong' }));
  if (ordinary.type !== 'Pong') {
    throw new Error('ordinary JSON server frame was not preserved');
  }
  const cases: Array<[string, unknown]> = [
    ['physical binary', Uint8Array.of(0x80).buffer],
    [
      'text in-memory variant',
      JSON.stringify({
        type: 'GameDataBinary',
        data: {
          from_player: SENDER,
          encoding: 'json',
          payload: [],
          seq: 1,
          epoch: 1,
        },
      }),
    ],
  ];
  for (const [name, input] of cases) {
    try {
      expectError(() => classifyJsonNegotiatedServerInput(input), 'game_data_format=json');
    } catch (error) {
      throw new Error(`${name}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
});

test('binary envelopes are strict and preserve deferred advisory accountability', () => {
  const report = {
    type: 'DeliveryReport',
    data: { per_class: countersWithUnsupported(1), gaps: [unsupportedGap(1)] },
  };
  const error = {
    type: 'Error',
    data: { error_code: 'UNSUPPORTED_GAME_DATA_FORMAT', message: 'unsupported' },
  };

  const backlog = [JSON.stringify(report), new Blob(['noise']), JSON.stringify(error)].map(
    classifyBrowserServerInput,
  );
  const handoff = backlog.splice(0);
  if (handoff[1]?.type !== NON_TEXT_APPLICATION_FRAME) {
    throw new Error('buffered Blob did not retain its non-text ordering marker');
  }
  const buffered = joinedState();
  dispatchClassifiedFrame(buffered, handoff[0] as ServerFrame);
  dispatchClassifiedFrame(buffered, handoff[1] as ServerFrame);
  dispatchClassifiedFrame(buffered, handoff[2] as ServerFrame);

  const binary = encode({
    from_player: BINARY_SENDER_BYTES,
    encoding: 'json',
    payload: Uint8Array.from([1]),
    seq: 1,
    epoch: 1,
  });
  const liveFrames = [
    classifyBrowserServerInput(JSON.stringify(report)),
    classifyBrowserServerInput(exactArrayBuffer(binary)),
    classifyBrowserServerInput(JSON.stringify(error)),
  ];
  if (liveFrames[1]?.type !== 'GameDataBinary' || liveFrames[2]?.type !== 'Error') {
    throw new Error('live ArrayBuffer sequence did not retain its decoded frame ordering');
  }
  const live = joinedState();
  dispatchClassifiedFrame(live, liveFrames[0] as ServerFrame);
  dispatchClassifiedFrame(live, liveFrames[1] as ServerFrame);
  dispatchClassifiedFrame(live, liveFrames[2] as ServerFrame);

  const canonicalEntries = binaryEnvelopeEntries();
  const canonical = encodeMessagePackMap(canonicalEntries);
  const malformedCases: Array<[string, Uint8Array]> = [
    [
      'positional array',
      encodedMessagePack([new Uint8Array(16), 'json', new Uint8Array(0), 1, 1]),
    ],
  ];

  let entries = binaryEnvelopeEntries();
  entries[0] = [encodedMessagePack(7), entries[0]![1]];
  malformedCases.push(['non-string key', encodeMessagePackMap(entries)]);

  entries = binaryEnvelopeEntries();
  entries[1] = [entries[1]![0], encodedMessagePack(7)];
  malformedCases.push(['numeric encoding', encodeMessagePackMap(entries)]);

  entries = binaryEnvelopeEntries();
  entries[0] = [entries[0]![0], encodedMessagePack(Array.from({ length: 16 }, () => 0))];
  malformedCases.push(['array UUID', encodeMessagePackMap(entries)]);

  entries = binaryEnvelopeEntries();
  entries[0] = [entries[0]![0], encodedMessagePack(new Uint8Array(15))];
  malformedCases.push(['short binary UUID', encodeMessagePackMap(entries)]);

  entries = binaryEnvelopeEntries();
  entries[2] = [entries[2]![0], encodedMessagePack([1, 2, 3])];
  malformedCases.push(['array payload', encodeMessagePackMap(entries)]);

  for (const [index, field] of [
    'from_player',
    'encoding',
    'payload',
    'seq',
    'epoch',
  ].entries()) {
    entries = binaryEnvelopeEntries();
    entries.splice(index, 1);
    malformedCases.push([`missing ${field}`, encodeMessagePackMap(entries)]);
  }

  entries = binaryEnvelopeEntries();
  entries[4] = [entries[3]![0], entries[4]![1]];
  const duplicateKey = encodeMessagePackMap(entries);
  malformedCases.push(['duplicate key', duplicateKey]);

  entries = binaryEnvelopeEntries();
  entries[4] = [encodedMessagePack('unexpected'), entries[4]![1]];
  malformedCases.push(['unknown key', encodeMessagePackMap(entries)]);

  for (const index of [3, 4]) {
    entries = binaryEnvelopeEntries();
    entries[index] = [entries[index]![0], encodedMessagePack(0)];
    malformedCases.push([
      index === 3 ? 'zero seq' : 'zero epoch',
      encodeMessagePackMap(entries),
    ]);
  }

  entries = binaryEnvelopeEntries();
  entries[3] = [entries[3]![0], encodedMessagePack(BigInt(Number.MAX_SAFE_INTEGER) + 1n)];
  malformedCases.push(['seq overflow', encodeMessagePackMap(entries)]);

  entries = binaryEnvelopeEntries();
  entries[4] = [entries[4]![0], encodedMessagePack(BigInt(0x1_0000_0000))];
  malformedCases.push(['epoch overflow', encodeMessagePackMap(entries)]);

  for (const index of [3, 4]) {
    entries = binaryEnvelopeEntries();
    entries[index] = [entries[index]![0], encodedFloat64(1)];
    malformedCases.push([
      index === 3 ? 'floating-point seq' : 'floating-point epoch',
      encodeMessagePackMap(entries),
    ]);

    entries = binaryEnvelopeEntries();
    entries[index] = [entries[index]![0], encodedMessagePack(-1)];
    malformedCases.push([
      index === 3 ? 'negative seq' : 'negative epoch',
      encodeMessagePackMap(entries),
    ]);
  }

  entries = binaryEnvelopeEntries();
  entries[2] = [entries[2]![0], Uint8Array.of(0xc6, 0xff, 0xff, 0xff, 0xff)];
  malformedCases.push(['declared huge payload', encodeMessagePackMap(entries)]);

  entries = binaryEnvelopeEntries();
  entries[2] = [
    entries[2]![0],
    concatenateBytes(new Uint8Array(512).fill(0x91), Uint8Array.of(0xc0)),
  ];
  malformedCases.push(['deep nested payload', encodeMessagePackMap(entries)]);

  malformedCases.push(['declared huge map', Uint8Array.of(0xdf, 0xff, 0xff, 0xff, 0xff)]);

  malformedCases.push(['truncation', canonical.slice(0, -1)]);
  malformedCases.push(['trailing scalar', concatenateBytes(canonical, encodedMessagePack(1))]);
  malformedCases.push(['concatenated map', concatenateBytes(canonical, canonical)]);

  for (const [name, malformed] of malformedCases) {
    if (
      classifyBrowserServerInput(exactArrayBuffer(malformed)).type !==
      NON_TEXT_APPLICATION_FRAME
    ) {
      throw new Error(`malformed binary input (${name}) did not retain its ordering marker`);
    }
  }
  const duplicateMarker = classifyBrowserServerInput(exactArrayBuffer(duplicateKey));
  if (!String(duplicateMarker.data['error']).includes('duplicate key: seq')) {
    throw new Error('duplicate binary envelope key was not rejected explicitly');
  }
});

test('outgoing GameData accepts JSON values and rejects coercive roots before send', () => {
  const sent: string[] = [];
  const send = (frame: string): void => {
    sent.push(frame);
  };
  const nullPrototype = Object.create(null) as Record<string, unknown>;
  nullPrototype['value'] = 1;
  const valid: unknown[] = [
    null,
    true,
    false,
    0,
    -1.5,
    'text',
    [],
    [null, 1, 'two'],
    {},
    { nested: { values: [1, 2, 3] } },
    nullPrototype,
  ];
  for (const [index, value] of valid.entries()) {
    if (index % 2 === 0) {
      sendGameData(send, value);
    } else {
      sendGameDataWithDelivery(send, value, 'latest', index);
    }
  }
  if (sent.length !== valid.length) {
    throw new Error('a valid JSON root was not sent');
  }

  const circular: Record<string, unknown> = {};
  circular['self'] = circular;
  const sparse = new Array(2);
  sparse[1] = 'present';
  const hookedObject: Record<string, unknown> = { value: 1 };
  Object.defineProperty(hookedObject, 'toJSON', { value: () => Number.NaN });
  const hookedArray: unknown[] = [1];
  Object.defineProperty(hookedArray, 'toJSON', { get: () => () => Number.NaN });
  let proxyToJsonReads = 0;
  const hostileProxy = new Proxy(
    { value: 1 },
    {
      get(target, property, receiver) {
        if (property === 'toJSON') {
          proxyToJsonReads += 1;
          return () => Number.POSITIVE_INFINITY;
        }
        return Reflect.get(target, property, receiver);
      },
    },
  );
  for (const [value, expected] of [
    [hookedObject, '{"value":1}'],
    [hookedArray, '[1]'],
    [hostileProxy, '{"value":1}'],
  ] as Array<[unknown, string]>) {
    const before = sent.length;
    sendGameData(send, value);
    const frame = JSON.parse(sent[before] as string) as Record<string, unknown>;
    const envelope = frame['data'] as Record<string, unknown>;
    if (JSON.stringify(envelope['data']) !== expected) {
      throw new Error('validated JSON normalization changed the descriptor-copied value');
    }
  }
  if (proxyToJsonReads !== 0) {
    throw new Error('serialization read toJSON from the caller-owned proxy');
  }
  const invalid: unknown[] = [
    undefined,
    () => undefined,
    Symbol('value'),
    1n,
    Number.NaN,
    Number.POSITIVE_INFINITY,
    Number.NEGATIVE_INFINITY,
    circular,
    sparse,
    new Date(0),
    { nested: undefined },
    [Symbol('nested')],
    { nested: Number.NaN },
  ];
  for (const [index, value] of invalid.entries()) {
    const before = sent.length;
    expectError(
      () =>
        index % 2 === 0
          ? sendGameData(send, value)
          : sendGameDataWithDelivery(send, value, 'latest', index),
      'invalid outgoing GameData JSON value:',
    );
    if (sent.length !== before) {
      throw new Error('JSON-unrepresentable root reached the wire callback');
    }
  }
});

let failures = 0;
for (const entry of tests) {
  try {
    await entry.run();
    console.error(`ok - ${entry.name}`);
  } catch (error) {
    failures += 1;
    console.error(`not ok - ${entry.name}: ${error instanceof Error ? error.stack : error}`);
  }
}
if (failures !== 0) {
  throw new Error(`${failures} accountability test(s) failed`);
}
