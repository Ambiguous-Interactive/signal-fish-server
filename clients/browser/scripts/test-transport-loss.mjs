// Investigation #853/#889: distinguish an observed UDP loss from callback loss.
import { createSocket } from 'node:dgram';
import { build } from 'esbuild';
import { chromium } from 'playwright-core';

const bundle = await build({
  stdin: {
    contents:
      "export { Engine } from './src/page/engine.ts'; export { ExchangeLedger } from './src/page/orchestrator.ts';",
    resolveDir: process.cwd(),
  },
  bundle: true,
  format: 'iife',
  globalName: 'transportControl',
  define: { SDK_VERSION: JSON.stringify('transport-loss-control') },
  write: false,
});
const browser = await chromium.launch({
  headless: true,
  args: ['--disable-features=WebRtcHideLocalIpsWithMdns'],
});
/** Attempt every cleanup and report failures without replacing the test error. */
async function cleanup(resources) {
  for (const close of resources) {
    try {
      await close();
    } catch (error) {
      console.error('UDP loss control cleanup failed:', error);
      process.exitCode = 1;
    }
  }
}

/** Drop one selected gameplay transmission and check exact receipt qualification. */
async function runLossControl(droppedLabel) {
  const sockets = [createSocket('udp4'), createSocket('udp4')];
  const boundSockets = new Set();
  const targets = [null, null];
  const forwarded = [0, 0];
  const dropped = [];
  const errors = [];
  let armed = false;
  let page;
  let observeDrop;
  const dropObserved = new Promise((resolve) => {
    observeDrop = resolve;
  });
  try {
    for (let side = 0; side < 2; side++) {
      const socket = sockets[side];
      socket.on('error', (error) => errors.push(String(error)));
      socket.on('listening', () => boundSockets.add(socket));
      socket.on('close', () => boundSockets.delete(socket));
      await new Promise((resolve, reject) => {
        const failed = (error) => reject(error);
        socket.once('error', failed);
        socket.bind(0, '127.0.0.1', () => {
          socket.off('error', failed);
          resolve();
        });
      });
      socket.on('message', (packet) => {
        // No application sends precede arming. The only large send before this
        // drop is the selected gameplay probe. DTLS 1.2 application
        // records and DTLS 1.3 unified headers both qualify. Small SCTP control packets pass.
        if (
          armed &&
          side === 0 &&
          (packet[0] === 23 || (packet[0] & 0xe0) === 0x20) &&
          packet.length >= 900
        ) {
          armed = false;
          dropped.push({ side, bytes: packet.length, contentType: packet[0] });
          observeDrop();
          return;
        }
        const target = targets[1 - side];
        if (target === null) {
          errors.push('UDP arrived before both targets were retained');
          return;
        }
        sockets[1 - side].send(packet, target.port, target.address, (error) => {
          if (error) errors.push(String(error));
          else forwarded[side]++;
        });
      });
    }
    page = await browser.newPage();
    await page.exposeFunction('lossCandidate', (side, encoded) => {
      const candidate = JSON.parse(encoded);
      const fields = candidate.candidate.split(' ');
      if (fields[2]?.toLowerCase() !== 'udp' || fields[7] !== 'host') return null;
      if (targets[side] !== null || fields[4].includes(':')) return null;
      if (!/^\d+\.\d+\.\d+\.\d+$/.test(fields[4]))
        throw new Error('control requires a concrete IPv4 host candidate');
      targets[side] = { address: fields[4], port: Number(fields[5]) };
      fields[4] = '127.0.0.1';
      fields[5] = String(sockets[1 - side].address().port);
      return JSON.stringify({ ...candidate, candidate: fields.join(' ') });
    });
    await page.exposeFunction('armLoss', () => {
      armed = true;
    });
    await page.exposeFunction('waitForLoss', () => dropObserved);
    await page.addScriptTag({ content: bundle.outputFiles[0].text });
    const result = await page.evaluate(async (droppedLabel) => {
      /** Reject any mismatch in the live packet and receipt history. */
      const check = (condition, message) => {
        if (!condition) throw new Error(message);
      };
      const deadline = performance.now() + 10000;
      /** Poll within one fixed deadline for all phases of this pairing. */
      const until = async (condition, message) => {
        while (!condition()) {
          check(performance.now() < deadline, message);
          await new Promise((resolve) => setTimeout(resolve, 10));
        }
      };
      const engines = [];
      const candidates = [[], []];
      const opens = [[], []];
      const receipts = [[], []];
      const terminal = [];
      const ledgers = [
        new transportControl.ExchangeLedger(),
        new transportControl.ExchangeLedger(),
      ];
      const payload = (side, label) =>
        JSON.stringify({
          from: `side-${side}`,
          channel: label,
          seq: 0,
          body: 'x'.repeat(1000),
        });
      const withoutCandidates = (sdp) =>
        sdp.replace(/^a=(?:candidate:.*|end-of-candidates)\r?\n/gm, '');
      try {
        for (let side = 0; side < 2; side++) {
          ledgers[side].noteConnected('peer');
          engines.push(
            new transportControl.Engine(false, {
              onLocalCandidate: (_peer, _generation, candidate) =>
                candidates[side].push(candidate),
              onPcState: (_peer, _generation, state) => {
                if (state === 'failed' || state === 'closed') terminal.push(state);
              },
              onChannelClosed: (_peer, _generation, label) => terminal.push(label),
              onChannelOpen: (_peer, _generation, label) => {
                opens[side].push(label);
                engines[side].noteChannelOpen('peer', label);
              },
              onChannelMessage: (_peer, _generation, label, text) => {
                check(
                  text === payload(1 - side, label),
                  'every surviving receipt keeps exact payload and direction',
                );
                receipts[side].push(label);
                ledgers[side].noteReceived('peer', label);
              },
            }),
          );
        }
        await engines[1].pairWith('peer', false, []);
        const offer = await engines[0].pairWith('peer', true, []);
        const answer = await engines[1].handleOffer('peer', withoutCandidates(offer));
        await engines[0].handleAnswer('peer', withoutCandidates(answer));
        await until(
          () =>
            engines.every(
              (engine) => engine.diagnosticSnapshot('peer').iceGatheringState === 'complete',
            ),
          'ICE gathering completed',
        );
        const relayed = [[], []];
        // Collect both targets before adding either proxy candidate.
        for (let side = 0; side < 2; side++) {
          for (const candidate of candidates[side]) {
            const rewritten = await window.lossCandidate(side, candidate);
            if (rewritten !== null) relayed[side].push(rewritten);
          }
          check(relayed[side].length === 1, 'one IPv4 UDP path per endpoint');
        }
        for (let side = 0; side < 2; side++)
          await engines[1 - side].handleRemoteCandidate('peer', relayed[side][0]);
        await until(
          () => opens.every((labels) => labels.length === 2),
          'both required channels open through the proxy',
        );
        check(
          receipts.every((messages) => messages.length === 0),
          'no application sends before loss arming',
        );
        await window.armLoss();
        engines[0].channel('peer', droppedLabel).send(payload(0, droppedLabel));
        ledgers[0].noteSent('peer', droppedLabel);
        await Promise.race([
          window.waitForLoss(),
          new Promise((_, reject) =>
            setTimeout(
              () => reject(new Error('selected UDP drop was not observed')),
              Math.max(0, deadline - performance.now()),
            ),
          ),
        ]);
        // Only after the drop is observed do other application sends become legal.
        for (const [side, label] of [
          [0, droppedLabel === 'reliable' ? 'unreliable' : 'reliable'],
          [1, 'reliable'],
          [1, 'unreliable'],
        ]) {
          engines[side].channel('peer', label).send(payload(side, label));
          ledgers[side].noteSent('peer', label);
        }
        await until(
          () =>
            receipts[0].length === 2 &&
            receipts[1].length === (droppedLabel === 'reliable' ? 2 : 1),
          'both reliable probes arrive even when their first transmission is lost',
        );
        for (const engine of engines) engine.sampleDataChannelStats('peer');
        await until(
          () =>
            engines.every(
              (engine) => !engine.diagnosticSnapshot('peer').dataChannelStats.pending,
            ),
          'stats queries complete',
        );
        const snapshots = engines.map((engine) => engine.diagnosticSnapshot('peer'));
        check(
          terminal.length === 0,
          'permitted unreliable loss does not terminate either physical link',
        );
        check(
          receipts[1].filter((label) => label === 'reliable').length === 1 &&
            receipts[1].filter((label) => label === 'unreliable').length ===
              (droppedLabel === 'reliable' ? 1 : 0),
          'the dropped unreliable probe has no fabricated callback',
        );
        for (let side = 0; side < 2; side++) {
          for (const label of ['reliable', 'unreliable']) {
            const totals = snapshots[side].dataChannelStats.channels[label];
            const expectedReceived =
              side === 1 && label === 'unreliable' && droppedLabel === 'unreliable' ? 0 : 1;
            check(
              totals?.id === snapshots[side].channels[label].id,
              'stats refer to the actual retained channel',
            );
            check(
              totals?.messagesSent === 1 && totals?.messagesReceived === expectedReceived,
              'stack counters distinguish the selected loss from successful enqueue',
            );
            check(
              totals?.bytesSent === new TextEncoder().encode(payload(side, label)).length &&
                totals?.bytesReceived ===
                  (expectedReceived
                    ? new TextEncoder().encode(payload(1 - side, label)).length
                    : 0),
              'stack byte counters preserve exact sent and surviving payload sizes',
            );
            check(
              snapshots[side].channels[label].receipts.raw === expectedReceived &&
                snapshots[side].channels[label].receipts.admitted === expectedReceived,
              'raw callbacks and admissions agree with independent stack receipts',
            );
          }
          check(
            snapshots[side].dataChannelStats.selectedCandidatePair?.state === 'succeeded',
            'the selected route survives the application loss',
          );
        }
        check(ledgers[0].unmetCriteria().length === 0, 'the untouched direction qualifies');
        check(
          JSON.stringify(ledgers[1].unmetCriteria()) ===
            JSON.stringify(
              droppedLabel === 'unreliable'
                ? ['exchange incomplete (received from peer; missing unreliable)']
                : [],
            ),
          'production strict qualification keeps exactly the observed unreliable debt',
        );
        return { receipts, debt: ledgers[1].diagnosticSnapshot('peer'), snapshots };
      } finally {
        for (const engine of engines) engine.removePeer('peer');
      }
    }, droppedLabel);
    if (errors.length || dropped.length !== 1 || forwarded.some((count) => count === 0))
      throw new Error(
        `UDP evidence mismatch: ${JSON.stringify({ errors, dropped, forwarded })}`,
      );
    console.log(
      JSON.stringify({ control: `${droppedLabel}-udp-loss`, dropped, forwarded, ...result }),
    );
  } finally {
    await cleanup([
      ...sockets
        .filter((socket) => boundSockets.has(socket))
        .map((socket) => () => new Promise((resolve) => socket.close(resolve))),
      () => page?.close(),
    ]);
  }
}
try {
  for (const label of ['unreliable', 'reliable']) await runLossControl(label);
} finally {
  await cleanup([() => browser.close()]);
}
