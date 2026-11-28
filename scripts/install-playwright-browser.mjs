#!/usr/bin/env node

// Use the browser client's locked Playwright package from any working directory.
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';

/** Preserve a failed Playwright command's exit status. */
class InstallCommandError extends Error {
  constructor(message, exitCode) {
    super(message);
    this.exitCode = exitCode;
  }
}

const DEFAULT_COMMAND_TIMEOUT_MS = 10 * 60_000;
const COMMAND_TIMEOUT_ENV = 'SIGNAL_FISH_PLAYWRIGHT_TIMEOUT_MS';

/** Bound network-backed Playwright commands so a stalled mirror cannot consume the job limit. */
function commandTimeoutMs() {
  const configured = process.env[COMMAND_TIMEOUT_ENV];
  if (configured === undefined) return DEFAULT_COMMAND_TIMEOUT_MS;
  const timeout = Number(configured);
  if (!Number.isSafeInteger(timeout) || timeout <= 0) {
    throw new Error(`${COMMAND_TIMEOUT_ENV} must be a positive integer number of milliseconds.`);
  }
  return timeout;
}

/** Kill the command and every descendant so a stalled package step leaves no locks behind. */
function killProcessTree(child) {
  if (process.platform === 'win32') {
    const result = spawnSync('taskkill', ['/pid', String(child.pid), '/T', '/F'], {
      stdio: 'ignore',
    });
    if (result.error) child.kill('SIGKILL');
    return;
  }
  try {
    process.kill(-child.pid, 'SIGKILL');
  } catch (error) {
    if (error.code !== 'ESRCH') {
      console.error(`Could not terminate the Playwright process group: ${error.message}`);
      child.kill('SIGKILL');
    }
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
  const timeoutMs = commandTimeoutMs();

  /** Run the pinned CLI and keep download and package setup failures fatal. */
  function runCli(args) {
    return new Promise((resolve, reject) => {
      const child = spawn(process.execPath, [cli, ...args], {
        stdio: 'inherit',
        detached: process.platform !== 'win32',
      });
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        killProcessTree(child);
      }, timeoutMs);

      child.on('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.on('close', (code, signal) => {
        clearTimeout(timer);
        if (timedOut) {
          reject(new InstallCommandError(
            `Playwright ${args.join(' ')} timed out after ${timeoutMs} ms.`,
            1,
          ));
        } else if (code !== 0) {
          reject(new InstallCommandError(
            `Playwright ${args.join(' ')} failed${signal ? ` (${signal})` : ''}.`,
            code ?? 1,
          ));
        } else {
          resolve();
        }
      });
    });
  }

  await runCli(['install', target]);
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
    await runCli(['install-deps', target]);
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
