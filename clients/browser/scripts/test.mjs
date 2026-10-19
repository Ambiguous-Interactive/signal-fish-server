import { build } from 'esbuild';

for (const entryPoint of [
  'src/shared/deadline.test.ts',
  'src/shared/backlog.test.ts',
  'src/page/accountability.test.ts',
  'src/page/wire.test.ts',
  'src/page/events.test.ts',
  'src/page/engine.test.ts',
  'src/page/orchestrator.test.ts',
  'src/page/retry.test.ts',
  'src/page/retry-signals.test.ts',
  'src/page/ingress.test.ts',
  'src/page/handshake.test.ts',
  'src/page/output-cancellation.test.ts',
  'src/page/peer-work.test.ts',
  'src/page/queued-retry.test.ts',
  'src/page/queued-lifecycle.test.ts',
  'src/page/partial-channel.test.ts',
  'src/page/buffered-retirement.test.ts',
  'src/page/run-cutoff.test.ts',
  'src/page/relay-cutoff.test.ts',
  'src/page/keepalive-ingress.test.ts',
  'src/cli/args.test.ts',
  'src/cli/output.test.ts',
]) {
  const result = await build({
    entryPoints: [entryPoint],
    bundle: true,
    define: { SDK_VERSION: JSON.stringify('test') },
    format: 'esm',
    platform: 'node',
    target: 'node20',
    write: false,
    logLevel: 'warning',
  });

  const output = result.outputFiles[0];
  if (output === undefined) {
    throw new Error(`esbuild produced no test bundle for ${entryPoint}`);
  }
  const moduleUrl = `data:text/javascript;base64,${Buffer.from(output.contents).toString('base64')}`;
  await import(moduleUrl);
}
