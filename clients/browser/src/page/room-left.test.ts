import { run } from './orchestrator.js';
import type { RunConfig } from '../shared/types.js';

const failures: string[] = [];
// Regression #1150, audit #826: RoomLeft ends physical gameplay and pending work.
for (const scenario of ['connected', 'offer-pending'] as const) {
  const originalPc = globalThis.RTCPeerConnection;
  const originalSocket = globalThis.WebSocket;
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  const originalNow = performance.now;
  const originalSetTimeout = globalThis.setTimeout;
  const originalClearTimeout = globalThis.clearTimeout;
  const originalError = console.error;
  const me = '00000000-0000-0000-0000-000000000001';
  const peer = '00000000-0000-0000-0000-000000000002';
  const events: Record<string, unknown>[] = [];
  const sent: Array<{ type: string; data: Record<string, unknown> }> = [];
  const connections: Connection[] = [];
  let now = 100;
  let socket!: Socket;
  let allowSend = false;
  let result: number | undefined;
  let releaseOffer!: () => void;
  let running: Promise<number> | undefined;
  const offerHeld = new Promise<void>((resolve) => {
    releaseOffer = resolve;
  });
  const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
  const check = (condition: boolean, message: string) => {
    if (!condition) failures.push(`${scenario}: ${message}`);
  };
  class Channel {
    readyState = 'open';
    bufferedAmount = 0;
    maxPacketLifeTime = null;
    readonly ordered: boolean;
    readonly maxRetransmits: number | null;
    onmessage: ((event: { data: string }) => void) | null = null;
    onopen: (() => void) | null = null;
    constructor(readonly label: string) {
      this.ordered = label === 'reliable';
      this.maxRetransmits = this.ordered ? null : 0;
    }
    send(): void {
      if (!allowSend) throw new Error('send temporarily unavailable');
      events.push({ event: 'test_send', label: this.label });
    }
    close(): void {
      this.readyState = 'closed';
    }
  }
  class Connection {
    closed = false;
    connectionState = 'connected';
    readonly channels: Channel[] = [];
    ondatachannel: ((event: { channel: Channel }) => void) | null = null;
    constructor() {
      connections.push(this);
    }
    createDataChannel(label: string): Channel {
      const channel = new Channel(label);
      channel.readyState = 'connecting';
      this.channels.push(channel);
      return channel;
    }
    async createOffer() {
      await offerHeld;
      return { type: 'offer', sdp: 'old-room-offer' };
    }
    async setLocalDescription(): Promise<void> {}
    close(): void {
      this.closed = true;
      for (const channel of this.channels) channel.close();
    }
  }
  class Socket {
    static OPEN = 1;
    readyState = 1;
    bufferedAmount = 0;
    onopen: (() => void) | null = null;
    onclose: (() => void) | null = null;
    onmessage: ((event: { data: string }) => void) | null = null;
    constructor() {
      socket = this;
      queueMicrotask(() => this.onopen?.());
    }
    deliver(type: string, data: Record<string, unknown> = {}) {
      this.onmessage?.({ data: JSON.stringify({ type, data }) });
    }
    send(line: string) {
      const frame = JSON.parse(line) as { type: string; data: Record<string, unknown> };
      sent.push(frame);
      if (frame.type === 'Authenticate')
        queueMicrotask(() => {
          this.deliver('Authenticated');
          this.deliver('ProtocolInfo', { protocol_version: 3 });
        });
      if (frame.type === 'JoinRoom')
        queueMicrotask(() => {
          this.deliver('RoomJoined', {
            player_id: me,
            room_id: me,
            room_code: 'TEST',
            lobby_state: 'finalized',
            ready_players: [],
            current_players: [me, peer].map((id) => ({ id, epoch: 1, seq: 0 })),
          });
          this.deliver('SessionPlan', {
            generation: me,
            topology: 'mesh',
            transport: 'webrtc',
            fallback: 'relay',
            ice_servers: [],
            peers: [{ player_id: peer, initiate: scenario === 'offer-pending' }],
          });
        });
    }
    close() {
      this.readyState = 3;
    }
  }
  try {
    globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
    globalThis.WebSocket = Socket as unknown as typeof WebSocket;
    performance.now = () => now;
    globalThis.setTimeout = (() => 1) as unknown as typeof setTimeout;
    globalThis.clearTimeout = (() => {}) as typeof clearTimeout;
    console.error = () => {};
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { __sf_emit: (line: string) => events.push(JSON.parse(line)) },
    });
    const config: RunConfig = {
      serverUrl: 'ws://mock.invalid/v3/ws',
      createRoom: true,
      joinCode: null,
      peers: 2,
      maxPlayers: null,
      expectTotalPeers: null,
      leaveOnGameStart: false,
      gameName: 'room-left',
      playerName: 'test',
      appId: 'test',
      platform: 'test',
      exchange: true,
      relayPayload: 'old-room-relay',
      crippleIce: false,
      p2pTimeoutSecs: 1,
      p2pRetryCount: 0,
      runForSecs: 120,
      successReleaseEnabled: false,
      protocolVersion: 3,
      supportedTopologies: ['mesh'],
      supportedTransports: ['webrtc', 'relay'],
      gameDataFormat: 'json',
      sdkVersion: 'test',
      elapsedBeforeStartMs: 0,
    };
    running = run(config).then((code) => {
      result = code;
      return code;
    });
    await turn();
    const old = connections[0];
    if (old === undefined) throw new Error('the real plan must create a connection');
    if (scenario === 'connected') {
      for (const label of ['reliable', 'unreliable']) {
        const channel = new Channel(label);
        old.channels.push(channel);
        old.ondatachannel?.({ channel });
      }
      await turn();
      check(
        events.some((event) => event['event'] === 'p2p_pair_connected'),
        'form the old pair',
      );
    }
    const oldCallbacks = (scenario === 'connected' ? old.channels : []).map((channel) => ({
      label: channel.label,
      message: channel.onmessage,
      open: channel.onopen,
    }));
    const eventsBefore = events.length;
    socket.deliver('RoomLeft');
    // These callbacks can already be queued when RoomLeft reaches its handler.
    for (const callback of oldCallbacks)
      callback.message?.({
        data: JSON.stringify({ from: peer, channel: callback.label, seq: 0 }),
      });
    await turn();
    check(old.closed, 'RoomLeft closes the old physical connection immediately');
    const sentBefore = sent.length;
    allowSend = true;
    releaseOffer();
    for (const callback of oldCallbacks) {
      callback.open?.();
      callback.message?.({
        data: JSON.stringify({ from: peer, channel: callback.label, seq: 0 }),
      });
    }
    now = 2100;
    socket.deliver('Pong');
    await turn();
    check(
      !sent
        .slice(sentBefore)
        .some((frame) => ['Signal', 'GameData', 'TransportStatus'].includes(frame.type)),
      'RoomLeft cancels old negotiation, relay, and transport-status writes',
    );
    check(
      !events
        .slice(eventsBefore)
        .some((event) =>
          [
            'channel_message',
            'channel_message_sent',
            'test_send',
            'p2p_pair_connected',
          ].includes(String(event['event'])),
        ),
      'saved old callbacks and retry timers cannot publish gameplay after RoomLeft',
    );
    if (scenario === 'connected') {
      now = 120100;
      socket.deliver('Pong');
      await turn();
      check(result === 1, 'leaving cannot convert an unpaid exchange into success');
      check(
        events.some(
          (event) =>
            event['event'] === 'error' &&
            String(event['message']).includes(`exchange incomplete (received from ${peer}`),
        ),
        'queued old-room receipts cannot pay the retained exchange debt',
      );
    } else {
      socket.deliver('RoomJoined', {
        current_players: [me, peer].map((id) => ({ id, epoch: 2, seq: 0 })),
        ready_players: [],
      });
      socket.deliver('SessionPlan', {
        generation: peer,
        topology: 'mesh',
        transport: 'webrtc',
        fallback: 'relay',
        ice_servers: [],
        peers: [{ player_id: peer, initiate: false }],
      });
      await turn();
      const fresh = connections[1];
      check(fresh !== undefined && !fresh.closed, 'a fresh plan can rebuild the same peer');
      if (fresh !== undefined) {
        for (const label of ['reliable', 'unreliable']) {
          const channel = new Channel(label);
          fresh.channels.push(channel);
          fresh.ondatachannel?.({ channel });
        }
        await turn();
        check(
          events.filter((event) => event['event'] === 'test_send').length === 2,
          'only fresh physical channels can send the new exchange',
        );
      }
    }
  } finally {
    releaseOffer();
    socket?.onclose?.();
    await running;
    globalThis.RTCPeerConnection = originalPc;
    globalThis.WebSocket = originalSocket;
    performance.now = originalNow;
    globalThis.setTimeout = originalSetTimeout;
    globalThis.clearTimeout = originalClearTimeout;
    console.error = originalError;
    if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
    else Object.defineProperty(globalThis, 'window', originalWindow);
  }
}
if (failures.length !== 0) throw new Error(failures.join('\n'));
console.error('ok - RoomLeft retires old gameplay, negotiation, and timers');
