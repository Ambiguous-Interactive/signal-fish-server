import { Engine, RELIABLE_LABEL, UNRELIABLE_LABEL } from './engine.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #911: remote announcements must use the same modes as locally created channels.
// Exercise the browser callback, including open-before-announcement races.
for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
  for (const alreadyOpen of [false, true]) {
    for (const ordered of [false, true]) {
      for (const maxRetransmits of [null, 0, 1]) {
        for (const maxPacketLifeTime of [null, 0, 1]) {
          const description = JSON.stringify({
            label,
            alreadyOpen,
            ordered,
            maxRetransmits,
            maxPacketLifeTime,
          });
          const valid =
            maxPacketLifeTime === null &&
            (label === RELIABLE_LABEL
              ? ordered && maxRetransmits === null
              : !ordered && maxRetransmits === 0);
          const originalPc = globalThis.RTCPeerConnection;
          const events: string[] = [];
          let connection: TestConnection | undefined;
          let paired = 0;
          let engine: Engine;
          class TestChannel {
            readyState = alreadyOpen ? 'open' : 'connecting';
            binaryType = 'blob';
            onopen: (() => void) | null = null;
            onmessage: ((event: { data: string }) => void) | null = null;
            onclose: (() => void) | null = null;
            onerror: (() => void) | null = null;
            constructor(
              readonly label: string,
              readonly ordered: boolean,
              readonly maxRetransmits: number | null,
              readonly maxPacketLifeTime: number | null,
            ) {}
            close(): void {
              assert(
                engine.channel('peer', this.label) === undefined,
                `${description}: close rejected channel before storage`,
              );
              assert(
                this.onopen === null &&
                  this.onmessage === null &&
                  this.onclose === null &&
                  this.onerror === null &&
                  this.binaryType === 'blob',
                `${description}: close rejected channel before handler registration`,
              );
              this.readyState = 'closed';
            }
          }
          class TestConnection {
            ondatachannel: ((event: { channel: TestChannel }) => void) | null = null;
            constructor() {
              connection = this;
            }
            close(): void {}
          }
          const announce = (channel: TestChannel) => {
            assert(
              typeof connection?.ondatachannel === 'function',
              'install announcement callback',
            );
            connection?.ondatachannel?.({ channel });
          };
          const makeValid = (name: string) =>
            new TestChannel(
              name,
              name === RELIABLE_LABEL,
              name === RELIABLE_LABEL ? null : 0,
              null,
            );
          try {
            globalThis.RTCPeerConnection =
              TestConnection as unknown as typeof RTCPeerConnection;
            engine = new Engine(false, {
              onLocalCandidate: () => events.push('candidate'),
              onPcState: () => events.push('pc-state'),
              onChannelClosed: () => events.push('close'),
              onChannelOpen: (peer, _generation, name) => {
                events.push(`open:${name}`);
                if (engine.noteChannelOpen(peer, name)) paired += 1;
              },
              onChannelMessage: (_peer, _generation, name, text) => {
                events.push(`${name}:${text}`);
              },
            });
            await engine.pairWith('peer', false, []);
            const announced = new TestChannel(
              label,
              ordered,
              maxRetransmits,
              maxPacketLifeTime,
            );
            announce(announced);
            await Promise.resolve();
            let accepted = announced;
            if (!valid) {
              assert(announced.readyState === 'closed', `${description}: reject invalid mode`);
              assert(
                engine.channel('peer', label) === undefined,
                `${description}: keep label available`,
              );
              announced.onopen?.();
              announced.onmessage?.({ data: 'invalid' });
              announced.onclose?.();
              announced.onerror?.();
              await Promise.resolve();
              assert(
                events.length === 0 && paired === 0,
                `${description}: suppress rejected events`,
              );
              accepted = makeValid(label);
              announce(accepted);
            }
            assert(
              engine.channel('peer', label) === (accepted as unknown as RTCDataChannel),
              `${description}: accept valid channel after the first announcement`,
            );
            const other = makeValid(
              label === RELIABLE_LABEL ? UNRELIABLE_LABEL : RELIABLE_LABEL,
            );
            announce(other);
            for (const channel of [accepted, other]) {
              channel.readyState = 'open';
              channel.onopen?.();
              channel.onmessage?.({ data: 'valid' });
            }
            await Promise.resolve();
            for (const channel of [accepted, other]) channel.onopen?.();
            assert(
              paired === 1 && events.filter((event) => event.startsWith('open:')).length === 2,
              `${description}: valid modes complete the pair exactly once`,
            );
            assert(
              events.includes('reliable:valid') && events.includes('unreliable:valid'),
              `${description}: valid modes deliver both required channels`,
            );
            assert(
              !events.includes('close') && engine.isPaired('peer'),
              `${description}: invalid first announcements do not retire the link`,
            );
            engine.removePeer('peer');
          } finally {
            globalThis.RTCPeerConnection = originalPc;
          }
        }
      }
    }
  }
}
console.error(
  'ok - remote required channels reject invalid modes before storage and accept valid followups',
);

// Pin the browser defaults and the initiator settings used by the remote gate.
{
  const originalPc = globalThis.RTCPeerConnection;
  class TestConnection {
    createDataChannel(label: string, options: RTCDataChannelInit = {}) {
      return {
        label,
        ordered: options.ordered ?? true,
        maxRetransmits: options.maxRetransmits ?? null,
        maxPacketLifeTime: options.maxPacketLifeTime ?? null,
        readyState: 'connecting',
      };
    }
    async createOffer() {
      return { sdp: 'offer' };
    }
    async setLocalDescription(): Promise<void> {}
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = TestConnection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    await engine.pairWith('peer', true, []);
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      const channel = engine.channel('peer', label);
      assert(
        channel?.ordered === (label === RELIABLE_LABEL) &&
          channel.maxRetransmits === (label === RELIABLE_LABEL ? null : 0) &&
          channel.maxPacketLifeTime === null,
        `${label}: initiator modes must match remote admission`,
      );
    }
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error('ok - initiator modes match required remote channel admission');

for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
  for (const initialState of ['connecting', 'open', 'closing', 'closed']) {
    for (const retireBeforeNotification of [false, true]) {
      const originalPc = globalThis.RTCPeerConnection;
      let connection: TestConnection | undefined;
      const events: string[] = [];
      class TestConnection {
        ondatachannel: ((event: { channel: TestChannel }) => void) | null = null;
        constructor() {
          connection = this;
        }
        close(): void {}
      }
      class TestChannel {
        readyState = initialState;
        ordered = label === RELIABLE_LABEL;
        maxRetransmits = label === RELIABLE_LABEL ? null : 0;
        maxPacketLifeTime = null;
        binaryType = 'blob';
        onopen: (() => void) | null = null;
        onclose: (() => void) | null = null;
        onerror: (() => void) | null = null;
        constructor(readonly label: string) {}
        close(): void {
          this.readyState = 'closed';
        }
      }
      try {
        globalThis.RTCPeerConnection = TestConnection as unknown as typeof RTCPeerConnection;
        const engine = new Engine(false, {
          onLocalCandidate: () => {},
          onPcState: () => {},
          onChannelOpen: (_peer, _generation, name) => events.push(`open:${name}`),
          onChannelClosed: (_peer, _generation, name) => events.push(`close:${name}`),
          onChannelMessage: () => {},
        });
        await engine.pairWith('peer', false, []);
        const channel = new TestChannel(label);
        connection?.ondatachannel?.({ channel });
        if (retireBeforeNotification) engine.removePeer('peer');
        await Promise.resolve();
        const expected =
          retireBeforeNotification || initialState === 'connecting'
            ? []
            : [`${initialState === 'open' ? 'open' : 'close'}:${label}`];
        assert(
          JSON.stringify(events) === JSON.stringify(expected),
          `${label}/${initialState}/${retireBeforeNotification}: observe the registration state`,
        );
        if (initialState === 'closing' || initialState === 'closed') {
          channel.onclose?.();
          channel.onerror?.();
          await Promise.resolve();
          assert(events.length === expected.length, 'report terminal registration only once');
        }
        engine.removePeer('peer');
        await engine.pairWith('peer', false, []);
        const replacement = new TestChannel(label);
        replacement.readyState = 'open';
        connection?.ondatachannel?.({ channel: replacement });
        await Promise.resolve();
        assert(events.at(-1) === `open:${label}`, 'a fresh link still opens');
        channel.onclose?.();
        channel.onerror?.();
        assert(events.at(-1) === `open:${label}`, 'old callbacks cannot close the replacement');
        engine.removePeer('peer');
      } finally {
        globalThis.RTCPeerConnection = originalPc;
      }
    }
  }
}
console.error(
  'ok - terminal channel registration reports once and permits fresh-link recovery',
);

