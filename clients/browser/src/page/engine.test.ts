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
