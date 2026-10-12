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
