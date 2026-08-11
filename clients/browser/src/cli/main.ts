// `signal-fish-reference-browser` CLI: parse flags, launch headless Chromium
// (playwright-core), inject the page engine, bridge its events to stdout
// JSONL, enforce the hard watchdog, and reap Chromium on every exit path.
//
// stdout is exclusively the JSONL event stream (the page's `__sf_emit` bridge
// plus the final `exiting` event); ALL logging — including forwarded page
// console output — goes to stderr.
//
// Chromium teardown is defense-in-depth:
//  1. every exit path awaits a bounded `BrowserServer.close()` (escalating to
//     `kill()`), including the SIGTERM/SIGINT/SIGHUP handlers and the
//     watchdog;
//  2. a detached reaper process watches this process's pid and SIGKILLs the
//     Chromium main process if this process dies unannounced (e.g. the test
//     harness's kill-on-drop SIGKILL, which no in-process handler can trap —
//     empirically, headless Chromium does NOT exit just because its parent
//     died). Chromium's helper processes exit with their main process.

import { spawn } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { chromium, type BrowserServer, type Page } from 'playwright-core';

import { EXIT_HARD_TIMEOUT, EXIT_PROTOCOL_ERROR, type RunConfig } from '../shared/types.js';
import { parseArgs, UsageError, USAGE, type CliOptions } from './args.js';

const startedAtMs = Date.now();

function logError(message: string): void {
  process.stderr.write(`[signal-fish-reference-browser] ${message}\n`);
}

// ---------------------------------------------------------------------------
// stdout JSONL writer (EPIPE-safe: latch suppression like the native client)
// ---------------------------------------------------------------------------

let stdoutClosed = false;
process.stdout.on('error', () => {
  stdoutClosed = true;
});

function writeEventLine(line: string): void {
  if (stdoutClosed) {
    return;
  }
  try {
    process.stdout.write(`${line}\n`);
  } catch (error) {
    stdoutClosed = true;
    logError(
      `stdout write failed (consumer closed?); suppressing all further events: ${String(error)}`,
    );
  }
}

function emitEvent(event: Record<string, unknown>): void {
  writeEventLine(JSON.stringify(event));
}

// ---------------------------------------------------------------------------
// Chromium lifecycle
// ---------------------------------------------------------------------------

/** Ceiling for the graceful browser close before escalating to kill(). */
const BROWSER_CLOSE_TIMEOUT_MS = 5_000;

let browserServer: BrowserServer | null = null;
/** Latched once an exit decision is made; later failures are ignored. */
let shuttingDown = false;

async function teardownBrowser(): Promise<void> {
  const server = browserServer;
  browserServer = null;
  if (server === null) {
    return;
  }
  let killTimer: ReturnType<typeof setTimeout> | undefined;
  const closeTimeout = new Promise<never>((_resolve, reject) => {
    killTimer = setTimeout(
      () => reject(new Error('browser close timed out')),
      BROWSER_CLOSE_TIMEOUT_MS,
    );
  });
  try {
    await Promise.race([server.close(), closeTimeout]);
  } catch (error) {
    logError(`graceful browser close failed (${String(error)}); killing`);
    try {
      await server.kill();
    } catch (killError) {
      logError(`browser kill failed: ${String(killError)}`);
    }
  } finally {
    clearTimeout(killTimer);
  }
}

/** Emit `exiting`, tear Chromium down, and end the process. */
async function exitWith(code: number): Promise<void> {
  if (shuttingDown) {
    return;
  }
  shuttingDown = true;
  emitEvent({ event: 'exiting', code });
  await teardownBrowser();
  process.exit(code);
}

/**
 * Watch `nodePid`; when it disappears, SIGKILL `chromePid`. Self-terminates
 * once Chromium is gone (the normal case: this CLI already reaped it).
 *
 * Pid-reuse guard: a pid freed inside the 500 ms poll window could be
 * recycled by an unrelated process, which a bare `kill(pid, 0)` liveness
 * probe cannot distinguish. Each pid's `/proc/<pid>/stat` field 22
 * (starttime, kernel-side and immutable for a process's lifetime) is
 * recorded up front and re-compared before any action: a mismatch means the
 * original process is gone, so the kill is skipped. Where /proc does not
 * exist (e.g. a macOS dev box), `startOf` returns null and the reaper falls
 * back to the bare signal-0 probe (the previous behavior).
 */
const REAPER_SCRIPT = `
const fs = require('fs');
const [nodePid, chromePid] = process.argv.slice(1).map(Number);
const startOf = (pid) => {
  try {
    const stat = fs.readFileSync('/proc/' + pid + '/stat', 'utf8');
    return stat.slice(stat.lastIndexOf(')') + 2).split(' ')[19];
  } catch {
    return null;
  }
};
const aliveBySignal = (pid) => {
  try { process.kill(pid, 0); return true; } catch { return false; }
};
const nodeStart = startOf(nodePid);
const chromeStart = startOf(chromePid);
const stillOriginal = (pid, start) =>
  start === null ? aliveBySignal(pid) : startOf(pid) === start;
const tick = () => {
  if (!stillOriginal(chromePid, chromeStart)) process.exit(0);
  if (!stillOriginal(nodePid, nodeStart)) {
    if (stillOriginal(chromePid, chromeStart)) {
      try { process.kill(chromePid, 'SIGKILL'); } catch {}
    }
    process.exit(0);
  }
  setTimeout(tick, 500);
};
tick();
`;