// Regression #999: complete remote SDP must not bypass --cripple-ice.
for (const crippled of [false, true]) {
  for (const path of ['offer', 'answer'] as const) {
    for (const newline of ['\r\n', '\n']) {
      const originalPc = globalThis.RTCPeerConnection;
      const applied: RTCSessionDescriptionInit[] = [];
      const candidates: string[] = [];
      class TestConnection {
        async setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> {
          applied.push(description);
        }
        async addIceCandidate(candidate: RTCIceCandidateInit): Promise<void> {
          candidates.push(candidate.candidate ?? '');
        }
        async createAnswer(): Promise<RTCSessionDescriptionInit> {
          return { type: 'answer', sdp: 'local-answer' };
        }
        async setLocalDescription(): Promise<void> {}
        close(): void {}
      }
      try {
        globalThis.RTCPeerConnection = TestConnection as unknown as typeof RTCPeerConnection;
        const engine = new Engine(crippled, {
          onLocalCandidate: () => {},
          onPcState: () => {},
          onChannelOpen: () => {},
          onChannelClosed: () => {},
          onChannelMessage: () => {},
        });
        if (crippled) await engine.handleRemoteCandidate('unpaired', 'x'.repeat(4097));
        await engine.pairWith('peer', false, []);
        await engine.handleRemoteCandidate('peer', 'queued');
        assert(candidates.length === 0, 'candidates wait until remote SDP is applied');
        const lines = [
          'v=0',
          'a=ice-ufrag:example',
          'a=candidate:1 1 udp 1234 192.0.2.1 1234 typ host',
          'm=application 9 UDP/DTLS/SCTP webrtc-datachannel',
          'a=candidate:2 1 udp 1233 192.0.2.2 1235 typ host',
          'a=end-of-candidates',
        ];
        // The final candidate deliberately has no newline.
        const sdp = [...lines, 'a=candidate:3 1 udp 1232 192.0.2.3 1236 typ host'].join(
          newline,
        );
        if (path === 'offer') await engine.handleOffer('peer', sdp);
        else await engine.handleAnswer('peer', sdp);
        const expected = crippled
          ? lines.filter((line) => !line.startsWith('a=candidate:')).join(newline) + newline
          : sdp;
        assert(
          applied.length === 1 && applied[0]?.type === path && applied[0]?.sdp === expected,
          `${path}/${crippled}/${JSON.stringify(newline)}: crippled SDP drops all embedded candidates; healthy SDP stays intact`,
        );
        await engine.handleRemoteCandidate('peer', 'direct');
        assert(
          JSON.stringify(candidates) === JSON.stringify(crippled ? [] : ['queued', 'direct']),
          `${path}/${crippled}: crippled mode drops queued and direct candidates; healthy mode applies both in order`,
        );
        engine.removePeer('peer');
      } finally {
        globalThis.RTCPeerConnection = originalPc;
      }
    }
  }
}
console.error(
  'ok - crippled offer and answer ignore embedded ICE while healthy SDP stays intact',
);

