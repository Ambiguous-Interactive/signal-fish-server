import { run } from './orchestrator.js';
import { EXIT_CRITERIA_UNMET, EXIT_SUCCESS, type RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Investigation #889: isolate missing inbound mesh traffic by peer and label.
for (const unpaidInitiates of [false, true]) {
  for (const missingLabel of ['reliable', 'unreliable']) {
    for (const recover of [false, true]) {
      const originalSocket = globalThis.WebSocket;
      const originalPc = globalThis.RTCPeerConnection;
      const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
      const originalNow = performance.now;
      const originalError = console.error;
      const transportDiagnostics: string[] = [];
      const originalSetTimeout = globalThis.setTimeout;
      const originalClearTimeout = globalThis.clearTimeout;
      const me = '00000000-0000-0000-0000-000000000002';
      const lower = '00000000-0000-0000-0000-000000000001';
      const higher = '00000000-0000-0000-0000-000000000003';
      // The lower UUID initiates each mesh edge, as in the server plan.
      const healthy = unpaidInitiates ? lower : higher;
      const unpaid = unpaidInitiates ? higher : lower;
      const events: Record<string, unknown>[] = [];
      const sent: Array<{ peer: string; label: string; text: string }> = [];
      const connections: Connection[] = [];
      let now = 100;
      let socket!: Socket;
      let running: Promise<number> | undefined;
      const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
      const context = `initiate=${unpaidInitiates}/${missingLabel}/recover=${recover}`;
      class Channel {
        readyState = 'connecting';
        bufferedAmount = 0;
        maxPacketLifeTime = null;
        readonly ordered: boolean;
        readonly maxRetransmits: number | null;
        onopen: (() => void) | null = null;
        onmessage: ((event: { data: string }) => void) | null = null;
        onerror: (() => void) | null = null;
        constructor(
          readonly owner: string,
          readonly label: string,
        ) {
          this.ordered = label === 'reliable';
          this.maxRetransmits = this.ordered ? null : 0;
        }
        send(text: string): void {
          assert(this.readyState === 'open', `${context}: send on a live channel`);
          sent.push({ peer: this.owner, label: this.label, text });
        }
        receive(from = this.owner): void {
          this.onmessage?.({ data: JSON.stringify({ from, channel: this.label, seq: 0 }) });
        }
        close(): void {
          this.readyState = 'closed';
        }
      }
      class Connection {
        readonly owner = connections.length === 0 ? healthy : unpaid;
        readonly channels = new Map<string, Channel>();
        connectionState = 'new';
        closed = false;
        ondatachannel: ((event: { channel: Channel }) => void) | null = null;
        constructor() {
          connections.push(this);
        }
        createDataChannel(label: string): Channel {
          const channel = new Channel(this.owner, label);
          this.channels.set(label, channel);
          return channel;
        }
        async createOffer() {
          return { type: 'offer', sdp: 'local-offer' };
        }
        async createAnswer() {
          return { type: 'answer', sdp: 'local-answer' };
        }
        async setLocalDescription(): Promise<void> {}
        async setRemoteDescription(): Promise<void> {}
        close(): void {
          this.closed = true;
          for (const channel of this.channels.values()) channel.close();
        }
        open(): void {
          this.connectionState = 'connected';
          for (const channel of this.channels.values()) {
            channel.readyState = 'open';
            channel.onopen?.();
          }
        }
        announce(): void {
          for (const label of ['reliable', 'unreliable']) {
            const channel = this.createDataChannel(label);
            this.ondatachannel?.({ channel });
          }
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
        deliver(type: string, data: Record<string, unknown> = {}): void {
          this.onmessage?.({ data: JSON.stringify({ type, data }) });
        }
        send(line: string): void {
          const frame = JSON.parse(line) as { type: string };
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
                current_players: [me, healthy, unpaid].map((id) => ({ id, epoch: 1, seq: 0 })),
              });
              this.deliver('SessionPlan', {
                generation: me,
                topology: 'mesh',
                transport: 'webrtc',
                fallback: 'relay',
                ice_servers: [],
                peers: [
                  { player_id: healthy, initiate: !unpaidInitiates },
                  { player_id: unpaid, initiate: unpaidInitiates },
                ],
              });
            });
        }
        close(): void {
          this.readyState = 3;
        }
      }
      try {
        console.error = (...items: unknown[]) =>
          transportDiagnostics.push(items.map(String).join(' '));
        performance.now = () => now;
        globalThis.setTimeout = (() => 1) as unknown as typeof setTimeout;
        globalThis.clearTimeout = (() => {}) as typeof clearTimeout;
        globalThis.WebSocket = Socket as unknown as typeof WebSocket;
        globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
        Object.defineProperty(globalThis, 'window', {
          configurable: true,
          value: {
            __sf_emit: (line: string) => {
              events.push(JSON.parse(line));
            },
          },
        });
        const config: RunConfig = {
          serverUrl: 'ws://mock.invalid/v3/ws',
          createRoom: true,
          joinCode: null,
          peers: 3,
          maxPlayers: null,
          expectTotalPeers: null,
          leaveOnGameStart: false,
          gameName: 'mesh-debt',
          playerName: 'test',
          appId: 'test',
          platform: 'test',
          exchange: true,
          relayPayload: null,
          crippleIce: false,
          p2pTimeoutSecs: 1,
          p2pRetryCount: 1,
          runForSecs: 2,
          successReleaseEnabled: false,
          protocolVersion: 3,
          supportedTopologies: ['mesh'],
          supportedTransports: ['webrtc', 'relay'],
          gameDataFormat: 'json',
          sdkVersion: 'test',
          elapsedBeforeStartMs: 0,
        };
        running = run(config);
        await turn();
        assert(connections.length === 2, `${context}: create both mesh links`);
        const healthyLink = connections[0]!;
        const unpaidLink = connections[1]!;
        const remoteDescription = (peer: string, initiate: boolean, retry = false) => {
          const signal = initiate ? { Answer: 'remote-answer' } : { Offer: 'remote-offer' };
          socket.deliver('Signal', {
            from: peer,
            generation: me,
            signal: retry ? { RetrySignal: { attempt: 1, signal } } : signal,
          });
        };
        remoteDescription(healthy, !unpaidInitiates);
        remoteDescription(unpaid, unpaidInitiates);
        if (unpaidInitiates) healthyLink.announce();
        else unpaidLink.announce();
        await turn();
        healthyLink.open();
        unpaidLink.open();
        await turn();
        assert(sent.length === 4, `${context}: both roles send both probes`);
        for (const peer of [healthy, unpaid])
          for (const label of ['reliable', 'unreliable']) {
            const probe = sent.find((item) => item.peer === peer && item.label === label);
            assert(
              probe !== undefined &&
                probe.text === JSON.stringify({ from: me, channel: label, seq: 0 }),
              `${context}: exact outbound probe for ${peer}/${label}`,
            );
          }
        for (const channel of healthyLink.channels.values()) channel.receive();
        for (const channel of unpaidLink.channels.values()) {
          channel.receive(channel.label === missingLabel ? healthy : unpaid);
        }
        await turn();
        assert(
          !events.some((event) => event['event'] === 'success_criteria_met'),
          `${context}: a wrong-player receipt does not complete exchange`,
        );
        unpaidLink.channels.get(missingLabel)!.onerror?.();
        await turn();
        assert(
          unpaidLink.closed && !healthyLink.closed,
          `${context}: retire only the unpaid pair`,
        );
        assert(
          events.filter((event) => event['event'] === 'p2p_pair_connected').length === 2,
          `${context}: both pairs formed before the loss`,
        );
        assert(
          !events.some(
            (event) =>
              event['event'] === 'transport_status_sent' && event['connected'] === false,
          ),
          `${context}: the healthy pair keeps overall WebRTC available`,
        );
        const healthyReceipts = events.filter(
          (event) => event['event'] === 'channel_message' && event['peer'] === healthy,
        ).length;
        for (const channel of healthyLink.channels.values()) channel.receive();
        await turn();
        assert(
          events.filter(
            (event) => event['event'] === 'channel_message' && event['peer'] === healthy,
          ).length ===
            healthyReceipts + 2,
          `${context}: healthy inbound traffic continues after sibling loss`,
        );
        if (recover) {
          socket.deliver('Signal', { from: unpaid, generation: me, signal: { PairRetry: 1 } });
          await turn();
          assert(connections.length === 3, `${context}: rebuild the unpaid pair`);
          const replacement = connections[2]!;
          remoteDescription(unpaid, unpaidInitiates, true);
          if (!unpaidInitiates) replacement.announce();
          await turn();
          replacement.open();
          await turn();
          replacement.channels.get(missingLabel)!.receive();
          await turn();
          assert(
            sent.length === 6 && !healthyLink.closed,
            `${context}: resend only the rebuilt pair`,
          );
          for (const peer of [healthy, unpaid])
            for (const label of ['reliable', 'unreliable']) {
              const probes = sent.filter((item) => item.peer === peer && item.label === label);
              assert(
                probes.length === (peer === unpaid ? 2 : 1) &&
                  probes.every(
                    (probe) =>
                      probe.text === JSON.stringify({ from: me, channel: label, seq: 0 }),
                  ),
                `${context}: exact replacement send count and payload for ${peer}/${label}`,
              );
            }
          assert(
            events.filter((event) => event['event'] === 'p2p_pair_reconnected').length === 1,
            `${context}: report one physical replacement`,
          );
        }
        now = 2100;
        socket.deliver('Pong');
        const code = await running;
        assert(
          code === (recover ? EXIT_SUCCESS : EXIT_CRITERIA_UNMET),
          `${context}: missing peer/label debt controls exit, got ${code}`,
        );
        if (!recover) {
          const timeout = transportDiagnostics.find((line) =>
            line.startsWith(`Exchange timeout for ${unpaid}: `),
          );
          assert(timeout !== undefined, `${context}: emit a per-peer exchange snapshot`);
          const snapshot = JSON.parse(timeout!.slice(timeout!.indexOf(': ') + 2)) as Record<
            string,
            unknown
          >;
          assert(
            JSON.stringify(snapshot) ===
              JSON.stringify({
                missing_sent: [],
                missing_received: [missingLabel],
                connected: false,
                link: null,
              }),
            `${context}: snapshot names only the unpaid receive label`,
          );
          const diagnostics = events
            .filter((event) => event['event'] === 'error')
            .map((event) => String(event['message']));
          assert(
            diagnostics.some((message) =>
              message.includes(`received from ${unpaid}; missing ${missingLabel}`),
            ),
            `${context}: identify the exact missing inbound peer and label: ${diagnostics.join('; ')}`,
          );
          assert(
            !diagnostics.some(
              (message) =>
                message.includes(`received from ${healthy}`) || message.includes('sent to'),
            ),
            `${context}: keep healthy receipts and successful sends paid`,
          );
        }
      } finally {
        socket?.onclose?.();
        await running;
        console.error = originalError;
        performance.now = originalNow;
        globalThis.setTimeout = originalSetTimeout;
        globalThis.clearTimeout = originalClearTimeout;
        globalThis.WebSocket = originalSocket;
        globalThis.RTCPeerConnection = originalPc;
        if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
        else Object.defineProperty(globalThis, 'window', originalWindow);
      }
    }
  }
}
console.error(
  'ok - mixed-role mesh isolates missing inbound peer/label debt and rebuild recovery',
);
