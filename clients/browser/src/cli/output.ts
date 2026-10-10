import type { Writable } from 'node:stream';

/** Resolve only after the stream consumes the event line, including backpressure. */
export function writeAcknowledgedEventLine(output: Writable, line: string): Promise<void> {
  return new Promise((resolve, reject) => {
    output.write(`${line}\n`, (error) => {
      if (error) reject(error);
      else resolve();
    });
  });
}
