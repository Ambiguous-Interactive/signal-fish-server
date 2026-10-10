import type { Writable } from 'node:stream';
import { EventOutput } from '../shared/event-output.js';

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
