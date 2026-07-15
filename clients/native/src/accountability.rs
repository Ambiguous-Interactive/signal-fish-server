//! Client-side protocol-v3 delivery accountability.

use std::collections::{BTreeMap, BTreeSet};

use signal_fish_server::protocol::{
    DeliveryClass, DeliveryCountersByClass, DeliveryGap, DeliveryGapReason, DeliveryReportPayload,
    LatestDeliveryCounters, PlayerId, PlayerInfo, ReliableDeliveryCounters, SenderWatermark,
    VolatileDeliveryCounters, DELIVERY_REPORT_MAX_GAPS,
};

#[derive(Debug, Clone, Copy)]
struct SenderProgress {
    epoch: u32,
    /// Last sequence already delivered or outside this recipient's obligation.
    last_seq: u64,
}

#[derive(Debug, Clone, Copy)]
struct RelayStatsSnapshot {
    interval_ms: u64,
    sent_to_you: u64,
    dropped_for_you: u64,
    backpressure_events: u64,
}

#[derive(Debug, Clone, Copy)]
struct DepartedSender {
    final_seq: u64,
}

/// Whether validated game data still belongs to the application-visible
/// incarnation or is trailing data overtaken by priority lifecycle control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameDataDisposition {
    Apply,
    Stale,
}

/// Stateful validator for server-stamped relay delivery.
#[derive(Debug)]
pub struct DeliveryAccountability {
    protocol_v3: bool,
    senders: BTreeMap<PlayerId, SenderProgress>,
    announced_epochs: BTreeMap<PlayerId, BTreeSet<u32>>,
    stale_senders: BTreeSet<PlayerId>,
    departed_senders: BTreeMap<(PlayerId, u32), DepartedSender>,
    pending_gaps: BTreeMap<(PlayerId, u32), Vec<DeliveryGap>>,
    unadvised_unsupported_gap: Option<DeliveryGap>,
    counters: Option<DeliveryCountersByClass>,
    last_relay_stats: Option<RelayStatsSnapshot>,
}

impl Default for DeliveryAccountability {
    fn default() -> Self {
        Self::new(true)
    }
}

impl DeliveryAccountability {
    pub fn new(protocol_v3: bool) -> Self {
        Self {
            protocol_v3,
            senders: BTreeMap::new(),
            announced_epochs: BTreeMap::new(),
            stale_senders: BTreeSet::new(),
            departed_senders: BTreeMap::new(),
            pending_gaps: BTreeMap::new(),
            unadvised_unsupported_gap: None,
            counters: None,
            last_relay_stats: None,
        }
    }

    /// Clear room-scoped sender cursors and exact causes. Delivery counters
    /// remain cumulative for the lifetime of the physical connection.
    pub fn reset_room(&mut self) {
        self.senders.clear();
        self.announced_epochs.clear();
        self.stale_senders.clear();
        self.departed_senders.clear();
        self.pending_gaps.clear();
    }

    /// Start accountability for a new physical connection.
    pub fn reset_connection(&mut self) {
        self.reset_room();
        self.unadvised_unsupported_gap = None;
        self.counters = None;
        self.last_relay_stats = None;
    }

