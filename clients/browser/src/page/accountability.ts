// Protocol-v3 relay-delivery accountability. This module deliberately parses
// the hand-modeled browser wire values at its boundary: JavaScript numbers
// cannot exactly represent every Rust u64, so accepting an unsafe integer
// would make sequence and cumulative-counter comparisons unsound.

export type DeliveryClass = 'reliable' | 'latest' | 'volatile';
export type GameDataDisposition = 'apply' | 'stale';

export class DeliveryAccountabilityViolation extends Error {}

interface SenderProgress {
  epoch: number;
  /** Last sequence already delivered or outside this recipient's obligation. */
  lastSeq: number;
}

interface SnapshotPlayer {
  id: string;
  epoch: number | null;
  seq: number | null;
}

interface ParsedGap {
  fromPlayer: string;
  epoch: number;
  fromSeq: number;
  toSeq: number;
  reason:
    | 'latest_superseded'
    | 'latest_dropped_full'
    | 'volatile_dropped'
    | 'unsupported_format';
}

interface RelayStatsSnapshot {
  intervalMs: number;
  sentToYou: number;
  droppedForYou: number;
  backpressureEvents: number;
}

interface DepartedSender {
  finalSeq: number;
}

const MAX_U32 = 0xffff_ffff;
const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
/** Mirrors protocol::DELIVERY_REPORT_MAX_GAPS in the Rust wire contract. */
export const DELIVERY_REPORT_MAX_GAPS = 256;
const GAP_REASONS = new Set([
  'latest_superseded',
  'latest_dropped_full',
  'volatile_dropped',
  'unsupported_format',
]);

const COUNTER_FIELDS = [
  ['reliable', ['delivered', 'abandoned', 'unsupported_format']],
  ['latest', ['delivered', 'superseded', 'dropped_full', 'abandoned', 'unsupported_format']],
  ['volatile', ['delivered', 'dropped', 'abandoned', 'unsupported_format']],
] as const;

/** Stateful validator for server-stamped relay delivery. */
export class DeliveryAccountability {
  private readonly protocolV3: boolean;
  private readonly senders = new Map<string, SenderProgress>();
  private readonly announcedEpochs = new Map<string, Set<number>>();
  private readonly staleSenders = new Set<string>();
  private readonly departedSenders = new Map<string, Map<number, DepartedSender>>();
  private readonly pendingGaps = new Map<string, ParsedGap[]>();
  private unadvisedUnsupportedGap: ParsedGap | null = null;
  private counters: number[] | null = null;
  private lastRelayStats: RelayStatsSnapshot | null = null;

  constructor(protocolV3 = true) {
    this.protocolV3 = protocolV3;
  }

  /** Clear room cursors/gaps while retaining connection-cumulative counters. */
  resetRoom(): void {
    this.senders.clear();
    this.announcedEpochs.clear();
    this.staleSenders.clear();
    this.departedSenders.clear();
    this.pendingGaps.clear();
    this.unadvisedUnsupportedGap = null;
  }

  /** Start accountability for a new physical connection. */
  resetConnection(): void {
    this.resetRoom();
    this.unadvisedUnsupportedGap = null;
    this.counters = null;
    this.lastRelayStats = null;
  }

  /** Establish a room/spectator snapshot without assuming pre-join seq zero. */
  rebaselineSnapshot(playersValue: unknown): void {
    const players = parsePlayers(playersValue, 'snapshot');
    this.resetRoom();
    for (const player of players) {
      if (this.protocolV3 && (player.epoch === null || player.seq === null)) {
        violation(`v3 snapshot omitted epoch/seq baseline for ${player.id}`);
      }
      if (!this.protocolV3 && (player.epoch !== null || player.seq !== null)) {
        violation(
          `v2 snapshot exposed delivery baseline (${player.epoch}, ${player.seq}) for ${player.id}`,
        );
      }
      if (player.epoch !== null && player.seq !== null) {
        this.senders.set(player.id, { epoch: player.epoch, lastSeq: player.seq });
      }
    }
  }