// Establishment diagnostics must separate SDP, trickle ICE, and transport phases.
for (const path of ['offer', 'answer'] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  let connection!: TestConnection;
  let rejectCandidate: ((error: Error) => void) | undefined;
  let holdCandidate = false;
  class TestConnection {
    connectionState = 'connecting';
    signalingState = 'stable';
    iceConnectionState = 'checking';
    iceGatheringState = 'gathering';
    sctp: { state: string; transport: { state: string } } | null = {
      state: 'connecting',
      transport: { state: 'connecting' },
    };
    constructor() {
      connection = this;
    }
    async setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> {
      this.signalingState = description.type === 'offer' ? 'have-remote-offer' : 'stable';
    }
    async addIceCandidate(): Promise<void> {
      if (holdCandidate)
        await new Promise<void>((_resolve, reject) => {
          rejectCandidate = reject;
        });
    }
    createDataChannel(label: string): object {
      return { label, readyState: 'connecting' };
    }
    async createOffer(): Promise<RTCSessionDescriptionInit> {
      return { type: 'offer', sdp: 'local-offer' };
    }
    async createAnswer(): Promise<RTCSessionDescriptionInit> {
      return { type: 'answer', sdp: 'local-answer' };
    }
    async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
      this.signalingState = description.type === 'offer' ? 'have-local-offer' : 'stable';
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = TestConnection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    const snapshot = () => engine.diagnosticSnapshot('peer') as Record<string, unknown>;
    assert(engine.diagnosticSnapshot('peer') === null, 'absent link has no phase snapshot');
    await engine.pairWith('peer', path === 'answer', []);
    let state = snapshot();
    assert(
      state['signalingState'] === (path === 'answer' ? 'have-local-offer' : 'stable') &&
        state['iceGatheringState'] === 'gathering' &&
        state['dtlsState'] === 'connecting' &&
        state['remoteDescriptionSet'] === false &&
        state['remoteCandidateCount'] === 0 &&
        state['pendingRemoteCandidates'] === 0,
      `${path}: snapshot exposes SDP and transport phases before remote negotiation`,
    );
    const candidate = JSON.stringify({ candidate: 'candidate-content', sdpMLineIndex: 0 });
    await engine.handleRemoteCandidate('peer', candidate);
    state = snapshot();
    assert(
      state['remoteCandidateCount'] === 1 && state['pendingRemoteCandidates'] === 1,
      `${path}: snapshot identifies ICE waiting for the remote description`,
    );
    if (path === 'offer') await engine.handleOffer('peer', 'remote-sdp-content');
    else await engine.handleAnswer('peer', 'remote-sdp-content');
    holdCandidate = true;
    const applying = engine.handleRemoteCandidate('peer', candidate);
    state = snapshot();
    assert(
      state['remoteCandidateCount'] === 2 && state['pendingRemoteCandidates'] === 0,
      'an admitted candidate can wait in the stack after leaving the engine queue',
    );
    rejectCandidate?.(new Error('stack rejected the candidate'));
    let rejected = false;
    try {
      await applying;
    } catch {
      rejected = true;
    }
    assert(rejected, 'candidate application reports the stack rejection');
    connection.iceGatheringState = 'complete';
    connection.iceConnectionState = 'connected';
    connection.sctp!.transport.state = 'connected';
    state = snapshot();
    assert(
      state['signalingState'] === 'stable' &&
        state['iceGatheringState'] === 'complete' &&
        state['iceConnectionState'] === 'connected' &&
        state['dtlsState'] === 'connected' &&
        state['sctpState'] === 'connecting' &&
        state['remoteDescriptionSet'] === true &&
        state['remoteCandidateCount'] === 2 &&
        state['pendingRemoteCandidates'] === 0,
      `${path}: snapshot shows completed ICE and DTLS while SCTP still waits`,
    );
    const text = JSON.stringify(state);
    assert(
      !text.includes('candidate-content') && !text.includes('remote-sdp-content'),
      'phase snapshot contains no negotiation payloads',
    );
    connection.sctp = null;
    state = snapshot();
    assert(
      state['dtlsState'] === null && state['sctpState'] === null,
      'unavailable transports remain explicit null states',
    );
    engine.removePeer('peer');
    assert(engine.diagnosticSnapshot('peer') === null, 'retired link has no phase snapshot');
    await engine.pairWith('peer', false, []);
    state = snapshot();
    assert(
      state['generation'] === 2 &&
        state['remoteDescriptionSet'] === false &&
        state['remoteCandidateCount'] === 0 &&
        state['pendingRemoteCandidates'] === 0,
      'replacement diagnostics start a fresh physical negotiation',
    );
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error('ok - browser phase snapshots distinguish SDP, ICE, DTLS, and SCTP progress');

// Regression #853: final states alone hide transient establishment progress.
{
  const originalPc = globalThis.RTCPeerConnection;
  const originalPerformance = globalThis.performance;
  let now = 1000;
  let connection!: Connection;
  class Connection {
    connectionState = 'new';
    signalingState = 'stable';
    iceConnectionState = 'new';
    iceGatheringState = 'new';
    onconnectionstatechange: (() => void) | null = null;
    oniceconnectionstatechange: (() => void) | null = null;
    onicegatheringstatechange: (() => void) | null = null;
    onsignalingstatechange: (() => void) | null = null;
    constructor() {
      connection = this;
    }
    close(): void {
      this.connectionState = 'closed';
      this.onconnectionstatechange?.();
    }
  }
  interface Observation {
    elapsedMs: number;
    connectionState: string;
    signalingState: string;
    iceConnectionState: string;
    iceGatheringState: string;
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    globalThis.performance = { now: () => now } as Performance;
    let failureHistory: Observation[] = [];
    let pcNotifications = 0;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: (_peer, _generation, state) => {
        pcNotifications += 1;
        if (state === 'failed') failureHistory = history();
      },
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    const history = () =>
      (engine.diagnosticSnapshot('peer') as { transportStateHistory: Observation[] })
        .transportStateHistory;
    await engine.pairWith('peer', false, []);
    assert(history().length === 1 && history()[0]?.elapsedMs === 0, 'record initial state');
    now += 5;
    connection.signalingState = 'have-remote-offer';
    connection.onsignalingstatechange?.();
    now += 5;
    connection.iceGatheringState = 'gathering';
    connection.onicegatheringstatechange?.();
    now += 5;
    connection.iceConnectionState = 'checking';
    connection.oniceconnectionstatechange?.();
    assert(pcNotifications === 0, 'history-only events enqueue no orchestrator work');
    now += 5;
    connection.connectionState = 'connecting';
    connection.onconnectionstatechange?.();
    const sequence = history();
    assert(
      sequence.length === 5 &&
        sequence.map((entry) => entry.elapsedMs).join(',') === '0,5,10,15,20' &&
        sequence[1]?.signalingState === 'have-remote-offer' &&
        sequence[2]?.iceGatheringState === 'gathering' &&
        sequence[3]?.iceConnectionState === 'checking' &&
        sequence[4]?.connectionState === 'connecting',
      'retain each observed transport transition with time since this link started',
    );
    now += 5;
    connection.onconnectionstatechange?.();
    connection.oniceconnectionstatechange?.();
    connection.onicegatheringstatechange?.();
    connection.onsignalingstatechange?.();
    assert(history().length === 5, 'repeated events and snapshots do not add observations');
    sequence[0]!.connectionState = 'mutated';
    sequence.pop();
    assert(
      history().length === 5 && history()[0]?.connectionState === 'new',
      'snapshot callers cannot mutate retained history',
    );
    now += 5;
    connection.iceConnectionState = 'connected';
    // A diagnostic can run before the corresponding browser event arrives.
    assert(history()[5]?.elapsedMs === 30, 'snapshot captures a pending state event');
    connection.oniceconnectionstatechange?.();
    assert(history().length === 6, 'later event deduplicates the snapshot observation');
    for (let index = 0; index < 70; index++) {
      now += 1;
      connection.iceConnectionState = index % 2 === 0 ? 'disconnected' : 'connected';
      connection.oniceconnectionstatechange?.();
    }
    const bounded = history();
    assert(JSON.stringify(bounded).length < 7000, 'full history fits the diagnostic limit');
    assert(
      bounded.length === 32 && bounded[0]?.elapsedMs === 69 && bounded[31]?.elapsedMs === 100,
      'retain exactly the newest 32 observations without resetting relative time',
    );
    now += 5;
    connection.connectionState = 'failed';
    connection.onconnectionstatechange?.();
    assert(
      failureHistory.at(-1)?.connectionState === 'failed' &&
        failureHistory.at(-1)?.elapsedMs === 105,
      'failure callback sees the terminal observation before teardown',
    );
    const retired = connection;
    const notificationsBeforeRetirement = pcNotifications;
    engine.removePeer('peer');
    retired.oniceconnectionstatechange?.();
    retired.onicegatheringstatechange?.();
    retired.onsignalingstatechange?.();
    assert(engine.diagnosticSnapshot('peer') === null, 'retired events cannot restore a link');
    now += 100;
    await engine.pairWith('peer', false, []);
    retired.connectionState = 'connected';
    retired.iceConnectionState = 'failed';
    retired.signalingState = 'closed';
    retired.iceGatheringState = 'complete';
    retired.onconnectionstatechange?.();
    retired.oniceconnectionstatechange?.();
    retired.onicegatheringstatechange?.();
    retired.onsignalingstatechange?.();
    assert(
      pcNotifications === notificationsBeforeRetirement &&
        history().length === 1 &&
        history()[0]?.elapsedMs === 0 &&
        history()[0]?.connectionState === 'new' &&
        history()[0]?.iceConnectionState === 'new',
      'replacement history starts fresh and ignores every old transport callback',
    );
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
    globalThis.performance = originalPerformance;
  }
}
console.error(
  'ok - browser transport history preserves bounded, isolated establishment progress',
);

// Regressions #1046/#1051: failure is terminal before application callbacks drain.
for (const failure of [
  'channel-error',
  'channel-close',
  'channel-closing',
  'pc-failed',
  'pc-closed',
] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  let connection!: Connection;
  const observed: string[] = [];
  class Channel {
    readyState = 'open';
    ordered = true;
    maxRetransmits = null;
    maxPacketLifeTime = null;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onclosing: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor(readonly label: string) {}
    close(): void {
      this.readyState = 'closed';
    }
  }
  class Connection {
    connectionState = 'connected';
    onconnectionstatechange: (() => void) | null = null;
    onicecandidate: ((event: { candidate: { toJSON(): object } }) => void) | null = null;
    ondatachannel: ((event: { channel: Channel }) => void) | null = null;
    constructor() {
      connection = this;
    }
    close(): void {}
    async addIceCandidate(): Promise<void> {
      throw new Error('terminal candidate reached stack');
    }
    async setRemoteDescription(): Promise<void> {
      throw new Error('terminal SDP reached stack');
    }
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => observed.push('candidate'),
      onPcState: () => observed.push('pc-state'),
      onChannelOpen: () => observed.push('open'),
      onChannelClosed: () => observed.push('close'),
      onChannelMessage: () => {
        observed.push('message');
      },
    });
    await engine.pairWith('peer', false, []);
    const channel = new Channel(RELIABLE_LABEL);
    connection.ondatachannel?.({ channel });
    await Promise.resolve();
    channel.onmessage?.({ data: 'before-failure' });
    assert(observed.join(',') === 'open,message', `${failure}: live input remains visible`);
    if (failure.startsWith('pc-')) {
      connection.connectionState = failure.slice(3);
      connection.onconnectionstatechange?.();
    } else if (failure === 'channel-error') channel.onerror?.();
    else if (failure === 'channel-closing') {
      channel.readyState = 'closing';
      channel.onclosing?.();
    } else {
      channel.close();
      channel.onclose?.();
    }
    const afterFailure = observed.length;
    channel.onopen?.();
    channel.onmessage?.({ data: 'after-failure' });
    channel.onerror?.();
    channel.onclose?.();
    connection.onicecandidate?.({ candidate: { toJSON: () => ({ candidate: 'late' }) } });
    const late = new Channel(RELIABLE_LABEL);
    connection.ondatachannel?.({ channel: late });
    await Promise.resolve();
    assert(observed.length === afterFailure, `${failure}: terminal input cannot add work`);
    assert(late.readyState === 'closed', `${failure}: terminal announcements close`);
    assert(
      engine.channel('peer', RELIABLE_LABEL) === undefined,
      `${failure}: no terminal sends`,
    );
    for (const operation of [
      () => engine.handleOffer('peer', 'late'),
      () => engine.handleAnswer('peer', 'late'),
      () => engine.handleRemoteCandidate('peer', '{}'),
    ]) {
      let error: unknown;
      try {
        await operation();
      } catch (failure) {
        error = failure;
      }
      assert(
        error instanceof Error && error.message.includes('terminal peer'),
        `${failure}: reject new negotiation before stack calls`,
      );
    }
    engine.removePeer('peer');
    await engine.pairWith('peer', false, []);
    const fresh = new Channel(RELIABLE_LABEL);
    connection.ondatachannel?.({ channel: fresh });
    await Promise.resolve();
    fresh.onmessage?.({ data: 'fresh' });
    assert(
      observed.slice(afterFailure).join(',') === 'open,message',
      `${failure}: fresh link accepts input`,
    );
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error(
  'ok - terminal browser links reject new input, sends, and negotiation until replacement',
);

// Regression #1055, audit #826/#853/#889: close() changes state before failure callbacks.
for (const failure of ['reliable', 'unreliable', 'pc-failed', 'pc-closed'] as const) {
  for (const admission of [
    'send',
    'message',
    'open',
    'candidate',
    'announcement',
    'offer',
    'answer',
    'remote-candidate',
  ] as const) {
    const description = `${failure}/${admission}`;
    const originalPc = globalThis.RTCPeerConnection;
    let connection!: Connection;
    const observed: string[] = [];
    class Channel {
      readyState = 'open';
      maxPacketLifeTime = null;
      readonly ordered: boolean;
      readonly maxRetransmits: number | null;
      onopen: (() => void) | null = null;
      onmessage: ((event: { data: string }) => void) | null = null;
      onclosing: (() => void) | null = null;
      onclose: (() => void) | null = null;
      onerror: (() => void) | null = null;
      constructor(readonly label: string) {
        this.ordered = label === RELIABLE_LABEL;
        this.maxRetransmits = this.ordered ? null : 0;
      }
      close(): void {
        // Match RTCDataChannel.close(): state changes now, event arrives later.
        this.readyState = 'closing';
      }
    }
    class Connection {
      connectionState = 'connected';
      onconnectionstatechange: (() => void) | null = null;
      ondatachannel: ((event: { channel: Channel }) => void) | null = null;
      onicecandidate: ((event: { candidate: { toJSON(): object } }) => void) | null = null;
      constructor() {
        connection = this;
      }
      close(): void {
        this.connectionState = 'closed';
      }
      async setRemoteDescription(): Promise<void> {
        observed.push('stack');
      }
      async addIceCandidate(): Promise<void> {
        observed.push('stack');
      }
      async createAnswer(): Promise<{ sdp: string }> {
        observed.push('stack');
        return { sdp: 'answer' };
      }
      async setLocalDescription(): Promise<void> {
        observed.push('stack');
      }
    }
    try {
      globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
      const engine = new Engine(false, {
        onLocalCandidate: () => observed.push('candidate'),
        onPcState: () => observed.push('loss'),
        onChannelClosed: () => observed.push('loss'),
        onChannelOpen: () => observed.push('open'),
        onChannelMessage: () => {
          observed.push('message');
        },
      });
      await engine.pairWith('peer', false, []);
      const channels = [new Channel(RELIABLE_LABEL), new Channel(UNRELIABLE_LABEL)];
      for (const channel of channels) connection.ondatachannel?.({ channel });
      await Promise.resolve();
      for (const channel of channels) engine.noteChannelOpen('peer', channel.label);
      observed.length = 0;
      if (failure.startsWith('pc-')) connection.connectionState = failure.slice(3);
      else channels.find((channel) => channel.label === failure)?.close();
      const live = channels.find((channel) => channel.label !== failure) ?? channels[0]!;
      const late = new Channel(RELIABLE_LABEL);
      let rejected = false;
      try {
        switch (admission) {
          case 'send':
            assert(
              engine.channel('peer', live.label) === undefined,
              `${description}: fence send before callback`,
            );
            break;
          case 'message':
            live.onmessage?.({ data: 'late gameplay' });
            break;
          case 'open':
            engine.noteChannelOpen('peer', live.label);
            break;
          case 'candidate':
            connection.onicecandidate?.({
              candidate: { toJSON: () => ({ candidate: 'late' }) },
            });
            break;
          case 'announcement':
            connection.ondatachannel?.({ channel: late });
            break;
          case 'offer':
            await engine.handleOffer('peer', 'late');
            break;
          case 'answer':
            await engine.handleAnswer('peer', 'late');
            break;
          case 'remote-candidate':
            await engine.handleRemoteCandidate('peer', '{}');
            break;
        }
      } catch (error) {
        if (!(error instanceof Error) || !error.message.includes('terminal peer')) throw error;
        rejected = true;
      }
      assert(
        observed.join(',') === 'loss',
        `${description}: observe loss once before admitting work; got ${observed}`,
      );
      assert(
        rejected === ['offer', 'answer', 'remote-candidate'].includes(admission),
        `${description}: reject negotiation`,
      );
      if (admission === 'announcement')
        assert(late.readyState === 'closing', `${description}: close late channel`);
      for (const channel of channels) {
        channel.onclosing?.();
        channel.onclose?.();
        channel.onerror?.();
        channel.onmessage?.({ data: 'late gameplay' });
      }
      connection.onconnectionstatechange?.();
      assert(
        observed.join(',') === 'loss',
        `${description}: queued callbacks cannot duplicate loss`,
      );
      const oldConnection = connection;
      engine.removePeer('peer');
      await engine.pairWith('peer', false, []);
      const fresh = new Channel(RELIABLE_LABEL);
      connection.ondatachannel?.({ channel: fresh });
      await Promise.resolve();
      oldConnection.onconnectionstatechange?.();
      for (const channel of channels) channel.onclosing?.();
      fresh.onmessage?.({ data: 'fresh gameplay' });
      assert(
        observed.join(',') === 'loss,open,message',
        `${description}: fresh generation survives old callbacks`,
      );
      engine.removePeer('peer');
    } finally {
      globalThis.RTCPeerConnection = originalPc;
    }
  }
}
console.error('ok - live terminal state fences browser work before queued failure callbacks');

for (const path of ['pair', 'offer', 'answer'] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  let connection!: Connection;
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  const stack: string[] = [];
  let losses = 0;
  class Connection {
    connectionState = 'connecting';
    constructor() {
      connection = this;
    }
    createDataChannel(label: string): object {
      return { label, readyState: 'connecting' };
    }
    async createOffer(): Promise<{ sdp: string }> {
      stack.push('offer');
      await blocked;
      return { sdp: 'offer' };
    }
    async setRemoteDescription(): Promise<void> {
      stack.push('remote');
      await blocked;
    }
    async setLocalDescription(): Promise<void> {
      stack.push('local');
    }
    async addIceCandidate(): Promise<void> {
      stack.push('candidate');
    }
    async createAnswer(): Promise<{ sdp: string }> {
      stack.push('answer');
      return { sdp: 'answer' };
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {
        losses += 1;
      },
      onChannelClosed: () => {
        losses += 1;
      },
      onChannelOpen: () => {},
      onChannelMessage: () => {},
    });
    if (path !== 'pair') {
      await engine.pairWith('peer', false, []);
      await engine.handleRemoteCandidate('peer', '{}');
    }
    const applying =
      path === 'pair'
        ? engine.pairWith('peer', true, [])
        : path === 'offer'
          ? engine.handleOffer('peer', 'offer')
          : engine.handleAnswer('peer', 'answer');
    connection.connectionState = 'failed';
    release();
    let error: unknown;
    try {
      await applying;
    } catch (failure) {
      error = failure;
    }
    assert(
      error instanceof Error && error.message.includes('terminated'),
      `${path}: reject resumed terminal negotiation`,
    );
    assert(
      stack.length === 1 && losses === 1,
      `${path}: no later SDP or candidate work before failure callback`,
    );
    engine.removePeer('peer');
  } finally {
    release();
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error(
  'ok - resumed browser negotiations observe live failure before browser callbacks',
);

// Failure observers may synchronously inspect and replace their physical link.
{
  const originalPc = globalThis.RTCPeerConnection;
  let connection!: Connection;
  let engine!: Engine;
  let losses = 0;
  class Connection {
    connectionState = 'connected';
    constructor() {
      connection = this;
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {
        losses += 1;
        assert(
          engine.channel('peer', RELIABLE_LABEL) === undefined,
          'fence before reentrant observer',
        );
        engine.removePeer('peer');
        void engine.pairWith('peer', false, []);
      },
      onChannelClosed: () => {},
      onChannelOpen: () => {},
      onChannelMessage: () => {},
    });
    await engine.pairWith('peer', false, []);
    const generation = engine.generationOf('peer');
    connection.connectionState = 'closed';
    assert(
      engine.channel('peer', RELIABLE_LABEL) === undefined,
      'old lookup cannot expose replacement',
    );
    assert(
      losses === 1 && engine.generationOf('peer') !== generation,
      'observer replaces terminal generation once',
    );
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error('ok - browser live-state fencing permits reentrant failure observers');

// Investigation #853/#889: rejected early ICE must preserve SDP, binding, and FIFO.
for (const path of ['offer', 'answer'] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalError = console.error;
  const calls: string[] = [];
  const applied: RTCIceCandidateInit[] = [];
  let releaseRemote!: () => void;
  let releaseCandidate!: () => void;
  const remoteHeld = new Promise<void>((resolve) => {
    releaseRemote = resolve;
  });
  const candidateHeld = new Promise<void>((resolve) => {
    releaseCandidate = resolve;
  });
  const first = {
    candidate: 'candidate:1 1 udp 1234 192.0.2.1 1234 typ host',
    sdpMid: 'data',
    sdpMLineIndex: 0,
    usernameFragment: 'remote-ufrag',
  };
  class Connection {
    createDataChannel(label: string): object {
      return { label, readyState: 'connecting' };
    }
    async createOffer(): Promise<RTCSessionDescriptionInit> {
      return { type: 'offer', sdp: 'local-offer' };
    }
    async setRemoteDescription(description: RTCSessionDescriptionInit): Promise<void> {
      calls.push(`remote:${description.type}`);
      await remoteHeld;
      calls.push('remote:complete');
    }
    async addIceCandidate(candidate: RTCIceCandidateInit): Promise<void> {
      applied.push(candidate);
      calls.push(`candidate:${applied.length}`);
      if (applied.length === 1) {
        await candidateHeld;
        throw new Error('one candidate is not usable');
      }
    }
    async createAnswer(): Promise<RTCSessionDescriptionInit> {
      calls.push('answer');
      return { type: 'answer', sdp: 'local-answer' };
    }
    async setLocalDescription(): Promise<void> {
      calls.push('local');
    }
    close(): void {}
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    console.error = () => {};
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    await engine.pairWith('peer', path === 'answer', []);
    calls.length = 0;
    await engine.handleRemoteCandidate('peer', JSON.stringify(first));
    await engine.handleRemoteCandidate('peer', 'bare-candidate');
    const negotiating =
      path === 'offer'
        ? engine.handleOffer('peer', 'remote-offer')
        : engine.handleAnswer('peer', 'remote-answer');
    await Promise.resolve();
    assert(calls.join(',') === `remote:${path}`, `${path}: ICE waits for remote SDP`);
    await engine.handleRemoteCandidate('peer', 'during-sdp');
    assert(applied.length === 0, `${path}: ICE arriving during remote SDP also waits`);
    releaseRemote();
    await Promise.resolve();
    await Promise.resolve();
    assert(
      calls.join(',') === `remote:${path},remote:complete,candidate:1`,
      `${path}: later candidates and answer wait for the first candidate`,
    );
    releaseCandidate();
    const answer = await negotiating;
    assert(
      JSON.stringify(applied) ===
        JSON.stringify([
          first,
          { candidate: 'bare-candidate', sdpMLineIndex: 0 },
          { candidate: 'during-sdp', sdpMLineIndex: 0 },
        ]),
      `${path}: retain exact media binding and apply early candidates in arrival order`,
    );
    assert(
      calls.join(',') ===
        `remote:${path},remote:complete,candidate:1,candidate:2,candidate:3${path === 'offer' ? ',answer,local' : ''}`,
      `${path}: rejected early ICE does not interrupt negotiation`,
    );
    assert(
      answer === (path === 'offer' ? 'local-answer' : undefined),
      `${path}: offer produces the answer after candidate rejection`,
    );
    await engine.handleRemoteCandidate('peer', JSON.stringify(first));
    assert(applied.length === 4, `${path}: later trickle ICE still reaches the same link`);
    engine.removePeer('peer');
  } finally {
    releaseRemote();
    releaseCandidate();
    globalThis.RTCPeerConnection = originalPc;
    console.error = originalError;
  }
}
console.error(
  'ok - early ICE preserves media bindings, order, and SDP after candidate rejection',
);

// Issues #853/#889: separate raw browser callbacks from Engine admission.
for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
  for (const rejection of ['size', 'type', 'backlog', 'terminal', 'live-terminal'] as const) {
    const originalPc = globalThis.RTCPeerConnection;
    let connection!: Connection;
    let admit = true;
    let admitted = 0;
    let losses = 0;
    let lossSnapshot: ReturnType<typeof snapshot> | null = null;
    class Channel {
      readonly id = 17;
      readonly ordered = label === RELIABLE_LABEL;
      readonly maxRetransmits = this.ordered ? null : 0;
      readonly maxPacketLifeTime = null;
      readyState = 'open';
      bufferedAmount = 0;
      onopen: (() => void) | null = null;
      onmessage: ((event: { data: unknown }) => void) | null = null;
      onerror:
        ((event?: { error?: { errorDetail: string; sctpCauseCode: number } }) => void) | null =
        null;
      constructor(readonly label: string) {}
      close() {
        this.readyState = 'closed';
      }
    }
    class Connection {
      connectionState = 'connected';
      ondatachannel: ((event: { channel: Channel }) => void) | null = null;
      constructor() {
        connection = this;
      }
      close() {}
    }
    interface Receipts {
      raw: number;
      admitted: number;
      rejected: number;
      discarded: number;
    }
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {
        losses += 1;
        lossSnapshot = snapshot();
      },
      onChannelMessage: () => {
        if (!admit) return false;
        admitted += 1;
        return true;
      },
    });
    const snapshot = () =>
      engine.diagnosticSnapshot('peer') as {
        channels: Record<string, { id: number; receipts: Receipts; error: object | null }>;
      };
    try {
      globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
      await engine.pairWith('peer', false, []);
      const channel = new Channel(label);
      connection.ondatachannel?.({ channel });
      await Promise.resolve();
      assert(snapshot().channels[label]?.id === 17, `${label}: retain SCTP stream ID`);
      assert(
        JSON.stringify(snapshot().channels[label]?.receipts) ===
          JSON.stringify({ raw: 0, admitted: 0, rejected: 0, discarded: 0 }),
        `${label}: fresh registration has no receipt evidence`,
      );
      channel.onmessage?.({ data: 'valid' });
      channel.onmessage?.({ data: new TextEncoder().encode('valid binary').buffer });
      assert(admitted === 2, `${label}: both input forms reach the application`);
      const copied = snapshot().channels[label]!.receipts;
      copied.raw = 999;
      assert(
        snapshot().channels[label]?.receipts.raw === 2,
        'snapshots cannot change retained counters',
      );
      if (rejection === 'terminal') {
        channel.onerror?.({ error: { errorDetail: 'sctp-failure', sctpCauseCode: 12 } });
        assert(
          JSON.stringify(snapshot().channels[label]?.error) ===
            JSON.stringify({ errorDetail: 'sctp-failure', sctpCauseCode: 12 }),
          'retain structured SCTP failure before loss callback tears down the link',
        );
      } else if (rejection === 'backlog') admit = false;
      else if (rejection === 'live-terminal') channel.readyState = 'closed';
      channel.onmessage?.({
        data:
          rejection === 'size' ? 'x'.repeat(65537) : rejection === 'type' ? {} : 'not admitted',
      });
      assert(
        JSON.stringify(snapshot().channels[label]?.receipts) ===
          JSON.stringify({
            raw: 3,
            admitted: 2,
            rejected: rejection.endsWith('terminal') ? 0 : 1,
            discarded: rejection.endsWith('terminal') ? 1 : 0,
          }),
        `${label}/${rejection}: distinguish callbacks, admissions, validation/backlog rejection, and terminal discard`,
      );
      assert(losses === 1, `${label}/${rejection}: preserve one terminal failure`);
      const atLoss = (lossSnapshot as ReturnType<typeof snapshot> | null)?.channels[label];
      assert(
        atLoss?.receipts.raw === (rejection === 'terminal' ? 2 : 3) &&
          atLoss?.receipts.rejected === (rejection.endsWith('terminal') ? 0 : 1) &&
          atLoss?.receipts.discarded === (rejection === 'live-terminal' ? 1 : 0),
        `${label}/${rejection}: loss observer sees the triggering receipt before teardown`,
      );
      if (rejection === 'terminal') {
        assert(
          JSON.stringify(atLoss?.error) ===
            JSON.stringify({ errorDetail: 'sctp-failure', sctpCauseCode: 12 }),
          'loss observer sees the SCTP error before teardown',
        );
      }
      const retired = channel;
      engine.removePeer('peer');
      await engine.pairWith('peer', false, []);
      const fresh = new Channel(label);
      connection.ondatachannel?.({ channel: fresh });
      await Promise.resolve();
      retired.onmessage?.({ data: 'retired' });
      retired.onerror?.({ error: { errorDetail: 'sctp-failure', sctpCauseCode: 13 } });
      assert(
        snapshot().channels[label]?.receipts.raw === 0 &&
          snapshot().channels[label]?.error === null,
        `${label}: old callbacks cannot contaminate replacement evidence`,
      );
      admit = true;
      fresh.onmessage?.({ data: 'fresh' });
      assert(
        snapshot().channels[label]?.receipts.admitted === 1,
        `${label}: replacement counts start fresh`,
      );
    } finally {
      engine.removePeer('peer');
      globalThis.RTCPeerConnection = originalPc;
    }
  }
}
console.error(
  'ok - browser diagnostics distinguish raw receipt, admission, rejection, and SCTP failure',
);

/* Regression #1157: preserve normalized byte charges without allocating
 * another payload buffer for every gameplay receipt. */
{
  const originalPc = globalThis.RTCPeerConnection;
  const OriginalEncoder = globalThis.TextEncoder;
  const OriginalDecoder = globalThis.TextDecoder;
  const OriginalArray = globalThis.Uint8Array;
  const encoder = new OriginalEncoder();
  const values: Array<string | ArrayBuffer> = [
    '',
    'movement',
    '\ufeff雪🌊',
    '\ud800',
    '\udfff',
    ...[128, 512, 2048, 8192, 32768].map((size) => 'x'.repeat(size)),
    'x'.repeat(65536),
    'x'.repeat(65537),
    'é'.repeat(32768),
    'é'.repeat(32769),
    'x'.repeat(65532) + '🌊',
    'x'.repeat(65533) + '🌊',
    'x'.repeat(65533) + '\ud800',
    'x'.repeat(65534) + '\ud800',
    Uint8Array.of(0xef, 0xbb, 0xbf, 0x61).buffer,
    // Each message is a complete value: truncated UTF-8 must not continue in
    // the next receipt, and a prior BOM must not affect the next message.
    Uint8Array.of(0xc3).buffer,
    Uint8Array.of(0xa9).buffer,
    Uint8Array.of(0xef, 0xbb, 0xbf, 0x62).buffer,
    new Uint8Array(21845).fill(0x80).buffer,
    new Uint8Array(21846).fill(0x80).buffer,
  ];
  for (const text of values.filter((value): value is string => typeof value === 'string')) {
    values.push(encoder.encode(text).buffer);
  }
  const fixtures = values.map((data) => {
    const text =
      typeof data === 'string'
        ? data
        : new TextDecoder('utf-8', { ignoreBOM: true }).decode(data);
    const bytes = encoder.encode(text).byteLength;
    return { data, text, bytes, accepted: bytes <= 65536 };
  });
  let payloadBuffers = 0;
  let encodedBytes = 0;
  let decoders = 0;
  const scratchSizes: number[] = [];
  class CountingEncoder extends OriginalEncoder {
    override encode(input?: string): Uint8Array<ArrayBuffer> {
      const result = super.encode(input);
      payloadBuffers += 1;
      encodedBytes += result.byteLength;
      return result;
    }
  }
  class CountingDecoder extends OriginalDecoder {
    constructor(label?: string, options?: TextDecoderOptions) {
      super(label, options);
      decoders += 1;
    }
  }
  let connection!: Connection;
  class Channel {
    readyState = 'open';
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    readonly maxPacketLifeTime = null;
    onmessage: ((event: { data: unknown }) => void) | null = null;
    constructor(readonly label: string) {
      this.ordered = label === RELIABLE_LABEL;
      this.maxRetransmits = this.ordered ? null : 0;
    }
    close() {
      this.readyState = 'closed';
    }
  }
  class Connection {
    connectionState = 'connected';
    ondatachannel: ((event: { channel: Channel }) => void) | null = null;
    constructor() {
      connection = this;
    }
    close() {}
  }
  try {
    globalThis.TextEncoder = CountingEncoder;
    globalThis.TextDecoder = CountingDecoder;
    globalThis.Uint8Array = new Proxy(OriginalArray, {
      construct(target, argumentsList, newTarget) {
        if (typeof argumentsList[0] === 'number') scratchSizes.push(argumentsList[0]);
        return Reflect.construct(target, argumentsList, newTarget);
      },
    });
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      let received: Array<{ text: string; bytes: number }> = [];
      let closed = 0;
      let largestBytes = 0;
      const firstScratch = scratchSizes.length;
      const firstDecoder = decoders;
      const engine = new Engine(false, {
        onLocalCandidate: () => {},
        onPcState: () => {},
        onChannelOpen: () => {},
        onChannelClosed: () => {
          closed += 1;
        },
        onChannelMessage: (_peer, _generation, _label, text, bytes) => {
          received.push({ text, bytes });
        },
      });
      for (const fixture of fixtures) {
        received = [];
        closed = 0;
        await engine.pairWith('peer', false, []);
        const channel = new Channel(label);
        connection.ondatachannel?.({ channel });
        const beforeScratch = scratchSizes.length;
        const beforeDecoders = decoders;
        const repeats = fixture.accepted ? 3 : 1;
        for (let repeat = 0; repeat < repeats; repeat += 1) {
          channel.onmessage?.({ data: fixture.data });
        }
        if (typeof fixture.data === 'string') {
          assert(decoders === beforeDecoders, 'text receipts do not create a binary decoder');
        }
        if (fixture.text.length === 0) {
          assert(scratchSizes.length === beforeScratch, 'empty receipts need no sizing buffer');
        }
        assert(
          received.length === (fixture.accepted ? repeats : 0),
          `${label}/${typeof fixture.data}/${fixture.bytes}: preserve admission boundary`,
        );
        if (fixture.accepted) {
          assert(
            received.every(
              ({ text, bytes }) => text === fixture.text && bytes === fixture.bytes,
            ),
            `${label}/${fixture.bytes}: preserve text normalization and exact backlog charge`,
          );
          assert(closed === 0, 'accepted gameplay keeps its link usable');
          assert(
            scratchSizes.length - beforeScratch <= 1,
            'repeated receipts reuse sizing storage',
          );
          largestBytes = Math.max(largestBytes, fixture.bytes);
          const retainedCapacity =
            scratchSizes.length === firstScratch ? 0 : scratchSizes.at(-1)!;
          assert(
            retainedCapacity <= Math.max(256, Math.min(65536, largestBytes * 6)),
            'small gameplay must not reserve the maximum payload buffer',
          );
        } else {
          assert(closed === 1, 'oversized normalized gameplay closes the pair once');
        }
        engine.removePeer('peer');
      }
      assert(
        decoders - firstDecoder === 1,
        `binary receipt decoding constructed ${decoders - firstDecoder} decoders for one engine`,
      );
      const sizes = scratchSizes.slice(firstScratch);
      assert(sizes.length <= 9, 'sizing storage has a bounded number of growth steps');
      assert(
        sizes.every(
          (size, index) => size <= 65536 && (index === 0 || size > sizes[index - 1]!),
        ),
        'sizing storage grows monotonically and remains bounded across peer replacement',
      );
    }
    assert(
      payloadBuffers === 0,
      `receive sizing allocated ${payloadBuffers} payload buffers (${encodedBytes} bytes)`,
    );
  } finally {
    globalThis.TextEncoder = OriginalEncoder;
    globalThis.TextDecoder = OriginalDecoder;
    globalThis.Uint8Array = OriginalArray;
    globalThis.RTCPeerConnection = originalPc;
  }
}
console.error(
  'ok - receive sizing preserves UTF-8 boundaries without per-message payload buffers',
);

