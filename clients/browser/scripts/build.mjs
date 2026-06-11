// Build both bundles with esbuild:
//  - dist/page.js  — IIFE, runs INSIDE headless Chromium (no Node APIs);
//  - dist/cli.js   — Node ESM entrypoint (playwright-core stays external and
//    resolves from node_modules at runtime).
//
// SDK_VERSION is the package version, injected as a compile-time constant
// (reported as Authenticate.sdk_version, mirroring the native client's
// CARGO_PKG_VERSION embedding).

import { readFileSync } from 'node:fs';
import { build } from 'esbuild';

const pkg = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'));
const define = { SDK_VERSION: JSON.stringify(pkg.version) };

await build({
  entryPoints: ['src/page/main.ts'],
  outfile: 'dist/page.js',
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: 'chrome120',
  define,
  logLevel: 'warning',
});

await build({
  entryPoints: ['src/cli/main.ts'],
  outfile: 'dist/cli.js',
  bundle: true,
  format: 'esm',
  platform: 'node',
  target: 'node20',
  external: ['playwright-core'],
  define,
  logLevel: 'warning',
});
