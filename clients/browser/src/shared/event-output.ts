import { Backlog, utf8Bytes } from './backlog.js';

export const MAX_PENDING_OUTPUT_EVENTS = 256;
export const MAX_OUTPUT_BYTES = 128 * 1024 * 1024;

/** Bound bridge and stream writes until their delivery acknowledgment. */
export class EventOutput {
  private readonly backlog: Backlog;
  private readonly pending = new Set<Promise<void>>();
  private failure: Error | null = null;
  private terminalSent = false;
  private readonly failed = new AbortController();

  constructor(
    private readonly write: (line: string) => void | Promise<void>,
    private readonly onFailure: (error: Error) => void = () => {},
    maxCount = MAX_PENDING_OUTPUT_EVENTS,
    private readonly maxBytes = MAX_OUTPUT_BYTES,
  ) {
    this.backlog = new Backlog(maxCount, maxBytes);
  }

  async emit(event: Record<string, unknown>): Promise<void> {
    try {
      await this.emitLine(JSON.stringify(event));
    } catch (error) {
      throw this.recordFailure(error);
    }
  }

  async emitLine(line: string): Promise<void> {
    if (this.failure !== null) throw this.failure;
    let release: (() => void) | null = null;
    let delivery: Promise<void> | undefined;
    try {
      const bytes = utf8Bytes(line) + 1;
      if (bytes <= this.maxBytes) release = this.backlog.reserve(bytes);
      if (release === null) throw new Error('output backlog limit exceeded');
      delivery = Promise.resolve(this.write(line));
      this.pending.add(delivery);
      await delivery;
    } catch (error) {
      throw this.recordFailure(error);
    } finally {
      release?.();
      if (delivery !== undefined) this.pending.delete(delivery);
    }
  }

  private recordFailure(error: unknown): Error {
    const failure = error instanceof Error ? error : new Error(String(error));
    if (this.failure === null) {
      this.failure = failure;
      this.failed.abort(failure);
      this.onFailure(failure);
    }
    return failure;
  }

  async terminal(event: Record<string, unknown>): Promise<void> {
    if (this.terminalSent) return;
    this.terminalSent = true;
    const line = JSON.stringify(event);
    if (utf8Bytes(line) + 1 > 4096) throw new Error('terminal event exceeds 4096 bytes');
    await this.write(line);
  }

  async flush(): Promise<void> {
    if (this.failure !== null) throw this.failure;
    let rejectFailure: ((reason: unknown) => void) | undefined;
    const failure = new Promise<never>((_resolve, reject) => {
      rejectFailure = reject;
    });
    const abort = () => rejectFailure?.(this.failure);
    this.failed.signal.addEventListener('abort', abort, { once: true });
    try {
      await Promise.race([Promise.all(this.pending), failure]);
    } finally {
      this.failed.signal.removeEventListener('abort', abort);
    }
    if (this.failure !== null) throw this.failure;
  }
}
