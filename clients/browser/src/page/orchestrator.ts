// Orchestrator: drives one full protocol-v3 client lifecycle inside the page.
//
// This is a faithful port of the native client's orchestrator
// (clients/native/src/client.rs) — same flow, same success criteria, same
// stdout event ordering:
//
// 1. Connect + Authenticate (v2 mode omits every v3 field).
// 2. Room: `JoinRoom` with no code creates; `--join-code` joins by code with
//    `join_only` set, so an unresolvable code is refused instead of silently
//    creating a room (issue #630).
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
// WebRTC callbacks both trigger async work. Room input uses one promise
// chain (`enqueue`). Each peer has a separate ordered SDP/ICE queue so a
// stalled WebRTC operation cannot hold relay input or other peers. Retained
// peer work keeps its input backlog credit until consumption or cancellation.

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
  monotonicNowMs,
  scheduleDeadline,
  type ScheduledDeadline,
} from '../shared/deadline.js';
import {
  classifySignal,
  emit,
  signalForRetryAttempt,
  tagRetrySignal,
  type SignalKind,
} from './events.js';
import { Engine, RELIABLE_LABEL, UNRELIABLE_LABEL, type PlanIceServer } from './engine.js';
import { DeliveryAccountability, DeliveryAccountabilityViolation } from './accountability.js';
import {
  HANDSHAKE_TIMEOUT_MS,
  NON_TEXT_APPLICATION_FRAME,
  bytesToBase64,
  classifyJsonNegotiatedServerInput,
  classifyMessagePackNegotiatedServerInput,
  classifyOpaqueNegotiatedServerInput,
  clientFrame,
  connect,
  joinRoomFrameData,
  negotiatedGameDataFormat,
  negotiatedProtocolVersion,
  sendGameData,
  sendOpaqueGameData,
  encodeOpaqueRelayProbe,
  sendMessagePackGameData,
  type ServerFrame,
} from './wire.js';
import { Backlog, utf8Bytes } from '../shared/backlog.js';
import { EventOutput } from '../shared/event-output.js';
import type { GameDataFormat } from '../shared/types.js';

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
 * One-shot grace when a deadline and browser socket tasks become ready
 * together. Give the socket callback another chance to observe live traffic
 * before declaring the unanswered Ping fatal. Mirrors the native grace.
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
// Stale work keeps credits until consumption, across all peers and generations.
const MAX_PENDING_CHANNEL_MESSAGES = 256;
const MAX_PENDING_CHANNEL_MESSAGE_BYTES = 1024 * 1024;
const MAX_SERVER_FRAME_BYTES = 64 * 1024 * 1024;
const MAX_PENDING_SERVER_FRAMES = 256;
const MAX_PENDING_SERVER_FRAME_BYTES = 128 * 1024 * 1024;

type RetainedServerFrame = { frame: ServerFrame; release: () => void };
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

/**
 * Consume one server frame of the authenticate handshake.
 *
 * The server reports an unsupported `game_data_format` with a budget-charged
 * `Error` BEFORE `Authenticated` (pinned wire order, server
 * `tests/e2e_tests.rs`) while downgrading the session to JSON, so the notice
 * adopts JSON in place and the handshake continues instead of failing a
 * viable session. A second notice, or any notice for an already-JSON
 * session, stays fatal.
 */
