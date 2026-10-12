import { build } from 'esbuild';
import { chromium } from 'playwright-core';

const bundle = await build({
  entryPoints: ['src/page/engine.ts'],
  bundle: true,
  format: 'iife',
  globalName: 'channelEngine',
  write: false,
});
const browser = await chromium.launch({
  headless: true,
  args: ['--disable-features=WebRtcHideLocalIpsWithMdns'],
});
try {
  const page = await browser.newPage();
  await page.addScriptTag({ content: bundle.outputFiles[0].text });
  const cases = [
    ['reliable-unordered', 'reliable', { ordered: false }],
    ['reliable-retransmit-limit', 'reliable', { maxRetransmits: 0 }],
    ['reliable-lifetime-limit', 'reliable', { maxPacketLifeTime: 0 }],
    ['unreliable-ordered', 'unreliable', { ordered: true, maxRetransmits: 0 }],
    ['unreliable-reliable', 'unreliable', { ordered: false }],
    ['unreliable-retransmits', 'unreliable', { ordered: false, maxRetransmits: 1 }],
    ['unreliable-lifetime', 'unreliable', { ordered: false, maxPacketLifeTime: 0 }],
  ];
  for (const [name, label, options] of cases) {
    const evidence = await page.evaluate(
      async ({ name, label, options }) => {
        // Regression #911: use real DCEP delivery through Engine.ondatachannel.
        const check = (condition, message) => {
          if (!condition) throw new Error(`${name}: ${message}`);
        };
        let admissionError;
        const deadline = Date.now() + 10000;
        const until = async (condition, message) => {
          while (!condition()) {
            if (admissionError) throw admissionError;
            check(Date.now() < deadline, `timed out: ${message}`);
            await new Promise((resolve) => setTimeout(resolve, 10));
          }
        };
        const NativeConnection = globalThis.RTCPeerConnection;
        let receiver;
        globalThis.RTCPeerConnection = class extends NativeConnection {
          constructor(configuration) {
            super(configuration);
            receiver = this;
          }
        };
        const events = [];
        let paired = 0;
        const engine = new channelEngine.Engine(false, {
          onLocalCandidate: () => {},
          onPcState: () => {},
          onChannelClosed: () => events.push('close'),
          onChannelOpen: (peer, _generation, channel) => {
            events.push(`open:${channel}`);
            if (engine.noteChannelOpen(peer, channel)) paired += 1;
          },
          onChannelMessage: (_peer, _generation, channel, text) => {
            events.push(`${channel}:${text}`);
          },
        });
        const sender = new NativeConnection();
        try {
          await engine.pairWith('peer', false, []);
          globalThis.RTCPeerConnection = NativeConnection;
          const originalAnnouncement = receiver.ondatachannel;
          let rejected;
          const validAnnouncements = [];
          receiver.ondatachannel = (event) => {
            try {
              const modeMatches =
                event.channel.ordered === (options.ordered ?? true) &&
                event.channel.maxRetransmits === (options.maxRetransmits ?? null) &&
                event.channel.maxPacketLifeTime === (options.maxPacketLifeTime ?? null);
              if (event.channel.label === label && modeMatches) {
                rejected = event.channel;
                check(event.channel.label === label, 'first announcement has the test label');
                originalAnnouncement.call(receiver, event);
                check(
                  engine.channel('peer', label) === undefined,
                  'reject before channel storage',
                );
                check(
                  event.channel.onopen === null &&
                    event.channel.onmessage === null &&
                    event.channel.onclose === null &&
                    event.channel.onerror === null,
                  'reject before callback registration',
                );
                check(
                  event.channel.readyState === 'closing' ||
                    event.channel.readyState === 'closed',
                  'close invalid remote channel',
                );
              } else {
                validAnnouncements.push(event);
              }
            } catch (error) {
              admissionError = error;
            }
          };
          const invalid = sender.createDataChannel(label, options);
          // Allocate all wire IDs before negotiation and rejection. Closing a
          // channel must not hide a stream-ID reuse failure (#872).
          const reliable = sender.createDataChannel('reliable');
          const unreliable = sender.createDataChannel('unreliable', {
            ordered: false,
            maxRetransmits: 0,
          });
          await sender.setLocalDescription(await sender.createOffer());
          // Use complete SDP so candidates cannot precede remote descriptions.
          await until(() => sender.iceGatheringState === 'complete', 'offer ICE gathering');
          await receiver.setRemoteDescription(sender.localDescription);
          await receiver.setLocalDescription(await receiver.createAnswer());
          await until(() => receiver.iceGatheringState === 'complete', 'answer ICE gathering');
          await sender.setRemoteDescription(receiver.localDescription);
          await until(
            () => rejected !== undefined && invalid.readyState === 'closed',
            'invalid channel closes',
          );
          check(
            events.length === 0 && paired === 0,
            'invalid channel creates no application events',
          );
          await until(() => validAnnouncements.length === 2, 'both valid announcements arrive');
          // DCEP can announce channels in either order. Deliver the valid
          // announcements after the rejection to prove the label stays free.
          for (const event of validAnnouncements) originalAnnouncement.call(receiver, event);
          await until(
            () =>
              paired === 1 &&
              reliable.readyState === 'open' &&
              unreliable.readyState === 'open',
            'valid followups connect on the same link',
          );
          reliable.send('valid');
          unreliable.send('valid');
          await until(
            () => events.includes('reliable:valid') && events.includes('unreliable:valid'),
            'valid followups deliver both messages',
          );
          check(
            paired === 1 &&
              events.filter((event) => event.startsWith('open:')).length === 2 &&
              !events.includes('close'),
            'one pair connection and no rejection callbacks',
          );
          return {
            name,
            rejectedMode: {
              ordered: rejected.ordered,
              maxRetransmits: rejected.maxRetransmits,
              maxPacketLifeTime: rejected.maxPacketLifeTime,
            },
            events,
          };
        } finally {
          globalThis.RTCPeerConnection = NativeConnection;
          engine.removePeer('peer');
          sender.close();
        }
      },
      { name, label, options },
    );
    console.error(`ok - Chromium required-channel mode admission ${JSON.stringify(evidence)}`);
  }
} finally {
  await browser.close();
}
