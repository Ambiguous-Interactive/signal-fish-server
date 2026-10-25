import { run } from './orchestrator.js';
import type { RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #1011: a stalled peer must not hold relay or another peer's SDP/ICE.
// Regression #1183: retired negotiation must release queued task credits.
// Audit #853/#889: qualify each suspended stack phase with both late outcomes.
const ordinaryOutcomes = [
  'resolve',
  'reject',
  'pending',
  'backlog',
  'retired-resolve',
  'retired-reject',
] as const;
// Regression #1087: failed initial offers must resolve availability before their deadline.
// Regression #1144: rejected remote SDP and answer generation also resolve availability.
const failedOutcomes = [
  'pc-failed',
  'pc-closed',
  'offer-rejected',
  'local-rejected',
  'remote-offer-rejected',
  'answer-rejected',
  'local-answer-rejected',
  'remote-answer-rejected',
  'candidate-rejected',
] as const;
const retirementPhases = [
  'offer',
  'local-offer',
  'remote-offer',
  'answer',
  'local-answer',
  'remote-answer',
  'candidate',
] as const;
for (const { oldOutcome, healthyConnected, retirementPhase } of [
  ...ordinaryOutcomes
    .filter((outcome) => !outcome.startsWith('retired-'))
    .map((oldOutcome) => ({ oldOutcome, healthyConnected: false, retirementPhase: 'offer' })),
  ...retirementPhases.flatMap((retirementPhase) =>
    (['retired-resolve', 'retired-reject'] as const).map((oldOutcome) => ({
      oldOutcome,
      healthyConnected: false,
      retirementPhase,
    })),
  ),
  ...failedOutcomes.flatMap((oldOutcome) =>
    [false, true].map((healthyConnected) => ({
      oldOutcome,
      healthyConnected,
      retirementPhase: 'offer',
    })),
  ),
]) {
  const retirementChurn = oldOutcome.startsWith('retired-');
  const responder =
    (retirementChurn && ['remote-offer', 'answer', 'local-answer'].includes(retirementPhase)) ||
    ['remote-offer-rejected', 'answer-rejected', 'local-answer-rejected'].includes(oldOutcome);
  const signalFailure = responder || oldOutcome === 'remote-answer-rejected';
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const me = '00000000-0000-0000-0000-000000000001';
  const slow = '00000000-0000-0000-0000-000000000002';
  const healthy = '00000000-0000-0000-0000-000000000003';
  const events: Record<string, unknown>[] = [];
  const sent: Record<string, unknown>[] = [];
  const connections: Connection[] = [];
  const calls: string[] = [];
  const timers = new Map<number, { at: number; callback: () => void }>();
  let timerId = 0;
  let now = 100;
  let socket!: Socket;
  let resolveOld!: () => void;
  let rejectOld!: (error: Error) => void;
  let resolveRetry!: () => void;
  const retryHeld = new Promise<void>((resolve) => {
    resolveRetry = resolve;
  });
  const held = new Promise<void>((resolve, reject) => {
    resolveOld = resolve;
    rejectOld = reject;
  });
  const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
  let running: Promise<number> | undefined;
  let holdReplacement = retirementChurn;
  const heldConnections = new Set<number>();
  const holdPhase = async (index: number, phase: string) => {
    if (
      retirementChurn &&
      phase === retirementPhase &&
      (index === 0 || (index > 1 && holdReplacement))
    ) {
      heldConnections.add(index);
      await held;
    }
  };
  class Connection {
    readonly index = connections.length;
    closed = false;
    connectionState = 'connecting';
    readonly channels: Array<{
      label: string;
      readyState: string;
      onopen: (() => void) | null;
    }> = [];
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string) {
      const channel = { label, readyState: 'connecting', onopen: null, send() {} };
      this.channels.push(channel);
      return channel;
    }
    async createOffer() {
      calls.push(`${this.index}:offer`);
      if (!retirementChurn && this.index === 0) {
        await held;
        if (oldOutcome === 'offer-rejected') throw new Error('offer failed');
      }
      await holdPhase(this.index, 'offer');
      if (this.index === 2 && oldOutcome === 'resolve') await retryHeld;
      return { type: 'offer', sdp: `offer-${this.index}` };
    }
    async createAnswer() {
      calls.push(`${this.index}:answer`);
      await holdPhase(this.index, 'answer');
      if (this.index === 0 && oldOutcome === 'answer-rejected')
        throw new Error('answer failed');
      return { type: 'answer', sdp: `answer-${this.index}` };
    }
    async setLocalDescription(description: { type: string }) {
      calls.push(`${this.index}:local`);
      await holdPhase(
        this.index,
        description.type === 'offer' ? 'local-offer' : 'local-answer',
      );
      if (
        this.index === 0 &&
        (oldOutcome === 'local-rejected' ||
          (oldOutcome === 'local-answer-rejected' && description.type === 'answer'))
      )
        throw new Error('local description failed');
    }
    async setRemoteDescription(description: { type: string }) {
      calls.push(`${this.index}:remote`);
      await holdPhase(
        this.index,
        description.type === 'offer' ? 'remote-offer' : 'remote-answer',
      );
      if (!retirementChurn && this.index === 0 && signalFailure) {
        await held;
        if (oldOutcome === 'remote-offer-rejected' || oldOutcome === 'remote-answer-rejected')
          throw new Error('remote description failed');
      }
    }
    async addIceCandidate() {
      calls.push(`${this.index}:candidate`);
      await holdPhase(this.index, 'candidate');
      if (this.index === 0 && oldOutcome === 'candidate-rejected')
        throw new Error('candidate failed');
    }
    close() {
      this.closed = true;
    }
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
    deliver(type: string, data: Record<string, unknown> = {}) {
      this.onmessage?.({ data: JSON.stringify({ type, data }) });
    }
    plan(peers: string[]) {
      this.deliver('SessionPlan', {
        generation: me,
        topology: 'mesh',
        transport: 'webrtc',
        fallback: 'relay',
        ice_servers: [],
        peers: peers.map((player_id) => ({
          player_id,
          initiate: player_id !== slow || !responder,
        })),
      });
    }
    signal(from: string, signal: Record<string, unknown>) {
      this.deliver('Signal', { from, generation: me, signal });
    }
    send(line: string) {
      const frame = JSON.parse(line) as Record<string, unknown>;
      sent.push(frame);
      if (frame['type'] === 'Authenticate')
        queueMicrotask(() => {
          this.deliver('Authenticated');
          this.deliver('ProtocolInfo', { protocol_version: 3 });
        });
      if (frame['type'] === 'JoinRoom')
        queueMicrotask(() => {
          this.deliver('RoomJoined', {
            player_id: me,
            room_id: me,
            room_code: 'TEST',
            lobby_state: 'finalized',
            ready_players: [],
            current_players: [me, slow, healthy].map((id) => ({ id, epoch: 1, seq: 0 })),
          });
          this.plan([slow, healthy]);
        });
    }
    close() {
      this.readyState = 3;
    }
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: {
        __sf_emit: (line: string) => {
          events.push(JSON.parse(line));
        },
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
      peers: 3,
      maxPlayers: null,
      expectTotalPeers: null,
      leaveOnGameStart: false,
      gameName: 'peer-work',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: signalFailure || retirementChurn,
      relayPayload: null,
      crippleIce: false,
      p2pTimeoutSecs: 1,
      p2pRetryCount: 1,
      runForSecs: 3,
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
    assert(connections.length === 2, 'healthy peer starts while the first offer is held');
    if (retirementChurn) {
      const negotiate = () => {
        socket.signal(slow, responder ? { Offer: 'phase-offer' } : { Answer: 'phase-answer' });
        socket.signal(slow, { IceCandidate: 'phase-candidate' });
      };
      negotiate();
      await turn();
      const oldCalls = calls.filter((call) => call.startsWith('0:')).join(',');
      const phaseCall = retirementPhase.split('-')[0];
      assert(
        calls.includes(`0:${phaseCall}`),
        `${retirementPhase}: old stack reaches held phase`,
      );
      let result: number | undefined;
      void running.then((code) => {
        result = code;
      });
      for (let replacement = 0; replacement < 12; replacement += 1) {
        for (let queued = 0; queued < 48; queued += 1)
          socket.signal(slow, { Answer: 'retired-answer' });
        await turn();
        socket.plan([healthy]);
        await turn();
        socket.plan([slow, healthy]);
        await turn();
        negotiate();
        await turn();
        assert(
          result === undefined,
          'retired negotiation releases task credits before replacement',
        );
      }
      holdReplacement = false;
      socket.plan([healthy]);
      await turn();
      socket.plan([slow, healthy]);
      await turn();
      const replacement = connections.at(-1)!;
      assert(
        heldConnections.size === 13,
        `${retirementPhase}: every retired link reached the held phase`,
      );
      const retiredCallHistory = () =>
        calls
          .filter((call) => !call.startsWith('1:') && !call.startsWith(`${replacement.index}:`))
          .join(',');
      const retiredCalls = retiredCallHistory();
      if (oldOutcome === 'retired-reject') rejectOld(new Error('retired stack failure'));
      else resolveOld();
      socket.signal(slow, responder ? { Offer: 'fresh-offer' } : { Answer: 'fresh-answer' });
      socket.signal(healthy, { Answer: 'healthy-answer' });
      socket.signal(healthy, { IceCandidate: 'healthy-candidate' });
      socket.deliver('GameData', {
        from_player: healthy,
        epoch: 1,
        seq: 1,
        data: { after_retirement: true },
      });
      await turn();
      for (const [connection, peer] of [
        [replacement, slow],
        [connections[1]!, healthy],
      ] as const) {
        if (connection.channels.length === 0) {
          for (const label of ['reliable', 'unreliable']) {
            const channel = {
              label,
              readyState: 'connecting',
              ordered: label === 'reliable',
              maxRetransmits: label === 'reliable' ? null : 0,
              maxPacketLifeTime: null,
              onopen: null,
              send() {},
            };
            connection.channels.push(channel);
            (
              connection as unknown as { ondatachannel: (event: unknown) => void }
            ).ondatachannel({ channel });
          }
        }
        connection.connectionState = 'connected';
        for (const channel of connection.channels) {
          channel.readyState = 'open';
          channel.onopen?.();
        }
        await turn();
        for (const channel of connection.channels) {
          (channel as unknown as { onmessage: (event: { data: string }) => void }).onmessage({
            data: JSON.stringify({ from: peer, channel: channel.label, seq: 0 }),
          });
        }
      }
      await turn();
      for (const eventName of ['channel_message_sent', 'channel_message']) {
        for (const peer of [slow, healthy]) {
          for (const label of ['reliable', 'unreliable']) {
            const matching = events.filter(
              (event) =>
                event['event'] === eventName &&
                event['peer'] === peer &&
                event['label'] === label,
            );
            assert(
              matching.length === 1 &&
                matching[0]?.['text'] ===
                  JSON.stringify({
                    from: eventName === 'channel_message_sent' ? me : peer,
                    channel: label,
                    seq: 0,
                  }),
              `${retirementPhase}/${oldOutcome}: exact fresh/sibling gameplay once (${eventName}/${peer}/${label})`,
            );
          }
        }
      }
      assert(
        result === undefined && !replacement.closed && !connections[1]!.closed,
        'fresh and sibling gameplay remain active after old operations settle',
      );
      assert(
        calls.filter((call) => call === `${replacement.index}:remote`).length === 1 &&
          !calls.includes(`${replacement.index}:candidate`) &&
          calls.includes('1:candidate'),
        `${retirementPhase}: only fresh SDP reaches the replacement; sibling ICE continues`,
      );
      assert(
        events.some((event) => event['event'] === 'game_data_received'),
        'relay gameplay continues after retirement churn',
      );
      assert(
        !events.some((event) => event['event'] === 'error'),
        `${retirementPhase}/${oldOutcome}: retired promise cannot report a current-peer error`,
      );
      assert(
        calls.filter((call) => call.startsWith('0:')).join(',') === oldCalls &&
          retiredCallHistory() === retiredCalls,
        `${retirementPhase}: retired negotiation cannot advance after its held phase`,
      );
      continue;
    }
    if (oldOutcome === 'backlog') {
      let result: number | undefined;
      void running.then((code) => {
        result = code;
      });
      for (let batch = 0; batch < 10 && result === undefined; batch += 1) {
        for (let index = 0; index < 64; index += 1)
          socket.signal(slow, { Answer: 'queued-answer' });
        await turn();
      }
      assert(
        result === 3,
        'retained peer work must keep shared task credits after frame dispatch',
      );
      assert(
        events.some(
          (event) =>
            event['event'] === 'error' &&
            String(event['message']).includes('task backlog limit'),
        ),
        'retained peer work reports the existing backlog error',
      );
      continue;
    }
    socket.signal(slow, responder ? { Offer: 'slow-offer' } : { Answer: 'slow-answer' });
    socket.signal(slow, { IceCandidate: 'slow-candidate' });
    socket.signal(healthy, { Answer: 'healthy-answer' });
    socket.signal(healthy, { IceCandidate: 'healthy-candidate' });
    socket.deliver('GameData', {
      from_player: healthy,
      epoch: 1,
      seq: 1,
      data: { live: true },
    });
    await turn();
    assert(
      (responder || !calls.includes('0:remote')) && !calls.includes('0:candidate'),
      'same-peer signals wait for the held offer',
    );
    assert(
      calls.indexOf('1:remote') > calls.indexOf('1:local') &&
        calls.indexOf('1:candidate') > calls.indexOf('1:remote'),
      'healthy SDP and ICE progress in order',
    );
    assert(
      events.filter((event) => event['event'] === 'game_data_received').length === 1,
      'relay arrives before the stalled peer deadline',
    );
    assert(
      sent.filter((frame) => frame['type'] === 'Signal').length === 1,
      'healthy offer is sent before the held offer resolves',
    );
    if (
      oldOutcome === 'pc-failed' ||
      oldOutcome === 'pc-closed' ||
      oldOutcome === 'offer-rejected' ||
      oldOutcome === 'local-rejected' ||
      signalFailure ||
      oldOutcome === 'candidate-rejected'
    ) {
      // The PC state changes before its browser event. Resuming the offer
      // observes the loss, but engine cleanup must not hide relay fallback.
      const failed = connections[0]!;
      if (healthyConnected) {
        connections[1]!.connectionState = 'connected';
        for (const channel of connections[1]!.channels) {
          channel.readyState = 'open';
          channel.onopen?.();
        }
        await turn();
      }
      if (oldOutcome.startsWith('pc-')) failed.connectionState = oldOutcome.slice(3);
      resolveOld();
      await turn();
      if (oldOutcome === 'candidate-rejected') {
        assert(
          calls.filter((call) => call === '0:candidate').length === 1,
          'the candidate reached the WebRTC stack',
        );
        assert(
          events.some(
            (event) =>
              event['event'] === 'error' &&
              String(event['message']).includes('candidate failed'),
          ),
          'the rejected candidate is reported',
        );
        assert(
          !failed.closed && !connections[1]!.closed,
          'candidate rejection keeps both peer links',
        );
        assert(
          !events.some((event) => event['event'] === 'transport_status_sent'),
          'candidate rejection does not force fallback',
        );
        failed.connectionState = 'connected';
        for (const channel of failed.channels) {
          channel.readyState = 'open';
          channel.onopen?.();
        }
        await turn();
        assert(
          events.some(
            (event) => event['event'] === 'p2p_pair_connected' && event['peer'] === slow,
          ),
          'the original link can connect after a candidate rejection',
        );
        continue;
      }
      assert(now === 100, `${oldOutcome}: no P2P deadline elapsed`);
      assert(
        events.filter(
          (event) =>
            event['event'] === 'transport_status_sent' &&
            event['connected'] === healthyConnected,
        ).length === 1,
        `${oldOutcome}: resolve current availability immediately after initial negotiation fails`,
      );
      assert(failed.closed && !connections[1]!.closed, 'retire only the failed link');
      assert(
        events.filter((event) => event['event'] === 'fallback_engaged').length ===
          (healthyConnected ? 0 : 1),
        'a connected sibling keeps WebRTC available',
      );
      socket.deliver('GameData', {
        from_player: healthy,
        epoch: 1,
        seq: 2,
        data: { after_failure: true },
      });
      socket.signal(healthy, { IceCandidate: 'healthy-after-failure' });
      socket.signal(slow, { PairRetry: 1 });
      await turn();
      assert(connections.length === 3, 'retry starts a fresh link within the original window');
      socket.signal(slow, {
        RetrySignal: {
          attempt: 1,
          signal: responder ? { Offer: 'retry-offer' } : { Answer: 'retry-answer' },
        },
      });
      socket.signal(slow, {
        RetrySignal: { attempt: 1, signal: { IceCandidate: 'retry-candidate' } },
      });
      await turn();
      assert(
        calls.includes('2:local') &&
          calls.includes('2:remote') &&
          calls.includes('2:candidate') &&
          (!calls.includes('0:remote') || signalFailure) &&
          !calls.includes('0:candidate'),
        'fresh retry negotiates without consuming failed-link signals',
      );
      if (responder) {
        const retry = connections[2]!;
        for (const label of ['reliable', 'unreliable']) {
          const channel = {
            label,
            readyState: 'connecting',
            ordered: label === 'reliable',
            maxRetransmits: label === 'reliable' ? null : 0,
            maxPacketLifeTime: null,
            onopen: null,
            send() {},
          };
          retry.channels.push(channel);
          (retry as unknown as { ondatachannel: (event: unknown) => void }).ondatachannel({
            channel,
          });
        }
      }
      connections[2]!.connectionState = 'connected';
      for (const channel of connections[2]!.channels) {
        channel.readyState = 'open';
        channel.onopen?.();
      }
      await turn();
      if (signalFailure) {
        for (const channel of connections[2]!.channels) {
          (channel as unknown as { onmessage: (event: { data: string }) => void }).onmessage({
            data: JSON.stringify({ from: slow, channel: channel.label, seq: 0 }),
          });
        }
        await turn();
        for (const eventName of ['channel_message_sent', 'channel_message']) {
          assert(
            events.filter((event) => event['event'] === eventName && event['peer'] === slow)
              .length === 2,
            `${oldOutcome}: fresh retry restores both gameplay labels (${eventName})`,
          );
        }
      }
      assert(
        events.filter(
          (event) => event['event'] === 'p2p_pair_connected' && event['peer'] === slow,
        ).length === 1 &&
          events.filter(
            (event) => event['event'] === 'transport_status_sent' && event['connected'],
          ).length === 1,
        'the fresh retry connects and reports WebRTC availability',
      );
      assert(
        calls.filter((call) => call === '1:candidate').length === 2 &&
          events.filter((event) => event['event'] === 'game_data_received').length === 2,
        'healthy signaling and relay continue after failed initial negotiation',
      );
      continue;
    }
    if (oldOutcome === 'resolve') {
      socket.signal(healthy, { PairRetry: 1 });
      await turn();
      socket.signal(healthy, {
        RetrySignal: { attempt: 1, signal: { Answer: 'retry-answer' } },
      });
      socket.signal(healthy, {
        RetrySignal: { attempt: 1, signal: { IceCandidate: 'retry-candidate' } },
      });
      await turn();
      assert(
        connections.length === 3 &&
          !calls.includes('2:remote') &&
          !calls.includes('2:candidate'),
        'retry SDP and ICE wait for the held retry offer',
      );
      resolveRetry();
      await turn();
      assert(
        calls.indexOf('2:remote') > calls.indexOf('2:local') &&
          calls.indexOf('2:candidate') > calls.indexOf('2:remote'),
        'retry preserves the same-peer queue through physical replacement',
      );
    }
    // Remove and re-add the same peer in the same generation. Its old queued
    // Answer and ICE must never run against the new physical connection.
    now += 100;
    socket.plan([healthy]);
    await turn();
    socket.plan([slow, healthy]);
    await turn();
    const replacement = connections.at(-1)!;
    assert(
      connections.length === (oldOutcome === 'resolve' ? 4 : 3) &&
        connections[0]?.closed === true,
      're-added peer gets a fresh link without awaiting old work',
    );
    if (oldOutcome === 'resolve') resolveOld();
    else if (oldOutcome === 'reject') rejectOld(new Error('retired stack failure'));
    await turn();
    assert(
      !calls.includes('0:local') &&
        !calls.includes(`${replacement.index}:remote`) &&
        !calls.includes(`${replacement.index}:candidate`),
      'retired work cannot continue or consume signals on its replacement',
    );
    assert(
      !events.some((event) => event['event'] === 'error'),
      'retired work does not report a current-peer failure',
    );
    // The retired operation timeout must not close the replacement.
    now = 1100;
    for (const [id, timer] of [...timers])
      if (timer.at <= now) {
        timers.delete(id);
        timer.callback();
      }
    await turn();
    assert(replacement.closed === false, 'an old deadline cannot close the replacement');
    resolveOld();
    await turn();
    assert(
      !calls.includes('0:local') &&
        !calls.includes(`${replacement.index}:remote`) &&
        !calls.includes(`${replacement.index}:candidate`),
      'expired old work cannot resume or consume replacement signals',
    );
  } finally {
    resolveRetry();
    resolveOld();
    socket?.onclose?.();
    await running;
    await turn();
    performance.now = originalNow;
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
console.error(
  'ok - stalled peer work preserves relay, healthy peers, ordering, and replacements',
);
