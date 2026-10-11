import type { Writable } from 'node:stream';
import { EventOutput } from '../shared/event-output.js';

/** Reserve the error before CLI shutdown fences page callbacks. */
export function stopOnOutputFailure(output: EventOutput, error: Error, stop: () => void): void {
  void output
    .terminalLine(JSON.stringify({ event: 'error', message: error.message.slice(0, 512) }), 1)
    .catch(() => {});
  stop();
}

/** Validate and forward an event from the page bridge. */
export function forwardPageEvent(
  output: EventOutput,
  line: unknown,
  terminal: unknown,
): Promise<void> {
  if (typeof line !== 'string') return Promise.reject(new Error('invalid page event line'));
  if (terminal !== undefined && typeof terminal !== 'boolean')
    return Promise.reject(new Error('invalid page terminal flag'));
  return terminal === true ? output.terminalLine(line, 1) : output.emitLine(line);
}

/** Resolve only after the stream consumes the event line, including backpressure. */
export function writeAcknowledgedEventLine(output: Writable, line: string): Promise<void> {
  return new Promise((resolve, reject) => {
    output.write(`${line}\n`, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}

/** Bound ordinary diagnostics before they enter the stderr stream. */
export class DiagnosticOutput {
  private readonly output: EventOutput;

  constructor(stream: Writable, onFailure: (error: Error) => void) {
    this.output = new EventOutput(
      (line) => writeAcknowledgedEventLine(stream, line),
      onFailure,
      256,
      1024 * 1024,
    );
  }

  write(message: string): Promise<void> {
    const bounded =
      message.length > 16_000 ? `${message.slice(0, 16_000)} [truncated]` : message;
    return this.output.emitLine(`[signal-fish-reference-browser] ${bounded}`);
  }
}
