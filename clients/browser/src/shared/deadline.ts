/** Largest delay accepted consistently by browser and Node setTimeout hosts. */
export const MAX_TIMER_DELAY_MS = 2_147_483_647;

/** Minimal timer host seam for deterministic deadline scheduling tests. */
export interface TimeoutHost {
  now(): number;
  setTimeout(callback: () => void, delayMs: number): unknown;
  clearTimeout(handle: unknown): void;
}

/** A scheduled absolute deadline that can be canceled idempotently. */
export interface ScheduledDeadline {
  cancel(): void;
}

const defaultTimeoutHost: TimeoutHost = {
  now: Date.now,
  setTimeout(callback, delayMs) {
    return setTimeout(callback, delayMs);
  },
  clearTimeout(handle) {
    clearTimeout(handle as ReturnType<typeof setTimeout>);
  },
};

/**
 * Add an exact whole-second duration to a millisecond base instant.
 * Unrepresentable absolute deadlines saturate in the distant future instead
 * of wrapping or falling back to an already-due instant.
 */
export function deadlineAfterSeconds(baseMs: number, seconds: number): number {
  if (!Number.isSafeInteger(baseMs)) {
    throw new RangeError(`deadline base must be a safe integer, got ${baseMs}`);
  }
  if (!Number.isSafeInteger(seconds) || seconds < 0) {
    throw new RangeError(
      `deadline seconds must be a non-negative safe integer, got ${seconds}`,
    );
  }
  if (seconds > Math.floor((Number.MAX_SAFE_INTEGER - baseMs) / 1_000)) {
    return Number.MAX_SAFE_INTEGER;
  }
  return baseMs + seconds * 1_000;
}

/**
 * Schedule an absolute deadline without passing an overflowing delay to the
 * host. Distant deadlines are revisited in bounded chunks until truly due.
 */
export function scheduleDeadline(
  deadlineMs: number,
  callback: () => void,
  host: TimeoutHost = defaultTimeoutHost,
): ScheduledDeadline {
  let canceled = false;
  let handle: unknown = null;

  const arm = (): void => {
    const remainingMs = deadlineMs - host.now();
    const delayMs = Math.min(Math.max(0, remainingMs), MAX_TIMER_DELAY_MS);
    handle = host.setTimeout(() => {
      handle = null;
      if (canceled) {
        return;
      }
      if (host.now() < deadlineMs) {
        arm();
      } else {
        callback();
      }
    }, delayMs);
  };

  arm();
  return {
    cancel(): void {
      if (canceled) {
        return;
      }
      canceled = true;
      if (handle !== null) {
        host.clearTimeout(handle);
        handle = null;
      }
    },
  };
}
