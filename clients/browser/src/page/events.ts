// JSONL stdout event reporting (the client's primary output contract).
//
// The page never touches stdout itself: every event line is handed to the
// `__sf_emit` binding the CLI exposed via `page.exposeFunction`, which writes
// it to the Node process's stdout. Binding invocations are delivered in call
// order over the CDP connection, so per-client event ordering stays causal.
// Event names, field names, and semantics are byte-identical to the native
// client's (clients/native/src/events.rs).

/** Classification of an opaque matchbox-shaped signal payload. */
export type SignalKind = 'offer' | 'answer' | 'ice_candidate' | 'other';

/** Classify an opaque signal value by its single matchbox-convention key. */
export function classifySignal(signal: unknown): SignalKind {
  if (typeof signal !== 'object' || signal === null || Array.isArray(signal)) {
    return 'other';
  }
  const keys = Object.keys(signal);
  if (keys.length !== 1) {
    return 'other';
  }
  switch (keys[0]) {
    case 'Offer':
      return 'offer';
    case 'Answer':
      return 'answer';
    case 'IceCandidate':
      return 'ice_candidate';
    default:
      return 'other';
  }
}

declare global {
  interface Window {
    /** stdout bridge exposed by the CLI before the page bundle is injected. */
    __sf_emit?: (line: string) => void;
    /** CLI-side release-file probe used only by deterministic test harnesses. */
    __sf_success_released?: () => Promise<boolean>;
  }
}

/**
 * Emit one event object as a JSONL stdout line. `event` must carry the
 * snake_case `event` tag as its first property (construction sites keep that
 * ordering so the emitted JSON matches the native client's field order).
 * Failures are swallowed: a torn-down bridge must never break the run.
 */
export function emit(event: Record<string, unknown>): void {
  try {
    window.__sf_emit?.(JSON.stringify(event));
  } catch (error) {
    // The bridge only fails during CLI teardown; stderr is still wired.
    console.error(`failed to emit stdout event: ${String(error)}`);
  }
}
