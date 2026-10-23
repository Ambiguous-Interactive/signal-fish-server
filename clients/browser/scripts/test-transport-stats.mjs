import { build } from 'esbuild';
import { chromium } from 'playwright-core';

const bundle = await build({
  entryPoints: ['src/page/engine.ts'],
  bundle: true,
  format: 'iife',
  globalName: 'channelEngine',
  write: false,
});
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  await page.addScriptTag({ content: bundle.outputFiles[0].text });
  const result = await page.evaluate(async () => {
    const check = (condition, message) => {
      if (!condition) throw new Error(message);
    };
    const deadline = performance.now() + 10000;
    const until = async (condition) => {
      while (!condition()) {
        check(performance.now() < deadline, 'transport stats control timed out');
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
    };
    const engines = [];
    const candidates = [[], []];
    const opens = [[], []];
    const receipts = [[], []];
    try {
      for (let side = 0; side < 2; side++)
        engines.push(
          new channelEngine.Engine(false, {
            onLocalCandidate: (_peer, _generation, candidate) =>
              candidates[side].push(candidate),
            onPcState() {},
            onChannelClosed() {},
            onChannelOpen: (_peer, _generation, label) => opens[side].push(label),
            onChannelMessage: (_peer, _generation, label, text) =>
              receipts[side].push([label, text]),
          }),
        );
      await engines[1].pairWith('peer', false, []);
      const offer = await engines[0].pairWith('peer', true, []);
      const answer = await engines[1].handleOffer('peer', offer);
      await engines[0].handleAnswer('peer', answer);
      // Wait for gathering once, then relay the exact collected candidates.
      await until(() =>
        engines.every(
          (engine) => engine.diagnosticSnapshot('peer').iceGatheringState === 'complete',
        ),
      );
      for (let side = 0; side < 2; side++)
        for (const candidate of candidates[side])
          await engines[1 - side].handleRemoteCandidate('peer', candidate);
      await until(() => opens.every((labels) => labels.length === 2));
      for (let side = 0; side < 2; side++) {
        for (const label of ['reliable', 'unreliable']) {
          engines[side].channel('peer', label).send(`side=${side}/${label}/🐟`);
        }
      }
      await until(() => receipts.every((messages) => messages.length === 2));
      for (const engine of engines) engine.sampleDataChannelStats('peer');
      await until(() =>
        engines.every((engine) => !engine.diagnosticSnapshot('peer').dataChannelStats.pending),
      );
      const snapshots = engines.map((engine) => engine.diagnosticSnapshot('peer'));
      for (let side = 0; side < 2; side++) {
        const snapshot = snapshots[side];
        check(
          snapshot.dataChannelStats.status === 'ready',
          `side ${side}: Chromium exposes channel reports`,
        );
        for (const label of ['reliable', 'unreliable']) {
          const totals = snapshot.dataChannelStats.channels[label];
          check(totals?.id === snapshot.channels[label].id, 'bind totals to actual channel ID');
          check(
            totals?.messagesSent === 1 && totals?.messagesReceived === 1,
            `side ${side}/${label}: exact application message totals ${JSON.stringify(totals)}`,
          );
          check(
            totals?.bytesSent === new TextEncoder().encode(`side=${side}/${label}/🐟`).length &&
              totals?.bytesReceived ===
                new TextEncoder().encode(`side=${1 - side}/${label}/🐟`).length,
            'exact multibyte payload byte totals',
          );
          check(
            snapshot.channels[label].receipts.raw === 1 &&
              snapshot.channels[label].receipts.admitted === 1,
            'independent browser callbacks agree after receipt',
          );
        }
      }
      return snapshots.map(({ dataChannelStats }) => dataChannelStats);
    } finally {
      for (const engine of engines) engine.removePeer('peer');
    }
  });
  console.log(JSON.stringify(result));
} finally {
  await browser.close();
}
