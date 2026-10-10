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
  // Node also emits failed write callbacks as error events.
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

// #879: exercise the shared limiter through actual Node stream callbacks.
const { EventOutput } = await import('../shared/event-output.js');
for (const scenario of ['count', 'bytes', 'closed', 'recovery'] as const) {
  const callbacks: Array<(error?: Error | null) => void> = [];
  const lines: string[] = [];
  const failures: Error[] = [];
  const output = new Writable({
    highWaterMark: 1,
    write(chunk, _encoding, callback) {
      lines.push(String(chunk));
      callbacks.push(callback);
    },
  });
  output.on('error', () => {});
  const writer = new EventOutput(
    (line) => writeAcknowledgedEventLine(output, line),
    (error) => failures.push(error),
    scenario === 'bytes' ? 3 : 2,
    8,
  );
  const observe = (delivery: Promise<void>) =>
    delivery.then(
      () => true,
      () => false,
    );
  const first = observe(writer.emitLine('abc'));
  const second = observe(writer.emitLine('abc'));
  let flushResult: boolean | undefined;
  const flush = observe(writer.flush()).then((result) => {
    flushResult = result;
    return result;
  });
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert(flushResult === undefined, 'flush waits for all actual stdout callbacks');
  assert(
    output.writableLength === 8,
    'the stream retains the exact eight-byte backlog boundary',
  );
  if (scenario === 'count' || scenario === 'bytes') {
    assert(
      !(await observe(writer.emitLine(scenario === 'count' ? '' : 'é'))),
      'reject count/UTF-8 overflow before writing',
    );
    assert(!(await flush), 'overload must interrupt a flush held by an older write');
    assert(failures.length === 1, 'notify the owner once');
    assert(!(await observe(writer.emitLine('x'))), 'failure remains terminal');
    assert(lines.length === 1, 'no rejected event may enter the Writable queue');
  }
  callbacks.shift()?.(scenario === 'closed' ? new Error('consumer closed') : undefined);
  await new Promise<void>((resolve) => setImmediate(resolve));
  if (scenario !== 'closed') callbacks.shift()?.();
  assert((await first) === (scenario !== 'closed'), 'preserve first write delivery');
  assert((await second) === (scenario !== 'closed'), 'preserve queued write delivery');
  if (scenario === 'recovery') {
    assert(await flush, 'all accepted lines flush after callbacks');
    const next = observe(writer.emitLine('1234567'));
    await new Promise<void>((resolve) => setImmediate(resolve));
    callbacks.shift()?.();
    assert(await next, 'consumed work restores exact byte capacity');
    assert(
      lines.join('') === 'abc\nabc\n1234567\n',
      'preserve FIFO without duplicate or missing lines',
    );
  } else if (scenario === 'closed') {
    assert(!(await flush) && failures.length === 1, 'closure fails flush and notifies once');
  }
  output.destroy();
}
console.error('ok - ordinary stdout writes stay bounded through consumption and closure');

const { DiagnosticOutput } = await import('./output.js');
for (const scenario of ['count', 'bytes'] as const) {
  const callbacks: Array<(error?: Error | null) => void> = [];
  const lines: string[] = [];
  const failures: Error[] = [];
  const stream = new Writable({
    highWaterMark: 1,
    write(chunk, _encoding, callback) {
      lines.push(String(chunk));
      callbacks.push(callback);
    },
  });
  stream.on('error', () => {});
  const diagnostics = new DiagnosticOutput(stream, (error) => failures.push(error));
  const results: Promise<boolean>[] = [];
  for (let i = 0; i < 257; i += 1)
    results.push(
      diagnostics.write(scenario === 'count' ? '' : '界'.repeat(16001)).then(
        () => true,
        () => false,
      ),
    );
  assert(
    !(await results[256]),
    'the actual diagnostic configuration must refuse before unbounded stderr buffering',
  );
  assert(failures.length === 1, 'the CLI shutdown owner receives one diagnostic failure');
  assert(stream.writableLength <= 1024 * 1024, 'stderr stays within its real one-MiB limit');
  if (scenario === 'bytes')
    assert(
      lines[0]?.includes('[truncated]') === true,
      'truncate long diagnostics before retention',
    );
  while (stream.writableLength > 0) {
    callbacks.shift()?.();
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
  const delivered = (await Promise.all(results)).filter(Boolean).length;
  assert(
    scenario === 'count' ? delivered === 256 : delivered > 0 && delivered < 256,
    'exercise independent count and retained UTF-8 byte limits',
  );
  stream.destroy();
}
console.error('ok - CLI stderr diagnostics have count, byte, line, and shutdown bounds');