function spawnReaper(chromePid: number | undefined): void {
  if (chromePid === undefined) {
    logError('chromium pid unavailable; skipping the orphan reaper');
    return;
  }
  const reaper = spawn(
    process.execPath,
    ['-e', REAPER_SCRIPT, String(process.pid), String(chromePid)],
    { detached: true, stdio: 'ignore' },
  );
  reaper.unref();
}

// ---------------------------------------------------------------------------
// Signal handling + crash safety (Chromium must never outlive this process)
// ---------------------------------------------------------------------------

const SIGNAL_NUMBERS: ReadonlyArray<readonly [NodeJS.Signals, number]> = [
  ['SIGTERM', 15],
  ['SIGINT', 2],
  ['SIGHUP', 1],
];

for (const [signal, signalNumber] of SIGNAL_NUMBERS) {
  process.on(signal, () => {
    if (shuttingDown) {
      return;
    }
    shuttingDown = true;
    logError(`received ${signal}; tearing down Chromium`);
    void teardownBrowser().finally(() => {
      process.exit(128 + signalNumber);
    });
  });
}

process.on('uncaughtException', (error) => {
  logError(`uncaught exception: ${String(error)}`);
  emitEvent({ event: 'error', message: `uncaught exception: ${String(error)}` });
  void exitWith(EXIT_HARD_TIMEOUT);
});

// ---------------------------------------------------------------------------
// Page setup + run
// ---------------------------------------------------------------------------

async function setupPage(options: CliOptions): Promise<Page> {
  // dist/page.js sits next to the bundled dist/cli.js.
  const bundlePath = path.join(path.dirname(fileURLToPath(import.meta.url)), 'page.js');
  const pageBundle = readFileSync(bundlePath, 'utf8');

  // Default: disable Chromium's mDNS host-candidate obfuscation so loopback
  // interop gets plain host candidates; `--mdns-obfuscation` keeps the
  // browser default (`.local` candidates) to exercise that trap.
  const args = options.mdnsObfuscation ? [] : ['--disable-features=WebRtcHideLocalIpsWithMdns'];
  // launchServer (rather than launch) exposes the Chromium pid for the
  // reaper; this CLI owns all signal handling, hence handleSIG*: false.
  browserServer = await chromium.launchServer({
    headless: true,
    args,
    handleSIGINT: false,
    handleSIGTERM: false,
    handleSIGHUP: false,
  });
  spawnReaper(browserServer.process().pid);

  const browser = await chromium.connect(browserServer.wsEndpoint());
  const page = await browser.newPage();
  page.on('console', (message) => {
    logError(`[page ${message.type()}] ${message.text()}`);
  });
  page.on('pageerror', (error) => {
    logError(`[page error] ${error.message}`);
  });
  // The stdout bridge must exist before the page bundle (and any event) does.
  await page.exposeFunction('__sf_emit', (line: unknown) => {
    if (typeof line === 'string') {
      writeEventLine(line);
    }
  });
  await page.exposeFunction('__sf_success_released', () => {
    const releaseFile = options.successReleaseFile;
    return releaseFile === null || existsSync(releaseFile);
  });
  await page.addScriptTag({ content: pageBundle });
  return page;
}

async function main(): Promise<void> {
  let parsed: CliOptions | null;
  try {
    parsed = parseArgs(process.argv.slice(2));
  } catch (error) {
    if (error instanceof UsageError) {
      // clap parity: usage problems exit 2 before the event stream starts.
      logError(`error: ${error.message}`);
      logError('run with --help for usage');
      process.exit(EXIT_PROTOCOL_ERROR);
    }
    throw error;
  }
  if (parsed === null) {
    process.stdout.write(USAGE);
    process.exit(0);
  }
  const options: CliOptions = parsed;

  // The watchdog is the binary's no-hang guarantee: it bounds EVERYTHING,
  // Chromium launch included, and expiry is a hard abort with exit code 4.
  const watchdog = setTimeout(() => {
    emitEvent({
      event: 'error',
      message: `--max-runtime-secs (${options.maxRuntimeSecs}) watchdog fired; hard abort`,
    });
    void exitWith(EXIT_HARD_TIMEOUT);
  }, options.maxRuntimeSecs * 1000);

  let code: number;
  try {
    const page = await setupPage(options);
    const config: RunConfig = {
      ...options.config,
      elapsedBeforeStartMs: Date.now() - startedAtMs,
    };
    code = await page.evaluate((runConfig) => {
      type Runner = (config: RunConfig) => Promise<number>;
      const runner = (globalThis as { __signalFishRun?: Runner }).__signalFishRun;
      if (runner === undefined) {
        throw new Error('page bundle did not register __signalFishRun');
      }
      return runner(runConfig);
    }, config);
  } catch (error) {
    if (shuttingDown) {
      // The watchdog or a signal already owns the exit; the evaluate failure
      // is just its fallout (the page died with the browser).
      return;
    }
    // Local infrastructure failure (Chromium launch/bridge), not a protocol
    // or transport outcome: mirrors the native client's runtime-start
    // failure path, which also exits EXIT_HARD_TIMEOUT.
    emitEvent({ event: 'error', message: `browser run failed: ${String(error)}` });
    code = EXIT_HARD_TIMEOUT;
  }
  clearTimeout(watchdog);
  await exitWith(code);
}

void main();
