// Orchestrator: drives one full protocol-v3 client lifecycle inside the page.
//
// This is a faithful port of the native client's orchestrator
// (clients/native/src/client.rs) — same flow, same success criteria, same
// stdout event ordering:
//
// 1. Connect + Authenticate (v2 mode omits every v3 field).
// 2. Room: `JoinRoom` with no code creates; `--join-code` joins by code.
// 3. Ready barrier: send `PlayerReady` only once the room is in the Lobby
//    state AND `--peers N` members are present (counting members alone races
//    the server's Waiting→Lobby transition). The lobby no longer auto-starts
//    on a full ready set: when the server reports every current member ready
//    (`LobbyStateChanged.all_ready`), the room creator sends an explicit
//    `StartGame`; joiners just await the `GameStarting` it produces.
// 4. Finalize: `GameStarting`, then (non-relay rooms) the per-recipient
//    `SessionPlan`.
// 5. P2P per `peers[].initiate` / `NewPeer.you_initiate`; the overall WebRTC
//    transport status resolves exactly once (Appendix G).
// 6. Relay floor: `GameData` over the WebSocket, exercised by
//    `--relay-payload`.
//
// Concurrency: the page is single-threaded, but server frames and browser
// WebRTC callbacks both trigger async work. Every input is appended to one
// promise chain (`enqueue`), so handlers never interleave and every stdout
// event is emitted in causal order — the JS equivalent of the native client's
// single `tokio::select!` task.

import {
  EXIT_CONNECTION_ERROR,
  EXIT_CRITERIA_UNMET,
  EXIT_PROTOCOL_ERROR,
  EXIT_SUCCESS,
  effectiveTotalPeers,
  isV3,
  type RunConfig,
} from '../shared/types.js';
import { classifySignal, emit, type SignalKind } from './events.js';
import { Engine, RELIABLE_LABEL, UNRELIABLE_LABEL, type PlanIceServer } from './engine.js';
import {
  HANDSHAKE_TIMEOUT_MS,
  clientFrame,
  connect,
  parseServerFrame,
  type ServerFrame,
} from './wire.js';

/**
 * Delay between the relay-probe trigger and the `--relay-payload` send
 * (mirrors the native RELAY_SEND_SETTLE; see client.rs for the rationale).
 */
const RELAY_SEND_SETTLE_MS = 250;

/** Grace period between meeting all success criteria and exiting. */
const EXIT_LINGER_MS = 250;

/** Retry cadence for an exchange send that raced a data-channel open edge. */
const EXCHANGE_RETRY_MS = 50;

/**
 * Keepalive cadence. `docs/guides/building-a-client.md` makes a periodic
 * `Ping` mandatory for every client (the server evicts idle connections);
 * this driver models that contract so anyone using it as a template inherits
 * the keepalive rather than the idle-timeout eviction. Mirrors the native
 * client's PING_INTERVAL.
 */
const PING_INTERVAL_MS = 10_000;

/**
 * How long a sent `Ping` may go unanswered before the run fails loudly. A
 * missing `Pong` inside this generous window means the connection (or the
 * server's control path) is broken. Mirrors the native client's PONG_TIMEOUT.
 */
const PONG_TIMEOUT_MS = 10_000;

/**
 * One-shot drain grace applied when the pong deadline first expires. The
 * deadline check runs in processTimers, BEFORE the chain drains queued
 * frames, so a `Pong` that already arrived could be declared missing without
 * ever being handled (deadline and frame ready in the same wake). The
 * extension guarantees at least one more processing pass. Mirrors the native
 * client's PONG_DRAIN_GRACE.
 */
const PONG_DRAIN_GRACE_MS = 1_000;

/**
 * Local send-buffer ceiling. Browser `WebSocket.send()` never blocks and
 * never fails on backpressure — it buffers unboundedly in `bufferedAmount`
 * while the tab's memory grows and delivery silently stalls. A conformance
 * driver must SURFACE local backpressure, not mask it, so crossing this
 * ceiling is a loud connection failure. (A production client would pace or
 * drop instead; the recipe lives in the building-a-client guide.)
 */
const SEND_BUFFER_LIMIT_BYTES = 1_048_576;

/** A failure that terminates the run with a specific exit code. */
class FatalError extends Error {
  readonly code: number;

  constructor(code: number, message: string) {
    super(message);
    this.code = code;
  }

  static protocol(message: string): FatalError {
    return new FatalError(EXIT_PROTOCOL_ERROR, message);
  }

  static connection(message: string): FatalError {
    return new FatalError(EXIT_CONNECTION_ERROR, message);
  }
}

/** Stringify an unknown thrown value for error events. */
function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Run the full client lifecycle; emits every stdout event except the final
 * `exiting` (owned by the CLI) and resolves with the exit code. Never rejects.
 */
export async function run(config: RunConfig): Promise<number> {
  try {
    return await new Orchestrator(config).run();
  } catch (error) {
    const code = error instanceof FatalError ? error.code : EXIT_PROTOCOL_ERROR;
    emit({ event: 'error', message: describe(error) });
    console.error(`fatal failure (code ${code}): ${describe(error)}`);
    return code;
  }
}

/** Single-chain state machine driving the session (see module docs). */
class Orchestrator {
  private readonly config: RunConfig;
  private readonly engine: Engine;
  private ws: WebSocket | null = null;
  private myId = '';