  /** Replace every cursor with the authoritative reconnect watermarks. */
  rebaselineReconnected(playersValue: unknown, watermarksValue: unknown): void {
    const players = parsePlayers(playersValue, 'reconnect snapshot');
    const watermarks = parseWatermarks(watermarksValue);
    const playersById = new Map(players.map((player) => [player.id, player]));
    const next = new Map<string, SenderProgress>();

    for (const player of players) {
      if (this.protocolV3 && (player.epoch === null || player.seq === null)) {
        violation(`v3 reconnect snapshot omitted epoch/seq baseline for ${player.id}`);
      }
      if (!this.protocolV3 && (player.epoch !== null || player.seq !== null)) {
        violation(
          `v2 reconnect snapshot exposed delivery baseline (${player.epoch}, ${player.seq}) for ${player.id}`,
        );
      }
    }
    if (!this.protocolV3) {
      if (watermarks.length !== 0) {
        violation('v2 Reconnected exposed sender_watermarks');
      }
      this.resetRoom();
      return;
    }

    for (const watermark of watermarks) {
      const player = playersById.get(watermark.id);
      if (player === undefined) {
        violation(`reconnect watermark names ${watermark.id} outside the room snapshot`);
      }
      if (player?.epoch !== watermark.epoch) {
        violation(
          `reconnect watermark epoch ${watermark.epoch} for ${watermark.id} ` +
            `disagrees with snapshot epoch ${player.epoch}`,
        );
      }
      if (player?.seq !== watermark.seq) {
        violation(
          `reconnect watermark seq ${watermark.seq} for ${watermark.id} ` +
            `disagrees with snapshot seq ${player.seq}`,
        );
      }
      next.set(watermark.id, { epoch: watermark.epoch, lastSeq: watermark.seq });
    }
    if (next.size !== playersById.size) {
      violation('reconnect watermarks do not cover the current room snapshot');
    }

    this.resetRoom();
    for (const [id, progress] of next) {
      this.senders.set(id, progress);
    }
  }

  notePlayerJoined(playerValue: unknown): void {
    const player = parsePlayer(playerValue, 'PlayerJoined');
    this.noteEpoch(player.id, player.epoch, player.seq, 'PlayerJoined');
  }

  notePlayerReconnected(playerIdValue: unknown, epochValue: unknown): void {
    const playerId = parsePlayerId(playerIdValue, 'PlayerReconnected.player_id');
    const epoch =
      epochValue === undefined || epochValue === null
        ? null
        : safeInteger(epochValue, 'PlayerReconnected.epoch', 1, MAX_U32);
    this.noteEpoch(playerId, epoch, epoch === null ? null : 0, 'PlayerReconnected');
  }

  notePlayerLeft(playerIdValue: unknown, epochValue?: unknown, finalSeqValue?: unknown): void {
    const playerId = parsePlayerId(playerIdValue, 'PlayerLeft.player_id');
    if (!this.protocolV3) {
      if (epochValue !== undefined || finalSeqValue !== undefined) {
        violation('v2 PlayerLeft exposed terminal delivery watermark fields');
      }
      return;
    }

    const epoch = safeInteger(epochValue, 'PlayerLeft.epoch', 1, MAX_U32);
    const finalSeq = safeInteger(finalSeqValue, 'PlayerLeft.final_seq', 0);
    const progress = this.senders.get(playerId);
    if (progress === undefined) {
      violation(`PlayerLeft terminal watermark names unknown sender ${playerId}`);
    }
    if (epoch < progress.epoch) {
      violation(
        `PlayerLeft epoch ${epoch} for ${playerId} moved backward from ${progress.epoch}`,
      );
    }
    if (epoch > progress.epoch && !this.announcedEpochs.get(playerId)?.has(epoch)) {
      violation(`PlayerLeft for ${playerId} used unannounced epoch ${epoch}`);
    }
    if (epoch === progress.epoch && finalSeq < progress.lastSeq) {
      violation(
        `PlayerLeft final_seq ${finalSeq} for ${playerId} moved backward from ${progress.lastSeq}`,
      );
    }
    let terminals = this.departedSenders.get(playerId);
    const existing = terminals?.get(epoch);
    if (existing !== undefined && existing.finalSeq !== finalSeq) {
      violation(`PlayerLeft terminal watermark changed for ${playerId} epoch ${epoch}`);
    }
    if (terminals !== undefined && [...terminals.keys()].some((value) => value > epoch)) {
      violation(
        `PlayerLeft terminal epoch ${epoch} for ${playerId} arrived after a newer leave`,
      );
    }
    const pending = this.pendingGaps.get(senderEpochKey(playerId, epoch)) ?? [];
    if (pending.some((gap) => gap.toSeq > finalSeq)) {
      violation(`gap report for ${playerId} extends beyond PlayerLeft final_seq ${finalSeq}`);
    }
    if (terminals === undefined) {
      terminals = new Map<number, DepartedSender>();
      this.departedSenders.set(playerId, terminals);
    }
    terminals.set(epoch, { finalSeq });
    this.staleSenders.add(playerId);
    this.tryRetireDeparted(playerId, epoch);
  }

