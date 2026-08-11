import { build } from 'esbuild';

for (const entryPoint of [
  'src/page/accountability.test.ts',
  'src/page/orchestrator.test.ts',
  'src/cli/args.test.ts',
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