    /// Establish a fresh room/spectator snapshot from exact recipient-visible
    /// relay baselines.
    pub fn rebaseline_snapshot(&mut self, players: &[PlayerInfo]) -> Result<(), String> {
        self.reset_room();
        let mut seen = BTreeSet::new();
        for player in players {
            if !seen.insert(player.id) {
                return Err(format!(
                    "delivery accountability violation: snapshot contains duplicate player {}",
                    player.id
                ));
            }
            match (self.protocol_v3, player.epoch, player.seq) {
                (true, Some(epoch), Some(seq)) => {
                    validate_epoch(player.id, epoch, "snapshot")?;
                    self.senders.insert(
                        player.id,
                        SenderProgress {
                            epoch,
                            last_seq: seq,
                        },
                    );
                }
                (true, _, _) => {
                    return Err(format!(
                        "delivery accountability violation: v3 snapshot omitted paired epoch/seq baseline for {}",
                        player.id
                    ));
                }
                (false, None, None) => {}
                (false, epoch, seq) => {
                    return Err(format!(
                        "delivery accountability violation: v2 snapshot exposed delivery baseline ({epoch:?}, {seq:?}) for {}",
                        player.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Replace room cursors with the authoritative reconnect watermarks.
    pub fn rebaseline_reconnected(
        &mut self,
        players: &[PlayerInfo],
        watermarks: &[SenderWatermark],
    ) -> Result<(), String> {
        self.reset_room();
        let mut snapshot_stamps = BTreeMap::new();
        let mut snapshot_ids = BTreeSet::new();
        for player in players {
            if !snapshot_ids.insert(player.id) {
                return Err(format!(
                    "delivery accountability violation: reconnect snapshot contains duplicate player {}",
                    player.id
                ));
            }
            match (self.protocol_v3, player.epoch, player.seq) {
                (true, Some(epoch), Some(seq)) => {
                    validate_epoch(player.id, epoch, "reconnect snapshot")?;
                    snapshot_stamps.insert(player.id, (epoch, seq));
                }
                (true, _, _) => {
                    return Err(format!(
                        "delivery accountability violation: v3 reconnect snapshot omitted paired epoch/seq baseline for {}",
                        player.id
                    ));
                }
                (false, None, None) => {}
                (false, epoch, seq) => {
                    return Err(format!(
                        "delivery accountability violation: v2 reconnect snapshot exposed delivery baseline ({epoch:?}, {seq:?}) for {}",
                        player.id
                    ));
                }
            }
        }
        if !self.protocol_v3 {
            if watermarks.is_empty() {
                return Ok(());
            }
            return Err(
                "delivery accountability violation: v2 Reconnected exposed sender_watermarks"
                    .to_string(),
            );
        }
        let mut seen = BTreeSet::new();
        for watermark in watermarks {
            validate_epoch(watermark.player_id, watermark.epoch, "reconnect watermark")?;
            if !seen.insert(watermark.player_id) {
                return Err(format!(
                    "delivery accountability violation: duplicate reconnect watermark for {}",
                    watermark.player_id
                ));
            }
            match snapshot_stamps.get(&watermark.player_id) {
                Some(stamp) if *stamp == (watermark.epoch, watermark.seq) => {}
                Some((epoch, seq)) => {
                    return Err(format!(
                        "delivery accountability violation: reconnect watermark ({}, {}) for {} disagrees with snapshot ({epoch}, {seq})",
                        watermark.epoch, watermark.seq, watermark.player_id
                    ));
                }
                None => {
                    return Err(format!(
                        "delivery accountability violation: reconnect watermark names {} outside the room snapshot",
                        watermark.player_id
                    ));
                }
            }
            self.senders.insert(
                watermark.player_id,
                SenderProgress {
                    epoch: watermark.epoch,
                    last_seq: watermark.seq,
                },
            );
        }
        if seen != snapshot_ids {
            return Err(format!(
                "delivery accountability violation: reconnect watermarks do not cover the current room snapshot (watermarks={seen:?}, snapshot={snapshot_ids:?})"
            ));
        }
        Ok(())
    }

    /// Record a live player incarnation boundary. Replayed/snapshot duplicates
    /// of an as-yet-unobserved epoch are idempotent.
    pub fn note_player_joined(&mut self, player: &PlayerInfo) -> Result<(), String> {
        self.note_epoch(player.id, player.epoch, player.seq, "PlayerJoined")
    }

    pub fn note_player_reconnected(
        &mut self,
        player_id: PlayerId,
        epoch: Option<u32>,
    ) -> Result<(), String> {
        self.note_epoch(player_id, epoch, epoch.map(|_| 0), "PlayerReconnected")
    }

    pub fn note_player_left(
        &mut self,
        player_id: PlayerId,
        epoch: Option<u32>,
        final_seq: Option<u64>,
    ) -> Result<(), String> {
        if !self.protocol_v3 {
            return if epoch.is_none() && final_seq.is_none() {
                Ok(())
            } else {
                Err("delivery accountability violation: v2 PlayerLeft exposed terminal delivery watermark fields".to_string())
            };
        }

        let (epoch, final_seq) = match (epoch, final_seq) {
            (Some(epoch), Some(final_seq)) => (epoch, final_seq),
            _ => return Err("delivery accountability violation: v3 PlayerLeft omitted epoch/final_seq terminal watermark".to_string()),
        };
        validate_epoch(player_id, epoch, "PlayerLeft")?;
        let progress = self.senders.get(&player_id).copied().ok_or_else(|| {
            format!(
                "delivery accountability violation: PlayerLeft terminal watermark names unknown sender {player_id}"
            )
        })?;
        if epoch < progress.epoch {
            return Err(format!(
                "delivery accountability violation: PlayerLeft epoch {epoch} for {player_id} moved backward from {}",
                progress.epoch
            ));
        }
        if epoch > progress.epoch
            && !self
                .announced_epochs
                .get(&player_id)
                .is_some_and(|announced| announced.contains(&epoch))
        {
            return Err(format!(
                "delivery accountability violation: PlayerLeft for {player_id} used unannounced epoch {epoch}"
            ));
        }
        if epoch == progress.epoch && final_seq < progress.last_seq {
            return Err(format!(
                "delivery accountability violation: PlayerLeft final_seq {final_seq} for {player_id} moved backward from {}",
                progress.last_seq
            ));
        }
        if let Some(existing) = self.departed_senders.get(&(player_id, epoch)) {
            if existing.final_seq != final_seq {
                return Err(format!(
                    "delivery accountability violation: PlayerLeft terminal watermark changed for {player_id} epoch {epoch}"
                ));
            }
        }
        if self
            .departed_senders
            .keys()
            .any(|(sender, terminal_epoch)| *sender == player_id && *terminal_epoch > epoch)
        {
            return Err(format!(
                "delivery accountability violation: PlayerLeft terminal epoch {epoch} for {player_id} arrived after a newer leave"
            ));
        }
        if self
            .pending_gaps
            .get(&(player_id, epoch))
            .is_some_and(|gaps| gaps.iter().any(|gap| gap.to_seq > final_seq))
        {
            return Err(format!(
                "delivery accountability violation: gap report for {player_id} extends beyond PlayerLeft final_seq {final_seq}"
            ));
        }
        self.departed_senders
            .insert((player_id, epoch), DepartedSender { final_seq });
        self.stale_senders.insert(player_id);
        self.try_retire_departed(player_id, epoch)
    }

    /// Accept an optional rate-limited unsupported-format advisory only after
    /// a causal exact report. The boolean identifies
    /// Error(UnsupportedGameDataFormat).
    pub fn observe_server_message(
        &mut self,
        is_unsupported_format_error: bool,
    ) -> Result<(), String> {
        if !self.protocol_v3 {
            return Ok(());
        }
        if is_unsupported_format_error && self.unadvised_unsupported_gap.take().is_none() {
            return Err("delivery accountability violation: Error(UnsupportedGameDataFormat) lacked a prior causal DeliveryReport".to_string());
        }
        Ok(())
    }

    /// A terminal socket outcome ends the observable stream, so no
    /// supplemental error is required after the final report.
    pub fn observe_terminal(&mut self) {
        self.unadvised_unsupported_gap = None;
    }

    pub fn record_relay_stats(
        &mut self,
        interval_ms: u64,
        sent_to_you: u64,
        dropped_for_you: u64,
        backpressure_events: u64,
    ) -> Result<(), String> {
        if !self.protocol_v3 {
            return Err(
                "delivery accountability violation: v2 connection received RelayStats".to_string(),
            );
        }
        if interval_ms == 0 {
            return Err(
                "delivery accountability violation: RelayStats interval_ms must be positive"
                    .to_string(),
            );
        }

        let next = RelayStatsSnapshot {
            interval_ms,
            sent_to_you,
            dropped_for_you,
            backpressure_events,
        };
        if let Some(previous) = self.last_relay_stats {
            if next.interval_ms != previous.interval_ms {
                return Err(format!(
                    "delivery accountability violation: RelayStats interval_ms changed within one connection (previous={}, next={})",
                    previous.interval_ms, next.interval_ms
                ));
            }
            if next.sent_to_you < previous.sent_to_you
                || next.dropped_for_you < previous.dropped_for_you
                || next.backpressure_events < previous.backpressure_events
            {
                return Err(
                    "delivery accountability violation: cumulative RelayStats counters moved backward"
                        .to_string(),
                );
            }
        }
        self.last_relay_stats = Some(next);
        Ok(())
    }

    fn note_epoch(
        &mut self,
        player_id: PlayerId,
        epoch: Option<u32>,
        seq: Option<u64>,
        source: &str,
    ) -> Result<(), String> {
        let (epoch, seq) = match (self.protocol_v3, epoch, seq) {
            (true, Some(epoch), Some(seq)) => (epoch, seq),
            (true, _, _) => {
                return Err(format!(
                    "delivery accountability violation: v3 {source} omitted paired epoch/seq baseline for {player_id}"
                ));
            }
            (false, None, None) => return Ok(()),
            (false, epoch, seq) => {
                return Err(format!(
                    "delivery accountability violation: v2 {source} exposed delivery baseline ({epoch:?}, {seq:?}) for {player_id}"
                ));
            }
        };
        validate_epoch(player_id, epoch, source)?;
        let Some(previous) = self.senders.get(&player_id) else {
            self.senders.insert(
                player_id,
                SenderProgress {
                    epoch,
                    last_seq: seq,
                },
            );
            self.stale_senders.remove(&player_id);
            return Ok(());
        };
        if previous.epoch == epoch && !self.stale_senders.contains(&player_id) {
            return Ok(());
        }
        if epoch <= previous.epoch {
            return Err(format!(
                "delivery accountability violation: {source} epoch {epoch} for {player_id} is not newer than {}",
                previous.epoch
            ));
        }
        let announced = self.announced_epochs.entry(player_id).or_default();
        if announced.contains(&epoch) {
            return Ok(());
        }
        if announced.last().is_some_and(|latest| epoch <= *latest) {
            return Err(format!(
                "delivery accountability violation: {source} epoch {epoch} for {player_id} is not newer than announced epochs {announced:?}"
            ));
        }
        announced.insert(epoch);
        self.stale_senders.insert(player_id);
        Ok(())
    }

    /// Record one causally prior exact gap report and its cumulative counters.
    pub fn record_report(&mut self, report: &DeliveryReportPayload) -> Result<(), String> {
        if !self.protocol_v3 {
            return Err(
                "delivery accountability violation: v2 connection received DeliveryReport"
                    .to_string(),
            );
        }
        if report.gaps.len() > DELIVERY_REPORT_MAX_GAPS {
            return Err(format!(
                "delivery accountability violation: DeliveryReport contains {} gap ranges, limit is {DELIVERY_REPORT_MAX_GAPS}",
                report.gaps.len()
            ));
        }
        let previous = self.counters.unwrap_or_default();
        validate_monotonic_counters(previous, report.per_class)?;

        // Validate the whole report before mutating state, including ranges
        // that overlap another range in this same report.
        let mut report_ranges: BTreeMap<(PlayerId, u32), Vec<(u64, u64)>> = BTreeMap::new();
        let mut causal_counts = [0u64; 4];
        let mut unsupported_seen = false;
        for gap in &report.gaps {
            self.validate_gap(gap)?;
            let count = gap
                .to_seq
                .checked_sub(gap.from_seq)
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| {
                    "delivery accountability violation: exact gap length overflowed".to_string()
                })?;
            let index = match gap.reason {
                DeliveryGapReason::LatestSuperseded => 0,
                DeliveryGapReason::LatestDroppedFull => 1,
                DeliveryGapReason::VolatileDropped => 2,
                DeliveryGapReason::UnsupportedFormat => {
                    if unsupported_seen || gap.from_seq != gap.to_seq || report.gaps.len() != 1 {
                        return Err("delivery accountability violation: unsupported-format report must name exactly one sequence".to_string());
                    }
                    unsupported_seen = true;
                    3
                }
            };
            causal_counts[index] = causal_counts[index].checked_add(count).ok_or_else(|| {
                "delivery accountability violation: causal gap count overflowed".to_string()
            })?;
            let ranges = report_ranges
                .entry((gap.from_player, gap.epoch))
                .or_default();
            if ranges
                .iter()
                .any(|(from, to)| gap.from_seq <= *to && *from <= gap.to_seq)
            {
                return Err(format!(
                    "delivery accountability violation: overlapping/duplicate gap {}..={} for {} epoch {} in one report",
                    gap.from_seq, gap.to_seq, gap.from_player, gap.epoch
                ));
            }
            ranges.push((gap.from_seq, gap.to_seq));
        }
        let delta = |next: u64, prior: u64| next - prior;
        let unsupported_delta = delta(
            report.per_class.reliable.unsupported_format,
            previous.reliable.unsupported_format,
        )
        .checked_add(delta(
            report.per_class.latest.unsupported_format,
            previous.latest.unsupported_format,
        ))
        .and_then(|sum| {
            sum.checked_add(delta(
                report.per_class.volatile.unsupported_format,
                previous.volatile.unsupported_format,
            ))
        })
        .ok_or_else(|| {
            "delivery accountability violation: unsupported-format delta overflowed".to_string()
        })?;
        let counter_deltas = [
            delta(
                report.per_class.latest.superseded,
                previous.latest.superseded,
            ),
            delta(
                report.per_class.latest.dropped_full,
                previous.latest.dropped_full,
            ),
            delta(report.per_class.volatile.dropped, previous.volatile.dropped),
            unsupported_delta,
        ];
        if counter_deltas != causal_counts {
            return Err(format!(
                "delivery accountability violation: loss counter deltas {counter_deltas:?} do not match exact gap units {causal_counts:?}"
            ));
        }
        for gap in &report.gaps {
            let gaps = self
                .pending_gaps
                .entry((gap.from_player, gap.epoch))
                .or_default();
            gaps.push(gap.clone());
            gaps.sort_unstable_by_key(|candidate| candidate.from_seq);
        }
        if unsupported_seen {
            self.unadvised_unsupported_gap = report.gaps.first().cloned();
        }
        self.counters = Some(report.per_class);
        for gap in &report.gaps {
            self.try_retire_departed(gap.from_player, gap.epoch)?;
        }
        Ok(())
    }

    /// Validate and advance one received GameData stamp.
    pub fn record_game_data(
        &mut self,
        from_player: PlayerId,
        seq: Option<u64>,
        epoch: Option<u32>,
        class: Option<DeliveryClass>,
        key: Option<u32>,
    ) -> Result<GameDataDisposition, String> {
        if !self.protocol_v3 {
            if seq.is_none() && epoch.is_none() && class.is_none() && key.is_none() {
                return Ok(GameDataDisposition::Apply);
            }
            return Err(format!(
                "delivery accountability violation: v2 GameData from {from_player} exposed v3 metadata (seq={seq:?}, epoch={epoch:?}, class={class:?}, key={key:?})"
            ));
        }
        validate_class_key(class, key)?;
        let (seq, epoch) = match (seq, epoch) {
            (None, None) => {
                return Err(
                    "delivery accountability violation: v3 GameData omitted its seq/epoch stamp"
                        .to_string(),
                );
            }
            (Some(seq), Some(epoch)) => (seq, epoch),
            _ => {
                return Err(format!(
                    "delivery accountability violation: GameData from {from_player} must carry seq and epoch together (seq={seq:?}, epoch={epoch:?})"
                ));
            }
        };
        if seq == 0 || epoch == 0 {
            return Err(format!(
                "delivery accountability violation: GameData from {from_player} has non-positive stamp ({epoch}, {seq})"
            ));
        }

        let progress = self.senders.get(&from_player).copied().ok_or_else(|| {
            format!(
                "delivery accountability violation: GameData from {from_player} arrived before a room/lifecycle baseline"
            )
        })?;
        if epoch < progress.epoch {
            return Err(format!(
                "delivery accountability violation: GameData from {from_player} moved backward to epoch {epoch} from {}",
                progress.epoch
            ));
        }
        if epoch > progress.epoch
            && !self
                .announced_epochs
                .get(&from_player)
                .is_some_and(|announced| announced.contains(&epoch))
        {
            return Err(format!(
                "delivery accountability violation: GameData from {from_player} used unannounced epoch {epoch} after {}",
                progress.epoch
            ));
        }
        if let Some(terminal) = self.departed_senders.get(&(from_player, epoch)) {
            if seq > terminal.final_seq {
                return Err(format!(
                    "delivery accountability violation: GameData from {from_player} advanced beyond PlayerLeft terminal ({epoch}, {})",
                    terminal.final_seq
                ));
            }
        }

        let gap_key = (from_player, epoch);
        let transitioned = epoch > progress.epoch;
        if transitioned {
            let older_terminals: Vec<_> = self
                .departed_senders
                .keys()
                .filter_map(|(sender, terminal_epoch)| {
                    (*sender == from_player && *terminal_epoch < epoch).then_some(*terminal_epoch)
                })
                .collect();
            for terminal_epoch in older_terminals {
                self.try_retire_departed(from_player, terminal_epoch)?;
            }
            if self
                .departed_senders
                .keys()
                .any(|(sender, terminal_epoch)| *sender == from_player && *terminal_epoch < epoch)
            {
                return Err(format!(
                    "delivery accountability violation: GameData from {from_player} advanced to epoch {epoch} before older PlayerLeft tails retired"
                ));
            }
            self.consume_exact_gap(gap_key, 1, seq)?;
            self.pending_gaps.retain(|(sender, pending_epoch), _gaps| {
                *sender != from_player || *pending_epoch >= epoch
            });
        } else {
            let last_seq = progress.last_seq;
            if seq <= last_seq {
                return Err(format!(
                        "delivery accountability violation: duplicate/backward GameData from {from_player} epoch {epoch}: {seq} after {last_seq}"
                    ));
            }
            let expected = last_seq.checked_add(1).ok_or_else(|| {
                    format!(
                        "delivery accountability violation: sequence overflow after {last_seq} from {from_player} epoch {epoch}"
                    )
                })?;
            self.consume_exact_gap(gap_key, expected, seq)?;
        }
        self.senders.insert(
            from_player,
            SenderProgress {
                epoch,
                last_seq: seq,
            },
        );
        if transitioned {
            let mut no_newer_announcement = true;
            if let Some(announced) = self.announced_epochs.get_mut(&from_player) {
                announced.retain(|announced_epoch| *announced_epoch > epoch);
                no_newer_announcement = announced.is_empty();
            }
            if no_newer_announcement && !self.departed_senders.contains_key(&(from_player, epoch)) {
                self.stale_senders.remove(&from_player);
            }
        }
        let stale = self.departed_senders.contains_key(&(from_player, epoch))
            || self
                .announced_epochs
                .get(&from_player)
                .is_some_and(|announced| {
                    announced
                        .iter()
                        .any(|announced_epoch| *announced_epoch > epoch)
                });
        let disposition = if stale {
            GameDataDisposition::Stale
        } else {
            GameDataDisposition::Apply
        };
        self.try_retire_departed(from_player, epoch)?;
        Ok(disposition)
    }

    fn validate_gap(&self, gap: &DeliveryGap) -> Result<(), String> {
        if gap.epoch == 0 || gap.from_seq == 0 || gap.to_seq < gap.from_seq {
            return Err(format!(
                "delivery accountability violation: invalid exact gap for {}: epoch {}, range {}..={}",
                gap.from_player, gap.epoch, gap.from_seq, gap.to_seq
            ));
        }
        let progress = self.senders.get(&gap.from_player).ok_or_else(|| {
            format!(
                "delivery accountability violation: gap report names unknown sender {}",
                gap.from_player
            )
        })?;
        if gap.epoch < progress.epoch {
            return Err(format!(
                "delivery accountability violation: gap report for {} moved backward to epoch {} from {}",
                gap.from_player, gap.epoch, progress.epoch
            ));
        }
        if gap.epoch > progress.epoch
            && !self
                .announced_epochs
                .get(&gap.from_player)
                .is_some_and(|announced| announced.contains(&gap.epoch))
        {
            return Err(format!(
                "delivery accountability violation: gap report for {} used unannounced epoch {} after {}",
                gap.from_player, gap.epoch, progress.epoch
            ));
        }
        if let Some(terminal) = self.departed_senders.get(&(gap.from_player, gap.epoch)) {
            if gap.to_seq > terminal.final_seq {
                return Err(format!(
                    "delivery accountability violation: gap report for {} extends beyond PlayerLeft terminal ({}, {})",
                    gap.from_player, gap.epoch, terminal.final_seq
                ));
            }
        }
        if progress.epoch == gap.epoch && gap.from_seq <= progress.last_seq {
            return Err(format!(
                "delivery accountability violation: gap {}..={} for {} epoch {} was reported after data at or beyond its start",
                gap.from_seq, gap.to_seq, gap.from_player, gap.epoch
            ));
        }
        if self
            .pending_gaps
            .get(&(gap.from_player, gap.epoch))
            .is_some_and(|pending| {
                pending.iter().any(|existing| {
                    gap.from_seq <= existing.to_seq && existing.from_seq <= gap.to_seq
                })
            })
        {
            return Err(format!(
                "delivery accountability violation: overlapping/duplicate gap {}..={} for {} epoch {}",
                gap.from_seq, gap.to_seq, gap.from_player, gap.epoch
            ));
        }
        Ok(())
    }

    fn consume_exact_gap(
        &mut self,
        key: (PlayerId, u32),
        expected: u64,
        received: u64,
    ) -> Result<(), String> {
        let Some(gaps) = self.pending_gaps.get_mut(&key) else {
            if received == expected {
                return Ok(());
            }
            return Err(format!(
                "delivery accountability violation: unexplained gap for {} epoch {}: expected {expected}, received {received}",
                key.0, key.1
            ));
        };
        if received == expected {
            if gaps
                .iter()
                .any(|gap| gap.from_seq <= received && received <= gap.to_seq)
            {
                return Err(format!(
                    "delivery accountability violation: prior gap report for {} epoch {} includes delivered seq {received}",
                    key.0, key.1
                ));
            }
            return Ok(());
        }

        let mut next = expected;
        let mut consumed = 0usize;
        for gap in gaps.iter() {
            if gap.from_seq != next || gap.to_seq >= received {
                break;
            }
            next = gap.to_seq.checked_add(1).ok_or_else(|| {
                format!(
                    "delivery accountability violation: gap range overflow for {} epoch {}",
                    key.0, key.1
                )
            })?;
            consumed += 1;
            if next == received {
                break;
            }
        }
        if next != received {
            return Err(format!(
                "delivery accountability violation: prior exact reports do not cover {} epoch {} gap {expected}..={}",
                key.0,
                key.1,
                received - 1
            ));
        }
        gaps.drain(..consumed);
        if gaps.is_empty() {
            self.pending_gaps.remove(&key);
        }
        Ok(())
    }

    fn try_retire_departed(&mut self, player_id: PlayerId, epoch: u32) -> Result<(), String> {
        let Some(terminal) = self.departed_senders.get(&(player_id, epoch)).copied() else {
            return Ok(());
        };
        let Some(progress) = self.senders.get(&player_id).copied() else {
            return Ok(());
        };

        let mut next = if terminal.final_seq == 0 || progress.epoch < epoch {
            1
        } else if progress.epoch == epoch {
            let last_seq = progress.last_seq;
            if last_seq >= terminal.final_seq {
                self.retire_departed(player_id, epoch);
                return Ok(());
            }
            last_seq + 1
        } else {
            return Err(format!(
                "delivery accountability violation: sender {player_id} advanced beyond its unresolved PlayerLeft epoch {}",
                epoch
            ));
        };

        let key = (player_id, epoch);
        let gaps = self
            .pending_gaps
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut consumed = 0usize;
        let mut covered = terminal.final_seq == 0;
        while next <= terminal.final_seq {
            let Some(gap) = gaps.get(consumed) else {
                return Ok(());
            };
            if gap.from_seq != next || gap.to_seq > terminal.final_seq {
                return Ok(());
            }
            consumed += 1;
            if gap.to_seq == terminal.final_seq {
                covered = true;
                break;
            }
            next = gap.to_seq + 1;
        }
        if !covered {
            return Ok(());
        }
        if consumed > 0 {
            let remove_key = {
                let gaps = self.pending_gaps.get_mut(&key).expect("gaps read above");
                gaps.drain(..consumed);
                gaps.is_empty()
            };
            if remove_key {
                self.pending_gaps.remove(&key);
            }
        }
        self.retire_departed(player_id, epoch);
        Ok(())
    }

    fn retire_departed(&mut self, player_id: PlayerId, epoch: u32) {
        self.departed_senders.remove(&(player_id, epoch));
        self.pending_gaps.remove(&(player_id, epoch));
        if let Some(announced) = self.announced_epochs.get_mut(&player_id) {
            announced.remove(&epoch);
            if announced.is_empty() {
                self.announced_epochs.remove(&player_id);
            }
        }
        let has_terminal = self
            .departed_senders
            .keys()
            .any(|(sender, _epoch)| *sender == player_id);
        if !has_terminal && !self.announced_epochs.contains_key(&player_id) {
            self.senders.remove(&player_id);
            self.stale_senders.remove(&player_id);
        }
    }
}

/// Validate the protocol's delivery class/key pairing.
pub fn validate_class_key(class: Option<DeliveryClass>, key: Option<u32>) -> Result<(), String> {
    match (class, key) {
        (None | Some(DeliveryClass::Reliable | DeliveryClass::Volatile), None)
        | (Some(DeliveryClass::Latest), Some(_)) => Ok(()),
        _ => Err(format!(
            "delivery accountability violation: invalid received class/key combination ({class:?}, {key:?})"
        )),
    }
}

fn validate_epoch(player_id: PlayerId, epoch: u32, source: &str) -> Result<(), String> {
    if epoch == 0 {
        return Err(format!(
            "delivery accountability violation: {source} advertised epoch 0 for {player_id}"
        ));
    }
    Ok(())
}

fn validate_monotonic_counters(
    previous: DeliveryCountersByClass,
    next: DeliveryCountersByClass,
) -> Result<(), String> {
    let reliable = |value: ReliableDeliveryCounters| {
        [value.delivered, value.abandoned, value.unsupported_format]
    };
    let latest = |value: LatestDeliveryCounters| {
        [
            value.delivered,
            value.superseded,
            value.dropped_full,
            value.abandoned,
            value.unsupported_format,
        ]
    };
    let volatile = |value: VolatileDeliveryCounters| {
        [
            value.delivered,
            value.dropped,
            value.abandoned,
            value.unsupported_format,
        ]
    };
    let monotonic = reliable(previous.reliable)
        .into_iter()
        .zip(reliable(next.reliable))
        .chain(latest(previous.latest).into_iter().zip(latest(next.latest)))
        .chain(
            volatile(previous.volatile)
                .into_iter()
                .zip(volatile(next.volatile)),
        )
        .all(|(before, after)| after >= before);
    if !monotonic {
        return Err(format!(
            "delivery accountability violation: cumulative per-class counters moved backward (previous={previous:?}, next={next:?})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use signal_fish_server::protocol::{
        DeliveryGapReason, LatestDeliveryCounters, ReliableDeliveryCounters,
        VolatileDeliveryCounters,
    };

    use super::*;

    fn id(value: u128) -> PlayerId {
        PlayerId::from_u128(value)
    }

    fn player(player_id: PlayerId, epoch: u32) -> PlayerInfo {
        PlayerInfo {
            id: player_id,
            name: "sender".to_string(),
            is_authority: false,
            is_ready: false,
            connected_at: "1970-01-01T00:00:00Z".parse().unwrap(),
            connection_info: None,
            epoch: Some(epoch),
            seq: Some(0),
            region_id: String::new(),
        }
    }

    fn player_at(player_id: PlayerId, epoch: u32, seq: u64) -> PlayerInfo {
        PlayerInfo {
            seq: Some(seq),
            ..player(player_id, epoch)
        }
    }

    fn counters(seed: u64) -> DeliveryCountersByClass {
        DeliveryCountersByClass {
            reliable: ReliableDeliveryCounters {
                delivered: seed,
                abandoned: 0,
                unsupported_format: 0,
            },
            latest: LatestDeliveryCounters {
                delivered: seed,
                superseded: 0,
                dropped_full: 0,
                abandoned: 0,
                unsupported_format: 0,
            },
            volatile: VolatileDeliveryCounters {
                delivered: seed,
                dropped: 0,
                abandoned: 0,
                unsupported_format: 0,
            },
        }
    }

    fn counters_with_superseded(count: u64) -> DeliveryCountersByClass {
        let mut value = counters(0);
        value.latest.superseded = count;
        value
    }

    fn counters_with_unsupported(count: u64) -> DeliveryCountersByClass {
        let mut value = counters(0);
        value.reliable.unsupported_format = count;
        value
    }

    fn unsupported_gap(sender: PlayerId, seq: u64) -> DeliveryGap {
        DeliveryGap {
            from_player: sender,
            epoch: 1,
            from_seq: seq,
            to_seq: seq,
            reason: DeliveryGapReason::UnsupportedFormat,
        }
    }

    fn gap(sender: PlayerId, from_seq: u64, to_seq: u64) -> DeliveryGap {
        gap_at_epoch(sender, 1, from_seq, to_seq)
    }

    fn gap_at_epoch(sender: PlayerId, epoch: u32, from_seq: u64, to_seq: u64) -> DeliveryGap {
        DeliveryGap {
            from_player: sender,
            epoch,
            from_seq,
            to_seq,
            reason: DeliveryGapReason::LatestSuperseded,
        }
    }

    #[test]
    fn class_key_validation_is_data_driven() {
        let cases = [
            (None, None, true),
            (Some(DeliveryClass::Reliable), None, true),
            (Some(DeliveryClass::Volatile), None, true),
            (Some(DeliveryClass::Latest), Some(7), true),
            (None, Some(7), false),
            (Some(DeliveryClass::Reliable), Some(7), false),
            (Some(DeliveryClass::Volatile), Some(7), false),
            (Some(DeliveryClass::Latest), None, false),
        ];
        for (class, key, valid) in cases {
            assert_eq!(
                validate_class_key(class, key).is_ok(),
                valid,
                "{class:?}/{key:?}"
            );
        }
    }

    #[test]
    fn only_prior_exact_ranges_authorize_a_gap() {
        let sender = id(1);
        let mut valid = DeliveryAccountability::default();
        valid.note_player_joined(&player(sender, 1)).unwrap();
        valid
            .record_game_data(sender, Some(1), Some(1), None, None)
            .unwrap();
        valid
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(2),
                gaps: vec![gap(sender, 2, 3)],
            })
            .unwrap();
        valid
            .record_game_data(sender, Some(4), Some(1), None, None)
            .unwrap();

        for (name, reports, next_seq) in [
            ("missing", Vec::new(), 3),
            ("incomplete", vec![gap(sender, 2, 2)], 4),
            ("overreaching", vec![gap(sender, 2, 4)], 4),
        ] {
            let mut state = DeliveryAccountability::default();
            state.note_player_joined(&player(sender, 1)).unwrap();
            state
                .record_game_data(sender, Some(1), Some(1), None, None)
                .unwrap();
            if !reports.is_empty() {
                state
                    .record_report(&DeliveryReportPayload {
                        per_class: counters_with_superseded(
                            reports
                                .iter()
                                .map(|gap| gap.to_seq - gap.from_seq + 1)
                                .sum(),
                        ),
                        gaps: reports,
                    })
                    .unwrap();
            }
            assert!(
                state
                    .record_game_data(sender, Some(next_seq), Some(1), None, None)
                    .is_err(),
                "{name} exact cause must fail"
            );
        }
    }

    #[test]
    fn same_socket_frontiers_survive_reconnect_watermark_rebaseline() {
        let sender = id(2);
        let mut state = DeliveryAccountability::default();
        state.note_player_joined(&player(sender, 1)).unwrap();
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters(2),
                gaps: Vec::new(),
            })
            .unwrap();
        state.record_relay_stats(1_000, 4, 2, 1).unwrap();
        assert!(state
            .record_report(&DeliveryReportPayload {
                per_class: counters(1),
                gaps: Vec::new(),
            })
            .is_err());

        state
            .rebaseline_reconnected(
                &[player_at(sender, 1, 9)],
                &[SenderWatermark {
                    player_id: sender,
                    epoch: 1,
                    seq: 9,
                }],
            )
            .unwrap();
        assert!(state
            .record_report(&DeliveryReportPayload {
                per_class: counters(1),
                gaps: Vec::new(),
            })
            .is_err());
        assert!(state.record_relay_stats(1_000, 3, 2, 1).is_err());
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters(3),
                gaps: Vec::new(),
            })
            .unwrap();
        state.record_relay_stats(1_000, 5, 3, 2).unwrap();
        state
            .record_game_data(sender, Some(10), Some(1), None, None)
            .unwrap();
        assert!(state
            .record_game_data(sender, Some(12), Some(1), None, None)
            .is_err());
    }

    #[test]
    fn same_epoch_lifecycle_is_idempotent_only_while_sender_is_present() {
        let sender = id(3);
        for watermark_seq in [0, 7] {
            let mut state = DeliveryAccountability::default();
            state
                .rebaseline_reconnected(
                    &[player_at(sender, 4, watermark_seq)],
                    &[SenderWatermark {
                        player_id: sender,
                        epoch: 4,
                        seq: watermark_seq,
                    }],
                )
                .unwrap();

            state.note_player_joined(&player(sender, 4)).unwrap();
            state.note_player_reconnected(sender, Some(4)).unwrap();

            state
                .note_player_left(sender, Some(4), Some(watermark_seq + 1))
                .unwrap();
            assert!(state.note_player_joined(&player(sender, 4)).is_err());
            assert!(state.note_player_reconnected(sender, Some(4)).is_err());
        }
    }

    #[test]
    fn overlapping_ranges_in_one_report_are_rejected_atomically() {
        let sender = id(4);
        let mut state = DeliveryAccountability::default();
        state.note_player_joined(&player(sender, 1)).unwrap();
        state
            .record_game_data(sender, Some(1), Some(1), None, None)
            .unwrap();

        assert!(state
            .record_report(&DeliveryReportPayload {
                per_class: counters(2),
                gaps: vec![gap(sender, 2, 3), gap(sender, 3, 4)],
            })
            .is_err());

        // Neither the counters nor either range from the rejected frame were
        // committed.
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(1),
                gaps: vec![gap(sender, 2, 2)],
            })
            .unwrap();
        state
            .record_game_data(sender, Some(3), Some(1), None, None)
            .unwrap();
    }

    #[test]
    fn priority_lifecycle_control_does_not_invalidate_queued_old_epoch_data() {
        let sender = id(5);
        let mut state = DeliveryAccountability::default();
        state.note_player_joined(&player(sender, 1)).unwrap();
        state
            .record_game_data(sender, Some(1), Some(1), None, None)
            .unwrap();

        state.note_player_left(sender, Some(1), Some(2)).unwrap();
        state.note_player_reconnected(sender, Some(2)).unwrap();
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(2),
                gaps: vec![gap_at_epoch(sender, 2, 1, 2)],
            })
            .unwrap();

        // Old data already queued before priority control still drains first.
        assert_eq!(
            state
                .record_game_data(sender, Some(2), Some(1), None, None)
                .unwrap(),
            GameDataDisposition::Stale
        );
        assert_eq!(
            state
                .record_game_data(sender, Some(3), Some(2), None, None)
                .unwrap(),
            GameDataDisposition::Apply
        );
        assert!(state
            .record_game_data(sender, Some(3), Some(1), None, None)
            .is_err());
        assert!(state
            .record_game_data(sender, Some(1), Some(99), None, None)
            .is_err());
        assert!(state
            .record_report(&DeliveryReportPayload {
                per_class: counters(2),
                gaps: vec![gap_at_epoch(sender, 99, 1, 1)],
            })
            .is_err());

        let mut mismatch = DeliveryAccountability::default();
        mismatch.note_player_joined(&player(sender, 1)).unwrap();
        assert!(mismatch
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(1),
                gaps: Vec::new(),
            })
            .is_err());
    }

    #[test]
    fn player_left_terminal_retires_delivered_and_exactly_omitted_tails() {
        let sender = id(6);
        let mut snapshot_tail = DeliveryAccountability::default();
        snapshot_tail
            .rebaseline_snapshot(&[player_at(sender, 1, 41)])
            .unwrap();
        snapshot_tail
            .note_player_left(sender, Some(1), Some(43))
            .unwrap();
        for seq in [42, 43] {
            assert_eq!(
                snapshot_tail
                    .record_game_data(sender, Some(seq), Some(1), None, None)
                    .unwrap(),
                GameDataDisposition::Stale
            );
        }
        assert!(snapshot_tail.senders.is_empty());
        assert!(snapshot_tail.departed_senders.is_empty());

        let mut delivered_tail = DeliveryAccountability::default();
        delivered_tail
            .note_player_joined(&player(sender, 1))
            .unwrap();
        delivered_tail
            .record_game_data(sender, Some(1), Some(1), None, None)
            .unwrap();
        delivered_tail
            .note_player_left(sender, Some(1), Some(4))
            .unwrap();
        delivered_tail
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(2),
                gaps: vec![gap(sender, 2, 3)],
            })
            .unwrap();
        assert_eq!(
            delivered_tail
                .record_game_data(sender, Some(4), Some(1), None, None)
                .unwrap(),
            GameDataDisposition::Stale
        );
        assert!(delivered_tail
            .record_game_data(sender, Some(5), Some(1), None, None)
            .is_err());

        let mut omitted_tail = DeliveryAccountability::default();
        omitted_tail.note_player_joined(&player(sender, 1)).unwrap();
        omitted_tail
            .note_player_left(sender, Some(1), Some(2))
            .unwrap();
        omitted_tail
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(2),
                gaps: vec![gap(sender, 1, 2)],
            })
            .unwrap();
        assert!(omitted_tail
            .record_game_data(sender, Some(1), Some(1), None, None)
            .is_err());

        let mut beyond = DeliveryAccountability::default();
        beyond.note_player_joined(&player(sender, 1)).unwrap();
        beyond.note_player_left(sender, Some(1), Some(2)).unwrap();
        assert!(beyond
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(3),
                gaps: vec![gap(sender, 1, 3)],
            })
            .is_err());
    }

    #[test]
    fn multiple_overtaking_player_left_epochs_retire_independently() {
        let sender = id(7);
        let mut state = DeliveryAccountability::default();
        state.note_player_joined(&player(sender, 1)).unwrap();
        for epoch in 1..=3 {
            if epoch > 1 {
                state.note_player_reconnected(sender, Some(epoch)).unwrap();
            }
            state
                .note_player_left(sender, Some(epoch), Some(2))
                .unwrap();
        }

        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(2),
                gaps: vec![gap_at_epoch(sender, 2, 1, 2)],
            })
            .unwrap();
        assert_eq!(
            state
                .record_game_data(sender, Some(1), Some(1), None, None)
                .unwrap(),
            GameDataDisposition::Stale
        );
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(3),
                gaps: vec![gap_at_epoch(sender, 1, 2, 2)],
            })
            .unwrap();
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(4),
                gaps: vec![gap_at_epoch(sender, 3, 1, 1)],
            })
            .unwrap();
        assert_eq!(
            state
                .record_game_data(sender, Some(2), Some(3), None, None)
                .unwrap(),
            GameDataDisposition::Stale
        );
        assert!(state.senders.is_empty());
        assert!(state.departed_senders.is_empty());
    }

    #[test]
    fn player_left_terminal_keeps_long_seat_churn_bounded_and_v2_frozen() {
        let mut state = DeliveryAccountability::default();
        for value in 1..=1_024 {
            let sender = id(value);
            state.note_player_joined(&player(sender, 1)).unwrap();
            state.note_player_left(sender, Some(1), Some(0)).unwrap();
        }
        assert!(state.senders.is_empty());
        assert!(state.announced_epochs.is_empty());
        assert!(state.stale_senders.is_empty());
        assert!(state.departed_senders.is_empty());
        assert!(state.pending_gaps.is_empty());

        let mut v2 = DeliveryAccountability::new(false);
        assert!(v2.note_player_left(id(1), None, None).is_ok());
        assert!(v2.note_player_left(id(1), Some(1), Some(0)).is_err());
    }

    #[test]
    fn room_snapshot_allows_a_late_join_baseline_and_reset_forgets_it() {
        let sender = id(3);
        let mut state = DeliveryAccountability::default();
        state
            .rebaseline_snapshot(&[player_at(sender, 4, 89)])
            .unwrap();
        state
            .record_game_data(sender, Some(90), Some(4), None, None)
            .unwrap();
        state.reset_room();
        assert!(state
            .record_game_data(sender, Some(91), Some(4), None, None)
            .is_err());

        // Room transitions do not reset physical-connection counters.
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters(2),
                gaps: Vec::new(),
            })
            .unwrap();
        state.rebaseline_snapshot(&[]).unwrap();
        assert!(state
            .record_report(&DeliveryReportPayload {
                per_class: counters(1),
                gaps: Vec::new(),
            })
            .is_err());
    }

    #[test]
    fn negotiated_mode_requires_exact_snapshot_and_metadata_shapes() {
        let sender = id(6);
        let mut missing_epoch = player(sender, 1);
        missing_epoch.epoch = None;
        missing_epoch.seq = None;

        let mut v3 = DeliveryAccountability::new(true);
        assert!(v3.rebaseline_snapshot(&[missing_epoch.clone()]).is_err());
        let mut missing_seq = player(sender, 1);
        missing_seq.seq = None;
        assert!(v3.rebaseline_snapshot(&[missing_seq]).is_err());
        assert!(v3
            .rebaseline_reconnected(&[player(sender, 1)], &[])
            .is_err());

        let mut v2 = DeliveryAccountability::new(false);
        v2.rebaseline_snapshot(&[missing_epoch]).unwrap();
        v2.observe_server_message(true).unwrap();
        assert_eq!(
            v2.record_game_data(sender, None, None, None, None).unwrap(),
            GameDataDisposition::Apply
        );
        assert!(v2
            .record_game_data(sender, Some(1), Some(1), None, None)
            .is_err());
        assert!(v2
            .record_report(&DeliveryReportPayload {
                per_class: counters(0),
                gaps: Vec::new(),
            })
            .is_err());
    }

    #[test]
    fn cumulative_gap_counters_hold_across_a_256_plus_one_frontier() {
        let sender = id(7);
        let mut state = DeliveryAccountability::default();
        state.note_player_joined(&player(sender, 1)).unwrap();
        let first_frontier: Vec<_> = (0..DELIVERY_REPORT_MAX_GAPS)
            .map(|index| gap(sender, (index as u64) * 2 + 1, (index as u64) * 2 + 1))
            .collect();
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(DELIVERY_REPORT_MAX_GAPS as u64),
                gaps: first_frontier,
            })
            .unwrap();
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(DELIVERY_REPORT_MAX_GAPS as u64 + 1),
                gaps: vec![gap(sender, 513, 513)],
            })
            .unwrap();

        let too_many: Vec<_> = (0..=DELIVERY_REPORT_MAX_GAPS)
            .map(|index| {
                gap(
                    sender,
                    (index as u64) * 2 + 1_001,
                    (index as u64) * 2 + 1_001,
                )
            })
            .collect();
        assert!(state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(DELIVERY_REPORT_MAX_GAPS as u64 * 2 + 2,),
                gaps: too_many,
            })
            .is_err());
    }

    #[test]
    fn snapshot_baseline_validates_only_the_recipient_visible_tail() {
        let sender = id(8);
        let mut state = DeliveryAccountability::default();
        state
            .rebaseline_snapshot(&[player_at(sender, 1, 90)])
            .unwrap();
        state
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(1),
                gaps: vec![gap(sender, 92, 92)],
            })
            .unwrap();
        state
            .record_game_data(sender, Some(91), Some(1), None, None)
            .unwrap();
        state
            .record_game_data(sender, Some(93), Some(1), None, None)
            .unwrap();

        let mut pre_baseline = DeliveryAccountability::default();
        pre_baseline
            .rebaseline_snapshot(&[player_at(sender, 1, 90)])
            .unwrap();
        assert!(pre_baseline
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_superseded(1),
                gaps: vec![gap(sender, 89, 89)],
            })
            .is_err());
    }

    #[test]
    fn unsupported_advisory_requires_a_prior_report_but_not_adjacency() {
        let sender = id(9);
        let report = DeliveryReportPayload {
            per_class: counters_with_unsupported(1),
            gaps: vec![unsupported_gap(sender, 1)],
        };

        let mut paired = DeliveryAccountability::default();
        paired.note_player_joined(&player(sender, 1)).unwrap();
        paired.record_report(&report).unwrap();
        paired.observe_server_message(false).unwrap();
        paired.observe_server_message(true).unwrap();
        paired.observe_server_message(false).unwrap();
        assert!(paired.observe_server_message(true).is_err());

        let mut rollover = DeliveryAccountability::default();
        rollover.note_player_joined(&player(sender, 1)).unwrap();
        rollover.record_report(&report).unwrap();
        rollover
            .record_report(&DeliveryReportPayload {
                per_class: counters_with_unsupported(2),
                gaps: vec![unsupported_gap(sender, 2)],
            })
            .unwrap();
        rollover.observe_server_message(true).unwrap();

        let mut terminal = DeliveryAccountability::default();
        terminal.note_player_joined(&player(sender, 1)).unwrap();
        terminal.record_report(&report).unwrap();
        terminal.observe_terminal();

        let mut mixed = DeliveryAccountability::default();
        mixed.note_player_joined(&player(sender, 1)).unwrap();
        assert!(mixed
            .record_report(&DeliveryReportPayload {
                per_class: {
                    let mut value = counters_with_unsupported(1);
                    value.latest.superseded = 1;
                    value
                },
                gaps: vec![unsupported_gap(sender, 1), gap(sender, 2, 2)],
            })
            .is_err());
    }

    #[test]
    fn relay_stats_are_positive_stable_and_cumulative_per_connection() {
        let mut valid = DeliveryAccountability::default();
        valid.record_relay_stats(1_000, 4, 2, 1).unwrap();
        valid.record_relay_stats(1_000, 5, 2, 3).unwrap();
        valid.reset_room();
        valid.record_relay_stats(1_000, 5, 3, 3).unwrap();

        let invalid = [
            (
                "zero interval",
                None,
                [0, 0, 0, 0],
                "interval_ms must be positive",
            ),
            (
                "changed interval",
                Some([1_000, 4, 2, 1]),
                [2_000, 4, 2, 1],
                "interval_ms changed",
            ),
            (
                "sent moved backward",
                Some([1_000, 4, 2, 1]),
                [1_000, 3, 2, 1],
                "counters moved backward",
            ),
            (
                "dropped moved backward",
                Some([1_000, 4, 2, 1]),
                [1_000, 4, 1, 1],
                "counters moved backward",
            ),
            (
                "backpressure moved backward",
                Some([1_000, 4, 2, 1]),
                [1_000, 4, 2, 0],
                "counters moved backward",
            ),
        ];
        for (name, first, next, expected) in invalid {
            let mut state = DeliveryAccountability::default();
            if let Some([interval, sent, dropped, backpressure]) = first {
                state
                    .record_relay_stats(interval, sent, dropped, backpressure)
                    .unwrap();
            }
            let [interval, sent, dropped, backpressure] = next;
            let error = state
                .record_relay_stats(interval, sent, dropped, backpressure)
                .unwrap_err();
            assert!(error.contains(expected), "{name}: {error}");
        }

        let mut v2 = DeliveryAccountability::new(false);
        assert!(v2.record_relay_stats(1_000, 0, 0, 0).is_err());

        valid.reset_connection();
        valid.record_relay_stats(2_000, 0, 0, 0).unwrap();
    }
}
