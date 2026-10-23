import { Engine } from './engine.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Investigation #889: stack delivery totals must remain distinct from JS callbacks.
const originalPc = globalThis.RTCPeerConnection;
const originalNow = performance.now;
let now = 100;
let connection!: Connection;
let resolveStats!: (report: Map<string, object>) => void;
let rejectStats!: (error: Error) => void;
class Channel {
  readonly id = 7;
  readonly label = 'unreliable';
  readonly ordered = false;
  readonly maxRetransmits = 0;
  readonly maxPacketLifeTime = null;
  readyState = 'open';
  bufferedAmount = 0;
  close(): void {}
}
class Connection {
  calls = 0;
  connectionState = 'connected';
  ondatachannel: ((event: { channel: Channel }) => void) | null = null;
  constructor() {
    connection = this;
  }
  getStats(): Promise<Map<string, object>> {
    this.calls += 1;
    return new Promise((resolve, reject) => {
      resolveStats = resolve;
      rejectStats = reject;
    });
  }
  close(): void {}
}
const stats = (overrides = {}) =>
  new Map([
    [
      'row',
      {
        type: 'data-channel',
        label: 'unreliable',
        dataChannelIdentifier: 7,
        state: 'open',
        messagesSent: 1,
        messagesReceived: 2,
        bytesSent: 31,
        bytesReceived: 62,
        ...overrides,
      },
    ],
  ]);
const engine = new Engine(false, {
  onLocalCandidate() {},
  onPcState() {},
  onChannelOpen() {},
  onChannelClosed() {},
  onChannelMessage() {},
});
interface Snapshot {
  dataChannelStats: {
    status: string;
    ageMs: number | null;
    pending: boolean;
    pendingAgeMs: number | null;
    channels: Record<string, { id: number; messagesReceived: number; bytesReceived: number }>;
  };
  channels: Record<string, { receipts: { raw: number; admitted: number } }>;
}
const snapshot = () => engine.diagnosticSnapshot('peer') as Snapshot;
try {
  globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
  performance.now = () => now;
  await engine.pairWith('peer', false, []);
  connection.ondatachannel?.({ channel: new Channel() });
  await Promise.resolve();
  assert(
    snapshot().dataChannelStats.status === 'not_requested',
    'no fabricated transport evidence',
  );
  engine.sampleDataChannelStats('peer');
  engine.sampleDataChannelStats('peer');
  assert(connection.calls === 1, 'one in-flight query bounds a hung browser stats call');
  assert(snapshot().dataChannelStats.pending, 'pending evidence is explicit');
  now = 120;
  assert(snapshot().dataChannelStats.pendingAgeMs === 20, 'a hung query exposes its age');
  resolveStats(stats());
  await Promise.resolve();
  const first = snapshot();
  assert(
    first.dataChannelStats.status === 'ready' && first.dataChannelStats.ageMs === 20,
    'age includes the asynchronous query duration',
  );
  assert(
    first.dataChannelStats.channels['unreliable']?.messagesReceived === 2 &&
      first.channels['unreliable']?.receipts.raw === 0,
    'browser stack receipts do not fabricate callback admission',
  );
  first.dataChannelStats.channels['unreliable']!.messagesReceived = 999;
  assert(
    snapshot().dataChannelStats.channels['unreliable']?.messagesReceived === 2,
    'snapshot counters are independent copies',
  );
  now = 130;
  engine.sampleDataChannelStats('peer');
  rejectStats(new Error('browser stats failed'));
  await Promise.resolve();
  await Promise.resolve();
  assert(
    snapshot().dataChannelStats.status === 'failed' &&
      snapshot().dataChannelStats.ageMs === 30 &&
      !snapshot().dataChannelStats.pending,
    'failed refresh retains explicitly aged previous evidence',
  );
  for (const overrides of [
    { dataChannelIdentifier: 8 },
    { label: 'reliable' },
    { type: 'candidate-pair' },
    { messagesReceived: NaN },
    { bytesReceived: -1 },
  ]) {
    engine.sampleDataChannelStats('peer');
    resolveStats(stats(overrides));
    await Promise.resolve();
    assert(
      snapshot().dataChannelStats.status === 'unavailable' &&
        Object.keys(snapshot().dataChannelStats.channels).length === 0,
      'missing, unrelated, or invalid channel reports cannot serve as delivery evidence',
    );
  }
  engine.sampleDataChannelStats('peer');
  const resolveOld = resolveStats;
  engine.removePeer('peer');
  await engine.pairWith('peer', false, []);
  connection.ondatachannel?.({ channel: new Channel() });
  await Promise.resolve();
  resolveOld(stats());
  await Promise.resolve();
  assert(
    snapshot().dataChannelStats.status === 'not_requested',
    'retired query cannot contaminate replacement',
  );
  // Some browser implementations do not expose getStats or data-channel rows.
  Object.defineProperty(connection, 'getStats', {
    configurable: true,
    value: () => {
      throw new Error('synchronous browser failure');
    },
  });
  engine.sampleDataChannelStats('peer');
  assert(
    snapshot().dataChannelStats.status === 'failed' && !snapshot().dataChannelStats.pending,
    'synchronous browser errors cannot escape gameplay',
  );
  Object.defineProperty(connection, 'getStats', { value: undefined });
  engine.sampleDataChannelStats('peer');
  assert(
    snapshot().dataChannelStats.status === 'unavailable',
    'absent browser API remains explicit',
  );
} finally {
  engine.removePeer('peer');
  performance.now = originalNow;
  globalThis.RTCPeerConnection = originalPc;
}
console.error('ok - browser stack delivery stats are bounded, aged, and generation fenced');
