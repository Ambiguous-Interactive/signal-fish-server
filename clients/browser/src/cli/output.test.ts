import { Writable } from 'node:stream';
import { writeAcknowledgedEventLine } from './output.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

for (const fails of [false, true]) {
  let consume: ((error?: Error | null) => void) | undefined;
  let observed = '';
  const output = new Writable({
    highWaterMark: 1,
    write(chunk, _encoding, callback) {
      observed += String(chunk);
      consume = callback;
    },
  });
  // Node also emits failed write callbacks as error events. The CLI owns this
  // permanent handler so ordinary shutdown writes never crash the process.
  output.on('error', () => {});
  let settled = false;
  const delivery = writeAcknowledgedEventLine(output, '{"event":"channel_message"}');
  const result = delivery.then(
    () => {
      settled = true;
      return true;
    },
    () => {
      settled = true;
      return false;
    },
  );
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert(!settled, 'the bridge must not acknowledge a line while stdout retains it');
  assert(output.writableNeedDrain, 'the test must exercise actual stream backpressure');
  assert(observed === '{"event":"channel_message"}\n', 'write one complete JSONL line');
  consume?.(fails ? new Error('consumer closed') : undefined);
  assert((await result) === !fails, 'write acknowledgment must preserve stream failures');
  output.destroy();
}
console.error('ok - stdout acknowledgments wait for stream consumption and propagate errors');
