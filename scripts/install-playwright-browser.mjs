#!/usr/bin/env node

// Use the browser client's locked Playwright package from any working directory.
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { spawnSync } from 'node:child_process';

/** Preserve a failed Playwright command's exit status. */
class InstallCommandError extends Error {
  constructor(message, exitCode) {
    super(message);
    this.exitCode = exitCode;
  }
}

/** Download the requested browser and prove that its executable can start. */
async function main() {
  const [target, ...flags] = process.argv.slice(2);
  const targets = ['chromium-headless-shell', 'chromium', 'firefox'];
  if (!targets.includes(target) || flags.length > 1 ||
      (flags.length === 1 && flags[0] !== '--with-deps')) {
    throw new Error('Usage: install-playwright-browser.mjs <chromium-headless-shell|chromium|firefox> [--with-deps]');
  }

  const require = createRequire(new URL('../clients/browser/package.json', import.meta.url));
  const cli = join(dirname(require.resolve('playwright-core/package.json')), 'cli.js');
  const { chromium, firefox } = require('playwright-core');

  /** Run the pinned CLI and keep download and package setup failures fatal. */
  function runCli(args) {
    const result = spawnSync(process.execPath, [cli, ...args], { stdio: 'inherit' });
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new InstallCommandError(
        `Playwright ${args.join(' ')} failed${result.signal ? ` (${result.signal})` : ''}.`,
        result.status ?? 1,
      );
    }
  }

  runCli(['install', target]);
  const browserType = target === 'firefox' ? firefox : chromium;
  const options = { headless: true, timeout: 30_000 };
  // Fortress uses full Chromium; the browser reference client uses the shell.
  if (target === 'chromium') options.executablePath = chromium.executablePath();

  let browser;
  try {
    browser = await browserType.launch(options);
  } catch (error) {
    if (flags.length === 0) throw error;
    console.error(`Browser startup failed: ${error.message}`);
    console.error('Install OS dependencies and try browser startup once more.');
    runCli(['install-deps', target]);
    browser = await browserType.launch(options);
  }
  // A close failure is fatal and does not trigger package installation.
  await browser.close();
  console.log(`Browser startup ready: ${target}`);
}

await main().catch((error) => {
  console.error(error.message);
  process.exitCode = error instanceof InstallCommandError ? error.exitCode : 1;
});