  observeServerMessage(isUnsupportedFormatError: boolean): void {
    if (!this.protocolV3) {
      return;
    }
    if (isUnsupportedFormatError) {
      if (this.unadvisedUnsupportedGap === null) {
        violation('Error(UnsupportedGameDataFormat) lacked a prior causal DeliveryReport');
      }
      this.unadvisedUnsupportedGap = null;
    }
  }

  observeTerminal(): void {
    this.unadvisedUnsupportedGap = null;
  }

  /** Validate one connection-cumulative relay statistics snapshot. */
  recordRelayStats(statsValue: unknown): void {
    if (!this.protocolV3) {
      violation('v2 connection received RelayStats');
    }
    const stats = asRecord(statsValue, 'RelayStats.data');
    const next: RelayStatsSnapshot = {
      intervalMs: safeInteger(stats['interval_ms'], 'RelayStats.interval_ms', 1),
      sentToYou: safeInteger(stats['sent_to_you'], 'RelayStats.sent_to_you', 0),
      droppedForYou: safeInteger(stats['dropped_for_you'], 'RelayStats.dropped_for_you', 0),
      backpressureEvents: safeInteger(
        stats['backpressure_events'],
        'RelayStats.backpressure_events',
        0,
      ),
    };
    const previous = this.lastRelayStats;
    if (previous !== null) {
      if (next.intervalMs !== previous.intervalMs) {
        violation('RelayStats interval_ms changed within one connection');
      }
      if (
        next.sentToYou < previous.sentToYou ||
        next.droppedForYou < previous.droppedForYou ||
        next.backpressureEvents < previous.backpressureEvents
      ) {
        violation('cumulative RelayStats counters moved backward');
      }
    }
    this.lastRelayStats = next;
  }

  private noteEpoch(
    playerId: string,
    epoch: number | null,
    seq: number | null,
    source: string,
  ): void {
    if (this.protocolV3 && (epoch === null || seq === null)) {
      violation(`v3 ${source} omitted epoch/seq baseline for ${playerId}`);
    }
    if (!this.protocolV3 && (epoch !== null || seq !== null)) {
      violation(`v2 ${source} exposed delivery baseline (${epoch}, ${seq}) for ${playerId}`);
    }
    if (epoch === null || seq === null) {
      return;
    }
    const previous = this.senders.get(playerId);
    if (previous === undefined) {
      this.senders.set(playerId, { epoch, lastSeq: seq });
      this.staleSenders.delete(playerId);
      return;
    }
    if (previous.epoch === epoch && !this.staleSenders.has(playerId)) {
      return;
    }
    if (epoch <= previous.epoch) {
      violation(`${source} epoch ${epoch} for ${playerId} is not newer than ${previous.epoch}`);
    }
    let announced = this.announcedEpochs.get(playerId);
    if (announced === undefined) {
      announced = new Set<number>();
      this.announcedEpochs.set(playerId, announced);
    }
    if (announced.has(epoch)) {
      return;
    }
    if ([...announced].some((announcedEpoch) => announcedEpoch >= epoch)) {
      violation(`${source} epoch ${epoch} for ${playerId} is not newer than announced epochs`);
    }
    announced.add(epoch);
    this.staleSenders.add(playerId);
  }

