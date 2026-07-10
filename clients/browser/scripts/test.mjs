import { build } from 'esbuild';

const result = await build({
  entryPoints: ['src/page/accountability.test.ts'],
  bundle: true,
  format: 'esm',
  platform: 'node',
  target: 'node20',
  write: false,
  logLevel: 'warning',
});

const output = result.outputFiles[0];
if (output === undefined) {
  throw new Error('esbuild produced no accountability test bundle');
}
const moduleUrl = `data:text/javascript;base64,${Buffer.from(output.contents).toString('base64')}`;
await import(moduleUrl);
