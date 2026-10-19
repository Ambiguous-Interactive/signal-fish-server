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
  for (const label of ['reliable', 'unreliable']) {
    const evidence = await page.evaluate(async (label) => {
      const check = (condition, message) => {
        if (!condition) throw new Error(`${label}: ${message}`);
      };
      const deadline = performance.now() + 10000;
      const until = async (condition, message) => {
        while (!condition()) {
          check(performance.now() < deadline, `timed out: ${message}`);
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
      const engine = new channelEngine.Engine(false, {
        onLocalCandidate: () => {},
        onPcState: () => {},
        onChannelClosed: (_peer, _generation, name) => events.push(`close:${name}`),
        onChannelOpen: (_peer, _generation, name) => events.push(`open:${name}`),
        onChannelMessage: () => {},
      });
      const sender = new NativeConnection();
      try {
        await engine.pairWith('peer', false, []);
        globalThis.RTCPeerConnection = NativeConnection;
        const originalAnnouncement = receiver.ondatachannel;
        let heldAnnouncement;
        receiver.ondatachannel = (event) => {
          heldAnnouncement = event;
        };
        const remoteChannel = sender.createDataChannel(
          label,
          label === 'unreliable' ? { ordered: false, maxRetransmits: 0 } : {},
        );
        await sender.setLocalDescription(await sender.createOffer());
        await until(() => sender.iceGatheringState === 'complete', 'offer ICE gathering');
        await receiver.setRemoteDescription(sender.localDescription);
        await receiver.setLocalDescription(await receiver.createAnswer());
        await until(() => receiver.iceGatheringState === 'complete', 'answer ICE gathering');
        await sender.setRemoteDescription(receiver.localDescription);
        await until(
          () => heldAnnouncement !== undefined && remoteChannel.readyState === 'open',
          'hold a real required-channel announcement',
        );
        remoteChannel.close();
        await until(
          () => heldAnnouncement.channel.readyState === 'closed',
          'remote reset closes the held channel before registration',
        );
        check(events.length === 0, 'no engine channel callback before registration');
        originalAnnouncement.call(receiver, heldAnnouncement);
        await Promise.resolve();
        check(
          JSON.stringify(events) === JSON.stringify([`close:${label}`]),
          'terminal registration reports immediately',
        );
        heldAnnouncement.channel.dispatchEvent(new Event('close'));
        heldAnnouncement.channel.dispatchEvent(new Event('error'));
        await Promise.resolve();
        check(events.length === 1, 'duplicate terminal events report once');
        return { label, readyState: heldAnnouncement.channel.readyState, events };
      } finally {
        globalThis.RTCPeerConnection = NativeConnection;
        engine.removePeer('peer');
        sender.close();
      }
    }, label);
    console.error(`ok - Chromium closed-before-registration ${JSON.stringify(evidence)}`);
  }
  // Regression #999: embedded candidates must preserve deterministic relay fallback.
  for (const crippled of [false, true])
    for (const path of ['offer', 'answer'])
      for (const closeLocally of crippled ? [false] : [false, true]) {
        const result = await page.evaluate(
          async ({ crippled, path, closeLocally }) => {
            const events = [];
            const remote = new RTCPeerConnection();
            const remoteChannels = [];
            remote.ondatachannel = ({ channel }) => {
              remoteChannels.push(channel);
              channel.onmessage = ({ data }) => events.push(`remote:${channel.label}:${data}`);
            };
            const engine = new channelEngine.Engine(crippled, {
              onLocalCandidate: () => {},
              onPcState: (_p, _g, state) => events.push(state),
              onChannelClosed: (_p, _g, label) => events.push(`closed:${label}`),
              onChannelOpen: (_p, _g, label) => events.push(label),
              onChannelMessage: (_p, _g, label, text) => events.push(`${label}:${text}`),
            });
            const until = async (condition, milliseconds = 8000) => {
              const end = performance.now() + milliseconds;
              while (!condition() && performance.now() < end)
                await new Promise((resolve) => setTimeout(resolve, 10));
            };
            let closingEvidence;
            try {
              const offer = await engine.pairWith('peer', path === 'answer', []);
              let remoteSdp;
              if (path === 'offer') {
                for (const channel of [
                  remote.createDataChannel('reliable'),
                  remote.createDataChannel('unreliable', { ordered: false, maxRetransmits: 0 }),
                ]) {
                  remoteChannels.push(channel);
                  channel.onmessage = ({ data }) =>
                    events.push(`remote:${channel.label}:${data}`);
                }
                await remote.setLocalDescription(await remote.createOffer());
                await until(() => remote.iceGatheringState === 'complete');
                remoteSdp = remote.localDescription.sdp;
                const answer = await engine.handleOffer('peer', remoteSdp);
                await remote.setRemoteDescription({ type: 'answer', sdp: answer });
              } else {
                await remote.setRemoteDescription({ type: 'offer', sdp: offer });
                await remote.setLocalDescription(await remote.createAnswer());
                await until(() => remote.iceGatheringState === 'complete');
                remoteSdp = remote.localDescription.sdp;
                await engine.handleAnswer('peer', remoteSdp);
              }
              if (!remoteSdp.includes('a=candidate:'))
                throw new Error('probe requires embedded candidates');
              await until(
                () => events.includes('reliable') && events.includes('unreliable'),
                crippled ? 1500 : 8000,
              );
              const connected = events.includes('reliable') && events.includes('unreliable');
              if (connected === crippled)
                throw new Error(
                  `incorrect result: ${JSON.stringify({ crippled, path, events })}`,
                );
              if (!crippled) {
                await until(
                  () =>
                    remoteChannels.length === 2 &&
                    remoteChannels.every((channel) => channel.readyState === 'open'),
                );
                for (const channel of remoteChannels) {
                  channel.send('to-engine');
                  engine.channel('peer', channel.label).send('to-remote');
                }
                const received = () =>
                  ['reliable', 'unreliable'].every(
                    (label) =>
                      events.includes(`${label}:to-engine`) &&
                      events.includes(`remote:${label}:to-remote`),
                  );
                await until(received);
                if (!received())
                  throw new Error('healthy pair must exchange both labels in both directions');
                const closingLabel = path === 'offer' ? 'reliable' : 'unreliable';
                const closingChannel = engine.channel('peer', closingLabel);
                closingChannel.addEventListener('closing', () => {
                  closingEvidence = {
                    state: closingChannel.readyState,
                    notified: events.includes(`closed:${closingLabel}`),
                    usable: ['reliable', 'unreliable'].some(
                      (label) => engine.channel('peer', label) !== undefined,
                    ),
                  };
                });
                if (closeLocally) {
                  closingChannel.close();
                  if (closingChannel.readyState !== 'closing')
                    throw new Error('local close must enter closing before its event task');
                  if (events.some((event) => event.startsWith('closed:')))
                    throw new Error('the close callback must not have run');
                  const siblingLabel = closingLabel === 'reliable' ? 'unreliable' : 'reliable';
                  if (engine.channel('peer', siblingLabel) !== undefined)
                    throw new Error(
                      'live closing state must fence sibling sends before the event',
                    );
                  closingEvidence = {
                    state: closingChannel.readyState,
                    notified: events.includes(`closed:${closingLabel}`),
                    usable: ['reliable', 'unreliable'].some(
                      (label) => engine.channel('peer', label) !== undefined,
                    ),
                  };
                } else {
                  remoteChannels.find((channel) => channel.label === closingLabel).close();
                }
                await until(() => closingEvidence !== undefined);
                if (
                  closingEvidence?.state !== 'closing' ||
                  !closingEvidence.notified ||
                  closingEvidence.usable
                )
                  throw new Error(
                    `closing must retire the pair before close: ${JSON.stringify(closingEvidence)}`,
                  );
                await until(() => closingChannel.readyState === 'closed');
                if (
                  closingChannel.readyState !== 'closed' ||
                  events.filter((event) => event.startsWith('closed:')).length !== 1
                )
                  throw new Error('closing and close must report one pair loss');
              }
              return {
                crippled,
                path,
                closeLocally,
                connected,
                closingEvidence,
                events,
                sender: remote.connectionState,
              };
            } finally {
              engine.removePeer('peer');
              remote.close();
            }
          },
          { crippled, path, closeLocally },
        );
        console.error(`ok - Chromium embedded SDP fallback ${JSON.stringify(result)}`);
      }
} finally {
  await browser.close();
}