  /** Record one causally prior exact-gap report and its cumulative counters. */
  recordReport(reportValue: unknown): void {
    if (!this.protocolV3) {
      violation('v2 connection received DeliveryReport');
    }
    const report = asRecord(reportValue, 'DeliveryReport.data');
    const nextCounters = parseCounters(report['per_class']);
    const previousCounters = this.counters ?? new Array<number>(12).fill(0);
    if (
      this.counters !== null &&
      nextCounters.some((counter, index) => counter < (this.counters?.[index] ?? 0))
    ) {
      violation('cumulative per-class counters moved backward');
    }

    const gaps = parseGaps(report['gaps']);
    if (gaps.length > DELIVERY_REPORT_MAX_GAPS) {
      violation(
        `DeliveryReport contains ${gaps.length} gap ranges, limit is ${DELIVERY_REPORT_MAX_GAPS}`,
      );
    }
    const reportRanges = new Map<string, ParsedGap[]>();
    const causalCounts = [0, 0, 0, 0];
    let unsupportedGap: ParsedGap | null = null;
    for (const gap of gaps) {
      this.validateGap(gap);
      const count = gap.toSeq - gap.fromSeq + 1;
      if (!Number.isSafeInteger(count)) {
        violation('exact gap length overflowed');
      }
      const index =
        gap.reason === 'latest_superseded'
          ? 0
          : gap.reason === 'latest_dropped_full'
            ? 1
            : gap.reason === 'volatile_dropped'
              ? 2
              : 3;
      if (index === 3) {
        // A report may carry several coalesced unsupported-format ranges, and
        // one range may span many sequences: the server merges consecutive
        // omissions from a sender so accountability does not cost a recipient
        // one frame per relayed message (server issue #212). Exactness is still
        // enforced by validateGap, the in-report overlap check below, and the
        // counter-delta comparison.
        unsupportedGap = gap;
      }
      causalCounts[index] = (causalCounts[index] ?? 0) + count;
      if (!Number.isSafeInteger(causalCounts[index])) {
        violation('causal gap count overflowed');
      }
      const key = senderEpochKey(gap.fromPlayer, gap.epoch);
      const siblings = reportRanges.get(key) ?? [];
      if (siblings.some((existing) => rangesOverlap(existing, gap))) {
        violation(
          `overlapping/duplicate gap ${gap.fromSeq}..=${gap.toSeq} for ` +
            `${gap.fromPlayer} epoch ${gap.epoch} in one report`,
        );
      }
      siblings.push(gap);
      reportRanges.set(key, siblings);
    }
    const unsupportedDelta =
      (nextCounters[2] ?? 0) -
      (previousCounters[2] ?? 0) +
      ((nextCounters[7] ?? 0) - (previousCounters[7] ?? 0)) +
      ((nextCounters[11] ?? 0) - (previousCounters[11] ?? 0));
    if (!Number.isSafeInteger(unsupportedDelta)) {
      violation('unsupported-format counter delta overflowed');
    }
    const counterDeltas = [
      (nextCounters[4] ?? 0) - (previousCounters[4] ?? 0),
      (nextCounters[5] ?? 0) - (previousCounters[5] ?? 0),
      (nextCounters[9] ?? 0) - (previousCounters[9] ?? 0),
      unsupportedDelta,
    ];
    if (counterDeltas.some((delta, index) => delta !== causalCounts[index])) {
      violation(
        `loss counter deltas ${JSON.stringify(counterDeltas)} do not match exact gap units ` +
          JSON.stringify(causalCounts),
      );
    }

    for (const gap of gaps) {
      const key = senderEpochKey(gap.fromPlayer, gap.epoch);
      const pending = this.pendingGaps.get(key) ?? [];
      pending.push(gap);
      pending.sort((left, right) => left.fromSeq - right.fromSeq);
      this.pendingGaps.set(key, pending);
    }
    if (unsupportedGap !== null) {
      // The advisory is authorized by an unsupported-format range having been
      // reported, whichever position it occupied in the report.
      this.unadvisedUnsupportedGap = unsupportedGap;
    }
    this.counters = nextCounters;
    for (const gap of gaps) {
      this.tryRetireDeparted(gap.fromPlayer, gap.epoch);
    }
  }

