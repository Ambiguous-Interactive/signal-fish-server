import { Backlog, utf8Bytes } from './backlog.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}
for (const value of [
  '',
  'plain',
  'é',
  '界',
  '🌊',
  '\ud800',
  '\udfff',
  '\ud800a',
  '\ud800\ud800\udfff',
]) {
  assert(
    utf8Bytes(value) === new TextEncoder().encode(value).byteLength,
    `exact UTF-8 charge for ${JSON.stringify(value)}`,
  );
}
const backlog = new Backlog(2, 4);
const first = backlog.reserve(4);
assert(first !== null, 'admit the exact byte boundary');
const empty = backlog.reserve(0);
assert(empty !== null, 'zero-byte work consumes one count credit');
assert(backlog.reserve(0) === null, 'count bounds empty work');
empty?.();
assert(backlog.reserve(1) === null, 'bytes remain charged until the owner releases');
first?.();
first?.();
const whole = backlog.reserve(4);
assert(
  whole !== null && backlog.reserve(1) === null,
  'releasing twice cannot create extra byte capacity',
);
whole?.();
for (const invalid of [-1, NaN, Infinity, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
  assert(backlog.reserve(invalid) === null, 'reject invalid accounting units');
}
console.error('ok - retention credits preserve exact UTF-8, byte and count boundaries');