export function advanceAuthenticateHandshake(
  frame: ServerFrame,
  effectiveFormat: GameDataFormat,
): { fatal: string } | { authenticated: boolean; effectiveFormat: GameDataFormat } {
  if (frame.type === 'Authenticated') {
    return { authenticated: true, effectiveFormat };
  }
  if (frame.type === 'AuthenticationError') {
    return {
      fatal:
        `authentication rejected: ${String(frame.data['error'])} ` +
        `(${String(frame.data['error_code'])})`,
    };
  }
  if (
    frame.type === 'Error' &&
    frame.data['error_code'] === 'UNSUPPORTED_GAME_DATA_FORMAT' &&
    effectiveFormat !== 'json'
  ) {
    return { authenticated: false, effectiveFormat: 'json' };
  }
  return { fatal: 'expected Authenticated' };
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

function isExchangeProbe(peer: string, label: string, text: string): boolean {
  if (label !== RELIABLE_LABEL && label !== UNRELIABLE_LABEL) {
    return false;
  }
  let payload: unknown;
  try {
    payload = JSON.parse(text);
  } catch {
    return false;
  }
  if (typeof payload !== 'object' || payload === null || Array.isArray(payload)) {
    return false;
  }
  const probe = payload as Record<string, unknown>;
  return probe['from'] === peer && probe['channel'] === label && probe['seq'] === 0;
}

/** Logical exchange debt survives transport teardown; receipts belong to an incarnation. */
export class ExchangeLedger {
  private readonly obligations = new Set<string>();
  private readonly receiptOwners = new Map<string, object>();
  private readonly sent = new Map<string, Set<string>>();
  private readonly received = new Map<string, Set<string>>();

  noteConnected(peer: string): void {
    this.obligations.add(peer);
  }

  /** Pending receipts survive physical loss, but never an authoritative replacement. */
  receiptOwner(peer: string): object {
    let owner = this.receiptOwners.get(peer);
    if (owner === undefined) {
      owner = {};
      this.receiptOwners.set(peer, owner);
    }
    return owner;
  }

  ownsReceipt(peer: string, owner: object): boolean {
    return this.receiptOwners.get(peer) === owner;
  }

  invalidatePendingReceipts(peer: string): void {
    this.receiptOwners.delete(peer);
  }

  /** Keep the obligation, but require fresh traffic from a restored player. */
  resetIncarnation(peer: string): void {
    this.invalidatePendingReceipts(peer);
    this.sent.delete(peer);
    this.received.delete(peer);
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

  unpaidPeers(): string[] {
    return [...this.obligations].filter((peer) => {
      const debt = this.diagnosticSnapshot(peer);
      return debt.missing_sent.length !== 0 || debt.missing_received.length !== 0;
    });
  }

  diagnosticSnapshot(peer: string): { missing_sent: string[]; missing_received: string[] } {
    return {
      missing_sent: [RELIABLE_LABEL, UNRELIABLE_LABEL].filter(
        (label) => !this.sent.get(peer)?.has(label),
      ),
      missing_received: [RELIABLE_LABEL, UNRELIABLE_LABEL].filter(
        (label) => !this.received.get(peer)?.has(label),
      ),
    };
  }

  unmetCriteria(): string[] {
    const unmet: string[] = [];
    for (const peer of this.obligations) {
      const directions: Array<[string, ReadonlySet<string> | undefined]> = [
        ['sent to', this.sent.get(peer)],
        ['received from', this.received.get(peer)],
      ];
      for (const [direction, labels] of directions) {
        const missing = [RELIABLE_LABEL, UNRELIABLE_LABEL].filter(
          (label) => labels?.has(label) !== true,
        );
        if (missing.length !== 0) {
          unmet.push(
            `exchange incomplete (${direction} ${peer}; missing ${missing.join(', ')})`,
          );
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
   * a ready player becoming unready, or an authoritative `GAME_START_NOT_READY`). */
  private sentSinceInvalidation = false;

  constructor(readyPlayers: readonly string[]) {
    this.readyPlayers = new Set(readyPlayers);
  }

  /** Replace the authoritative readiness set. Losing a ready player re-arms
   * the latch so readiness recovery can retry before a delayed rejection.
   * Unchanged snapshots and readiness additions preserve the latch. */
  snapshot(readyPlayers: readonly string[]): void {
    const nextReadyPlayers = new Set(readyPlayers);
    for (const player of this.readyPlayers) {
      if (!nextReadyPlayers.has(player)) {
        this.sentSinceInvalidation = false;
        break;
      }
    }
    this.readyPlayers.clear();
    for (const player of nextReadyPlayers) {
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
  private readonly output = new EventOutput(
    (line, terminal) => {
      const bridge = window.__sf_emit;
      if (bridge === undefined) throw new Error('stdout bridge is unavailable');
      return bridge(line, terminal);
    },
    (error) =>
      this.failFatal(
        FatalError.connection(`stdout event delivery failed: ${error.message.slice(0, 512)}`),
      ),
  );

  private async emit(event: Record<string, unknown>, requireDelivery = false): Promise<void> {
    if (this.finished) {
      if (requireDelivery) throw FatalError.connection('the client is stopping');
      return;
    }
    try {
      await this.output.emit(event);
    } catch {
      if (requireDelivery) throw FatalError.connection('stdout event delivery failed');
    }
  }
  /** Stop a suspended handler before it mutates a retired session. */
  private assertActive(): void {
    if (this.handshakeDeadline !== null && monotonicNowMs() >= this.handshakeDeadline) {
      this.failFatal(FatalError.connection('server handshake response timed out'));
    }
    if (this.finished || this.handshakeFatal !== null) {
      throw this.handshakeFatal ?? FatalError.connection('the client is stopping');
    }
  }

  private readonly engine: Engine;
  private ws: WebSocket | null = null;
  private myId = '';

  // --- Handshake frame queue (sequential pull until the room is joined) ---
  private streaming = false;
  private readonly bufferedFrames: RetainedServerFrame[] = [];
  private readonly serverBacklog = new Backlog(
    MAX_PENDING_SERVER_FRAMES,
    MAX_PENDING_SERVER_FRAME_BYTES,
  );
  private currentHandshakeRelease: (() => void) | null = null;
  private readonly retainedHandshakeReleases: Array<() => void> = [];
  private frameWaiter: {
    resolve: (frame: RetainedServerFrame) => void;
    reject: (error: FatalError) => void;
  } | null = null;
  /** Fatal handshake failure observed while no frame pull was pending. */
  private handshakeFatal: FatalError | null = null;
  private handshakeDeadline: number | null = null;
  private readonly handshakeFailed = new AbortController();

  // --- Serialized input processing (the "orchestrator task") ---
  private chain: Promise<void> = Promise.resolve();
  private readonly taskBacklog = new Backlog(512, MAX_PENDING_SERVER_FRAME_BYTES);
  private pendingChannelMessages = 0;
  private pendingChannelMessageBytes = 0;
  private finishResolve: ((code: number) => void) | null = null;
  private finished = false;
  private readonly peerWorkStopped = new AbortController();

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
  private readonly pairRoles = new Map<string, boolean>();
  private readonly peerTasks = new Map<string, { tail: Promise<void>; token: object }>();
  private readonly pendingPeerOperations = new Set<{ deadline: number; expire: () => void }>();
  private readonly pairRetryState = new Map<string, { attempt: number; connected: boolean }>();
  private readonly retryingPairs = new Set<string>();
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
  private readonly exchangeSentChannels = new WeakSet<RTCDataChannel>();
  private readonly pendingExchangePeers = new Set<string>();
  private pendingExchangeReceipts = 0;
  private readonly admittedRelayReceipts = new WeakSet<ServerFrame>();
  private pendingRelayReceipts = 0;
  private readonly admittedServerFrames = new WeakSet<ServerFrame>();
  private pendingAdmittedServerFrames = 0;
  private pendingAdmittedTasks = 0;
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
  /**
   * The game-data encoding the session actually runs on. Starts at the
   * requested `config.gameDataFormat` and moves to JSON when the server's
   * pinned downgrade notice arrives (`advanceAuthenticateHandshake`); every
   * wire-shape decision after the handshake reads this, never the request.
   */
  private effectiveFormat: GameDataFormat;

  constructor(config: RunConfig) {
    this.config = config;
    this.effectiveFormat = config.gameDataFormat;
    this.engine = new Engine(config.crippleIce, {
      onLocalCandidate: (peer, generation, candidateJson) => {
        // Crippled mode never reaches here (the engine drops gathered
        // candidates), so this is always a real trickle-ICE relay.
        this.enqueue(
          async () => {
            if (!this.engine.isCurrentGeneration(peer, generation)) {
              return;
            }
            this.sendSignal(peer, 'ice_candidate', { IceCandidate: candidateJson });
          },
          undefined,
          utf8Bytes(candidateJson),
        );
      },
      onPcState: (peer, generation, state) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          this.emit({ event: 'pc_state', peer, state });
          if (isTerminalPeerConnectionState(state)) {
            this.handlePeerTransportLoss(peer, `connection state ${state}`);
          }
        });
      },
      onChannelOpen: (peer, generation, label) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          this.emit({ event: 'channel_open', peer, label });
          if (this.engine.noteChannelOpen(peer, label)) {
            this.onPairConnected(peer);
          }
        });
      },
      onChannelClosed: (peer, generation, label, reason) => {
        this.enqueue(async () => {
          if (!this.engine.isCurrentGeneration(peer, generation)) {
            return;
          }
          if (reason !== undefined) this.emit({ event: 'error', message: reason });
          this.emit({ event: 'channel_closed', peer, label });
          this.handlePeerTransportLoss(peer, `required channel ${label} closed or failed`);
        });
      },
      onChannelMessage: (peer, generation, label, text, textBytes, arrivedAt) => {
        if (this.finished || !this.engine.isCurrentGeneration(peer, generation)) return;
        // Reserve before enqueue retains the message closure.
        if (
          this.pendingChannelMessages >= MAX_PENDING_CHANNEL_MESSAGES ||
          textBytes > MAX_PENDING_CHANNEL_MESSAGE_BYTES - this.pendingChannelMessageBytes
        )
          return false;
        // Arrival owns the receipt window, even if output delays its handler.
        const exchangeReceipt =
          this.config.exchange &&
          this.exchangeWindowOpen(arrivedAt) &&
          isExchangeProbe(peer, label, text);
        const receiptOwner = exchangeReceipt ? this.exchangeLedger.receiptOwner(peer) : null;
        let receiptPending = exchangeReceipt;
        if (receiptPending) this.pendingExchangeReceipts += 1;
        this.pendingChannelMessages += 1;
        this.pendingChannelMessageBytes += textBytes;
        this.enqueue(
          async () => {
            if (receiptPending) {
              receiptPending = false;
              this.pendingExchangeReceipts -= 1;
            }
            /* Physical timeout preserves evidence admitted for the current membership. */
            if (
              receiptOwner !== null
                ? !this.exchangeLedger.ownsReceipt(peer, receiptOwner)
                : !this.engine.isCurrentGeneration(peer, generation)
            ) {
              return;
            }
            if (exchangeReceipt) {
              this.exchangeLedger.noteReceived(peer, label);
            }
            await this.emit({ event: 'channel_message', peer, label, text }, true);
          },
          () => {
            if (receiptPending) this.pendingExchangeReceipts -= 1;
            this.pendingChannelMessages -= 1;
            this.pendingChannelMessageBytes -= textBytes;
          },
        );
        return true;
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
    try {
      return await this.runSession();
    } catch (error) {
      const fatal = error instanceof FatalError ? error : FatalError.protocol(describe(error));
      void this.output
        .terminal({ event: 'error', message: fatal.message.slice(0, 512) })
        .catch(() => {});
      console.error(`fatal failure (code ${fatal.code}): ${fatal.message}`);
      return fatal.code;
    } finally {
      this.teardown();
    }
  }

  private async runSession(): Promise<number> {
    // The soft run window starts at PROCESS start: the CLI reports the time
    // already spent launching Chromium so the window matches the native
    // client's process-start semantics.
    const now = monotonicNowMs();
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
    this.emit({ event: 'connected' });
    ws.onmessage = (event: MessageEvent<unknown>) => {
      this.onServerInput(event.data);
    };
    ws.onclose = () => {
      this.onSocketClosed();
    };

    const negotiatedVersion = await this.handshake(() => this.authenticate());
    this.negotiatedVersion = negotiatedVersion;
    this.accountability = new DeliveryAccountability(negotiatedVersion >= 3);
    const lobbyState = await this.handshake(() => this.joinRoom());

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
      this.relaySendAt = monotonicNowMs() + RELAY_SEND_SETTLE_MS;
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
      // Opaque game-data encodings are an explicit opt-in (#627).
      game_data_format: this.config.gameDataFormat,
    };
    if (v3) {
      // v2 mode omits every v3 field so the wire shape is pure v2.
      data['protocol_version'] = this.config.protocolVersion;
      data['supported_transports'] = this.config.supportedTransports;
      data['supported_topologies'] = this.config.supportedTopologies;
    }
    this.sendFrame(clientFrame('Authenticate', data));

    // The server may answer the requested `game_data_format` with its pinned
    // downgrade notice before `Authenticated`; adopt JSON and continue.
    let effectiveFormat: GameDataFormat = this.config.gameDataFormat;
    for (;;) {
      const step = advanceAuthenticateHandshake(
        await this.nextHandshakeFrame(),
        effectiveFormat,
      );
      if ('fatal' in step) {
        throw FatalError.protocol(step.fatal);
      }
      effectiveFormat = step.effectiveFormat;
      if (step.authenticated) {
        break;
      }
      this.emit({
        event: 'error',
        message: 'server refused the requested game data format; session downgraded to JSON',
      });
    }
    this.emit({ event: 'authenticated' });
    this.effectiveFormat = effectiveFormat;

    const infoResponse = await this.nextHandshakeFrame();
    let negotiated: number;
    try {
      negotiated = negotiatedProtocolVersion(infoResponse, this.config.protocolVersion);
    } catch (error) {
      throw FatalError.protocol(describe(error));
    }
    // An opaque request must appear in ProtocolInfo.game_data_formats before
    // any opaque byte goes on the wire (#627): a knob-off deployment
    // advertises exactly ["json"] and silently downgraded the request.
    if (effectiveFormat !== 'json') {
      try {
        negotiatedGameDataFormat(infoResponse, effectiveFormat);
      } catch (error) {
        throw FatalError.protocol(describe(error));
      }
    }
    this.emit({
      event: 'protocol_info',
      negotiated_version: negotiated,
    });
    return negotiated;
  }

  /** Create or join the room; returns the room's lobby state at join time. */
  private async joinRoom(): Promise<string> {
    this.sendFrame(clientFrame('JoinRoom', joinRoomFrameData(this.config)));

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
        throw FatalError.protocol('expected RoomJoined');
      }
      response = await this.nextHandshakeFrame(true);
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
      this.emit({ event: 'room_created', room_code: String(payload['room_code']) });
    }
    const lobbyState = String(payload['lobby_state']);
    this.myId = String(payload['player_id']);
    this.emit({
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

  /** One deadline covers a command's replies and output, including all preludes. */
  private async handshake<T>(operation: () => Promise<T>): Promise<T> {
    const deadline = monotonicNowMs() + HANDSHAKE_TIMEOUT_MS;
    this.handshakeDeadline = deadline;
    const timer = scheduleDeadline(deadline, () => {
      this.failFatal(FatalError.connection('server handshake response timed out'));
    });
    try {
      const result = await operation();
      await this.flushHandshakeOutput();
      this.assertActive();
      return result;
    } finally {
      timer.cancel();
      this.handshakeDeadline = null;
    }
  }

  /** Pull the next server frame under the current command's deadline. */
  private async nextHandshakeFrame(retainPrevious = false): Promise<ServerFrame> {
    await this.flushHandshakeOutput();
    if (this.currentHandshakeRelease !== null) {
      if (retainPrevious) this.retainedHandshakeReleases.push(this.currentHandshakeRelease);
      else this.currentHandshakeRelease();
      this.currentHandshakeRelease = null;
    }
    this.assertActive();
    const buffered = this.bufferedFrames.shift();
    const retained =
      buffered ??
      (await new Promise<RetainedServerFrame>((resolve, reject) => {
        this.frameWaiter = {
          resolve: (frame) => {
            this.frameWaiter = null;
            resolve(frame);
          },
          reject: (error) => {
            this.frameWaiter = null;
            reject(error);
          },
        };
      }));
    this.currentHandshakeRelease = retained.release;
    this.assertActive();
    return retained.frame;
  }

  private async flushHandshakeOutput(): Promise<void> {
    if (this.handshakeFatal !== null) throw this.handshakeFatal;
    let rejectFailure: ((reason: unknown) => void) | undefined;
    const failure = new Promise<never>((_resolve, reject) => {
      rejectFailure = reject;
    });
    const abort = () => rejectFailure?.(this.handshakeFatal);
    this.handshakeFailed.signal.addEventListener('abort', abort, { once: true });
    try {
      await Promise.race([this.output.flush(), failure]);
    } finally {
      this.handshakeFailed.signal.removeEventListener('abort', abort);
    }
  }

  private releaseHandshakeFrames(): void {
    this.currentHandshakeRelease?.();
    this.currentHandshakeRelease = null;
    for (const release of this.retainedHandshakeReleases.splice(0)) release();
  }

  /** Transfer the charged handshake backlog to streaming without renewing credits. */
  private startStreaming(): void {
    this.releaseHandshakeFrames();
    this.streaming = true;
    for (const { frame, release } of this.bufferedFrames.splice(0)) {
      this.enqueue(() => this.handleServerMessage(frame), release);
    }
    if (this.handshakeFatal !== null) this.fail(this.handshakeFatal);
  }

  private onServerInput(data: unknown): void {
    // Arrival owns relay eligibility even when decoding or output runs late.
    const relayWindowOpen = this.exchangeWindowOpen();
    if (this.finished || this.handshakeFatal !== null) return;
    const wireBytes =
      typeof data === 'string'
        ? utf8Bytes(data)
        : data instanceof ArrayBuffer || ArrayBuffer.isView(data)
          ? data.byteLength
          : 0;
    if (wireBytes > MAX_SERVER_FRAME_BYTES) {
      this.failFatal(FatalError.connection('WebSocket receive frame exceeds 64 MiB'));
      return;
    }
    const retainedBytes = ArrayBuffer.isView(data) ? data.buffer.byteLength : wireBytes;
    const release = this.serverBacklog.reserve(retainedBytes);
    if (release === null) {
      this.failFatal(FatalError.connection('WebSocket receive backlog limit exceeded'));
      return;
    }
    let frame: ServerFrame;
    try {
      frame =
        this.effectiveFormat === 'json'
          ? classifyJsonNegotiatedServerInput(data)
          : this.effectiveFormat === 'message_pack'
            ? classifyMessagePackNegotiatedServerInput(
                data,
                this.negotiatedVersion ?? this.config.protocolVersion,
              )
            : classifyOpaqueNegotiatedServerInput(
                data,
                this.negotiatedVersion ?? this.config.protocolVersion,
                this.effectiveFormat,
              );
    } catch (error) {
      release();
      this.failFatal(FatalError.protocol(`invalid ServerMessage frame: ${describe(error)}`));
      return;
    }
    // Valid ingress proves server liveness before stdout can delay dispatch.
    // A queued old frame must never acknowledge a Ping sent after its arrival.
    this.pongDeadline = null;
    this.pongGraceApplied = false;
    const payload = frame.data['data'];
    const isRelayReceipt =
      frame.type === 'GameDataBinary' ||
      (frame.type === 'GameData' &&
        typeof payload === 'object' &&
        payload !== null &&
        'relay_msg' in payload);
    if (relayWindowOpen && this.config.relayPayload !== null && isRelayReceipt) {
      this.admittedRelayReceipts.add(frame);
      this.pendingRelayReceipts += 1;
    }
    if (relayWindowOpen) {
      this.admittedServerFrames.add(frame);
      this.pendingAdmittedServerFrames += 1;
    }
    this.routeServerFrame({
      frame,
      release: () => {
        if (this.admittedRelayReceipts.delete(frame)) this.pendingRelayReceipts -= 1;
        if (this.admittedServerFrames.delete(frame)) this.pendingAdmittedServerFrames -= 1;
        release();
      },
    });
  }

  private routeServerFrame(retained: RetainedServerFrame): void {
    if (this.streaming) {
      this.enqueue(() => this.handleServerMessage(retained.frame), retained.release);
    } else if (this.frameWaiter !== null) {
      this.frameWaiter.resolve(retained);
    } else {
      this.bufferedFrames.push(retained);
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
    if (this.handshakeFatal === null) {
      this.handshakeFatal = fatal;
      this.handshakeFailed.abort(fatal);
    }
    this.frameWaiter?.reject(this.handshakeFatal);
    for (const retained of this.bufferedFrames.splice(0)) retained.release();
  }

  // -------------------------------------------------------------------------
  // Serialized input processing + timers
  // -------------------------------------------------------------------------

  /**
   * Append one input handler to the processing chain. After it settles, due
   * timers run and the next wake is scheduled (the native loop's
   * `process_timers` + `next_wake` pattern).
   */
  private enqueue(task: () => Promise<void>, release?: () => void, retainedBytes = 0): void {
    if (this.finished) {
      release?.();
      return;
    }
    const releaseTask = this.taskBacklog.reserve(retainedBytes);
    if (releaseTask === null) {
      release?.();
      this.failFatal(FatalError.connection('browser input task backlog limit exceeded'));
      return;
    }
    let admittedTask = this.exchangeWindowOpen();
    if (admittedTask) this.pendingAdmittedTasks += 1;
    const consumeAdmission = () => {
      if (!admittedTask) return;
      admittedTask = false;
      this.pendingAdmittedTasks -= 1;
    };
    this.chain = this.chain
      .then(async () => {
        try {
          if (this.finished) {
            return;
          }
          consumeAdmission();
          await task();
          await this.output.flush();
          if (this.finished) return;
          this.processTimers();
          this.scheduleWake();
        } finally {
          consumeAdmission();
          release?.();
          releaseTask();
        }
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
    void this.output
      .terminal({ event: 'error', message: fatal.message.slice(0, 512) })
      .catch(() => {});
    console.error(`fatal failure (code ${fatal.code}): ${fatal.message}`);
    this.finish(fatal.code);
  }

  private finish(code: number): void {
    if (this.finished) {
      return;
    }
    this.finished = true;
    this.peerWorkStopped.abort();
    if (this.wakeTimer !== null) {
      this.wakeTimer.cancel();
      this.wakeTimer = null;
    }
    if (code === EXIT_SUCCESS) {
      void this.output.flush().then(
        () => this.finishResolve?.(code),
        () => this.finishResolve?.(EXIT_CONNECTION_ERROR),
      );
    } else this.finishResolve?.(code);
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
    const now = monotonicNowMs();
    /*
      Arrived server frames and engine callbacks can change lifecycle debt.
      Late traffic holds no marker, so it cannot keep an expired run alive.
    */
    const terminalDecisionDue =
      now >= this.runDeadline || (this.lingerUntil !== null && now >= this.lingerUntil);
    if (
      terminalDecisionDue &&
      (this.pendingAdmittedTasks > 0 ||
        this.pendingAdmittedServerFrames > 0 ||
        this.pendingExchangeReceipts + this.pendingRelayReceipts > 0)
    )
      return;

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
      for (const peer of this.expectedPeers) {
        if (!this.connectedPairs.has(peer)) {
          console.error(
            `P2P timeout for ${peer}: ${JSON.stringify(this.engine.diagnosticSnapshot(peer))}`,
          );
        }
      }
      // The current P2P window expired: report any real state change.
      this.resolveTransportStatus();
      this.p2pDeadline = null;
      this.retryingPairs.clear();
    }

    if (this.successReleasePollAt !== null && now >= this.successReleasePollAt) {
      this.pollSuccessRelease();
    }

    /*
      Peer-operation timers share the run cutoff with the room wake. Retire
      their physical work first even when the room wake runs before them.
      Logical retry and exchange obligations remain part of the exit decision.
    */
    if (
      now >= this.runDeadline &&
      !shouldDeferSuccessAtRunDeadline(
        this.config.successReleaseEnabled,
        this.successCriteriaReported,
        this.successReleaseGranted,
        this.lingerUntil,
      )
    ) {
      for (const pending of Array.from(this.pendingPeerOperations)) {
        if (pending.deadline <= now) pending.expire();
      }
      /* Queued or finishing handlers carry no physical timeout of their own.
       * Cancel their tokens without publishing a transport-loss decision. */
      this.peerTasks.clear();
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
      if (this.config.exchange) {
        for (const peer of this.exchangeLedger.unpaidPeers()) {
          const snapshot = {
            ...this.exchangeLedger.diagnosticSnapshot(peer),
            connected: this.connectedPairs.has(peer),
            link: this.engine.diagnosticSnapshot(peer),
          };
          console.error(`Exchange timeout for ${peer}: ${JSON.stringify(snapshot)}`);
        }
      }
      this.emit({
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
      this.emit({ event: 'success_criteria_met' });
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
            this.successReleasePollAt = monotonicNowMs() + 100;
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
    if (this.admittedServerFrames.delete(frame)) this.pendingAdmittedServerFrames -= 1;
    const admittedRelayReceipt = this.admittedRelayReceipts.delete(frame);
    if (admittedRelayReceipt) this.pendingRelayReceipts -= 1;
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
        const freshIncarnation = accountability.notePlayerJoined(player);
        if ((this.negotiatedVersion ?? 2) >= 3 && !freshIncarnation) {
          break;
        }
        const id = String(player['id']);
        const returning = this.membersSeen.has(id) && !this.present.has(id);
        if ((this.negotiatedVersion ?? 2) >= 3 || returning) {
          this.resetPeerIncarnation(id);
        }
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
        this.emit({ event: 'peer_joined', player_id: id });
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
        this.emit({ event: 'player_left', player_id: id });
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
        this.emit({ event: 'game_starting', is_authority: mine?.['is_authority'] === true });
        if (this.config.relayPayload !== null && !this.relaySent) {
          this.relaySendAt = monotonicNowMs() + RELAY_SEND_SETTLE_MS;
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
        this.emit({
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
          rejectUnsupportedDirectPlan(
            data['direct_endpoint'],
            (frame) => this.sendFrame(frame),
            (event) => {
              void this.emit(event);
            },
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
        const planDeadline = deadlineAfterSeconds(monotonicNowMs(), this.config.p2pTimeoutSecs);
        for (const peer of peers) {
          this.assertActive();
          const peerId = String(peer['player_id']);
          if (transport === 'webrtc' && added.delete(peerId)) {
            this.schedulePeerTask(peerId, () =>
              this.establishPair(peerId, peer['initiate'] === true, planDeadline),
            );
            this.assertActive();
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
        this.emit({ event: 'new_peer', peer_id: peerId, you_initiate: youInitiate });
        // Same pairing path as a plan peer (the late-join offerer rule).
        this.schedulePeerTask(peerId, () => this.establishPair(peerId, youInitiate));
        break;
      }
      case 'Signal': {
        const peer = String(data['from']);
        const generation = parseSessionGeneration(data['generation']) ?? '';
        // A retry starts a new physical link. It cannot wait behind the old
        // link's stalled SDP/ICE, whose timeout would discard the queued retry.
        if (this.canStartPairRetry(peer, generation, data['signal'])) {
          this.peerTasks.delete(peer);
        }
        this.schedulePeerTask(
          peer,
          () => this.handleSignal(peer, generation, data['signal']),
          utf8Bytes(JSON.stringify(data)),
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
          if (admittedRelayReceipt) this.relayReceivedFrom.add(from);
        }
        this.emit({ event: 'game_data_received', from, payload });
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
        const bytes = payload instanceof Uint8Array ? payload : new Uint8Array(0);
        console.debug(
          `received accountable opaque binary GameData from ${from} ` +
            `(${String(data['encoding'])}, ${bytes.byteLength} bytes)`,
        );
        // Opaque negotiation carries the relay floor's payloads as binary
        // frames, so every decoded frame from a peer satisfies the
        // relay-received criterion exactly like the text `relay_msg` arm
        // (#627); the opaque payload cannot be inspected for that key.
        if (admittedRelayReceipt) this.relayReceivedFrom.add(from);
        // Event-stream parity with the native client (#627): surface the
        // receipt losslessly (encoding token + base64 payload) instead of
        // guessing at the opaque bytes.
        this.emit({
          event: 'game_data_received',
          from,
          payload: { encoding: String(data['encoding']), payload: bytesToBase64(bytes) },
        });
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
        const freshIncarnation = accountability.notePlayerReconnected(rawId, data['epoch']);
        if ((this.negotiatedVersion ?? 2) >= 3 && !freshIncarnation) {
          break;
        }
        const id = String(rawId);
        restoreReconnectedMember(this.present, this.membersSeen, id);
        this.resetPeerIncarnation(id);
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
        this.emit({ event: 'peer_joined', player_id: id });
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
        this.emit({
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
          this.emit({
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
        this.emit({
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
        // Liveness was recorded at ingress, before queued output.
        break;
      default:
        console.error(
          frame.type === NON_TEXT_APPLICATION_FRAME
            ? 'ignoring non-text websocket application frame'
            : 'ignoring unrecognized server message',
        );
        break;
    }
  }

  // -------------------------------------------------------------------------
  // P2P pairing + signaling (mirrors the native establish/handle/apply trio)
  // -------------------------------------------------------------------------

  private canStartPairRetry(peer: string, generation: string, signal: unknown): boolean {
    if (
      this.pendingMembershipPlans.has(peer) ||
      !isCurrentSessionGeneration(this.currentSessionGeneration, generation) ||
      classifySignal(signal) !== 'pair_retry' ||
      !this.expectedPeers.has(peer) ||
      !this.pairRoles.has(peer)
    )
      return false;
    const attempt = (signal as Record<string, unknown>)['PairRetry'];
    const previous = this.pairRetryState.get(peer);
    return (
      typeof attempt === 'number' &&
      Number.isInteger(attempt) &&
      attempt >= 1 &&
      attempt <= this.config.p2pRetryCount &&
      attempt > (previous?.attempt ?? 0) &&
      ((this.p2pDeadline !== null && monotonicNowMs() < this.p2pDeadline) ||
        previous?.connected === true)
    );
  }

  /** Keep each peer's SDP/ICE ordered without holding the room input chain. */
  private schedulePeerTask(peer: string, task: () => Promise<void>, retainedBytes = 0): void {
    const release = this.taskBacklog.reserve(retainedBytes);
    if (release === null) {
      this.failFatal(FatalError.connection('browser input task backlog limit exceeded'));
      return;
    }
    const generation = this.currentSessionGeneration;
    const previous = this.peerTasks.get(peer);
    const token = previous?.token ?? {};
    const slot = { tail: Promise.resolve(), token };
    this.peerTasks.set(peer, slot);
    let operation: Promise<void>;
    const execute = async () => {
      if (this.finished || this.currentSessionGeneration !== generation) return;
      // A queued task may outlive a departure or replacement in the same plan.
      if (this.peerTasks.get(peer)?.token !== token) return;
      await task();
    };
    operation = (previous === undefined ? execute() : previous.tail.then(execute)).finally(
      release,
    );
    slot.tail = operation;
    void operation.then(
      () => {
        if (this.peerTasks.get(peer)?.tail === operation) this.peerTasks.delete(peer);
        this.enqueue(async () => {});
      },
      (error: unknown) => {
        this.failFatal(
          error instanceof FatalError
            ? error
            : FatalError.protocol(`peer operation failed: ${describe(error)}`),
        );
      },
    );
  }

  /**
   * Pair with `peer` per the server's directive: create the connection, offer
   * when told to, then drain any defensively buffered signals.
   */
  private async establishPair(
    peer: string,
    initiate: boolean,
    retainedDeadline: number | null = null,
  ): Promise<void> {
    this.assertActive();
    const sessionGeneration = this.currentSessionGeneration;
    const taskToken = this.peerTasks.get(peer)?.token;
    if (this.pendingMembershipPlans.has(peer)) return;
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
    this.pairRoles.set(peer, initiate);
    if ((newlyExpected || needsConnection) && !this.connectedPairs.has(peer)) {
      this.p2pDeadline =
        retainedDeadline ?? deadlineAfterSeconds(monotonicNowMs(), this.config.p2pTimeoutSecs);
    }
    if (
      this.config.p2pTimeoutSecs !== 0 &&
      retainedDeadline !== null &&
      monotonicNowMs() >= retainedDeadline
    ) {
      return;
    }
    try {
      const offerSdp = await this.awaitPeerOperation(
        peer,
        this.engine.pairWith(peer, initiate, this.lastIceServers),
      );
      this.assertActive();
      if (
        this.currentSessionGeneration !== sessionGeneration ||
        this.peerTasks.get(peer)?.token !== taskToken
      )
        return;
      if (offerSdp !== null) {
        this.sendSignal(peer, 'offer', { Offer: offerSdp });
      }
    } catch (error) {
      if (error instanceof FatalError && error.code === EXIT_CONNECTION_ERROR) throw error;
      this.assertActive();
      if (this.peerTasks.get(peer)?.token !== taskToken) return;
      // A single failed pair is not fatal: the p2p timeout resolves the
      // overall status and the relay floor still carries data.
      this.emit({ event: 'error', message: `pairing with ${peer} failed: ${describe(error)}` });
    }
    if (
      this.currentSessionGeneration !== sessionGeneration ||
      this.peerTasks.get(peer)?.token !== taskToken ||
      !this.expectedPeers.has(peer)
    )
      return;
    const buffered = this.pendingSignals.get(peer);
    if (buffered !== undefined) {
      this.pendingSignals.delete(peer);
      for (const signal of buffered) {
        this.assertActive();
        if (
          this.currentSessionGeneration !== sessionGeneration ||
          this.peerTasks.get(peer)?.token !== taskToken ||
          !this.expectedPeers.has(peer)
        )
          return;
        await this.applySignal(peer, signal);
        this.assertActive();
      }
    }
  }

  private async awaitPeerOperation<T>(peer: string, operation: Promise<T>): Promise<T> {
    this.assertActive();
    const generation = this.engine.generationOf(peer);
    const isCurrent = () =>
      generation !== undefined && this.engine.isCurrentGeneration(peer, generation);
    const runDeadline = shouldDeferSuccessAtRunDeadline(
      this.config.successReleaseEnabled,
      this.successCriteriaReported,
      this.successReleaseGranted,
      this.lingerUntil,
    )
      ? Number.POSITIVE_INFINITY
      : this.runDeadline;
    const operationDeadline =
      this.config.p2pTimeoutSecs === 0
        ? deadlineAfterSeconds(monotonicNowMs(), this.config.runForSecs)
        : ((this.connectedPairs.has(peer) ? null : this.p2pDeadline) ??
          deadlineAfterSeconds(monotonicNowMs(), this.config.p2pTimeoutSecs));
    const deadline = Math.min(operationDeadline, runDeadline);
    let rejectWait: ((reason: Error) => void) | undefined;
    const expired = new Promise<never>((_resolve, reject) => {
      rejectWait = reject;
    });
    const stopped = () => rejectWait?.(FatalError.connection('the client is stopping'));
    this.peerWorkStopped.signal.addEventListener('abort', stopped, { once: true });
    const expire = () => {
      try {
        if (isCurrent()) this.handlePeerTransportLoss(peer, 'negotiation deadline expired');
      } catch (error) {
        this.failFatal(
          error instanceof FatalError
            ? error
            : FatalError.connection(`peer teardown failed: ${describe(error)}`),
        );
      } finally {
        rejectWait?.(new Error('browser peer negotiation deadline expired'));
      }
    };
    const pending = { deadline, expire };
    this.pendingPeerOperations.add(pending);
    const timer = scheduleDeadline(deadline, expire);
    try {
      const result = await Promise.race([operation, expired]);
      if (monotonicNowMs() >= deadline) {
        throw new Error('browser peer negotiation deadline expired');
      }
      if (!isCurrent()) throw new Error('the peer link was retired');
      return result;
    } catch (error) {
      if (monotonicNowMs() >= deadline && isCurrent()) {
        this.handlePeerTransportLoss(peer, 'negotiation deadline expired');
      }
      throw error;
    } finally {
      this.pendingPeerOperations.delete(pending);
      timer.cancel();
      this.peerWorkStopped.signal.removeEventListener('abort', stopped);
    }
  }

  private resetPeerIncarnation(peer: string): void {
    this.exchangeLedger.resetIncarnation(peer);
    if (this.expectedPeers.has(peer)) this.prepareRetainedPairReplacement(peer);
  }

  private removePairObligation(peer: string): void {
    this.exchangeLedger.invalidatePendingReceipts(peer);
    this.peerTasks.delete(peer);
    this.expectedPeers.delete(peer);
    this.pairRoles.delete(peer);
    this.pairRetryState.delete(peer);
    this.retryingPairs.delete(peer);
    this.connectedPairs.delete(peer);
    this.pairConnectedReported.delete(peer);
    this.peerStatusFrom.delete(peer);
    this.pendingExchangePeers.delete(peer);
    this.reportedExchangeDiagnostics.delete(peer);
    this.pendingSignals.delete(peer);
    this.engine.removePeer(peer);
  }

  /** Retire physical state without publishing an interim transport decision. */
  private retirePeerLink(peer: string, invalidateTasks = true): void {
    if (invalidateTasks) this.peerTasks.delete(peer);
    this.connectedPairs.delete(peer);
    this.pendingExchangePeers.delete(peer);
    this.reportedExchangeDiagnostics.delete(peer);
    this.pendingSignals.delete(peer);
    this.engine.removePeer(peer);
  }

  /** Tear down an unusable P2P link while retaining its planned obligation. */
  private handlePeerTransportLoss(peer: string, reason: string): void {
    const link = this.engine.diagnosticSnapshot(peer);
    if (link !== null) {
      console.error(`P2P link lost for ${peer}: ${JSON.stringify({ reason, link })}`);
    }
    this.retirePeerLink(peer);
    if (this.webrtcPlanSeen) {
      this.resolveTransportStatus();
    }
  }

  /** Retire one retained physical link for a newer authoritative generation. */
  private prepareRetainedPairReplacement(peer: string): void {
    this.exchangeLedger.invalidatePendingReceipts(peer);
    this.pairRetryState.delete(peer);
    this.retryingPairs.add(peer);
    this.retirePeerLink(peer);
  }

  /**
   * Emit `signal_received` and route an inbound signal (buffering it when the
   * peer is not paired yet).
   */
  private async handleSignal(from: string, generation: string, signal: unknown): Promise<void> {
    if (this.pendingMembershipPlans.has(from)) return;
    if (!isCurrentSessionGeneration(this.currentSessionGeneration, generation)) {
      console.debug(
        `discarding signal from ${from} for stale/unknown generation ${generation}`,
      );
      return;
    }
    signal = signalForRetryAttempt(signal, this.pairRetryState.get(from)?.attempt ?? 0);
    if (signal === undefined) return;
    const kind = classifySignal(signal);
    this.emit({ event: 'signal_received', from, kind });
    this.assertActive();
    if (kind === 'pair_retry') {
      await this.applySignal(from, signal);
      return;
    }
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
    this.assertActive();
    const taskToken = this.peerTasks.get(from)?.token;
    const sessionGeneration = this.currentSessionGeneration;
    const isCurrentTask = () =>
      this.currentSessionGeneration === sessionGeneration &&
      this.peerTasks.get(from)?.token === taskToken;
    const kind = classifySignal(signal);
    const payload = signal as Record<string, unknown>;
    try {
      switch (kind) {
        case 'offer': {
          const sdp = payload['Offer'];
          if (typeof sdp !== 'string') {
            throw new Error('Offer payload is not a string');
          }
          const answerSdp = await this.awaitPeerOperation(
            from,
            this.engine.handleOffer(from, sdp),
          );
          this.assertActive();
          if (!isCurrentTask()) return;
          this.sendSignal(from, 'answer', { Answer: answerSdp });
          break;
        }
        case 'answer': {
          const sdp = payload['Answer'];
          if (typeof sdp !== 'string') {
            throw new Error('Answer payload is not a string');
          }
          await this.awaitPeerOperation(from, this.engine.handleAnswer(from, sdp));
          this.assertActive();
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
          await this.awaitPeerOperation(
            from,
            this.engine.handleRemoteCandidate(from, candidate),
          );
          this.assertActive();
          break;
        }
        case 'pair_retry': {
          const attempt = payload['PairRetry'];
          // The reference-only extension requires explicit opt-in on both endpoints.
          if (
            typeof attempt !== 'number' ||
            !Number.isInteger(attempt) ||
            attempt < 1 ||
            attempt > this.config.p2pRetryCount
          ) {
            throw new Error('PairRetry attempt is outside the configured retry budget');
          }
          const initiate = this.pairRoles.get(from);
          if (!this.expectedPeers.has(from) || initiate === undefined) {
            throw new Error('PairRetry from an unplanned peer');
          }
          const previous = this.pairRetryState.get(from);
          if (!this.canStartPairRetry(from, this.currentSessionGeneration ?? '', signal)) break;
          const deadline = this.p2pDeadline;
          const windowOpen = deadline !== null && monotonicNowMs() < deadline;
          // A peer can close the old data channel before its marker reaches
          // the signaling socket. Remember successful formation in this generation.
          this.pairRetryState.set(from, {
            attempt,
            connected: previous?.connected ?? false,
          });
          this.retryingPairs.add(from);
          this.retirePeerLink(from, false);
          await this.establishPair(from, initiate, windowOpen ? deadline : null);
          break;
        }
        case 'other':
          throw new Error('signal does not match the matchbox PeerSignal convention');
      }
    } catch (error) {
      if (error instanceof FatalError && error.code === EXIT_CONNECTION_ERROR) throw error;
      this.assertActive();
      if (this.peerTasks.get(from)?.token !== taskToken) return;
      this.emit({ event: 'error', message: `signal from ${from} failed: ${describe(error)}` });
    }
  }

  /**
   * Both channels toward `peer` are open: emit the pair event, run the
   * optional exchange, and check the all-pairs resolution condition.
   */
  private onPairConnected(peer: string): void {
    this.assertActive();
    this.pairRetryState.set(peer, {
      attempt: this.pairRetryState.get(peer)?.attempt ?? 0,
      connected: true,
    });
    // The engine reports formation once per physical link. Logical formation
    // survives authoritative replacements and expired retry waits.
    this.retryingPairs.delete(peer);
    if (noteCurrentPairConnected(this.connectedPairs, this.pairConnectedReported, peer)) {
      this.emit({ event: 'p2p_pair_connected', peer });
    } else {
      this.emit({ event: 'p2p_pair_reconnected', peer });
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

  private exchangeWindowOpen(now = monotonicNowMs()): boolean {
    return (
      now < this.runDeadline ||
      shouldDeferSuccessAtRunDeadline(
        this.config.successReleaseEnabled,
        this.successCriteriaReported,
        this.successReleaseGranted,
        this.lingerUntil !== null && now < this.lingerUntil ? this.lingerUntil : null,
      )
    );
  }

  private flushPendingExchangeSends(): void {
    for (const peer of Array.from(this.pendingExchangePeers)) {
      if (this.trySendExchange(peer)) {
        this.pendingExchangePeers.delete(peer);
      }
    }
    this.exchangeRetryAt =
      this.pendingExchangePeers.size === 0 ? null : monotonicNowMs() + EXCHANGE_RETRY_MS;
  }

  /**
   * Send each exchange probe once per physical channel. Logical evidence
   * survives rebuilds, but a successful old-link send may never reach the peer.
   *
   * Browser data-channel open callbacks and `send()` readiness can be slightly
   * reordered in Chromium under load. The pair remains valid; defer the send
   * until the channel itself reports `open` at the point of use.
   */
  private trySendExchange(peer: string): boolean {
    this.assertActive();
    let complete = true;
    for (const label of [RELIABLE_LABEL, UNRELIABLE_LABEL]) {
      this.assertActive();
      if (!this.exchangeWindowOpen()) {
        return false;
      }
      const channel = this.engine.channel(peer, label);
      if (channel === undefined) {
        const diagnosticKey = `missing\0${peer}\0${label}`;
        if (!this.reportedExchangeDiagnostics.has(diagnosticKey)) {
          this.reportedExchangeDiagnostics.add(diagnosticKey);
          this.emit({
            event: 'error',
            message: `open pair with ${peer} is missing channel ${label}`,
          });
        }
        complete = false;
        continue;
      }
      if (this.exchangeSentChannels.has(channel)) {
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
          this.emit({ event: 'error', message });
          console.error(message);
        }
        complete = false;
        continue;
      }
      this.exchangeSentChannels.add(channel);
      this.exchangeLedger.noteSent(peer, label);
      this.emit({ event: 'channel_message_sent', peer, label, text });
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
    this.emit({ event: 'transport_status_sent', transport: 'webrtc', connected });
    if (!connected) {
      this.emit({ event: 'fallback_engaged' });
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
   * join, a departure, readiness loss, or an authoritative `GAME_START_NOT_READY`
   * invalidates that latch (see `StartGameGate`), and the next recomputed
   * all-ready moment re-issues — the documented client contract
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
    if (!this.exchangeWindowOpen()) return;
    if (this.config.relayPayload === null) {
      this.relaySent = true;
      return;
    }
    let sent = false;
    const sendEncodedFrame = (frame: string | ArrayBuffer) => {
      if (!this.exchangeWindowOpen()) return;
      this.sendRawFrame(frame);
      sent = true;
    };
    if (this.effectiveFormat === 'json') {
      sendGameData(sendEncodedFrame, {
        relay_msg: this.config.relayPayload,
      });
    } else if (this.effectiveFormat === 'message_pack') {
      sendMessagePackGameData(sendEncodedFrame, {
        relay_msg: this.config.relayPayload,
      });
    } else {
      // Opaque negotiation relays the raw payload as one binary frame (#627).
      sendOpaqueGameData(
        sendEncodedFrame,
        encodeOpaqueRelayProbe(this.myId, new TextEncoder().encode(this.config.relayPayload)),
      );
    }
    if (!sent) return;
    this.relaySent = true;
    this.relaySendAt = null;
    this.emit({ event: 'game_data_sent' });
  }

  private sendSignal(to: string, kind: SignalKind, signal: Record<string, unknown>): void {
    if (this.currentSessionGeneration === null) {
      throw FatalError.protocol(`cannot signal peer ${to} before an authoritative SessionPlan`);
    }
    this.sendFrame(
      clientFrame('Signal', {
        to,
        generation: this.currentSessionGeneration,
        signal: tagRetrySignal(signal, this.pairRetryState.get(to)?.attempt ?? 0),
      }),
    );
    this.emit({ event: 'signal_sent', to, kind });
  }

  private sendFrame(frame: string): void {
    this.sendRawFrame(frame);
  }

  private sendRawFrame(frame: string | ArrayBuffer): void {
    this.assertActive();
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
    for (const peer of this.peerTasks.keys()) {
      unmet.push(`peer operation pending (${peer})`);
    }
    for (const peer of this.retryingPairs) {
      unmet.push(`pair retry incomplete (${peer})`);
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
      for (const peer of this.pendingExchangePeers) {
        if (this.connectedPairs.has(peer)) {
          unmet.push(`exchange sends pending (${peer})`);
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
   * A WebRTC plan with at least one pairing was issued, so the transport-fallback
   * status report is owed before this client may exit successfully.
   */
  private webrtcSessionExpected(): boolean {
    return this.webrtcPlanSeen && this.expectedPeers.size > 0;
  }

  /** Post-run cleanup: close the socket (Chromium teardown reaps the rest). */
  private teardown(): void {
    this.finished = true;
    this.peerWorkStopped.abort();
    this.wakeTimer?.cancel();
    this.wakeTimer = null;
    this.releaseHandshakeFrames();
    for (const peer of this.expectedPeers) this.engine.removePeer(peer);
    for (const retained of this.bufferedFrames.splice(0)) retained.release();
    if (this.ws !== null) {
      this.ws.onclose = null;
      this.ws.onmessage = null;
      try {
        this.ws.close();
      } catch {
        // Already closed/failed; nothing to release.
      }
    }
  }
}