  /** Validate and advance one received GameData stamp. */
  recordGameData(dataValue: unknown): GameDataDisposition {
    const data = asRecord(dataValue, 'GameData.data');
    const fromPlayer = parsePlayerId(data['from_player'], 'GameData.from_player');
    if (!this.protocolV3) {
      if (
        data['seq'] === undefined &&
        data['epoch'] === undefined &&
        data['class'] === undefined &&
        data['key'] === undefined
      ) {
        return 'apply';
      }
      violation(`v2 GameData from ${fromPlayer} exposed v3 metadata`);
    }
    validateDeliveryClassAndKey(data['class'], data['key']);
    const seqMissing = data['seq'] === undefined || data['seq'] === null;
    const epochMissing = data['epoch'] === undefined || data['epoch'] === null;
    if (seqMissing && epochMissing) {
      violation('v3 GameData omitted its seq/epoch stamp');
    }
    if (seqMissing || epochMissing) {
      violation('GameData must carry seq and epoch together');
    }

    const seq = safeInteger(data['seq'], 'GameData.seq', 1);
    const epoch = safeInteger(data['epoch'], 'GameData.epoch', 1, MAX_U32);
    const progress = this.senders.get(fromPlayer);
    if (progress === undefined) {
      violation(`GameData from ${fromPlayer} arrived before a room/lifecycle baseline`);
    }
    if (epoch < progress.epoch) {
      violation(
        `GameData from ${fromPlayer} moved backward to epoch ${epoch} from ${progress.epoch}`,
      );
    }
    if (epoch > progress.epoch && !this.announcedEpochs.get(fromPlayer)?.has(epoch)) {
      violation(`GameData from ${fromPlayer} used unannounced epoch ${epoch}`);
    }
    const terminal = this.departedSenders.get(fromPlayer)?.get(epoch);
    if (terminal !== undefined && seq > terminal.finalSeq) {
      violation(
        `GameData from ${fromPlayer} advanced beyond PlayerLeft terminal ` +
          `(${epoch}, ${terminal.finalSeq})`,
      );
    }

    const key = senderEpochKey(fromPlayer, epoch);
    const transitioned = epoch > progress.epoch;
    if (transitioned) {
      for (const terminalEpoch of [...(this.departedSenders.get(fromPlayer)?.keys() ?? [])]) {
        if (terminalEpoch < epoch) {
          this.tryRetireDeparted(fromPlayer, terminalEpoch);
        }
      }
      if (
        [...(this.departedSenders.get(fromPlayer)?.keys() ?? [])].some(
          (terminalEpoch) => terminalEpoch < epoch,
        )
      ) {
        violation(
          `GameData from ${fromPlayer} advanced to epoch ${epoch} before older PlayerLeft tails retired`,
        );
      }
      this.consumeExactGap(key, fromPlayer, epoch, 1, seq);
      this.discardEarlierEpochs(fromPlayer, epoch, false);
    } else {
      if (seq <= progress.lastSeq) {
        violation(
          `duplicate/backward GameData from ${fromPlayer} epoch ${epoch}: ` +
            `${seq} after ${progress.lastSeq}`,
        );
      }
      const expected = progress.lastSeq + 1;
      if (!Number.isSafeInteger(expected)) {
        violation(
          `sequence overflow after ${progress.lastSeq} from ${fromPlayer} epoch ${epoch}`,
        );
      }
      this.consumeExactGap(key, fromPlayer, epoch, expected, seq);
    }
    progress.epoch = epoch;
    progress.lastSeq = seq;
    if (transitioned) {
      const announced = this.announcedEpochs.get(fromPlayer);
      if (announced !== undefined) {
        for (const announcedEpoch of announced) {
          if (announcedEpoch <= epoch) {
            announced.delete(announcedEpoch);
          }
        }
        if (announced.size === 0 && !this.departedSenders.get(fromPlayer)?.has(epoch)) {
          this.staleSenders.delete(fromPlayer);
        }
      }
    }
    const disposition =
      this.departedSenders.get(fromPlayer)?.has(epoch) === true ||
      [...(this.announcedEpochs.get(fromPlayer) ?? [])].some(
        (announcedEpoch) => announcedEpoch > epoch,
      )
        ? 'stale'
        : 'apply';
    this.tryRetireDeparted(fromPlayer, epoch);
    return disposition;
  }

