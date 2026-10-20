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
          const snapshot = engine.diagnosticSnapshot('peer');
          for (const channel of [reliable, unreliable]) {
            const diagnostic = snapshot.channels[channel.label];
            check(
              Number.isInteger(diagnostic.id) &&
                diagnostic.id >= 0 &&
                diagnostic.receipts.raw === 1 &&
                diagnostic.receipts.admitted === 1 &&
                diagnostic.receipts.rejected === 0 &&
                diagnostic.receipts.discarded === 0 &&
                diagnostic.error === null,
              `count raw/admitted callbacks for ${channel.label}: ${JSON.stringify(diagnostic)}`,
            );
          }
          return {
            name,
            receiptDiagnostics: snapshot.channels,
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
  // Real Chromium diagnostics must capture a rejected callback before link teardown.
  {
    const evidence = await page.evaluate(async () => {
      const NativeConnection = globalThis.RTCPeerConnection;
      let receiver;
      globalThis.RTCPeerConnection = class extends NativeConnection {
        constructor(configuration) {
          super(configuration);
          receiver = this;
        }
      };
      const deadline = Date.now() + 10000;
      const until = async (condition, message) => {
        while (!condition()) {
          if (Date.now() >= deadline) throw new Error(`receipt diagnostic: ${message}`);
          await new Promise((resolve) => setTimeout(resolve, 10));
        }
      };
      const admitted = [];
      let loss;
      const engine = new channelEngine.Engine(false, {
        onLocalCandidate: () => {},
        onPcState: () => {},
        onChannelOpen: (peer, _generation, label) => engine.noteChannelOpen(peer, label),
        onChannelClosed: (peer, _generation, label, reason) => {
          loss = { label, reason, snapshot: engine.diagnosticSnapshot(peer) };
          engine.removePeer(peer);
        },
        onChannelMessage: (_peer, _generation, label, _text, bytes) => {
          admitted.push({ label, bytes });
          return true;
        },
      });
      const sender = new NativeConnection();
      try {
        await engine.pairWith('peer', false, []);
        globalThis.RTCPeerConnection = NativeConnection;
        const reliable = sender.createDataChannel('reliable');
        sender.createDataChannel('unreliable', { ordered: false, maxRetransmits: 0 });
        await sender.setLocalDescription(await sender.createOffer());
        await until(() => sender.iceGatheringState === 'complete', 'offer ICE');
        await engine
          .handleOffer('peer', sender.localDescription.sdp)
          .then((sdp) => sender.setRemoteDescription({ type: 'answer', sdp }));
        // Complete responder ICE is optional here: sender SDP already carries candidates.
        await until(
          () =>
            reliable.readyState === 'open' &&
            engine.channel('peer', 'reliable')?.readyState === 'open',
          'reliable opens',
        );
        reliable.send('small');
        reliable.send(new Uint8Array(65536));
        await until(() => admitted.length === 2, 'exact-limit text/binary admission');
        reliable.send('x'.repeat(65537));
        await until(() => loss !== undefined, 'oversized raw callback reports loss');
        const diagnostic = loss.snapshot.channels.reliable;
        if (
          diagnostic.receipts.raw !== 3 ||
          diagnostic.receipts.admitted !== 2 ||
          diagnostic.receipts.rejected !== 1 ||
          diagnostic.receipts.discarded !== 0 ||
          loss.reason !== 'incoming data-channel message exceeds 65536 bytes' ||
          engine.diagnosticSnapshot('peer') !== null ||
          admitted[1]?.bytes !== 65536
        )
          throw new Error(`receipt diagnostic mismatch: ${JSON.stringify({ loss, admitted })}`);
        return { admitted, loss };
      } finally {
        globalThis.RTCPeerConnection = NativeConnection;
        engine.removePeer('peer');
        sender.close();
      }
    });
    console.error(
      `ok - Chromium raw receipt and rejection diagnostics ${JSON.stringify(evidence)}`,
    );
  }

  // Audits #853/#889: distinguish raw receipt, engine admission, and same-stream reset.
  for (const initiate of [false, true])
    for (const label of ['reliable', 'unreliable'])
      for (const acknowledge of [false, true])
        for (const payloadMode of ['probe', 'fragmented-text', 'fragmented-binary']) {
          const evidence = await page.evaluate(
            async ({ initiate, label, acknowledge, payloadMode }) => {
              const events = [];
              const check = (condition, message) => {
                if (!condition)
                  throw new Error(
                    `${JSON.stringify({ initiate, label, acknowledge, payloadMode })}: ${message}; ${JSON.stringify(events)}`,
                  );
              };
              const deadline = performance.now() + 10000;
              const until = async (condition) => {
                while (!condition()) {
                  check(performance.now() < deadline, 'timed out');
                  await new Promise((resolve) => setTimeout(resolve, 10));
                }
              };
              // Maximum-size messages require SCTP fragmentation. Keep payloads
              // out of evidence and compare the full decoded value at both seams.
              const payloads =
                payloadMode === 'probe'
                  ? ['before-reset']
                  : Array.from({ length: 3 }, (_, index) => `${index}:` + 'λ'.repeat(32767));
              const invalidReceipts = [];
              const receiptName = (data, raw = false, receivedLabel = label) => {
                if (typeof data !== 'string' && !(data instanceof ArrayBuffer)) {
                  invalidReceipts.push('unsupported input type');
                  return 'invalid';
                }
                const text = typeof data === 'string' ? data : new TextDecoder().decode(data);
                const index = payloads.indexOf(text);
                if (index >= 0) {
                  if (receivedLabel !== label) invalidReceipts.push('wrong channel label');
                  if (
                    raw &&
                    (payloadMode === 'fragmented-binary') !== data instanceof ArrayBuffer
                  )
                    invalidReceipts.push('wrong wire type');
                  return `payload-${index}`;
                }
                if (text === 'first-open' && typeof data === 'string') return text;
                invalidReceipts.push('unexpected message body');
                return 'invalid';
              };
              const NativeConnection = globalThis.RTCPeerConnection;
              const observe = (channel) => {
                channel.addEventListener('message', ({ data }) =>
                  events.push(`raw:${channel.label}:${receiptName(data, true, channel.label)}`),
                );
                channel.addEventListener('closing', () =>
                  events.push(`closing:${channel.label}`),
                );
                channel.addEventListener('close', () => events.push(`close:${channel.label}`));
                events.push(`registered:${channel.label}:${channel.readyState}`);
                return channel;
              };
              let receiver;
              globalThis.RTCPeerConnection = class extends NativeConnection {
                constructor(configuration) {
                  super(configuration);
                  receiver = this;
                  this.addEventListener('datachannel', ({ channel }) => observe(channel));
                }
                createDataChannel(...args) {
                  return observe(super.createDataChannel(...args));
                }
              };
              const engine = new channelEngine.Engine(false, {
                onLocalCandidate: () => {},
                onPcState: () => {},
                onChannelOpen: (peer, _generation, name) => {
                  events.push(`engine-open:${name}`);
                  engine.noteChannelOpen(peer, name);
                },
                onChannelClosed: (_peer, _generation, name) => events.push(`loss:${name}`),
                onChannelMessage: (_peer, _generation, name, text) =>
                  events.push(`admitted:${name}:${receiptName(text, false, name)}`),
              });
              const remote = new NativeConnection();
              const channels = [];
              const startChannel = (channel) => {
                channels.push(channel);
                let sent = false;
                const sendOnOpen = () => {
                  if (sent || channel.readyState !== 'open') return;
                  sent = true;
                  events.push(`sender-open:${channel.label}`);
                  channel.send('first-open');
                  events.push(`first-sent:${channel.label}`);
                };
                channel.addEventListener('open', sendOnOpen);
                sendOnOpen();
              };
              remote.ondatachannel = ({ channel }) => startChannel(channel);
              try {
                const offer = await engine.pairWith('peer', initiate, []);
                globalThis.RTCPeerConnection = NativeConnection;
                if (initiate) {
                  await until(() => receiver.iceGatheringState === 'complete');
                  await remote.setRemoteDescription(receiver.localDescription);
                  await remote.setLocalDescription(await remote.createAnswer());
                  await until(() => remote.iceGatheringState === 'complete');
                  await engine.handleAnswer('peer', remote.localDescription.sdp);
                } else {
                  startChannel(remote.createDataChannel('reliable'));
                  startChannel(
                    remote.createDataChannel('unreliable', {
                      ordered: false,
                      maxRetransmits: 0,
                    }),
                  );
                  await remote.setLocalDescription(await remote.createOffer());
                  await until(() => remote.iceGatheringState === 'complete');
                  const answer = await engine.handleOffer('peer', remote.localDescription.sdp);
                  await until(() => receiver.iceGatheringState === 'complete');
                  await remote.setRemoteDescription(receiver.localDescription);
                  check(typeof answer === 'string', 'the answer path completed');
                }
                check(
                  initiate === (typeof offer === 'string'),
                  'exercise the intended offerer',
                );
                await until(
                  () =>
                    channels.length === 2 &&
                    channels.every(
                      (channel) =>
                        channel.readyState === 'open' &&
                        engine.channel('peer', channel.label)?.readyState === 'open',
                    ),
                );
                // Send from each endpoint's first usable channel event, without
                // waiting for the receiver to announce or form the pair (#853).
                await until(() => events.includes('raw:reliable:first-open'));
                check(
                  events.includes('admitted:reliable:first-open'),
                  'first reliable probe crosses engine admission',
                );
                const channel = channels.find((channel) => channel.label === label);
                check(
                  payloads.every(
                    (payload) =>
                      new TextEncoder().encode(payload).length ===
                      (payloadMode === 'probe' ? 12 : 65536),
                  ),
                  'exact accepted message boundary',
                );
                for (const payload of payloads)
                  channel.send(
                    payloadMode === 'fragmented-binary'
                      ? new TextEncoder().encode(payload)
                      : payload,
                  );
                events.push(`sent:${label}`);
                if (acknowledge) {
                  // This control requires all receipts before reset, including
                  // no-retransmit input, to supply a causal delivery boundary.
                  let acknowledged = false;
                  channel.addEventListener('message', ({ data }) => {
                    if (data === 'ack') acknowledged = true;
                  });
                  await until(() =>
                    payloads.every((_, index) =>
                      events.includes(`admitted:${label}:payload-${index}`),
                    ),
                  );
                  engine.channel('peer', label).send('ack');
                  await until(() => acknowledged);
                  events.push(`acknowledged:${label}`);
                }
                channel.close();
                events.push(`reset:${label}`);
                await until(() => events.includes(`close:${label}`));
                check(
                  invalidReceipts.length === 0,
                  `reject changed bodies or wire types: ${JSON.stringify(invalidReceipts)}`,
                );
                const loss = events.indexOf(`loss:${label}`);
                for (const name of ['reliable', 'unreliable']) {
                  const firstRaw = events.indexOf(`raw:${name}:first-open`);
                  const firstAdmitted = events.indexOf(`admitted:${name}:first-open`);
                  check(
                    events.filter((event) => event === `first-sent:${name}`).length === 1,
                    'one first-open send per label',
                  );
                  check(
                    events.filter((event) => event === `raw:${name}:first-open`).length <= 1 &&
                      events.filter((event) => event === `admitted:${name}:first-open`)
                        .length <= 1,
                    'first-open receipt and admission are unique',
                  );
                  check(
                    firstRaw >= 0 && firstRaw < loss
                      ? firstAdmitted > firstRaw && firstAdmitted < loss
                      : firstAdmitted < 0,
                    'only first-open receipts before pair loss reach engine admission',
                  );
                }
                for (let index = 0; index < payloads.length; index += 1) {
                  const raw = events.indexOf(`raw:${label}:payload-${index}`);
                  const admitted = events.indexOf(`admitted:${label}:payload-${index}`);
                  // No-retransmit delivery is optional. Every raw receipt before loss
                  // must cross engine admission; reliable same-stream sends must arrive.
                  check(
                    label !== 'reliable' || (raw >= 0 && raw < loss),
                    'reliable send survives same-stream reset',
                  );
                  check(
                    raw >= 0 && raw < loss ? admitted > raw && admitted < loss : admitted < 0,
                    'only raw receipts before pair loss reach engine admission',
                  );
                  check(
                    events.filter((event) => event === `raw:${label}:payload-${index}`)
                      .length <= 1 &&
                      events.filter((event) => event === `admitted:${label}:payload-${index}`)
                        .length === (raw >= 0 && raw < loss ? 1 : 0),
                    'one admission per raw receipt',
                  );
                }
                if (label === 'reliable') {
                  const rawOrder = events.filter((event) =>
                    event.startsWith(`raw:${label}:payload-`),
                  );
                  check(
                    JSON.stringify(rawOrder) ===
                      JSON.stringify(
                        payloads.map((_, index) => `raw:${label}:payload-${index}`),
                      ),
                    'reliable fragments reassemble in send order',
                  );
                }
                check(
                  events.filter((event) => event === `loss:${label}`).length === 1,
                  'one pair loss',
                );
                check(
                  ['reliable', 'unreliable'].every(
                    (name) => engine.channel('peer', name) === undefined,
                  ),
                  'reset fences both labels',
                );
                return { initiate, label, acknowledge, payloadMode, events };
              } finally {
                globalThis.RTCPeerConnection = NativeConnection;
                engine.removePeer('peer');
                remote.close();
              }
            },
            { initiate, label, acknowledge, payloadMode },
          );
          console.error(`ok - Chromium same-stream send/reset ${JSON.stringify(evidence)}`);
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