// #853/#889: capture the failed initial link before cleanup removes its history.
for (const failureStage of [
  'create_reliable_channel',
  'create_unreliable_channel',
  'create_offer',
  'set_local_description',
  'validate_offer',
  'retired_offer',
  'retired_local_description',
  'diagnostic_failure',
  'snapshot_failure',
] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalError = console.error;
  const diagnostics: string[] = [];
  const connections: Connection[] = [];
  const failure = new Error('original stack failure');
  let rejectOperation!: (error: Error) => void;
  class Connection {
    state = 'connecting';
    stateUnavailable = false;
    get connectionState(): string {
      if (this.stateUnavailable) throw new Error('browser state unavailable');
      return this.state;
    }
    signalingState = 'stable';
    iceConnectionState = 'checking';
    iceGatheringState = 'gathering';
    closed = false;
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string): object {
      if (failureStage === `create_${label}_channel`) throw failure;
      return { label, readyState: 'connecting', bufferedAmount: 0, id: null };
    }
    async createOffer(): Promise<RTCSessionDescriptionInit> {
      if (failureStage === 'retired_offer')
        return new Promise((_resolve, reject) => {
          rejectOperation = reject;
        });
      if (failureStage === 'snapshot_failure') {
        this.stateUnavailable = true;
        throw failure;
      }
      if (failureStage === 'create_offer' || failureStage === 'diagnostic_failure')
        throw failure;
      return failureStage === 'validate_offer'
        ? { type: 'offer' }
        : { type: 'offer', sdp: 'private SDP' };
    }
    async setLocalDescription(): Promise<void> {
      this.signalingState = 'have-local-offer';
      if (failureStage === 'retired_local_description')
        return new Promise((_resolve, reject) => {
          rejectOperation = reject;
        });
      if (failureStage === 'set_local_description') throw failure;
    }
    close(): void {
      this.closed = true;
      this.state = 'closed';
    }
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    console.error = (line: string) => {
      if (failureStage === 'diagnostic_failure')
        throw new Error('diagnostic output unavailable');
      diagnostics.push(line);
    };
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    const pending = engine.pairWith('peer', true, []);
    if (failureStage.startsWith('retired_')) {
      await Promise.resolve();
      engine.removePeer('peer');
      await engine.pairWith('peer', false, []);
      rejectOperation(failure);
    }
    let rejected: unknown;
    try {
      await pending;
    } catch (error) {
      rejected = error;
    }
    assert(
      failureStage === 'validate_offer'
        ? rejected instanceof Error && rejected.message.includes('produced no SDP')
        : rejected === failure,
      `${failureStage}: cleanup preserves the original failure`,
    );
    assert(connections[0]?.closed === true, `${failureStage}: failed link closes`);
    if (failureStage.startsWith('retired_')) {
      await Promise.resolve();
      assert(diagnostics.length === 0, 'late rejection cannot snapshot the replacement');
      assert(
        engine.isPaired('peer') && connections[1]?.closed === false,
        'late rejection preserves replacement',
      );
    } else if (failureStage === 'diagnostic_failure' || failureStage === 'snapshot_failure') {
      assert(!engine.isPaired('peer'), 'failed diagnostics still forget the failed link');
      await engine.pairWith('peer', false, []);
      assert(engine.isPaired('peer'), 'failed diagnostics do not block fresh pairing');
    } else {
      assert(!engine.isPaired('peer'), `${failureStage}: failed link is forgotten`);
      assert(diagnostics.length === 1, `${failureStage}: retain one pre-cleanup diagnostic`);
      const line = diagnostics[0]!;
      const snapshot = JSON.parse(line.slice(line.indexOf('{'))) as {
        phase: string;
        link: {
          generation: number;
          connectionState: string;
          signalingState: string;
          transportStateHistory: Array<{ connectionState: string }>;
          channels: Record<string, unknown>;
        };
      };
      assert(
        snapshot.phase === failureStage,
        `${failureStage}: identify the failing browser operation`,
      );
      assert(
        snapshot.link.generation === 1 && snapshot.link.connectionState === 'connecting',
        `${failureStage}: snapshot the failed link before close`,
      );
      assert(
        snapshot.link.transportStateHistory.at(-1)?.connectionState === 'connecting',
        `${failureStage}: preserve pre-close transport history`,
      );
      assert(
        snapshot.link.signalingState ===
          (['set_local_description', 'validate_offer'].includes(failureStage)
            ? 'have-local-offer'
            : 'stable'),
        `${failureStage}: retain SDP progress`,
      );
      assert(
        (snapshot.link.channels['reliable'] !== null) ===
          (failureStage !== 'create_reliable_channel'),
        `${failureStage}: retain first-channel creation progress`,
      );
      assert(
        (snapshot.link.channels['unreliable'] !== null) ===
          !['create_reliable_channel', 'create_unreliable_channel'].includes(failureStage),
        `${failureStage}: retain second-channel creation progress`,
      );
      assert(!line.includes('private SDP'), `${failureStage}: snapshot excludes SDP`);
      await engine.pairWith('peer', false, []);
      assert(engine.isPaired('peer'), `${failureStage}: fresh pairing remains possible`);
    }
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
    console.error = originalError;
  }
}
console.error(
  'ok - initial browser negotiation failures retain operation and link evidence before cleanup',
);