  private validateGap(gap: ParsedGap): void {
    const progress = this.senders.get(gap.fromPlayer);
    if (progress === undefined) {
      violation(`gap report names unknown sender ${gap.fromPlayer}`);
    }
    if (gap.epoch < progress.epoch) {
      violation(
        `gap report for ${gap.fromPlayer} moved backward to epoch ${gap.epoch} ` +
          `from ${progress.epoch}`,
      );
    }
    if (
      gap.epoch > progress.epoch &&
      !this.announcedEpochs.get(gap.fromPlayer)?.has(gap.epoch)
    ) {
      violation(`gap report for ${gap.fromPlayer} used unannounced epoch ${gap.epoch}`);
    }
    const terminal = this.departedSenders.get(gap.fromPlayer)?.get(gap.epoch);
    if (terminal !== undefined && gap.toSeq > terminal.finalSeq) {
      violation(
        `gap report for ${gap.fromPlayer} extends beyond PlayerLeft terminal ` +
          `(${gap.epoch}, ${terminal.finalSeq})`,
      );
    }
    if (progress.epoch === gap.epoch && gap.fromSeq <= progress.lastSeq) {
      violation(
        `gap ${gap.fromSeq}..=${gap.toSeq} for ${gap.fromPlayer} epoch ${gap.epoch} ` +
          'was reported after data at or beyond its start',
      );
    }
    const key = senderEpochKey(gap.fromPlayer, gap.epoch);
    if ((this.pendingGaps.get(key) ?? []).some((existing) => rangesOverlap(existing, gap))) {
      violation(
        `overlapping/duplicate gap ${gap.fromSeq}..=${gap.toSeq} for ` +
          `${gap.fromPlayer} epoch ${gap.epoch}`,
      );
    }
  }

  private consumeExactGap(
    key: string,
    fromPlayer: string,
    epoch: number,
    expected: number,
    received: number,
  ): void {
    const gaps = this.pendingGaps.get(key);
    if (gaps === undefined) {
      if (received !== expected) {
        violation(
          `unexplained gap for ${fromPlayer} epoch ${epoch}: ` +
            `expected ${expected}, received ${received}`,
        );
      }
      return;
    }
    if (received === expected) {
      if (gaps.some((gap) => gap.fromSeq <= received && received <= gap.toSeq)) {
        violation(
          `prior gap report for ${fromPlayer} epoch ${epoch} includes delivered seq ${received}`,
        );
      }
      return;
    }

    let next = expected;
    let consumed = 0;
    for (const gap of gaps) {
      if (gap.fromSeq !== next || gap.toSeq >= received) {
        break;
      }
      next = gap.toSeq + 1;
      consumed += 1;
      if (next === received) {
        break;
      }
    }
    if (next !== received) {
      violation(
        `prior exact reports do not cover ${fromPlayer} epoch ${epoch} ` +
          `gap ${expected}..=${received - 1}`,
      );
    }
    gaps.splice(0, consumed);
    if (gaps.length === 0) {
      this.pendingGaps.delete(key);
    }
  }