  // --- Handshake frame queue (sequential pull until the room is joined) ---
  private streaming = false;
  private readonly bufferedFrames: ServerFrame[] = [];
  private frameWaiter: {
    resolve: (frame: ServerFrame) => void;
    reject: (error: FatalError) => void;
  } | null = null;
  /** Fatal handshake failure observed while no frame pull was pending. */
  private handshakeFatal: FatalError | null = null;

  // --- Serialized input processing (the "orchestrator task") ---
  private chain: Promise<void> = Promise.resolve();
  private finishResolve: ((code: number) => void) | null = null;
  private finished = false;

  // --- Session state (field-for-field mirror of the native Orchestrator) ---
  private readonly present = new Set<string>();
  private readonly membersSeen = new Set<string>();
  private inLobby = false;
  private readySent = false;
  /**
   * The explicit `StartGame` has been sent (room creator only, once). Guards
   * against re-sending on subsequent `LobbyStateChanged` broadcasts.
   */
  private startGameSent = false;
  private gameStarted = false;
  private lateJoined = false;
  private webrtcPlanSeen = false;
  private readonly expectedPeers = new Set<string>();
  private readonly connectedPairs = new Set<string>();
  private lastIceServers: PlanIceServer[] = [];
  private transportStatus: boolean | null = null;
  private p2pDeadline: number | null = null;
  private relaySendAt: number | null = null;
  private relaySent = false;
  private readonly relayReceivedFrom = new Set<string>();
  private readonly peerStatusFrom = new Set<string>();
  private readonly sentLabels = new Map<string, Set<string>>();
  private readonly receivedLabels = new Map<string, Set<string>>();
  private readonly pendingExchangePeers = new Set<string>();
  private readonly reportedExchangeSendFailures = new Set<string>();
  private exchangeRetryAt: number | null = null;
  private readonly pendingSignals = new Map<string, unknown[]>();
  private runDeadline = 0;
  private lingerUntil: number | null = null;
  /** Next keepalive `Ping` send (the mandatory client keepalive contract). */
  private nextPingAt = 0;
  /**
   * Deadline for the `Pong` answering the most recent `Ping`; `null` while no
   * answer is outstanding.
   */
  private pongDeadline: number | null = null;
  /**
   * Whether the one-shot PONG_DRAIN_GRACE_MS extension was applied to the
   * current deadline (the second expiry is fatal).
   */
  private pongGraceApplied = false;
  private wakeTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(config: RunConfig) {
    this.config = config;
    this.engine = new Engine(config.crippleIce, {
      onLocalCandidate: (peer, candidateJson) => {
        // Crippled mode never reaches here (the engine drops gathered
        // candidates), so this is always a real trickle-ICE relay.
        this.enqueue(async () => {
          this.sendSignal(peer, 'ice_candidate', { IceCandidate: candidateJson });
        });
      },
      onPcState: (peer, state) => {
        this.enqueue(async () => {
          emit({ event: 'pc_state', peer, state });
        });
      },
      onChannelOpen: (peer, label) => {
        this.enqueue(async () => {
          emit({ event: 'channel_open', peer, label });
          if (this.engine.noteChannelOpen(peer, label)) {
            this.onPairConnected(peer);
          }
        });
      },
      onChannelMessage: (peer, label, text) => {
        this.enqueue(async () => {
          let labels = this.receivedLabels.get(peer);
          if (labels === undefined) {
            labels = new Set();
            this.receivedLabels.set(peer, labels);
          }
          labels.add(label);
          emit({ event: 'channel_message', peer, label, text });
        });
      },
    });
  }

  async run(): Promise<number> {
    // The soft run window starts at PROCESS start: the CLI reports the time
    // already spent launching Chromium so the window matches the native
    // client's process-start semantics.
    this.runDeadline =
      Date.now() + this.config.runForSecs * 1000 - this.config.elapsedBeforeStartMs;
    this.nextPingAt = Date.now() + PING_INTERVAL_MS;

    let ws: WebSocket;
    try {
      ws = await connect(this.config.serverUrl);
    } catch (error) {
      throw FatalError.connection(describe(error));
    }
    this.ws = ws;
    emit({ event: 'connected' });
    ws.onmessage = (event: MessageEvent<unknown>) => {
      if (typeof event.data !== 'string') {
        console.error('ignoring non-text websocket frame');
        return;
      }
      this.onFrameText(event.data);
    };
    ws.onclose = () => {
      this.onSocketClosed();
    };

    await this.authenticate();
    const lobbyState = await this.joinRoom();

    // Joining an already-Lobby room (seat fill) means readiness is
    // immediately possible; a Finalized room means the session is already
    // running — GameStarting was broadcast before we joined and will never
    // be re-sent, so the criterion is satisfied on entry.
    this.inLobby = lobbyState === 'lobby';
    this.gameStarted = lobbyState === 'finalized';
    this.lateJoined = lobbyState === 'finalized';
    // Late joiners arm the relay probe on entry (see RELAY_SEND_SETTLE_MS):
    // the GameStarting trigger pre-dates the join and never re-fires.
    if (this.config.relayPayload !== null && lobbyState === 'finalized') {
      this.relaySendAt = Date.now() + RELAY_SEND_SETTLE_MS;
    }
    this.maybeSendReady();

    const code = await new Promise<number>((resolve) => {
      this.finishResolve = resolve;
      this.startStreaming();
      this.scheduleWake();
    });
    this.teardown();
    return code;
  }