// Investigations #853/#889: distinguish rejected ICE from missing channel DATA.
for (const path of ['offer', 'answer', 'trickle'] as const) {
  for (const retireWhilePending of [false, true]) {
    const originalPc = globalThis.RTCPeerConnection;
    const originalError = console.error;
    let rejection: unknown = new DOMException('candidate:private-address', 'OperationError');
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let rejectCandidate = true;
    class Connection {
      createDataChannel(label: string): object {
        return { label, readyState: 'connecting' };
      }
      async createOffer(): Promise<RTCSessionDescriptionInit> {
        return { type: 'offer', sdp: 'offer' };
      }
      async createAnswer(): Promise<RTCSessionDescriptionInit> {
        return { type: 'answer', sdp: 'answer' };
      }
      async setRemoteDescription(): Promise<void> {}
      async setLocalDescription(): Promise<void> {}
      async addIceCandidate(): Promise<void> {
        if (rejectCandidate) {
          await held;
          throw rejection;
        }
      }
      close(): void {}
    }
    const engine = new Engine(false, {
      onLocalCandidate: () => {},
      onPcState: () => {},
      onChannelOpen: () => {},
      onChannelClosed: () => {},
      onChannelMessage: () => {},
    });
    const snapshot = () =>
      engine.diagnosticSnapshot('peer') as {
        iceCandidateFailures: {
          count: number;
          last: { source: string; name: string | null } | null;
        };
      };
    try {
      globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
      console.error = () => {};
      await engine.pairWith('peer', path === 'answer', []);
      if (path === 'trickle') await engine.handleAnswer('peer', 'remote-answer');
      else await engine.handleRemoteCandidate('peer', 'early-candidate');
      const operation =
        path === 'offer'
          ? engine.handleOffer('peer', 'remote-offer')
          : path === 'answer'
            ? engine.handleAnswer('peer', 'remote-answer')
            : engine.handleRemoteCandidate('peer', 'later-candidate');
      const outcome = operation.then(
        () => null,
        (error: unknown) => error,
      );
      await Promise.resolve();
      await Promise.resolve();
      if (retireWhilePending) {
        engine.removePeer('peer');
        await engine.pairWith('peer', false, []);
      }
      release();
      const failure = await outcome;
      assert(
        path === 'trickle' ? failure === rejection : retireWhilePending || failure === null,
        `${path}: preserve original trickle rejection and buffered recovery`,
      );
      const evidence = snapshot().iceCandidateFailures;
      assert(
        evidence.count === (retireWhilePending ? 0 : 1),
        `${path}: failure evidence belongs only to its physical link`,
      );
      assert(
        JSON.stringify(evidence.last) ===
          JSON.stringify(
            retireWhilePending
              ? null
              : { source: path === 'trickle' ? 'trickle' : 'buffered', name: 'OperationError' },
          ),
        `${path}: retain browser error type and candidate application phase`,
      );
      assert(
        !JSON.stringify(snapshot()).includes('private-address'),
        `${path}: error text and candidate contents stay outside the snapshot`,
      );
      if (!retireWhilePending) {
        const detached = snapshot().iceCandidateFailures;
        if (detached.last !== null) detached.last.name = 'changed';
        assert(
          snapshot().iceCandidateFailures.last?.name === 'OperationError',
          'copy evidence',
        );
      }
      rejectCandidate = false;
      await engine.handleAnswer('peer', 'remote-answer');
      let failures = retireWhilePending ? 0 : 1;
      const longName = new Error('candidate:private-address');
      longName.name = 'x'.repeat(128);
      for (const error of [longName, 'opaque candidate rejection']) {
        rejection = error;
        rejectCandidate = true;
        const observed = await engine.handleRemoteCandidate('peer', 'failed-candidate').then(
          () => null,
          (failure: unknown) => failure,
        );
        failures += 1;
        const evidence = snapshot().iceCandidateFailures;
        assert(observed === error, `${path}: keep each original rejection`);
        assert(evidence.count === failures, `${path}: count repeated candidate failures`);
        assert(evidence.last?.source === 'trickle', `${path}: keep the latest failure source`);
        assert(
          evidence.last?.name === (error === longName ? 'x'.repeat(64) : null),
          `${path}: bound error names and tolerate untyped rejections`,
        );
      }
      rejectCandidate = false;
      await engine.handleRemoteCandidate('peer', 'working-candidate');
      assert(
        snapshot().iceCandidateFailures.count === failures,
        `${path}: successful later ICE preserves the rejection count`,
      );
      engine.removePeer('peer');
      await engine.pairWith('peer', false, []);
      assert(snapshot().iceCandidateFailures.count === 0, `${path}: fresh link starts clean`);
      engine.removePeer('peer');
    } finally {
      release();
      globalThis.RTCPeerConnection = originalPc;
      console.error = originalError;
    }
  }
}
console.error('ok - browser ICE failure evidence preserves recovery and physical ownership');