  private discardEarlierEpochs(playerId: string, epoch: number, includeCurrent: boolean): void {
    const prefix = `${playerId}\u0000`;
    for (const key of this.pendingGaps.keys()) {
      if (!key.startsWith(prefix)) {
        continue;
      }
      const pendingEpoch = Number(key.slice(prefix.length));
      if (pendingEpoch < epoch || (includeCurrent && pendingEpoch === epoch)) {
        this.pendingGaps.delete(key);
      }
    }
  }

  private tryRetireDeparted(playerId: string, epoch: number): void {
    const terminal = this.departedSenders.get(playerId)?.get(epoch);
    const progress = this.senders.get(playerId);
    if (terminal === undefined || progress === undefined) {
      return;
    }

    let next: number;
    if (terminal.finalSeq === 0) {
      next = 1;
    } else if (progress.epoch < epoch) {
      next = 1;
    } else if (progress.epoch === epoch) {
      if (progress.lastSeq >= terminal.finalSeq) {
        this.retireDeparted(playerId, epoch);
        return;
      }
      next = progress.lastSeq + 1;
    } else {
      return;
    }

    const key = senderEpochKey(playerId, epoch);
    const gaps = this.pendingGaps.get(key) ?? [];
    let consumed = 0;
    while (next <= terminal.finalSeq) {
      const gap = gaps[consumed];
      if (gap === undefined || gap.fromSeq !== next || gap.toSeq > terminal.finalSeq) {
        return;
      }
      next = gap.toSeq + 1;
      consumed += 1;
    }
    if (consumed > 0) {
      gaps.splice(0, consumed);
      if (gaps.length === 0) {
        this.pendingGaps.delete(key);
      }
    }
    this.retireDeparted(playerId, epoch);
  }

  private retireDeparted(playerId: string, epoch: number): void {
    const terminals = this.departedSenders.get(playerId);
    terminals?.delete(epoch);
    if (terminals?.size === 0) {
      this.departedSenders.delete(playerId);
    }
    this.pendingGaps.delete(senderEpochKey(playerId, epoch));
    const announced = this.announcedEpochs.get(playerId);
    announced?.delete(epoch);
    if (announced?.size === 0) {
      this.announcedEpochs.delete(playerId);
    }
    if (!this.departedSenders.has(playerId) && !this.announcedEpochs.has(playerId)) {
      this.senders.delete(playerId);
      this.staleSenders.delete(playerId);
    }
  }
}

function parsePlayers(value: unknown, source: string): SnapshotPlayer[] {
  const players = asArray(value, source);
  const parsed = players.map((player) => parsePlayer(player, source));
  const seen = new Set<string>();
  for (const player of parsed) {
    if (seen.has(player.id)) {
      violation(`${source} contains duplicate player ${player.id}`);
    }
    seen.add(player.id);
  }
  return parsed;
}

function parsePlayer(value: unknown, source: string): SnapshotPlayer {
  const player = asRecord(value, source);
  const id = parsePlayerId(player['id'], `${source}.id`);
  const epochValue = player['epoch'];
  const epoch =
    epochValue === undefined ? null : safeInteger(epochValue, `${source}.epoch`, 1, MAX_U32);
  const seqValue = player['seq'];
  const seq = seqValue === undefined ? null : safeInteger(seqValue, `${source}.seq`, 0);
  if ((epoch === null) !== (seq === null)) {
    violation(`${source} must carry epoch and seq together`);
  }
  return { id, epoch, seq };
}

function parseWatermarks(value: unknown): Array<{ id: string; epoch: number; seq: number }> {
  if (value === undefined || value === null) {
    return [];
  }
  const seen = new Set<string>();
  return asArray(value, 'Reconnected.sender_watermarks').map((item, index) => {
    const watermark = asRecord(item, `Reconnected.sender_watermarks[${index}]`);
    const id = parsePlayerId(watermark['player_id'], `sender_watermarks[${index}].player_id`);
    if (seen.has(id)) {
      violation(`duplicate reconnect watermark for ${id}`);
    }
    seen.add(id);
    return {
      id,
      epoch: safeInteger(watermark['epoch'], `sender_watermarks[${index}].epoch`, 1, MAX_U32),
      seq: safeInteger(watermark['seq'], `sender_watermarks[${index}].seq`, 0),
    };
  });
}

