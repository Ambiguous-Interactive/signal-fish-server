import { run } from './orchestrator.js';
import type { RunConfig } from '../shared/types.js';

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

// Regression #1015: a resumed buffer must retain its original link ownership.
for (const phase of ['remote-offer', 'answer', 'local-answer', 'candidate'] as const) {
  for (const replacement of [
    'retry',
    'generation',
    'departure',
    'incarnation',
    'loss',
    'closed',
    'channel-close',
    'channel-error',
    'timeout',
    'late-completion',
    'none',
  ] as const) {
    for (const oldOutcome of (replacement === 'none'
      ? ['resolve']
      : ['resolve', 'reject', 'pending']) as readonly string[]) {
      const originalError = console.error;
      const diagnostics: string[] = [];
      const originalPc = globalThis.RTCPeerConnection;
      const originalSocket = globalThis.WebSocket;
      const originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
      const originalNow = performance.now;
      const originalSetTimeout = globalThis.setTimeout;
      const originalClearTimeout = globalThis.clearTimeout;
      const me = '00000000-0000-0000-0000-000000000001';
      const peer = '00000000-0000-0000-0000-000000000002';
      const nextGeneration = '00000000-0000-0000-0000-000000000003';
      const initiate = false;
      const events: Record<string, unknown>[] = [];
      const sent: Record<string, unknown>[] = [];
      const connections: Connection[] = [];
      const calls: string[] = [];
      const timers = new Map<number, { at: number; callback: () => void }>();
      let timerId = 0;
      let now = 100;
      let socket!: Socket;
      let oldReached = false;
      let resolveOld!: () => void;
      let rejectOld!: (reason: Error) => void;
      let resolveReplacement!: () => void;
      const held = new Promise<void>((resolve, reject) => {
        resolveOld = resolve;
        rejectOld = reject;
      });
      const replacementHeld = new Promise<void>((resolve) => {
        resolveReplacement = resolve;
      });
      const turn = () => new Promise<void>((resolve) => originalSetTimeout(resolve, 0));
      let running: Promise<number> | undefined;
      class Connection {
        readonly index = connections.length;
        connectionState = 'new';
        onconnectionstatechange: (() => void) | null = null;
        closed = false;
        constructor() {
          connections.push(this);
        }
        createDataChannel(label: string) {
          return { label, readyState: 'connecting' };
        }
        async step(name: string) {
          calls.push(`${this.index}:${name}`);
          if (this.index === 1 && phase === name) {
            oldReached = true;
            await held;
          }
          if (this.index === 2 && name.startsWith('remote-')) await replacementHeld;
        }
        async createOffer() {
          await this.step('offer');
          return { type: 'offer', sdp: `offer-${this.index}` };
        }
        async createAnswer() {
          await this.step('answer');
          return { type: 'answer', sdp: `answer-${this.index}` };
        }
        async setLocalDescription(description: { type: string }) {
          await this.step(`local-${description.type}`);
        }
        async setRemoteDescription(description: { type: string }) {
          await this.step(`remote-${description.type}`);
        }
        async addIceCandidate() {
          await this.step('candidate');
        }
        ondatachannel: ((event: { channel: Channel }) => void) | null = null;
        close() {
          this.closed = true;
        }
      }
      class Channel {
        readonly ordered: boolean;
        readonly maxRetransmits: number | null;
        constructor(readonly label: string) {
          this.ordered = label === 'reliable';
          this.maxRetransmits = this.ordered ? null : 0;
        }
        readonly maxPacketLifeTime = null;
        readyState = 'connecting';
        bufferedAmount = 17;
        onclose: (() => void) | null = null;
        onerror: (() => void) | null = null;
        close() {
          this.readyState = 'closed';
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
        signal(signal: Record<string, unknown>, generation = me, from = peer) {
          this.deliver('Signal', { from, generation, signal });
        }
        send(line: string) {
          const frame = JSON.parse(line) as Record<string, unknown>;
          sent.push(frame);
          if (frame['type'] === 'Authenticate')
            queueMicrotask(() => {
              this.deliver('Authenticated');
              this.deliver('ProtocolInfo', { protocol_version: 3 });
            });
          if (frame['type'] === 'JoinRoom')
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
                peers: [{ player_id: peer, initiate }],
              });
            });
        }
        close() {
          this.readyState = 3;
        }
      }
      try {
        console.error = (...args: unknown[]) => diagnostics.push(args.map(String).join(' '));
        globalThis.RTCPeerConnection = Connection as unknown as typeof RTCPeerConnection;
        globalThis.WebSocket = Socket as unknown as typeof WebSocket;
        Object.defineProperty(globalThis, 'window', {
          configurable: true,
          value: { __sf_emit: (line: string) => events.push(JSON.parse(line)) },
        });
        performance.now = () => now;
        globalThis.setTimeout = ((callback: () => void, delay = 0) => {
          const id = ++timerId;
          timers.set(id, { at: now + delay, callback });
          return id;
        }) as unknown as typeof setTimeout;
        globalThis.clearTimeout = ((id: number) =>
          timers.delete(id)) as unknown as typeof clearTimeout;
        const config: RunConfig = {
          serverUrl: 'ws://mock.invalid/v3/ws',
          createRoom: true,
          joinCode: null,
          peers: 3,
          maxPlayers: null,
          expectTotalPeers: null,
          leaveOnGameStart: false,
          gameName: 'buffered-retirement',
          playerName: 'test',
          appId: 'test',
          platform: 'test',
          exchange: false,
          relayPayload: null,
          crippleIce: false,
          p2pTimeoutSecs: 1,
          p2pRetryCount: 1,
          runForSecs: 3,
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
        const first = connections[0]!;
        first.connectionState = 'failed';
        first.onconnectionstatechange?.();
        await turn();
        socket.signal({ Offer: 'buffered-old-offer' });
        socket.signal({ IceCandidate: 'buffered-old-candidate' });
        socket.signal({ IceCandidate: 'buffered-old-tail' });
        socket.signal({ Offer: 'buffered-old-tail-offer' });
        await turn();
        socket.deliver('NewPeer', { peer_id: peer, you_initiate: false });
        await turn();
        assert(oldReached, `${phase}: hold the selected browser operation`);
        const sentBeforeRetry = sent.filter((frame) => frame['type'] === 'Signal').length;
        const errorsBeforeRetry = events.filter((event) => event['event'] === 'error').length;
        now = 700;
        if (replacement === 'none') {
          resolveOld();
          await turn();
          assert(
            calls.filter((call) => call === '1:candidate').length === 2,
            'live drain consumes both buffered candidates',
          );
          assert(
            sent.filter((frame) => frame['type'] === 'Signal').length === 2,
            'live drain sends both buffered-offer answers',
          );
          assert(
            calls.filter((call) => call.startsWith('1:')).join(',') ===
              [
                'remote-offer',
                'answer',
                'local-answer',
                'candidate',
                'candidate',
                'remote-offer',
                'answer',
                'local-answer',
              ]
                .map((call) => `1:${call}`)
                .join(','),
            'live drain preserves buffered SDP and ICE order',
          );
          continue;
        }
        let replacementGeneration = me;
        const channelLabel =
          phase === 'answer' || phase === 'candidate' ? 'unreliable' : 'reliable';
        const lossReplacements = [
          'loss',
          'closed',
          'channel-close',
          'channel-error',
          'timeout',
          'late-completion',
        ];
        if (replacement === 'retry') {
          socket.signal({ PairRetry: 1 });
        } else if (lossReplacements.includes(replacement)) {
          if (replacement === 'loss' || replacement === 'closed') {
            connections[1]!.connectionState = replacement === 'loss' ? 'failed' : 'closed';
            connections[1]!.onconnectionstatechange?.();
          } else if (replacement === 'late-completion') {
            now = 1100;
            if (oldOutcome === 'reject') rejectOld(new Error('late browser failure'));
            else resolveOld();
          } else if (replacement === 'timeout') {
            now = 1100;
            for (const [id, timer] of [...timers]) {
              if (timer.at <= now) {
                timers.delete(id);
                timer.callback();
              }
            }
          } else {
            const channel = new Channel(channelLabel);
            connections[1]!.ondatachannel?.({ channel });
            if (replacement === 'channel-close') {
              channel.close();
              channel.onclose?.();
            } else channel.onerror?.();
          }
          await turn();
          const losses = diagnostics.filter((line) =>
            line.startsWith(`P2P link lost for ${peer}: `),
          );
          assert(
            losses.length === 2,
            `${replacement}/${phase}: each failed link retains one snapshot before removal; ${JSON.stringify(diagnostics)}`,
          );
          const snapshot = JSON.parse(losses[1]!.split(': ').slice(1).join(': ')) as Record<
            string,
            unknown
          >;
          assert(
            (snapshot['link'] as Record<string, unknown>)['generation'] === 2,
            'loss describes the retired physical generation',
          );
          const expectedReason =
            replacement === 'loss'
              ? 'connection state failed'
              : replacement === 'closed'
                ? 'connection state closed'
                : replacement === 'timeout' || replacement === 'late-completion'
                  ? 'negotiation deadline expired'
                  : `required channel ${channelLabel} closed or failed`;
          assert(
            snapshot['reason'] === expectedReason,
            `${replacement}: retain the terminal cause`,
          );
          if (replacement.startsWith('channel-')) {
            const channels = (snapshot['link'] as Record<string, unknown>)[
              'channels'
            ] as Record<string, Record<string, unknown>>;
            assert(
              channels[channelLabel]!['bufferedAmount'] === 17,
              'capture channel details before teardown',
            );
          }
          socket.deliver('NewPeer', { peer_id: peer, you_initiate: false });
        } else {
          if (replacement === 'departure')
            socket.deliver('PlayerLeft', { player_id: peer, epoch: 1, final_seq: 0 });
          if (replacement === 'departure' || replacement === 'incarnation')
            socket.deliver('PlayerJoined', { player: { id: peer, epoch: 2, seq: 0 } });
          if (replacement === 'generation') replacementGeneration = nextGeneration;
          socket.deliver('SessionPlan', {
            generation: replacementGeneration,
            topology: 'mesh',
            transport: 'webrtc',
            fallback: 'relay',
            ice_servers: [],
            peers: [{ player_id: peer, initiate: false }],
          });
        }
        if (replacement === 'incarnation')
          socket.deliver('NewPeer', { peer_id: peer, you_initiate: false });
        const replacementSignal = (signal: Record<string, unknown>) =>
          replacement === 'retry' ? { RetrySignal: { attempt: 1, signal } } : signal;
        socket.signal(replacementSignal({ Offer: 'replacement-offer' }), replacementGeneration);
        socket.signal(
          replacementSignal({ IceCandidate: 'replacement-candidate' }),
          replacementGeneration,
        );
        await turn();
        assert(
          connections.length === 3 && connections[1]?.closed === true,
          `${replacement}/${phase}: replace the held link; count=${connections.length}; events=${JSON.stringify(events)}`,
        );
        const lossesBeforeOldResult = diagnostics.filter((line) =>
          line.startsWith('P2P link lost for'),
        ).length;
        assert(
          lossesBeforeOldResult === (lossReplacements.includes(replacement) ? 2 : 1),
          'intentional replacement does not report a link failure',
        );
        resolveReplacement();
        await turn();
        const remote = calls.indexOf(`2:remote-${initiate ? 'answer' : 'offer'}`);
        const candidate = calls.indexOf('2:candidate');
        assert(remote >= 0 && candidate > remote, `${phase}: replacement SDP precedes ICE`);
        assert(
          calls.filter((call) => call === '2:candidate').length === 1,
          `${phase}: only replacement ICE reaches the new link`,
        );
        if (oldOutcome === 'resolve') resolveOld();
        else if (oldOutcome === 'reject') rejectOld(new Error('retired operation failed'));
        await turn();
        now = 1100;
        for (const [id, timer] of [...timers]) {
          if (timer.at <= now) {
            timers.delete(id);
            timer.callback();
          }
        }
        await turn();
        assert(
          diagnostics.filter((line) => line.startsWith('P2P link lost for')).length ===
            lossesBeforeOldResult,
          'retired results and deadlines cannot report replacement loss',
        );
        assert(
          connections[2]?.closed === false,
          `${replacement}/${phase}: old expiry cannot close the new link`,
        );
        assert(
          calls.filter((call) => call === '2:candidate').length === 1,
          `${replacement}/${phase}/${oldOutcome}: a retired buffered-signal loop must not apply old ICE to the replacement`,
        );
        assert(
          calls.filter((call) => call === '2:remote-offer').length === 1,
          'retired buffered SDP cannot reach the replacement',
        );
        assert(
          sent.filter((frame) => frame['type'] === 'Signal').length === sentBeforeRetry + 1,
          `${phase}: only the replacement sends new SDP`,
        );
        assert(
          events.filter((event) => event['event'] === 'error').length === errorsBeforeRetry,
          `${phase}: late old results cannot report a current-link failure`,
        );
      } finally {
        resolveReplacement();
        resolveOld();
        socket?.onclose?.();
        await running;
        await turn();
        console.error = originalError;
        performance.now = originalNow;
        globalThis.setTimeout = originalSetTimeout;
        globalThis.clearTimeout = originalClearTimeout;
        globalThis.RTCPeerConnection = originalPc;
        globalThis.WebSocket = originalSocket;
        if (originalWindow === undefined) Reflect.deleteProperty(globalThis, 'window');
        else Object.defineProperty(globalThis, 'window', originalWindow);
      }
    }
  }
}
console.error('ok - buffered signals stay with their original physical link');