  // -------------------------------------------------------------------------
  // Handshake (sequential pulls, mirroring the native wire helpers)
  // -------------------------------------------------------------------------

  /** Send `Authenticate` and consume `Authenticated` + `ProtocolInfo`. */
  private async authenticate(): Promise<void> {
    const v3 = isV3(this.config);
    const data: Record<string, unknown> = {
      app_id: this.config.appId,
      sdk_version: this.config.sdkVersion,
      platform: this.config.platform,
      game_data_format: 'json',
    };
    if (v3) {
      // v2 mode omits every v3 field so the wire shape is pure v2.
      data['protocol_version'] = this.config.protocolVersion;
      data['supported_transports'] = this.config.supportedTransports;
      data['supported_topologies'] = this.config.supportedTopologies;
    }
    this.sendFrame(clientFrame('Authenticate', data));

    const authResponse = await this.nextHandshakeFrame();
    if (authResponse.type === 'Authenticated') {
      emit({ event: 'authenticated' });
    } else if (authResponse.type === 'AuthenticationError') {
      throw FatalError.protocol(
        `authentication rejected: ${String(authResponse.data['error'])} ` +
          `(${String(authResponse.data['error_code'])})`,
      );
    } else {
      throw FatalError.protocol(`expected Authenticated, got ${authResponse.type}`);
    }

    const infoResponse = await this.nextHandshakeFrame();
    if (infoResponse.type !== 'ProtocolInfo') {
      throw FatalError.protocol(`expected ProtocolInfo, got ${infoResponse.type}`);
    }
    // v2 connections omit the field; the effective version is 2.
    const negotiated = infoResponse.data['protocol_version'];
    emit({
      event: 'protocol_info',
      negotiated_version: typeof negotiated === 'number' ? negotiated : 2,
    });
  }

  /** Create or join the room; returns the room's lobby state at join time. */
  private async joinRoom(): Promise<string> {
    this.sendFrame(
      clientFrame('JoinRoom', {
        game_name: this.config.gameName,
        room_code: this.config.joinCode,
        player_name: this.config.playerName,
        max_players: this.config.peers,
        supports_authority: false,
      }),
    );

    // Read until `RoomJoined`, tolerating interleaved lobby MEMBERSHIP deltas.
    //
    // The server registers a joiner as a room-broadcast recipient before it
    // finishes assembling and enqueuing that joiner's own `RoomJoined`, so a
    // second player joining the same room in the same instant can have its
    // `PlayerJoined` delivered ahead of our `RoomJoined`. That interleaving is
    // benign: `present` is a set and `RoomJoined.current_players` is the
    // authoritative baseline, so a delta seen just before it is idempotent.
    // We fold such deltas in and keep waiting. Any OTHER pre-`RoomJoined`
    // frame is still a genuine protocol violation and fails loudly. (Mirror
    // of the native client's join handshake.)
    const earlyJoined: string[] = [];
    const earlyLeft: string[] = [];
    let response = await this.nextHandshakeFrame();
    while (response.type !== 'RoomJoined') {
      if (response.type === 'RoomJoinFailed') {
        throw FatalError.protocol(
          `room join failed: ${String(response.data['reason'])} ` +
            `(${String(response.data['error_code'])})`,
        );
      }
      if (response.type === 'PlayerJoined') {
        const player = response.data['player'] as Record<string, unknown> | undefined;
        const id = player?.['id'];
        if (typeof id === 'string') {
          earlyJoined.push(id);
        }
      } else if (response.type === 'PlayerLeft') {
        const id = response.data['player_id'];
        if (typeof id === 'string') {
          earlyLeft.push(id);
        }
      } else {
        throw FatalError.protocol(`expected RoomJoined, got ${response.type}`);
      }
      response = await this.nextHandshakeFrame();
    }
    const payload = response.data;
    if (this.config.createRoom) {
      emit({ event: 'room_created', room_code: String(payload['room_code']) });
    }
    const lobbyState = String(payload['lobby_state']);
    this.myId = String(payload['player_id']);
    emit({
      event: 'room_joined',
      room_id: String(payload['room_id']),
      player_id: this.myId,
      lobby_state: lobbyState,
    });
    const currentPlayers = payload['current_players'];
    if (Array.isArray(currentPlayers)) {
      for (const player of currentPlayers as Array<Record<string, unknown>>) {
        const id = player['id'];
        if (typeof id === 'string') {
          this.present.add(id);
        }
      }
    }
    // Fold in deltas observed ahead of the baseline (set-idempotent).
    for (const id of earlyJoined) {
      this.present.add(id);
    }
    for (const id of earlyLeft) {
      this.present.delete(id);
    }
    this.present.add(this.myId);
    for (const id of this.present) {
      this.membersSeen.add(id);
    }
    return lobbyState;
  }