export function validateDeliveryClassAndKey(
  classValue: unknown,
  keyValue: unknown,
): { className: DeliveryClass | null; key: number | null } {
  const className =
    classValue === undefined || classValue === null
      ? null
      : classValue === 'reliable' || classValue === 'latest' || classValue === 'volatile'
        ? classValue
        : violation(`invalid delivery class ${String(classValue)}`);
  const key =
    keyValue === undefined || keyValue === null
      ? null
      : safeInteger(keyValue, 'GameData.key', 0, MAX_U32);
  const valid =
    (className === null && key === null) ||
    ((className === 'reliable' || className === 'volatile') && key === null) ||
    (className === 'latest' && key !== null);
  if (!valid) {
    violation(`invalid received class/key combination (${String(className)}, ${String(key)})`);
  }
  return { className, key };
}

function parseCounters(value: unknown): number[] {
  const perClass = asRecord(value, 'DeliveryReport.per_class');
  const counters: number[] = [];
  for (const [className, fields] of COUNTER_FIELDS) {
    const group = asRecord(perClass[className], `DeliveryReport.per_class.${className}`);
    for (const field of fields) {
      counters.push(
        safeInteger(group[field], `DeliveryReport.per_class.${className}.${field}`, 0),
      );
    }
  }
  return counters;
}

function parseGaps(value: unknown): ParsedGap[] {
  if (value === undefined) {
    return [];
  }
  return asArray(value, 'DeliveryReport.gaps').map((item, index) => {
    const gap = asRecord(item, `DeliveryReport.gaps[${index}]`);
    const reason = gap['reason'];
    if (typeof reason !== 'string' || !GAP_REASONS.has(reason)) {
      violation(`DeliveryReport.gaps[${index}].reason is invalid`);
    }
    const fromSeq = safeInteger(gap['from_seq'], `DeliveryReport.gaps[${index}].from_seq`, 1);
    const toSeq = safeInteger(gap['to_seq'], `DeliveryReport.gaps[${index}].to_seq`, 1);
    if (toSeq < fromSeq) {
      violation(`DeliveryReport.gaps[${index}] has a reversed range`);
    }
    return {
      fromPlayer: parsePlayerId(
        gap['from_player'],
        `DeliveryReport.gaps[${index}].from_player`,
      ),
      epoch: safeInteger(gap['epoch'], `DeliveryReport.gaps[${index}].epoch`, 1, MAX_U32),
      fromSeq,
      toSeq,
      reason: reason as ParsedGap['reason'],
    };
  });
}

function safeInteger(value: unknown, field: string, minimum: number, maximum?: number): number {
  if (
    typeof value !== 'number' ||
    !Number.isSafeInteger(value) ||
    value < minimum ||
    (maximum !== undefined && value > maximum)
  ) {
    violation(`${field} must be a safe integer in range ${minimum}..=${maximum ?? 'MAX_SAFE'}`);
  }
  return value;
}

function parsePlayerId(value: unknown, field: string): string {
  if (typeof value !== 'string' || !CANONICAL_UUID.test(value)) {
    violation(`${field} must be a canonical lowercase UUID`);
  }
  return value;
}

function asRecord(value: unknown, field: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    violation(`${field} must be an object`);
  }
  return value as Record<string, unknown>;
}

function asArray(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) {
    violation(`${field} must be an array`);
  }
  return value;
}

function senderEpochKey(playerId: string, epoch: number): string {
  return `${playerId}\u0000${epoch}`;
}

function rangesOverlap(left: ParsedGap, right: ParsedGap): boolean {
  return left.fromSeq <= right.toSeq && right.fromSeq <= left.toSeq;
}

function violation(message: string): never {
  throw new DeliveryAccountabilityViolation(`delivery accountability violation: ${message}`);
}
