// Page bundle entrypoint (IIFE injected into headless Chromium by the CLI).
//
// Registers the single page-side API: `window.__signalFishRun(config)`, which
// runs one full client lifecycle and resolves with the exit code. The CLI
// exposes the `__sf_emit` stdout bridge BEFORE injecting this bundle, so
// every event the run emits reaches the Node process in order.

import type { RunConfig } from '../shared/types.js';
import { run } from './orchestrator.js';

declare global {
  interface Window {
    __signalFishRun?: (config: RunConfig) => Promise<number>;
  }
}

window.__signalFishRun = run;