  /** Pull the next server frame during the sequential handshake phase. */
  private nextHandshakeFrame(): Promise<ServerFrame> {
    const buffered = this.bufferedFrames.shift();
    if (buffered !== undefined) {
      return Promise.resolve(buffered);
    }
    if (this.handshakeFatal !== null) {
      return Promise.reject(this.handshakeFatal);
    }
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.frameWaiter = null;
        reject(FatalError.connection('timed out waiting for a ServerMessage'));
      }, HANDSHAKE_TIMEOUT_MS);
      this.frameWaiter = {
        resolve: (frame) => {
          clearTimeout(timer);
          this.frameWaiter = null;
          resolve(frame);
        },
        reject: (error) => {
          clearTimeout(timer);
          this.frameWaiter = null;
          reject(error);
        },
      };
    });
  }

  /** Route frames buffered during the handshake, then go fully event-driven. */
  private startStreaming(): void {
    const backlog = this.bufferedFrames.splice(0);
    for (const frame of backlog) {
      this.enqueue(() => this.handleServerMessage(frame));
    }
    this.streaming = true;
    // A fatal recorded in the gap between the last handshake pull and this
    // flip would otherwise have no consumer left to reject; end the run now.
    if (this.handshakeFatal !== null) {
      this.fail(this.handshakeFatal);
    }
  }

  private onFrameText(text: string): void {
    let frame: ServerFrame;
    try {
      frame = parseServerFrame(text);
    } catch (error) {
      this.failFatal(FatalError.protocol(`invalid ServerMessage frame: ${describe(error)}`));
      return;
    }
    if (this.streaming) {
      this.enqueue(() => this.handleServerMessage(frame));
    } else if (this.frameWaiter !== null) {
      this.frameWaiter.resolve(frame);
    } else {
      this.bufferedFrames.push(frame);
    }
  }

  private onSocketClosed(): void {
    if (this.finished) {
      return;
    }
    this.failFatal(
      FatalError.connection('websocket closed by server before success criteria were met'),
    );
  }

  /**
   * Route a fatal failure to whichever phase owns the run. Mid-handshake,
   * `finishResolve` is still null, so `fail()` would emit an error event yet
   * resolve nothing — the run would stall until the pending frame pull's
   * timeout fired a SECOND, misleading error. Instead the live pull (or the
   * next one) rejects with the real cause, which `run()` surfaces exactly
   * once with the orchestrator's exit code.
   */
  private failFatal(fatal: FatalError): void {
    if (this.streaming) {
      this.fail(fatal);
      return;
    }
    if (this.frameWaiter !== null) {
      this.frameWaiter.reject(fatal);
    } else if (this.handshakeFatal === null) {
      this.handshakeFatal = fatal;
    }
  }

  // -------------------------------------------------------------------------
  // Serialized input processing + timers
  // -------------------------------------------------------------------------

  /**
   * Append one input handler to the processing chain. After it settles, due
   * timers run and the next wake is scheduled (the native loop's
   * `process_timers` + `next_wake` pattern).
   */
  private enqueue(task: () => Promise<void>): void {
    this.chain = this.chain
      .then(async () => {
        if (this.finished) {
          return;
        }
        await task();
        this.processTimers();
        this.scheduleWake();
      })
      .catch((error: unknown) => {
        this.fail(
          error instanceof FatalError
            ? error
            : FatalError.protocol(`unexpected failure: ${describe(error)}`),
        );
      });
  }

  /** Resolve the run with a fatal error (exactly once). */
  private fail(fatal: FatalError): void {
    if (this.finished) {
      return;
    }
    emit({ event: 'error', message: fatal.message });
    console.error(`fatal failure (code ${fatal.code}): ${fatal.message}`);
    this.finish(fatal.code);
  }

  private finish(code: number): void {
    if (this.finished) {
      return;
    }
    this.finished = true;
    if (this.wakeTimer !== null) {
      clearTimeout(this.wakeTimer);
      this.wakeTimer = null;
    }
    this.finishResolve?.(code);
  }

  /** Earliest pending timer (the run deadline at the latest). */
  private nextWake(): number {
    let wake = this.runDeadline;
    if (!this.relaySent && this.relaySendAt !== null) {
      wake = Math.min(wake, this.relaySendAt);
    }
    if (this.exchangeRetryAt !== null) {
      wake = Math.min(wake, this.exchangeRetryAt);
    }
    if (this.transportStatus === null && this.p2pDeadline !== null) {
      wake = Math.min(wake, this.p2pDeadline);
    }
    if (this.lingerUntil !== null) {
      wake = Math.min(wake, this.lingerUntil);
    }
    wake = Math.min(wake, this.nextPingAt);
    if (this.pongDeadline !== null) {
      wake = Math.min(wake, this.pongDeadline);
    }
    return wake;
  }

  private scheduleWake(): void {
    if (this.finished || this.finishResolve === null) {
      return;
    }
    if (this.wakeTimer !== null) {
      clearTimeout(this.wakeTimer);
    }
    const delay = Math.max(0, this.nextWake() - Date.now());
    this.wakeTimer = setTimeout(() => {
      this.wakeTimer = null;
      // Through the chain so a wake never interleaves with a live handler.
      this.enqueue(async () => {});
    }, delay);
  }

  /** Fire due timers; finishes the run when it is over. */
  private processTimers(): void {
    if (this.finished) {
      return;
    }
    const now = Date.now();

    // Mandatory keepalive: send `Ping` on cadence and demand a timely `Pong`.
    // An unanswered ping is a broken connection or control path — fail loudly
    // rather than idle into the server's eviction.
    //
    // AT MOST ONE ping is outstanding at a time: a new `Ping` is sent only
    // once the previous one's `Pong` has cleared the deadline. A single
    // deadline then unambiguously tracks the single in-flight ping — a stale
    // `Pong` can never clear the deadline of a newer, still-pending ping.
    //
    // The first deadline expiry only arms the drain grace
    // (see PONG_DRAIN_GRACE_MS): a `Pong` already queued gets one guaranteed
    // processing pass before the miss is declared fatal.
    if (this.pongDeadline !== null && now >= this.pongDeadline) {
      if (this.pongGraceApplied) {
        throw FatalError.connection(
          `server did not answer Ping within ${PONG_TIMEOUT_MS}ms (+${PONG_DRAIN_GRACE_MS}ms drain grace)`,
        );
      }
      this.pongDeadline = now + PONG_DRAIN_GRACE_MS;
      this.pongGraceApplied = true;
    }
    if (now >= this.nextPingAt && this.pongDeadline === null) {
      this.sendFrame(clientFrame('Ping'));
      this.nextPingAt = now + PING_INTERVAL_MS;
      this.pongDeadline = now + PONG_TIMEOUT_MS;
      this.pongGraceApplied = false;
    }

    if (!this.relaySent && this.relaySendAt !== null && now >= this.relaySendAt) {
      this.sendRelayPayload();
    }

    if (this.exchangeRetryAt !== null && now >= this.exchangeRetryAt) {
      this.flushPendingExchangeSends();
    }

    if (this.transportStatus === null && this.p2pDeadline !== null && now >= this.p2pDeadline) {
      // The P2P window expired: resolve with whatever is connected now.
      this.resolveTransportStatus();
    }

    if (this.lingerUntil === null && this.criteriaMet()) {
      this.lingerUntil = now + EXIT_LINGER_MS;
    }
    if (this.lingerUntil !== null && now >= this.lingerUntil) {
      // Criteria can regress during the linger (a late NewPeer or a freshly
      // connected pair adds new obligations); re-validate at expiry.
      if (this.criteriaMet()) {
        this.finish(EXIT_SUCCESS);
        return;
      }
      console.error('success criteria regressed during the exit linger; continuing');
      this.lingerUntil = null;
    }

    if (now >= this.runDeadline) {
      if (this.criteriaMet()) {
        this.finish(EXIT_SUCCESS);
        return;
      }
      emit({
        event: 'error',
        message: `--run-for-secs elapsed with unmet success criteria: ${this.unmetCriteria().join(', ')}`,
      });
      this.finish(EXIT_CRITERIA_UNMET);
    }
  }

  // -------------------------------------------------------------------------
  // Server message handling (mirrors the native handle_server_message)
  // -------------------------------------------------------------------------

  private async handleServerMessage(frame: ServerFrame): Promise<void> {
    // ANY inbound frame proves the connection and the server are alive, so it
    // satisfies the keepalive liveness check — not just `Pong`. Cleared BEFORE
    // dispatch, so the very frame that kicks off a long handler (e.g. WebRTC
    // pairing) already refreshes liveness; processTimers cannot then declare a
    // still-pending ping dead just because that handler ran past the window.
    this.pongDeadline = null;
    this.pongGraceApplied = false;
    const data = frame.data;
    switch (frame.type) {
      case 'PlayerJoined': {
        const player = data['player'] as Record<string, unknown>;
        const id = String(player['id']);
        this.present.add(id);
        this.membersSeen.add(id);
        emit({ event: 'peer_joined', player_id: id });
        this.maybeSendReady();
        break;
      }
      case 'PlayerLeft': {
        const id = String(data['player_id']);
        this.present.delete(id);
        // A departed peer can no longer satisfy any pairing-derived
        // criterion (see the native client for the full rationale).
        this.expectedPeers.delete(id);
        this.pendingSignals.delete(id);
        // The departure can complete the Appendix G all-pairs condition.
        if (this.transportStatus === null && this.allExpectedPairsConnected()) {
          this.resolveTransportStatus();
        }
        emit({ event: 'player_left', player_id: id });
        break;
      }
      case 'GameStarting': {
        this.gameStarted = true;
        const connections = (data['peer_connections'] ?? []) as Array<Record<string, unknown>>;
        const mine = connections.find((peer) => peer['player_id'] === this.myId);
        emit({ event: 'game_starting', is_authority: mine?.['is_authority'] === true });
        if (this.config.relayPayload !== null && !this.relaySent) {
          this.relaySendAt = Date.now() + RELAY_SEND_SETTLE_MS;
        }
        break;
      }
      case 'SessionPlan': {
        const peers = (data['peers'] ?? []) as Array<Record<string, unknown>>;
        const iceServers = (data['ice_servers'] ?? []) as PlanIceServer[];
        const transport = String(data['transport']);
        emit({
          event: 'session_plan',
          topology: String(data['topology']),
          transport,
          host: data['host'] ?? null,
          peers: peers.map((peer) => ({
            player_id: String(peer['player_id']),
            initiate: peer['initiate'] === true,
          })),
          ice_servers_count: iceServers.length,
          fallback: String(data['fallback']),
        });
        if (transport === 'webrtc') {
          this.webrtcPlanSeen = true;
          this.lastIceServers = iceServers;
          for (const peer of peers) {
            await this.establishPair(String(peer['player_id']), peer['initiate'] === true);
          }
        }
        break;
      }
      case 'NewPeer': {
        const peerId = String(data['peer_id']);
        const youInitiate = data['you_initiate'] === true;
        emit({ event: 'new_peer', peer_id: peerId, you_initiate: youInitiate });
        // Same pairing path as a plan peer (late join, Appendix E).
        await this.establishPair(peerId, youInitiate);
        break;
      }
      case 'Signal': {
        await this.handleSignal(String(data['from']), data['signal']);
        break;
      }
      case 'GameData': {
        const from = String(data['from_player']);
        const payload = data['data'];
        if (
          typeof payload === 'object' &&
          payload !== null &&
          'relay_msg' in (payload as Record<string, unknown>)
        ) {
          this.relayReceivedFrom.add(from);
        }
        emit({ event: 'game_data_received', from, payload });
        break;
      }
      case 'PeerTransportStatus': {
        const peer = String(data['peer_id']);
        this.peerStatusFrom.add(peer);
        emit({
          event: 'peer_transport_status',
          peer,
          transport: String(data['transport']),
          connected: data['connected'] === true,
        });
        break;
      }
      case 'Error': {
        if (data['error_code'] === 'SLOW_CONSUMER') {
          // The server is closing this connection because it could not drain
          // its outbound queue in time. Surface it distinctly so a run
          // failure is attributable to consumption speed rather than a
          // generic server error; the imminent socket close (not this frame)
          // decides the outcome. Mirrors the native client's dedicated arm.
          console.error(
            `server disconnecting us as a slow consumer: ${String(data['message'])}`,
          );
          emit({
            event: 'error',
            message: `server disconnecting us as a slow consumer: ${String(data['message'])}`,
          });
          break;
        }
        // Other server-reported errors are surfaced but non-fatal: the relay
        // floor (and the run window) decide the outcome.
        emit({
          event: 'error',
          message: `server error: ${String(data['message'])} (${String(data['error_code'])})`,
        });
        break;
      }
      case 'LobbyStateChanged': {
        if (data['lobby_state'] === 'lobby') {
          this.inLobby = true;
          this.maybeSendReady();
          // The lobby no longer auto-starts: the room creator issues the
          // explicit StartGame that produces GameStarting once the server
          // reports the full ready set.
          this.maybeSendStartGame(Boolean(data['all_ready']));
        }
        break;
      }
      case 'Pong':
        // Keepalive round-trip complete.
        this.pongDeadline = null;
        this.pongGraceApplied = false;
        break;
      default:
        console.error(`ignoring server message ${frame.type}`);
        break;
    }
  }

  // -------------------------------------------------------------------------
  // P2P pairing + signaling (mirrors the native establish/handle/apply trio)
  // -------------------------------------------------------------------------

  /**
   * Pair with `peer` per the server's directive: create the connection, offer
   * when told to, then drain any defensively buffered signals.
   */
  private async establishPair(peer: string, initiate: boolean): Promise<void> {
    if (peer === this.myId) {
      console.error(`server asked us to pair with ourselves (${peer}); ignoring`);
      return;
    }
    if (this.config.leaveOnGameStart) {
      // A seat-vacating client never pairs: the plan/NewPeer is logged but
      // not acted on, so it produces zero signaling traffic.
      return;
    }
    this.expectedPeers.add(peer);
    if (this.p2pDeadline === null) {
      this.p2pDeadline = Date.now() + this.config.p2pTimeoutSecs * 1000;
    }
    try {
      const offerSdp = await this.engine.pairWith(peer, initiate, this.lastIceServers);
      if (offerSdp !== null) {
        this.sendSignal(peer, 'offer', { Offer: offerSdp });
      }
    } catch (error) {
      // A single failed pair is not fatal: the p2p timeout resolves the
      // overall status and the relay floor still carries data.
      emit({ event: 'error', message: `pairing with ${peer} failed: ${describe(error)}` });
    }
    const buffered = this.pendingSignals.get(peer);
    if (buffered !== undefined) {
      this.pendingSignals.delete(peer);
      for (const signal of buffered) {
        await this.applySignal(peer, signal);
      }
    }
  }

  /**
   * Emit `signal_received` and route an inbound signal (buffering it when the
   * peer is not paired yet).
   */
  private async handleSignal(from: string, signal: unknown): Promise<void> {
    emit({ event: 'signal_received', from, kind: classifySignal(signal) });
    if (!this.engine.isPaired(from)) {
      let buffered = this.pendingSignals.get(from);
      if (buffered === undefined) {
        buffered = [];
        this.pendingSignals.set(from, buffered);
      }
      buffered.push(signal);
      return;
    }
    await this.applySignal(from, signal);
  }

  /**
   * Feed a signal into the engine. Engine-level failures are surfaced as
   * error events but never abort the run (the relay floor stays live).
   */
  private async applySignal(from: string, signal: unknown): Promise<void> {
    const kind = classifySignal(signal);
    const payload = signal as Record<string, unknown>;
    try {
      switch (kind) {
        case 'offer': {
          const sdp = payload['Offer'];
          if (typeof sdp !== 'string') {
            throw new Error('Offer payload is not a string');
          }
          const answerSdp = await this.engine.handleOffer(from, sdp);
          this.sendSignal(from, 'answer', { Answer: answerSdp });
          break;
        }
        case 'answer': {
          const sdp = payload['Answer'];
          if (typeof sdp !== 'string') {
            throw new Error('Answer payload is not a string');
          }
          await this.engine.handleAnswer(from, sdp);
          break;
        }
        case 'ice_candidate': {
          if (this.config.crippleIce) {
            // The outbound half is dropped by the engine; inbound candidates
            // are dropped here, never applied.
            break;
          }
          const candidate = payload['IceCandidate'];
          if (typeof candidate !== 'string') {
            throw new Error('IceCandidate payload is not a string');
          }
          await this.engine.handleRemoteCandidate(from, candidate);
          break;
        }
        case 'other':
          throw new Error('signal does not match the matchbox PeerSignal convention');
      }
    } catch (error) {
      emit({ event: 'error', message: `signal from ${from} failed: ${describe(error)}` });
    }
  }

  /**
   * Both channels toward `peer` are open: emit the pair event, run the
   * optional exchange, and check the all-pairs resolution condition.
   */
  private onPairConnected(peer: string): void {
    this.connectedPairs.add(peer);
    emit({ event: 'p2p_pair_connected', peer });
    if (this.config.exchange) {
      this.pendingExchangePeers.add(peer);
      this.flushPendingExchangeSends();
    }
    // All expected pairs connected resolves the overall status early.
    if (this.transportStatus === null && this.allExpectedPairsConnected()) {
      this.resolveTransportStatus();
    }
  }

  private flushPendingExchangeSends(): void {
    for (const peer of Array.from(this.pendingExchangePeers)) {
      if (this.trySendExchange(peer)) {
        this.pendingExchangePeers.delete(peer);
      }
    }
    this.exchangeRetryAt =
      this.pendingExchangePeers.size === 0 ? null : Date.now() + EXCHANGE_RETRY_MS;
  }

  /**
   * Try to send all still-unsent exchange labels for a connected pair.
   *
   * Browser data-channel open callbacks and `send()` readiness can be slightly
   * reordered in Chromium under load. The pair remains valid; defer the send
   * until the channel itself reports `open` at the point of use.
   */
  private trySendExchange(peer: string): boolean {
    let complete = true;
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      const sent = this.sentLabels.get(peer);
      if (sent?.has(label)) {
        continue;
      }
      const channel = this.engine.channel(peer, label);
      if (channel === undefined) {
        emit({
          event: 'error',
          message: `open pair with ${peer} is missing channel ${label}`,
        });
        complete = false;
        continue;
      }
      if (channel.readyState !== 'open') {
        complete = false;
        continue;
      }
      // The exact documented exchange payload (stable field order).
      const text = `{"from":"${this.myId}","channel":"${label}","seq":0}`;
      try {
        channel.send(text);
      } catch (error) {
        const diagnosticKey = `${peer}\0${label}`;
        if (!this.reportedExchangeSendFailures.has(diagnosticKey)) {
          this.reportedExchangeSendFailures.add(diagnosticKey);
          const message =
            `data-channel exchange send to ${peer} on ${label} failed while channel was open; ` +
            `bufferedAmount=${channel.bufferedAmount}; will retry: ${describe(error)}`;
          emit({ event: 'error', message });
          console.error(message);
        }
        complete = false;
        continue;
      }
      let labels = this.sentLabels.get(peer);
      if (labels === undefined) {
        labels = new Set();
        this.sentLabels.set(peer, labels);
      }
      labels.add(label);
      emit({ event: 'channel_message_sent', peer, label, text });
    }
    return complete;
  }

  /**
   * Appendix G early-resolution condition: at least one expected pair, and
   * every CURRENTLY expected peer's pair is connected.
   */
  private allExpectedPairsConnected(): boolean {
    if (this.expectedPeers.size === 0) {
      return false;
    }
    for (const peer of this.expectedPeers) {
      if (!this.connectedPairs.has(peer)) {
        return false;
      }
    }
    return true;
  }

  /**
   * Resolve and report the single overall WebRTC transport status
   * (Appendix G): `connected: true` iff at least one pair is connected at
   * resolution time; a zero-pair resolution engages the relay fallback.
   */
  private resolveTransportStatus(): void {
    const connected = this.connectedPairs.size > 0;
    this.sendFrame(clientFrame('TransportStatus', { transport: 'webrtc', connected }));
    this.transportStatus = connected;
    emit({ event: 'transport_status_sent', transport: 'webrtc', connected });
    if (!connected) {
      emit({ event: 'fallback_engaged' });
    }
  }

  /**
   * Send `PlayerReady` once the expected member count is seated AND the
   * server has moved the room into the Lobby state (readiness is rejected
   * while the room is `Waiting`).
   */
  private maybeSendReady(): void {
    if (!this.readySent && this.inLobby && this.present.size >= this.config.peers) {
      this.sendFrame(clientFrame('PlayerReady'));
      this.readySent = true;
    }
  }

  /**
   * Send the explicit `StartGame` that finalizes the lobby, exactly once, when
   * this client created the room AND the server reports every current member
   * ready (`LobbyStateChanged.all_ready`).
   *
   * The protocol no longer auto-starts a full, all-ready room: finalization is
   * driven by an explicit `StartGame` from the authority — or, when no
   * authority is designated (the interop rooms never set one), any member. The
   * room creator is elected as that member here: it is always a v3 participant
   * that is present through finalization, so the choice is deterministic and
   * needs no cross-client coordination. Joiners send no `StartGame`; they
   * simply await the `GameStarting` the creator's call produces. `all_ready`
   * already implies a full, seated, ready room (every client gates
   * `PlayerReady` on having seen all `--peers` members), and the server
   * re-checks readiness under its room lock, so this never races a late joiner.
   * The send is idempotent-guarded by `startGameSent`.
   *
   * Assumption: readiness is monotonic until finalize — no member leaves or
   * un-readies between `all_ready` and the server processing this `StartGame`.
   * That holds for every interop scenario (rooms cap at `--peers`; the only
   * departures are AFTER `GameStarting`). A pre-finalize departure is a
   * deliberate non-goal here (it could leave the latch set after a `NotReady`);
   * a production game client would re-issue `StartGame` on the next ready set.
   */
  private maybeSendStartGame(allReady: boolean): void {
    if (this.config.createRoom && allReady && !this.startGameSent && !this.gameStarted) {
      this.sendFrame(clientFrame('StartGame'));
      this.startGameSent = true;
      console.error('all members ready; sent StartGame to finalize the lobby');
    }
  }

  /** Send the `--relay-payload` GameData over the relay floor. */
  private sendRelayPayload(): void {
    if (this.config.relayPayload === null) {
      this.relaySent = true;
      return;
    }
    this.sendFrame(clientFrame('GameData', { data: { relay_msg: this.config.relayPayload } }));
    this.relaySent = true;
    this.relaySendAt = null;
    emit({ event: 'game_data_sent' });
  }

  private sendSignal(to: string, kind: SignalKind, signal: Record<string, unknown>): void {
    this.sendFrame(clientFrame('Signal', { to, signal }));
    emit({ event: 'signal_sent', to, kind });
  }

  private sendFrame(frame: string): void {
    if (this.ws === null || this.ws.readyState !== WebSocket.OPEN) {
      throw FatalError.connection('websocket is not open');
    }
    // Browser `send()` never blocks or errors on backpressure — it buffers
    // unboundedly in `bufferedAmount`. Crossing the ceiling means the
    // connection has silently stopped draining; surface it loudly (see
    // SEND_BUFFER_LIMIT_BYTES).
    if (this.ws.bufferedAmount > SEND_BUFFER_LIMIT_BYTES) {
      throw FatalError.connection(
        `local websocket send buffer saturated: bufferedAmount=${this.ws.bufferedAmount} ` +
          `exceeds ${SEND_BUFFER_LIMIT_BYTES} bytes`,
      );
    }
    try {
      this.ws.send(frame);
    } catch (error) {
      throw FatalError.connection(`websocket send failed: ${describe(error)}`);
    }
  }

  // -------------------------------------------------------------------------
  // Success criteria (verbatim port of the native unmet_criteria)
  // -------------------------------------------------------------------------

  private criteriaMet(): boolean {
    return this.unmetCriteria().length === 0;
  }

  private unmetCriteria(): string[] {
    const unmet: string[] = [];
    if (!this.gameStarted) {
      unmet.push('GameStarting not received');
    }
    const requiredMembers = effectiveTotalPeers(this.config);
    if (this.membersSeen.size < requiredMembers) {
      unmet.push(
        `observed ${this.membersSeen.size} of ${requiredMembers} expected distinct members`,
      );
    }
    if (this.webrtcSessionExpected()) {
      if (this.transportStatus === null) {
        unmet.push('transport status not resolved');
      }
      // Wait for each expected pair peer's own status fan-out before exiting
      // (waived after a late join — fan-outs are never replayed). See the
      // native client for the no-deadlock argument.
      if (!this.lateJoined) {
        for (const peer of this.expectedPeers) {
          if (!this.peerStatusFrom.has(peer)) {
            unmet.push(`no PeerTransportStatus from ${peer}`);
          }
        }
      }
    }
    if (this.config.exchange) {
      // Exchange obligations cover the expected peers whose pair actually
      // connected (a never-connected pair owes no channel traffic).
      for (const peer of this.expectedPeers) {
        if (!this.connectedPairs.has(peer)) {
          continue;
        }
        const directions: Array<[string, Set<string> | undefined]> = [
          ['sent to', this.sentLabels.get(peer)],
          ['received from', this.receivedLabels.get(peer)],
        ];
        for (const [direction, labels] of directions) {
          const complete =
            labels !== undefined && labels.has(RELIABLE_LABEL) && labels.has(UNRELIABLE_LABEL);
          if (!complete) {
            unmet.push(`exchange incomplete (${direction} ${peer})`);
          }
        }
      }
    }
    if (this.config.relayPayload !== null) {
      if (!this.relaySent) {
        unmet.push('relay payload not sent');
      }
      // Expect one payload from each of the other `peers - 1` members
      // (waived after a late join: pre-join payloads are never replayed).
      if (!this.lateJoined && this.relayReceivedFrom.size + 1 < this.config.peers) {
        unmet.push(
          `relay payloads observed from ${this.relayReceivedFrom.size} of ` +
            `${this.config.peers - 1} peers`,
        );
      }
    }
    return unmet;
  }

  /**
   * A WebRTC plan with at least one pairing was issued, so the Appendix G
   * status report is owed before this client may exit successfully.
   */
  private webrtcSessionExpected(): boolean {
    return this.webrtcPlanSeen && this.expectedPeers.size > 0;
  }

  /** Post-run cleanup: close the socket (Chromium teardown reaps the rest). */
  private teardown(): void {
    if (this.ws !== null) {
      this.ws.onclose = null;
      try {
        this.ws.close();
      } catch {
        // Already closed/failed; nothing to release.
      }
    }
  }
}
