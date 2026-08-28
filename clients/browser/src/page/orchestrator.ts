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
//    on a full ready set: when every current member is ready — recomputed
//    from the authoritative `ready_players` snapshots over current membership,
//    since a join invalidates a cached all-ready (no corrective broadcast) and
//    a departure can restore it without one — the room creator sends an
//    explicit `StartGame`; joiners just await the `GameStarting` it produces.
// 4. Finalize: `GameStarting`, then every v3 recipient's authoritative
//    `SessionPlan` (including explicit relay/empty plans).
// 5. P2P per `peers[].initiate` (with `NewPeer` retained for compatible
//    servers); the overall WebRTC transport status reports initial resolution
//    and later real state changes.
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
import {
  deadlineAfterSeconds,
  scheduleDeadline,
  type ScheduledDeadline,
} from '../shared/deadline.js';
import { classifySignal, emit, type SignalKind } from './events.js';
import { Engine, RELIABLE_LABEL, UNRELIABLE_LABEL, type PlanIceServer } from './engine.js';
import { DeliveryAccountability, DeliveryAccountabilityViolation } from './accountability.js';
import {
  HANDSHAKE_TIMEOUT_MS,
  NON_TEXT_APPLICATION_FRAME,
  classifyJsonNegotiatedServerInput,
  clientFrame,
  connect,
  negotiatedProtocolVersion,
  sendGameData,
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
/** Defensive bounds for same-generation signals racing local pair creation. */
const MAX_PENDING_SIGNALS_PER_PEER = 32;
const MAX_PENDING_SIGNALS_TOTAL = 128;
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

/** Validate and apply the room baseline before any observable join success. */
export function applyJoinAccountabilityBaseline(
  accountability: DeliveryAccountability,
  currentPlayers: unknown,
  earlyJoined: readonly Record<string, unknown>[],
  earlyLeft: readonly Record<string, unknown>[],
): void {
  accountability.rebaselineSnapshot(currentPlayers);
  for (const player of earlyJoined) {
    accountability.notePlayerJoined(player);
  }
  for (const event of earlyLeft) {
    accountability.notePlayerLeft(event['player_id'], event['epoch'], event['final_seq']);
  }
}

/** Consume connection-level accountability frames that may precede RoomJoined. */
export function observeJoinHandshakeFrame(
  accountability: DeliveryAccountability,
  frame: ServerFrame,
): boolean {
  const isUnsupportedFormatError =
    frame.type === 'Error' && frame.data['error_code'] === 'UNSUPPORTED_GAME_DATA_FORMAT';
  accountability.observeServerMessage(isUnsupportedFormatError);
  switch (frame.type) {
    case 'DeliveryReport':
      accountability.recordReport(frame.data);
      return true;
    case 'RelayStats':
      accountability.recordRelayStats(frame.data);
      return true;
    case 'Error':
      return isUnsupportedFormatError;
    default:
      return false;
  }
}

/** Restore application membership after a retained seat reconnects. */
export function restoreReconnectedMember(
  present: Set<string>,
  membersSeen: Set<string>,
  playerId: string,
): void {
  present.add(playerId);
  membersSeen.add(playerId);
}

/** Return a changed overall WebRTC state, or null for a duplicate report. */
export function changedTransportStatus(
  previous: boolean | null,
  connectedPairCount: number,
): boolean | null {
  const current = connectedPairCount > 0;
  return previous === current ? null : current;
}

export function shouldResolveConnectedPair(
  previous: boolean | null,
  allExpectedPairsConnected: boolean,
): boolean {
  return previous !== null || allExpectedPairsConnected;
}

/** Record current connectivity; report the logical pair once per obligation. */
export function noteCurrentPairConnected(
  connected: Set<string>,
  reported: Set<string>,
  peer: string,
): boolean {
  connected.add(peer);
  if (reported.has(peer)) {
    return false;
  }
  reported.add(peer);
  return true;
}

export function isTerminalPeerConnectionState(state: string): boolean {
  return state === 'failed' || state === 'closed';
}

export function shouldBufferSignalForUnpairedPeer(
  expectedPeers: ReadonlySet<string>,
  peer: string,
): boolean {
  return expectedPeers.has(peer);
}

const SESSION_GENERATION_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function parseSessionGeneration(value: unknown): string | null {
  return typeof value === 'string' && SESSION_GENERATION_PATTERN.test(value) ? value : null;
}

export function isCurrentSessionGeneration(current: string | null, incoming: string): boolean {
  return current !== null && current === incoming;
}

export function tryBufferPlannedSignal(
  pending: Map<string, unknown[]>,
  peer: string,
  signal: unknown,
): boolean {
  let total = 0;
  for (const signals of pending.values()) {
    total += signals.length;
  }
  const peerSignals = pending.get(peer);
  if (
    total >= MAX_PENDING_SIGNALS_TOTAL ||
    (peerSignals !== undefined && peerSignals.length >= MAX_PENDING_SIGNALS_PER_PEER)
  ) {
    return false;
  }
  if (peerSignals === undefined) {
    pending.set(peer, [signal]);
  } else {
    peerSignals.push(signal);
  }
  return true;
}

export function requiresAuthoritativeFinalizationPlan(negotiatedVersion: number): boolean {
  return negotiatedVersion >= 3;
}

export function authoritativePeerDelta(
  current: ReadonlySet<string>,
  plannedPeers: readonly string[],
): { removed: string[]; added: string[]; retained: string[] } {
  const planned = new Set(plannedPeers);
  return {
    removed: [...current].filter((peer) => !planned.has(peer)),
    added: [...planned].filter((peer) => !current.has(peer)),
    retained: [...planned].filter((peer) => current.has(peer)),
  };
}

export function connectionTargetsForGeneration(
  delta: Readonly<{ added: readonly string[]; retained: readonly string[] }>,
  rebuildRetained: boolean,
): Set<string> {
  const targets = new Set(delta.added);
  if (rebuildRetained) {
    for (const peer of delta.retained) {
      targets.add(peer);
    }
  }
  return targets;
}

export function requireFinalizedMembershipPlan(
  pending: Map<string, number>,
  negotiatedVersion: number,
  lobbyState: string | null,
  playerId: string,
  epoch: unknown,
): boolean {
  if (
    negotiatedVersion < 3 ||
    lobbyState !== 'finalized' ||
    typeof epoch !== 'number' ||
    !Number.isSafeInteger(epoch) ||
    epoch <= 0
  ) {
    return false;
  }
  pending.set(playerId, epoch);
  return true;
}

export function clearDepartedMembershipPlan(
  pending: Map<string, number>,
  playerId: string,
  epoch: unknown,
): void {
  if (pending.get(playerId) === epoch) {
    pending.delete(playerId);
  }
}

/** Describe why this WebRTC-only reference client rejects a Direct plan. */
export function directPlanRejectionMessage(endpoint: unknown): string {
  const candidate =
    typeof endpoint === 'object' && endpoint !== null
      ? (endpoint as Record<string, unknown>)
      : undefined;
  const host = candidate?.['host'];
  const port = candidate?.['port'];
  const target =
    typeof host === 'string' &&
    host.length > 0 &&
    typeof port === 'number' &&
    Number.isInteger(port) &&
    port > 0 &&
    port <= 65_535
      ? `for ${host}:${port}`
      : 'without a validated endpoint';
  return `direct SessionPlan ${target} is unsupported by the browser reference client; using relay fallback`;
}

/**
 * Apply the complete browser-reference Direct rejection contract. Keeping the
 * status frame and observable events behind one seam makes the real
 * orchestrator path directly testable without a browser/WebRTC runtime.
 */
export function rejectUnsupportedDirectPlan(
  endpoint: unknown,
  sendFrame: (frame: string) => void,
  emitEvent: (event: Record<string, unknown>) => void = emit,
): void {
  emitEvent({ event: 'error', message: directPlanRejectionMessage(endpoint) });
  sendFrame(clientFrame('TransportStatus', { transport: 'direct', connected: false }));
  emitEvent({ event: 'transport_status_sent', transport: 'direct', connected: false });
  emitEvent({ event: 'fallback_engaged' });
}

/** Resolve authoritative WebRTC peer obligations; non-WebRTC plans clear all. */
export function sessionPlanPeerIds(
  transport: string,
  peers: readonly Record<string, unknown>[],
  myId: string,
): string[] {
  return transport === 'webrtc'
    ? peers.map((peer) => String(peer['player_id'])).filter((peer) => peer !== myId)
    : [];
}

/** Defensively extract the player-id strings from an authoritative snapshot. */
function parsePlayerIds(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((id): id is string => typeof id === 'string') : [];
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

/** Schedule one release-file probe without spinning while its Promise is live. */
export function scheduleSuccessReleasePoll(
  current: number | null,
  inFlight: boolean,
  now: number,
): number | null {
  return inFlight ? null : (current ?? now);
}

/** The outstanding Pong owns keepalive wake scheduling until it arrives. */
export function nextKeepaliveWake(nextPingAt: number, pongDeadline: number | null): number {
  return pongDeadline ?? nextPingAt;
}

/** A harness hold and its post-release linger supersede the soft deadline. */
export function shouldDeferSuccessAtRunDeadline(
  successReleaseEnabled: boolean,
  successCriteriaReported: boolean,
  successReleaseGranted: boolean,
  lingerUntil: number | null,
): boolean {
  return (
    successCriteriaReported &&
    ((successReleaseEnabled && !successReleaseGranted) || lingerUntil !== null)
  );
}

/** Monotonic logical exchange debt and evidence per connected peer. */
export class ExchangeLedger {
  private readonly obligations = new Set<string>();
  private readonly sent = new Map<string, Set<string>>();
  private readonly received = new Map<string, Set<string>>();

  noteConnected(peer: string): void {
    this.obligations.add(peer);
  }

  hasSent(peer: string, label: string): boolean {
    return this.sent.get(peer)?.has(label) === true;
  }

  noteSent(peer: string, label: string): void {
    let labels = this.sent.get(peer);
    if (labels === undefined) {
      labels = new Set();
      this.sent.set(peer, labels);
    }
    labels.add(label);
  }

  noteReceived(peer: string, label: string): void {
    let labels = this.received.get(peer);
    if (labels === undefined) {
      labels = new Set();
      this.received.set(peer, labels);
    }
    labels.add(label);
  }

  unmetCriteria(): string[] {
    const unmet: string[] = [];
    for (const peer of this.obligations) {
      const directions: Array<[string, ReadonlySet<string> | undefined]> = [
        ['sent to', this.sent.get(peer)],
        ['received from', this.received.get(peer)],
      ];
      for (const [direction, labels] of directions) {
        const complete =
          labels !== undefined && labels.has(RELIABLE_LABEL) && labels.has(UNRELIABLE_LABEL);
        if (!complete) {
          unmet.push(`exchange incomplete (${direction} ${peer})`);
        }
      }
    }
    return unmet;
  }
}

/**
 * Readiness snapshot + explicit-`StartGame` latch for the room creator (the
 * field-for-field mirror of the native `StartGameGate`).
 *
 * The creator's send is latched so repeated `LobbyStateChanged` broadcasts
 * cannot duplicate it, but the latch must not survive an invalidation: a later
 * join is always unready and emits only `PlayerJoined` (no corrective
 * broadcast), so a cached `all_ready: true` goes stale and the server's
 * authoritative gate rejects a stale-latch send with `GAME_START_NOT_READY`.
 * Per the documented client contract
 * (docs/guides/building-a-client.md "StartGame authorization and readiness"),
 * the creator recomputes readiness from the authoritative `ready_players`
 * snapshot over the CURRENT membership and re-issues `StartGame` once every
 * current player is ready again — via the next `all_ready` toggle, or
 * immediately when the unready member leaves (departures emit no readiness
 * broadcast).
 */
export class StartGameGate {
  private readonly readyPlayers = new Set<string>();
  /** `StartGame` went out since the last invalidation (a join, a departure,
   * or an authoritative `GAME_START_NOT_READY`). */
  private sentSinceInvalidation = false;

  constructor(readyPlayers: readonly string[]) {
    this.readyPlayers = new Set(readyPlayers);
  }

  /** Replace the readiness set from an authoritative snapshot (`RoomJoined`
   * baseline or `LobbyStateChanged`). Never re-arms the latch by itself: a
   * snapshot without an invalidation must not duplicate a send. */
  snapshot(readyPlayers: readonly string[]): void {
    this.readyPlayers.clear();
    for (const player of readyPlayers) {
      this.readyPlayers.add(player);
    }
  }

  /** A member joined. Joiners are always unready and their `PlayerJoined`
   * arrives with no corrective broadcast, so the cached all-ready snapshot is
   * stale and the latch re-arms. */
  memberJoined(player: string): void {
    this.readyPlayers.delete(player);
    this.sentSinceInvalidation = false;
  }

  /** A member departed. Readiness belongs to the CURRENT membership, so a
   * departure can restore all-ready with no readiness broadcast at all; the
   * previous send's premise (that exact membership) is gone either way. */
  memberLeft(player: string): void {
    this.readyPlayers.delete(player);
    this.sentSinceInvalidation = false;
  }

  /** The authoritative `StartGame` gate rejected our send
   * (`GAME_START_NOT_READY`): the cached snapshot is stale and the latch
   * re-arms. */
  startRejected(): void {
    this.sentSinceInvalidation = false;
  }

  /** Every current member is ready. Membership, not a raw count, decides —
   * mirroring the server's gate, which also requires a non-empty room. */
  allCurrentReady(present: ReadonlySet<string>): boolean {
    if (present.size === 0) {
      return false;
    }
    for (const player of present) {
      if (!this.readyPlayers.has(player)) {
        return false;
      }
    }
    return true;
  }

  /** Whether the creator should send (or re-issue) the explicit `StartGame`. */
  shouldSend(createRoom: boolean, gameStarted: boolean, present: ReadonlySet<string>): boolean {
    return (
      createRoom && !gameStarted && !this.sentSinceInvalidation && this.allCurrentReady(present)
    );
  }

  /** Record that the `StartGame` went out on the wire. */
  noteSent(): void {
    this.sentSinceInvalidation = true;
  }

  /** Reset to an empty baseline (`RoomLeft`): the room's readiness is gone. */
  reset(): void {
    this.readyPlayers.clear();
    this.sentSinceInvalidation = false;
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
  private accountability: DeliveryAccountability | null = null;
  private negotiatedVersion: number | null = null;
  private lobbyState: string | null = null;
  private inLobby = false;
  private readySent = false;
  /**
   * Readiness snapshot + explicit-`StartGame` latch for the room creator.
   * Guards against duplicate sends on repeated `LobbyStateChanged` broadcasts
   * while still re-issuing after a membership invalidation.
   */
  private readonly startGameGate = new StartGameGate([]);
  private gameStarted = false;
  private lateJoined = false;
  private initialSessionPlanPending = false;
  private readonly pendingMembershipPlans = new Map<string, number>();
  private webrtcPlanSeen = false;
  /** Opaque generation from the latest authoritative SessionPlan. */
  private currentSessionGeneration: string | null = null;
  private readonly expectedPeers = new Set<string>();
  private readonly connectedPairs = new Set<string>();
  private readonly pairConnectedReported = new Set<string>();
  private lastIceServers: PlanIceServer[] = [];
  private transportStatus: boolean | null = null;
  private p2pDeadline: number | null = null;
  private relaySendAt: number | null = null;
  private relaySent = false;
  private readonly relayReceivedFrom = new Set<string>();
  private readonly peerStatusFrom = new Set<string>();
  private readonly exchangeLedger = new ExchangeLedger();
  private readonly pendingExchangePeers = new Set<string>();
  private readonly reportedExchangeDiagnostics = new Set<string>();
  private exchangeRetryAt: number | null = null;
  private readonly pendingSignals = new Map<string, unknown[]>();
  private runDeadline = 0;
  private lingerUntil: number | null = null;
  private successCriteriaReported = false;
  private successReleaseGranted = false;
  private successReleasePollAt: number | null = null;
  private successReleasePollInFlight = false;
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
  private wakeTimer: ScheduledDeadline | null = null;

  constructor(config: RunConfig) {
    this.config = config;
    this.engine = new Engine(config.crippleIce, {
      onLocalCandidate: (peer, generation, candidateJson) => {
        // Crippled mode never reaches here (the engine drops gathered
        // candidates), so this is always a real trickle-ICE relay.
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          this.sendSignal(peer, 'ice_candidate', { IceCandidate: candidateJson });
        });
      },
      onPcState: (peer, generation, state) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          emit({ event: 'pc_state', peer, state });
          if (isTerminalPeerConnectionState(state)) {
            this.handlePeerTransportLoss(peer);
          }
        });
      },
      onChannelOpen: (peer, generation, label) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          emit({ event: 'channel_open', peer, label });
          if (this.engine.noteChannelOpen(peer, label)) {
            this.onPairConnected(peer);
          }
        });
      },
      onChannelClosed: (peer, generation, label) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          emit({ event: 'channel_closed', peer, label });
          this.handlePeerTransportLoss(peer);
        });
      },
      onChannelMessage: (peer, generation, label, text) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          this.exchangeLedger.noteReceived(peer, label);
          emit({ event: 'channel_message', peer, label, text });
        });
      },
    });
  }

  private accountabilityState(): DeliveryAccountability {
    if (this.accountability === null || this.negotiatedVersion === null) {
      throw FatalError.protocol('application frame arrived before ProtocolInfo negotiation');
    }
    return this.accountability;
  }

  async run(): Promise<number> {
    // The soft run window starts at PROCESS start: the CLI reports the time
    // already spent launching Chromium so the window matches the native
    // client's process-start semantics.
    const now = Date.now();
    const processStartedAtMs = now - this.config.elapsedBeforeStartMs;
    this.runDeadline = deadlineAfterSeconds(processStartedAtMs, this.config.runForSecs);
    this.nextPingAt = now + PING_INTERVAL_MS;

    let ws: WebSocket;
    try {
      ws = await connect(this.config.serverUrl);
    } catch (error) {
      throw FatalError.connection(describe(error));
    }
    this.ws = ws;
    emit({ event: 'connected' });
    ws.onmessage = (event: MessageEvent<unknown>) => {
      this.onServerInput(event.data);
    };
    ws.onclose = () => {
      this.onSocketClosed();
    };

    const negotiatedVersion = await this.authenticate();
    this.negotiatedVersion = negotiatedVersion;
    this.accountability = new DeliveryAccountability(negotiatedVersion >= 3);
    const lobbyState = await this.joinRoom();

    // Joining an already-Lobby room (seat fill) means readiness is
    // immediately possible; a Finalized room means the session is already
    // running — GameStarting was broadcast before we joined and will never
    // be re-sent, so the criterion is satisfied on entry.
    this.inLobby = lobbyState === 'lobby';
    this.lobbyState = lobbyState;
    this.gameStarted = lobbyState === 'finalized';
    this.lateJoined = lobbyState === 'finalized';
    this.initialSessionPlanPending = negotiatedVersion >= 3 && lobbyState === 'finalized';
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
  private async authenticate(): Promise<number> {
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
    let negotiated: number;
    try {
      negotiated = negotiatedProtocolVersion(infoResponse, this.config.protocolVersion);
    } catch (error) {
      throw FatalError.protocol(describe(error));
    }
    emit({
      event: 'protocol_info',
      negotiated_version: negotiated,
    });
    return negotiated;
  }

  /** Create or join the room; returns the room's lobby state at join time. */
  private async joinRoom(): Promise<string> {
    this.sendFrame(
      clientFrame('JoinRoom', {
        game_name: this.config.gameName,
        room_code: this.config.joinCode,
        player_name: this.config.playerName,
        max_players: this.config.maxPlayers ?? this.config.peers,
        supports_authority: false,
      }),
    );

    // Read until the atomic `RoomJoined` membership baseline. Connection-level
    // accountability frames can legitimately precede it. Membership deltas
    // remain accepted for compatibility and deterministic test channels.
    const earlyJoined: Record<string, unknown>[] = [];
    const earlyLeft: Record<string, unknown>[] = [];
    let response = await this.nextHandshakeFrame();
    while (response.type !== 'RoomJoined') {
      if (observeJoinHandshakeFrame(this.accountabilityState(), response)) {
        response = await this.nextHandshakeFrame();
        continue;
      }
      if (response.type === 'RoomJoinFailed') {
        throw FatalError.protocol(
          `room join failed: ${String(response.data['reason'])} ` +
            `(${String(response.data['error_code'])})`,
        );
      }
      if (response.type === 'PlayerJoined') {
        const player = response.data['player'] as Record<string, unknown> | undefined;
        if (player !== undefined) {
          earlyJoined.push(player);
        }
      } else if (response.type === 'PlayerLeft') {
        const id = response.data['player_id'];
        if (typeof id === 'string') {
          earlyLeft.push(response.data);
        }
      } else {
        throw FatalError.protocol(`expected RoomJoined, got ${response.type}`);
      }
      response = await this.nextHandshakeFrame();
    }
    const payload = response.data;
    const currentPlayers = payload['current_players'];
    applyJoinAccountabilityBaseline(
      this.accountabilityState(),
      currentPlayers,
      earlyJoined,
      earlyLeft,
    );
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
    if (Array.isArray(currentPlayers)) {
      for (const player of currentPlayers as Array<Record<string, unknown>>) {
        const id = player['id'];
        if (typeof id === 'string') {
          this.present.add(id);
        }
      }
    }
    // Fold in deltas observed ahead of the baseline (set-idempotent).
    this.startGameGate.snapshot(parsePlayerIds(payload['ready_players']));
    for (const player of earlyJoined) {
      const id = player['id'];
      if (typeof id === 'string') {
        this.present.add(id);
        // A pre-baseline joiner is unready like any other joiner.
        this.startGameGate.memberJoined(id);
      }
    }
    for (const event of earlyLeft) {
      const id = String(event['player_id']);
      this.present.delete(id);
      this.startGameGate.memberLeft(id);
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

  private onServerInput(data: unknown): void {
    let frame: ServerFrame;
    try {
      frame = classifyJsonNegotiatedServerInput(data);
    } catch (error) {
      this.failFatal(FatalError.protocol(`invalid ServerMessage frame: ${describe(error)}`));
      return;
    }
    this.routeServerFrame(frame);
  }

  private routeServerFrame(frame: ServerFrame): void {
    if (this.streaming) {
      this.enqueue(() => this.handleServerMessage(frame));
    } else if (this.frameWaiter !== null) {
      this.frameWaiter.resolve(frame);
    } else {
      this.bufferedFrames.push(frame);
    }
  }

  private onSocketClosed(): void {
    this.accountability?.observeTerminal();
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
            : error instanceof DeliveryAccountabilityViolation
              ? FatalError.protocol(error.message)
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
      this.wakeTimer.cancel();
      this.wakeTimer = null;
    }
    this.finishResolve?.(code);
  }

  /** Earliest pending timer (the run deadline at the latest). */
  private nextWake(): number {
    const deferringRunDeadline = shouldDeferSuccessAtRunDeadline(
      this.config.successReleaseEnabled,
      this.successCriteriaReported,
      this.successReleaseGranted,
      this.lingerUntil,
    );
    let wake = deferringRunDeadline ? Number.POSITIVE_INFINITY : this.runDeadline;
    if (!this.relaySent && this.relaySendAt !== null) {
      wake = Math.min(wake, this.relaySendAt);
    }
    if (this.exchangeRetryAt !== null) {
      wake = Math.min(wake, this.exchangeRetryAt);
    }
    if (this.p2pDeadline !== null) {
      wake = Math.min(wake, this.p2pDeadline);
    }
    if (this.lingerUntil !== null) {
      wake = Math.min(wake, this.lingerUntil);
    }
    if (!this.successReleasePollInFlight && this.successReleasePollAt !== null) {
      wake = Math.min(wake, this.successReleasePollAt);
    }
    wake = Math.min(wake, nextKeepaliveWake(this.nextPingAt, this.pongDeadline));
    return wake;
  }

  private scheduleWake(): void {
    if (this.finished || this.finishResolve === null) {
      return;
    }
    if (this.wakeTimer !== null) {
      this.wakeTimer.cancel();
    }
    this.wakeTimer = scheduleDeadline(this.nextWake(), () => {
      this.wakeTimer = null;
      // Through the chain so a wake never interleaves with a live handler.
      this.enqueue(async () => {});
    });
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

    if (this.p2pDeadline !== null && now >= this.p2pDeadline) {
      // The current P2P window expired: report any real state change.
      this.resolveTransportStatus();
      this.p2pDeadline = null;
    }

    if (this.successReleasePollAt !== null && now >= this.successReleasePollAt) {
      this.pollSuccessRelease();
    }

    this.armSuccessLinger(now);
    if (this.lingerUntil !== null && now >= this.lingerUntil) {
      // Criteria can regress during the linger (an authoritative plan or a
      // freshly connected pair adds new obligations); re-validate at expiry.
      if (this.criteriaMet()) {
        this.finish(EXIT_SUCCESS);
        return;
      }
      console.error('success criteria regressed during the exit linger; continuing');
      this.lingerUntil = null;
    }

    const deferringRunDeadline = shouldDeferSuccessAtRunDeadline(
      this.config.successReleaseEnabled,
      this.successCriteriaReported,
      this.successReleaseGranted,
      this.lingerUntil,
    );
    if (now >= this.runDeadline && !deferringRunDeadline) {
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

  private armSuccessLinger(now: number): void {
    if (!this.criteriaMet()) {
      if (
        this.config.successReleaseEnabled &&
        this.successCriteriaReported &&
        !this.successReleaseGranted
      ) {
        this.lingerUntil = null;
        this.successReleasePollAt = scheduleSuccessReleasePoll(
          this.successReleasePollAt,
          this.successReleasePollInFlight,
          now,
        );
      }
      return;
    }
    if (this.config.successReleaseEnabled && !this.successCriteriaReported) {
      emit({ event: 'success_criteria_met' });
      this.successCriteriaReported = true;
    }
    if (this.config.successReleaseEnabled && !this.successReleaseGranted) {
      this.lingerUntil = null;
      this.successReleasePollAt = scheduleSuccessReleasePoll(
        this.successReleasePollAt,
        this.successReleasePollInFlight,
        now,
      );
      return;
    }
    this.successReleasePollAt = null;
    if (this.lingerUntil === null) {
      this.lingerUntil = now + EXIT_LINGER_MS;
    }
  }

  private pollSuccessRelease(): void {
    if (this.successReleasePollInFlight || this.successReleaseGranted) {
      return;
    }
    const probe = window.__sf_success_released;
    if (probe === undefined) {
      throw FatalError.protocol('success-release bridge is unavailable');
    }
    this.successReleasePollInFlight = true;
    this.successReleasePollAt = null;
    void probe()
      .then((released) => {
        this.enqueue(async () => {
          this.successReleasePollInFlight = false;
          this.successReleaseGranted = released;
          if (!released) {
            this.successReleasePollAt = Date.now() + 100;
          }
        });
      })
      .catch((error: unknown) => {
        this.enqueue(async () => {
          this.successReleasePollInFlight = false;
          this.fail(FatalError.protocol(`success-release probe failed: ${describe(error)}`));
        });
      });
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
    const accountability = this.accountabilityState();
    accountability.observeServerMessage(
      frame.type === 'Error' && data['error_code'] === 'UNSUPPORTED_GAME_DATA_FORMAT',
    );
    switch (frame.type) {
      case 'RoomJoined':
        accountability.rebaselineSnapshot(data['current_players']);
        this.startGameGate.snapshot(parsePlayerIds(data['ready_players']));
        break;
      case 'RoomLeft':
        accountability.resetRoom();
        this.initialSessionPlanPending = false;
        this.pendingMembershipPlans.clear();
        this.lobbyState = null;
        this.currentSessionGeneration = null;
        this.pendingSignals.clear();
        this.startGameGate.reset();
        break;
      case 'PlayerJoined': {
        const player = data['player'] as Record<string, unknown>;
        accountability.notePlayerJoined(player);
        const id = String(player['id']);
        this.present.add(id);
        this.membersSeen.add(id);
        // A joiner is always unready and no corrective broadcast fires, so a
        // cached all-ready snapshot just went stale (and any in-flight latch
        // with it). No re-issue is possible yet — the joiner is provably not
        // ready — so this only re-arms the gate for the joiner's future
        // toggle. (Mirrors the native client.)
        this.startGameGate.memberJoined(id);
        if (
          requireFinalizedMembershipPlan(
            this.pendingMembershipPlans,
            this.negotiatedVersion ?? 2,
            this.lobbyState,
            id,
            player['epoch'],
          )
        ) {
          this.lingerUntil = null;
        }
        emit({ event: 'peer_joined', player_id: id });
        this.maybeSendReady();
        break;
      }
      case 'PlayerLeft': {
        const rawId = data['player_id'];
        accountability.notePlayerLeft(rawId, data['epoch'], data['final_seq']);
        const id = String(rawId);
        this.present.delete(id);
        // Readiness belongs to the current membership: the departure may
        // restore all-ready with no readiness broadcast (an unready late
        // joiner leaving), so re-arm and recompute. (Mirrors the native
        // client.)
        this.startGameGate.memberLeft(id);
        clearDepartedMembershipPlan(this.pendingMembershipPlans, id, data['epoch']);
        // A departed peer can no longer satisfy any pairing-derived
        // criterion (see the native client for the full rationale).
        this.removePairObligation(id);
        // Departure can either complete the remaining set or remove the last
        // live P2P path. Real state transitions are reported; duplicates are
        // suppressed by resolveTransportStatus.
        if (
          this.webrtcPlanSeen &&
          (this.transportStatus !== null ||
            this.expectedPeers.size === 0 ||
            this.allExpectedPairsConnected())
        ) {
          this.resolveTransportStatus();
        }
        emit({ event: 'player_left', player_id: id });
        // The departure may have made the remaining membership all-ready
        // again with no readiness broadcast; recompute and re-issue if so
        // (post-finalize departures are blocked by the gate's `gameStarted`
        // guard).
        this.maybeSendStartGame();
        break;
      }
      case 'GameStarting': {
        this.gameStarted = true;
        this.lobbyState = 'finalized';
        if (requiresAuthoritativeFinalizationPlan(this.negotiatedVersion ?? 2)) {
          this.initialSessionPlanPending = true;
          this.lingerUntil = null;
        }
        const connections = (data['peer_connections'] ?? []) as Array<Record<string, unknown>>;
        const mine = connections.find((peer) => peer['player_id'] === this.myId);
        emit({ event: 'game_starting', is_authority: mine?.['is_authority'] === true });
        if (this.config.relayPayload !== null && !this.relaySent) {
          this.relaySendAt = Date.now() + RELAY_SEND_SETTLE_MS;
        }
        break;
      }
      case 'SessionPlan': {
        this.initialSessionPlanPending = false;
        this.pendingMembershipPlans.clear();
        const peers = (data['peers'] ?? []) as Array<Record<string, unknown>>;
        const iceServers = (data['ice_servers'] ?? []) as PlanIceServer[];
        const transport = String(data['transport']);
        const generation = parseSessionGeneration(data['generation']);
        if (generation === null) {
          throw FatalError.protocol('SessionPlan.generation must be a UUID');
        }
        const generationChanged = this.currentSessionGeneration !== generation;
        this.currentSessionGeneration = generation;
        if (generationChanged) {
          this.pendingSignals.clear();
        }
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
        if (this.config.leaveOnGameStart) {
          break;
        }
        if (transport === 'direct') {
          rejectUnsupportedDirectPlan(data['direct_endpoint'], (frame) =>
            this.sendFrame(frame),
          );
        }
        if (transport === 'webrtc') {
          this.webrtcPlanSeen = true;
          this.lastIceServers = iceServers;
        }
        const plannedPeers = sessionPlanPeerIds(transport, peers, this.myId);
        const delta = authoritativePeerDelta(this.expectedPeers, plannedPeers);
        for (const peer of delta.removed) {
          this.removePairObligation(peer);
        }
        const rebuildRetained = generationChanged && transport === 'webrtc';
        const added = connectionTargetsForGeneration(delta, rebuildRetained);
        if (rebuildRetained) {
          for (const peer of delta.retained) {
            this.prepareRetainedPairReplacement(peer);
          }
          if (this.webrtcPlanSeen && this.transportStatus !== null) {
            this.resolveTransportStatus();
          }
        }
        for (const peer of peers) {
          const peerId = String(peer['player_id']);
          if (transport === 'webrtc' && added.delete(peerId)) {
            await this.establishPair(peerId, peer['initiate'] === true);
          }
        }
        if (this.expectedPeers.size === 0 || this.allExpectedPairsConnected()) {
          this.p2pDeadline = null;
        }
        if (
          this.transportStatus !== null ||
          (transport === 'webrtc' &&
            (this.expectedPeers.size === 0 || this.allExpectedPairsConnected()))
        ) {
          this.resolveTransportStatus();
        }
        break;
      }
      case 'NewPeer': {
        const peerId = String(data['peer_id']);
        const youInitiate = data['you_initiate'] === true;
        emit({ event: 'new_peer', peer_id: peerId, you_initiate: youInitiate });
        // Same pairing path as a plan peer (the late-join offerer rule).
        await this.establishPair(peerId, youInitiate);
        break;
      }
      case 'Signal': {
        await this.handleSignal(
          String(data['from']),
          parseSessionGeneration(data['generation']) ?? '',
          data['signal'],
        );
        break;
      }
      case 'GameData': {
        const disposition = accountability.recordGameData(data);
        if (disposition === 'stale') {
          console.error(
            `discarding stale trailing GameData from ${String(data['from_player'])}`,
          );
          break;
        }
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
      case 'GameDataBinary': {
        const disposition = accountability.recordGameData(data);
        const from = String(data['from_player']);
        if (disposition === 'stale') {
          console.error(`discarding stale trailing binary GameData from ${from}`);
          break;
        }
        const payload = data['payload'];
        console.debug(
          `received accountable opaque binary GameData from ${from} ` +
            `(${String(data['encoding'])}, ${payload instanceof Uint8Array ? payload.byteLength : 0} bytes)`,
        );
        break;
      }
      case NON_TEXT_APPLICATION_FRAME:
        throw FatalError.protocol(
          `unexpected non-text application frame: ${String(data['error'] ?? 'unsupported browser payload')}`,
        );
      case 'DeliveryReport':
        accountability.recordReport(data);
        break;
      case 'RelayStats':
        accountability.recordRelayStats(data);
        break;
      case 'Reconnected': {
        accountability.rebaselineReconnected(
          data['current_players'],
          data['sender_watermarks'],
        );
        this.lobbyState = String(data['lobby_state']);
        this.inLobby = this.lobbyState === 'lobby';
        this.gameStarted = this.lobbyState === 'finalized';
        // The reconnect snapshot's readiness is authoritative: lobby readiness
        // can be pruned while we were away, so a cached pre-disconnect
        // snapshot is exactly the stale `all_ready` class this gate exists to
        // avoid. (Mirrors the native client.)
        this.startGameGate.snapshot(parsePlayerIds(data['ready_players']));
        if ((this.negotiatedVersion ?? 2) >= 3 && this.lobbyState === 'finalized') {
          this.initialSessionPlanPending = true;
          this.lingerUntil = null;
        }
        break;
      }
      case 'PlayerReconnected': {
        const rawId = data['player_id'];
        accountability.notePlayerReconnected(rawId, data['epoch']);
        const id = String(rawId);
        restoreReconnectedMember(this.present, this.membersSeen, id);
        if (
          requireFinalizedMembershipPlan(
            this.pendingMembershipPlans,
            this.negotiatedVersion ?? 2,
            this.lobbyState,
            id,
            data['epoch'],
          )
        ) {
          this.lingerUntil = null;
        }
        emit({ event: 'peer_joined', player_id: id });
        this.maybeSendReady();
        break;
      }
      case 'SpectatorJoined':
        accountability.rebaselineSnapshot(data['current_players']);
        break;
      case 'SpectatorLeft':
        accountability.resetRoom();
        break;
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
        if (data['error_code'] === 'GAME_START_NOT_READY') {
          // The authoritative StartGame gate rejected our send: the room's
          // current membership is not all ready (e.g. a join slipped in
          // between the all_ready broadcast and our StartGame). Re-arm the
          // latch so the next recomputed all-ready moment re-issues — the
          // documented client contract (docs/guides/building-a-client.md
          // "StartGame authorization and readiness"). Mirrors the native
          // client's dedicated arm.
          this.startGameGate.startRejected();
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
        this.lobbyState = String(data['lobby_state']);
        this.startGameGate.snapshot(parsePlayerIds(data['ready_players']));
        if (this.lobbyState === 'lobby') {
          this.inLobby = true;
          this.maybeSendReady();
          // The lobby no longer auto-starts: the room creator issues the
          // explicit StartGame that produces GameStarting once the recomputed
          // readiness gate reports the full ready set.
          this.maybeSendStartGame();
        }
        break;
      }
      case 'Pong':
        // Keepalive round-trip complete.
        this.pongDeadline = null;
        this.pongGraceApplied = false;
        break;
      default:
        console.error(
          frame.type === NON_TEXT_APPLICATION_FRAME
            ? 'ignoring non-text websocket application frame'
            : `ignoring server message ${frame.type}`,
        );
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
      // A seat-vacating client logs pairing directives but never acts on them,
      // so it produces zero signaling traffic.
      return;
    }
    const newlyExpected = !this.expectedPeers.has(peer);
    const needsConnection = !this.engine.isPaired(peer);
    this.expectedPeers.add(peer);
    if ((newlyExpected || needsConnection) && !this.connectedPairs.has(peer)) {
      this.p2pDeadline = deadlineAfterSeconds(Date.now(), this.config.p2pTimeoutSecs);
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

  private removePairObligation(peer: string): void {
    this.expectedPeers.delete(peer);
    this.connectedPairs.delete(peer);
    this.pairConnectedReported.delete(peer);
    this.peerStatusFrom.delete(peer);
    this.pendingExchangePeers.delete(peer);
    this.reportedExchangeDiagnostics.delete(peer);
    this.pendingSignals.delete(peer);
    this.engine.removePeer(peer);
  }

  /** Tear down an unusable P2P link while retaining its planned obligation. */
  private handlePeerTransportLoss(peer: string): void {
    this.connectedPairs.delete(peer);
    this.pendingExchangePeers.delete(peer);
    this.reportedExchangeDiagnostics.delete(peer);
    this.pendingSignals.delete(peer);
    this.engine.removePeer(peer);
    if (this.webrtcPlanSeen) {
      this.resolveTransportStatus();
    }
  }

  /** Retire one retained physical link for a newer authoritative generation. */
  private prepareRetainedPairReplacement(peer: string): void {
    this.connectedPairs.delete(peer);
    this.pendingExchangePeers.delete(peer);
    this.reportedExchangeDiagnostics.delete(peer);
    this.pendingSignals.delete(peer);
    this.engine.removePeer(peer);
  }

  /**
   * Emit `signal_received` and route an inbound signal (buffering it when the
   * peer is not paired yet).
   */
  private async handleSignal(from: string, generation: string, signal: unknown): Promise<void> {
    if (!isCurrentSessionGeneration(this.currentSessionGeneration, generation)) {
      console.debug(
        `discarding signal from ${from} for stale/unknown generation ${generation}`,
      );
      return;
    }
    emit({ event: 'signal_received', from, kind: classifySignal(signal) });
    if (!this.engine.isPaired(from)) {
      if (shouldBufferSignalForUnpairedPeer(this.expectedPeers, from)) {
        if (!tryBufferPlannedSignal(this.pendingSignals, from, signal)) {
          console.error(`dropping planned-peer signal from ${from}: defensive buffer is full`);
        }
      } else {
        console.debug(`discarding signal from ${from}: peer is absent from the plan`);
      }
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
    if (noteCurrentPairConnected(this.connectedPairs, this.pairConnectedReported, peer)) {
      emit({ event: 'p2p_pair_connected', peer });
    }
    if (this.config.exchange) {
      this.exchangeLedger.noteConnected(peer);
      this.pendingExchangePeers.add(peer);
      this.flushPendingExchangeSends();
    }
    // All expected pairs connected resolves the overall status early.
    if (shouldResolveConnectedPair(this.transportStatus, this.allExpectedPairsConnected())) {
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
      if (this.exchangeLedger.hasSent(peer, label)) {
        continue;
      }
      const channel = this.engine.channel(peer, label);
      if (channel === undefined) {
        const diagnosticKey = `missing\0${peer}\0${label}`;
        if (!this.reportedExchangeDiagnostics.has(diagnosticKey)) {
          this.reportedExchangeDiagnostics.add(diagnosticKey);
          emit({
            event: 'error',
            message: `open pair with ${peer} is missing channel ${label}`,
          });
        }
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
        const diagnosticKey = `send\0${peer}\0${label}`;
        if (!this.reportedExchangeDiagnostics.has(diagnosticKey)) {
          this.reportedExchangeDiagnostics.add(diagnosticKey);
          const message =
            `data-channel exchange send to ${peer} on ${label} failed while channel was open; ` +
            `bufferedAmount=${channel.bufferedAmount}; will retry: ${describe(error)}`;
          emit({ event: 'error', message });
          console.error(message);
        }
        complete = false;
        continue;
      }
      this.exchangeLedger.noteSent(peer, label);
      emit({ event: 'channel_message_sent', peer, label, text });
    }
    return complete;
  }

  /**
   * Transport-fallback early-resolution condition: at least one expected pair, and
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

  /** Report a changed overall WebRTC state; suppress duplicate snapshots. */
  private resolveTransportStatus(): void {
    const connected = changedTransportStatus(this.transportStatus, this.connectedPairs.size);
    if (connected === null) {
      return;
    }
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
   * Send the explicit `StartGame` that finalizes the lobby when this client
   * created the room AND every current member is ready per the recomputed
   * readiness gate.
   *
   * The protocol no longer auto-starts a full, all-ready room: finalization is
   * driven by an explicit `StartGame` from the authority — or, when no
   * authority is designated (the interop rooms never set one), any member. The
   * room creator is elected as that member here: it is always a v3 participant
   * that is present through finalization, so the choice is deterministic and
   * needs no cross-client coordination. Joiners send no `StartGame`; they
   * simply await the `GameStarting` the creator's call produces.
   *
   * The gate latches the send against duplicate all-ready broadcasts, but a
   * join, a departure, or an authoritative `GAME_START_NOT_READY` invalidates
   * that latch (see `StartGameGate`), and the next recomputed all-ready moment
   * re-issues — the documented client contract
   * (docs/guides/building-a-client.md "StartGame authorization and readiness").
   * The server re-checks readiness under its room lock, so a premature
   * re-issue is rejected non-fatally and simply re-arms the gate.
   */
  private maybeSendStartGame(): void {
    if (this.startGameGate.shouldSend(this.config.createRoom, this.gameStarted, this.present)) {
      this.sendFrame(clientFrame('StartGame'));
      this.startGameGate.noteSent();
      console.error('all members ready; sent StartGame to finalize the lobby');
    }
  }

  /** Send the `--relay-payload` GameData over the relay floor. */
  private sendRelayPayload(): void {
    if (this.config.relayPayload === null) {
      this.relaySent = true;
      return;
    }
    sendGameData((frame) => this.sendFrame(frame), {
      relay_msg: this.config.relayPayload,
    });
    this.relaySent = true;
    this.relaySendAt = null;
    emit({ event: 'game_data_sent' });
  }

  private sendSignal(to: string, kind: SignalKind, signal: Record<string, unknown>): void {
    if (this.currentSessionGeneration === null) {
      throw FatalError.protocol(`cannot signal peer ${to} before an authoritative SessionPlan`);
    }
    this.sendFrame(
      clientFrame('Signal', {
        to,
        generation: this.currentSessionGeneration,
        signal,
      }),
    );
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
    if (this.initialSessionPlanPending || this.pendingMembershipPlans.size > 0) {
      unmet.push(
        `awaiting authoritative SessionPlan (${this.initialSessionPlanPending ? 'session' : 'membership'} trigger, ${this.pendingMembershipPlans.size} membership epoch(s))`,
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
      // A connected peer's logical debt survives later membership or physical
      // transport teardown. Never-connected fallback peers create no debt.
      unmet.push(...this.exchangeLedger.unmetCriteria());
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
   * A WebRTC plan with at least one pairing was issued, so the transport-fallback
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
