import { EventOutput } from '../shared/event-output.js';

// JSONL stdout event reporting (the client's primary output contract).
//
// The page never touches stdout itself: every event line is handed to the
// `__sf_emit` binding the CLI exposed via `page.exposeFunction`, which writes
// it to the Node process's stdout. Binding invocations are delivered in call
// order over the CDP connection, so per-client event ordering stays causal.
// Event names, field names, and semantics are byte-identical to the native
// client's (clients/native/src/events.rs).

/** Classification of an opaque matchbox-shaped signal payload. */
export type SignalKind = 'offer' | 'answer' | 'ice_candidate' | 'pair_retry' | 'other';

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
    case 'PairRetry':
      return 'pair_retry';
    default:
      return 'other';
  }
}

/** Keep default Matchbox signals unchanged; identify each opt-in retry link. */
export function signalForRetryAttempt(signal: unknown, attempt: number): unknown {
  const kind = classifySignal(signal);
  if (kind === 'pair_retry') return signal;
  if (typeof signal !== 'object' || signal === null || Array.isArray(signal)) {
    return attempt === 0 ? signal : undefined;
  }
  if (!Object.hasOwn(signal, 'RetrySignal')) return attempt === 0 ? signal : undefined;
  if (attempt === 0 || Object.keys(signal).length !== 1) return undefined;
  const envelope = (signal as Record<string, unknown>)['RetrySignal'];
  if (typeof envelope !== 'object' || envelope === null || Array.isArray(envelope))
    return undefined;
  const tagged = envelope as Record<string, unknown>;
  if (Object.keys(tagged).length !== 2 || tagged['attempt'] !== attempt) return undefined;
  const payload = tagged['signal'];
  const payloadKind = classifySignal(payload);
  return payloadKind === 'offer' || payloadKind === 'answer' || payloadKind === 'ice_candidate'
    ? payload
    : undefined;
}

export function tagRetrySignal(
  signal: Record<string, unknown>,
  attempt: number,
): Record<string, unknown> {
  return attempt > 0 && classifySignal(signal) !== 'pair_retry'
    ? { RetrySignal: { attempt, signal } }
    : signal;
}

declare global {
  interface Window {
    /** stdout bridge exposed by the CLI before the page bundle is injected. */
    __sf_emit?: (line: string, terminal?: boolean) => void | Promise<void>;
    /** CLI-side release-file probe used only by deterministic test harnesses. */
    __sf_success_released?: () => Promise<boolean>;
  }
}

const output = new EventOutput((line) => {
  const bridge = window.__sf_emit;
  if (bridge === undefined) throw new Error('stdout bridge is unavailable');
  return bridge(line);
});

/** Emit one bounded event and propagate required delivery failures. */
export async function emit(
  event: Record<string, unknown>,
  requireDelivery = false,
): Promise<void> {
  try {
    await output.emit(event);
  } catch (error) {
    console.error(`failed to emit stdout event: ${String(error)}`);
    if (requireDelivery) throw new Error('stdout event delivery failed');
  }
}
