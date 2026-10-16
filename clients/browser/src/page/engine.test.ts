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
