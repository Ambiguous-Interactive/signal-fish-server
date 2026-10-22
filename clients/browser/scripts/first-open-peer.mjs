// Native interop helper: real browser Engine, with no gameplay-ready barrier.
import { build } from 'esbuild';
import { chromium } from 'playwright-core';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

const bundle = await build({
  entryPoints: [fileURLToPath(new URL('../src/page/engine.ts', import.meta.url))],
  bundle: true,
  format: 'iife',
  globalName: 'channelEngine',
  write: false,
});
const browser = await chromium.launch({
  headless: true,
  args: ['--disable-features=WebRtcHideLocalIpsWithMdns'],
});
process.once('SIGTERM', () => void browser.close().finally(() => process.exit(143)));
const emit = (event) => process.stdout.write(`${JSON.stringify(event)}\n`);
try {
  const page = await browser.newPage();
  page.on('pageerror', (error) => emit({ type: 'error', reason: error.message }));
  await page.exposeFunction('publish', emit);
  await page.addScriptTag({ content: bundle.outputFiles[0].text });
  await page.evaluate(
    async ({ initiate, binaryMode, burst }) => {
      const publish = (event) => void globalThis.publish(event);
      const payload = (sender, label, binary) => {
        const prefix = `${sender}:${label}:${binary ? 'binary' : 'text'}:`;
        const remaining = 65536 - new TextEncoder().encode(prefix).byteLength;
        return prefix + '界'.repeat(Math.floor(remaining / 3)) + 'x'.repeat(remaining % 3);
      };
      const NativePeerConnection = globalThis.RTCPeerConnection;
      let connection;
      globalThis.RTCPeerConnection = class extends NativePeerConnection {
        constructor(...args) {
          super(...args);
          connection = this;
        }
      };
      let pairs = 0;
      const opens = new Set();
      const receipts = new Set();
      const engine = new channelEngine.Engine(false, {
        onLocalCandidate: (_peer, _generation, candidate) =>
          publish({ type: 'candidate', candidate }),
        onPcState: (_peer, _generation, state) => {
          if (state === 'failed' || state === 'closed') publish({ type: 'error', state });
        },
        onChannelClosed: (_peer, _generation, label, reason) =>
          publish({ type: 'error', label, reason }),
        onChannelOpen: (peer, _generation, label) => {
          if (opens.has(label)) throw new Error(`duplicate open: ${label}`);
          opens.add(label);
          // Send within this label's first open callback, before pair bookkeeping.
          const channel = engine.channel(peer, label);
          for (const binary of burst ? [false, true] : [binaryMode]) {
            const body = payload('browser', label, binary);
            channel.send(binary ? new TextEncoder().encode(body) : body);
          }
          publish({ type: 'open', label });
          if (engine.noteChannelOpen(peer, label)) pairs += 1;
        },
        onChannelMessage: (_peer, _generation, label, text, bytes) => {
          const binary = text.startsWith(`native:${label}:binary:`);
          const key = `${label}:${binary}`;
          if (
            (!burst && binary !== binaryMode) ||
            bytes !== 65536 ||
            text !== payload('native', label, binary) ||
            receipts.has(key)
          ) {
            publish({
              type: 'error',
              label,
              reason: 'corrupt or duplicate first-open receipt',
            });
            return false;
          }
          receipts.add(key);
          publish({ type: 'receipt', label, binary, bytes });
          return true;
        },
      });
      globalThis.firstOpen = {
        engine,
        transportStats: async () =>
          [...(await connection.getStats()).values()].filter((entry) =>
            ['data-channel', 'candidate-pair', 'transport'].includes(entry.type),
          ),
        snapshot: () => ({
          pairs,
          opens: [...opens],
          receipts: [...receipts],
          link: engine.diagnosticSnapshot('native'),
        }),
      };
      const offer = await engine.pairWith('native', initiate, []);
      publish(offer === null ? { type: 'ready' } : { type: 'offer', sdp: offer });
    },
    {
      initiate: process.argv[2] === 'true',
      binaryMode: process.argv[3] === 'true',
      burst: process.argv[4] === 'burst',
    },
  );
  const input = createInterface({ input: process.stdin });
  for await (const line of input) {
    const command = JSON.parse(line);
    await page.evaluate(async (command) => {
      const { engine, snapshot, transportStats } = globalThis.firstOpen;
      if (command.type === 'offer') {
        const sdp = await engine.handleOffer('native', command.sdp);
        await globalThis.publish({ type: 'answer', sdp });
      } else if (command.type === 'answer') {
        await engine.handleAnswer('native', command.sdp);
      } else if (command.type === 'candidate') {
        await engine.handleRemoteCandidate('native', command.candidate);
      } else if (command.type === 'snapshot') {
        await globalThis.publish({
          type: 'snapshot',
          ...snapshot(),
          transportStats: await transportStats(),
        });
      }
    }, command);
  }
} finally {
  await browser.close();
}
