import { Writable } from 'node:stream';
import { forwardPageEvent, writeAcknowledgedEventLine } from './output.js';

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

// #881: preserve both terminal lines across the page-to-CLI boundary.
{
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
  const writer = new EventOutput(
    (line) => writeAcknowledgedEventLine(stream, line),
    (error) => failures.push(error),
    256,
    128 * 1024 * 1024,
    2,
  );
  const observe = (delivery: Promise<void>) =>
    delivery.then(
      () => true,
      () => false,
    );
  const page = new EventOutput((line, terminal) => forwardPageEvent(writer, line, terminal));
  const ordinary = Array.from({ length: 256 }, (_, index) =>
    observe(page.emit({ event: 'ordinary', index })),
  );
  const pageError = observe(page.terminal({ event: 'error' }));
  const duplicate = observe(forwardPageEvent(writer, '{"event":"duplicate"}', true));
  const exiting = observe(writer.terminal({ event: 'exiting', code: 2 }));
  assert(failures.length === 0, 'a reserved page error must not overflow ordinary output');
  assert(await duplicate, 'a repeated page terminal event uses no additional slot');
  assert(
    !(await observe(forwardPageEvent(writer, '界'.repeat(1366), true))),
    'duplicate terminal lines still obey the serialized UTF-8 byte limit',
  );
  assert(
    !(await observe(writer.terminalLine('', 2))),
    'reject slots outside the fixed terminal reserve',
  );
  assert(
    !(await observe(forwardPageEvent(writer, {}, true))),
    'reject non-string bridge lines',
  );
  assert(
    !(await observe(forwardPageEvent(writer, '', 'true'))),
    'reject non-boolean terminal flags',
  );
  assert(
    failures.length === 0,
    'terminal reserve and malformed inputs do not stop ordinary output',
  );
  assert(
    !(await observe(forwardPageEvent(writer, '{"event":"overflow"}', false))),
    'the actual CLI still rejects the 257th ordinary event',
  );
  assert(failures.length === 1, 'ordinary overflow notifies the CLI shutdown owner once');
  assert(stream.writableLength > 0, 'ordinary writes must still be held by stdout');
  while (stream.writableLength > 0) {
    callbacks.shift()?.();
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
  assert(await pageError, 'deliver the page error despite a full ordinary backlog');
  assert(await exiting, 'deliver the independent CLI exiting slot');
  assert((await Promise.all(ordinary)).every(Boolean), 'deliver every accepted ordinary event');
  assert(
    lines[256] === '{"event":"error"}\n',
    'page error follows ordinary events in FIFO order',
  );
  assert(lines[257] === '{"event":"exiting","code":2}\n', 'CLI exiting follows the page error');
  assert(lines.length === 258, 'terminal reserves admit only one line per slot');
  assert(
    failures.length === 1,
    'terminal delivery preserves the existing ordinary overflow decision',
  );
  stream.destroy();
}
console.error('ok - page terminal output bypasses the full ordinary CLI backlog');

{
  const flags: Array<boolean | undefined> = [];
  const lines: string[] = [];
  const output = new EventOutput((line, terminal) => {
    lines.push(line);
    flags.push(terminal);
  });
  const observe = (delivery: Promise<void>) =>
    delivery.then(
      () => true,
      () => false,
    );
  assert(
    !(await observe(output.terminalLine('a'.repeat(4096)))),
    'count the terminal newline in its cap',
  );
  assert(
    await observe(output.terminalLine('a'.repeat(4095))),
    'accept the exact terminal byte boundary',
  );
  await output.terminalLine('ignored');
  assert(
    !(await observe(output.terminalLine('', 1))),
    'the default output owns one terminal slot',
  );
  await output.emitLine('ordinary');
  assert(lines.length === 2, 'oversize and duplicate terminal lines retain no output');
  assert(
    flags[0] === true && flags[1] === undefined,
    'only terminal writes carry the bridge bypass flag',
  );
}
console.error('ok - terminal slots and UTF-8 limits apply before bridge retention');

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
