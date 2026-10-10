import { emit } from './events.js';

const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
let pending = 0;
let release: (() => void) | undefined;
const blocked = new Promise<void>((resolve) => {
  release = resolve;
});
const deliveries: Promise<void>[] = [];
try {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: {
      __sf_emit: () => {
        pending += 1;
        return blocked;
      },
    },
  });
  for (let index = 0; index < 257; index += 1) {
    deliveries.push(emit({ event: 'pc_state', peer: 'test', state: 'connected' }));
  }
  if (pending > 256)
    throw new Error(
      `ordinary output retained ${pending} unacknowledged bridge calls; expected at most 256 (#879)`,
    );
} finally {
  release?.();
  await Promise.all(deliveries);
  if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
  else Object.defineProperty(globalThis, 'window', originalWindow);
}