// A queued datachannel event can outlive removal and replacement of its peer link.
for (const replace of [false, true]) {
  const originalPc = globalThis.RTCPeerConnection;
  const connections: LateConnection[] = [];
  const callbacks: string[] = [];
  class LateConnection {
    connectionState = 'new';
    ondatachannel: ((event: { channel: LateChannel }) => void) | null = null;
    constructor() {
      connections.push(this);
    }
    close(): void {
      this.connectionState = 'closed';
    }
  }
  class LateChannel {
    label = RELIABLE_LABEL;
    ordered = true;
    maxRetransmits = null;
    maxPacketLifeTime = null;
    readyState = 'open';
    onmessage = null;
    close(): void {
      this.readyState = 'closed';
    }
  }
  try {
    globalThis.RTCPeerConnection = LateConnection as unknown as typeof RTCPeerConnection;
    const engine = new Engine(false, {
      onLocalCandidate: () => callbacks.push('candidate'),
      onPcState: () => callbacks.push('state'),
      onChannelOpen: () => callbacks.push('open'),
      onChannelClosed: () => callbacks.push('closed'),
      onChannelMessage: () => {
        callbacks.push('message');
      },
    });
    await engine.pairWith('peer', false, []);
    const old = connections[0];
    assert(old !== undefined, 'create original peer');
    engine.removePeer('peer');
    if (replace) await engine.pairWith('peer', false, []);
    const channel = new LateChannel();
    const before = callbacks.length;
    old?.ondatachannel?.({ channel });
    await Promise.resolve();
    assert(channel.readyState === 'closed', `replacement=${replace}: close late announcement`);
    assert(channel.onmessage === null, 'do not register late channel handlers');
    assert(callbacks.length === before, 'late announcement emits no callbacks');
    assert(
      engine.channel('peer', RELIABLE_LABEL) === undefined,
      'replacement has no old channel',
    );
    engine.removePeer('peer');
  } finally {
    globalThis.RTCPeerConnection = originalPc;
  }
}
