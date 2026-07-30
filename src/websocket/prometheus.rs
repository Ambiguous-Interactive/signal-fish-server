use crate::metrics::{MetricsSnapshot, OperationLatencyMetrics};
use chrono::Utc;

/// Render unified metrics snapshot into Prometheus text exposition format.
pub(crate) fn render_prometheus_metrics(snapshot: &MetricsSnapshot) -> String {
    use std::fmt::Write;

    fn write_metric(buf: &mut String, name: &str, help: &str, metric_type: &str, value: f64) {
        let _ = writeln!(buf, "# HELP {name} {help}");
        let _ = writeln!(buf, "# TYPE {name} {metric_type}");
        let _ = writeln!(buf, "{name} {value}");
    }

    fn counter(buf: &mut String, name: &str, help: &str, value: u64) {
        write_metric(buf, name, help, "counter", value as f64);
    }

    fn gauge(buf: &mut String, name: &str, help: &str, value: u64) {
        write_metric(buf, name, help, "gauge", value as f64);
    }

    fn gauge_f64(buf: &mut String, name: &str, help: &str, value: f64) {
        write_metric(buf, name, help, "gauge", value);
    }

    fn emit_latency_metrics(
        buf: &mut String,
        metric_prefix: &str,
        description: &str,
        metrics: &OperationLatencyMetrics,
    ) {
        if let Some(value) = metrics.average_ms {
            gauge_f64(
                buf,
                &format!("{metric_prefix}_average_ms"),
                &format!("Average {description} latency in milliseconds"),
                value,
            );
        }
        if let Some(value) = metrics.p50_ms {
            gauge_f64(
                buf,
                &format!("{metric_prefix}_p50_ms"),
                &format!("p50 {description} latency in milliseconds"),
                value,
            );
        }
        if let Some(value) = metrics.p95_ms {
            gauge_f64(
                buf,
                &format!("{metric_prefix}_p95_ms"),
                &format!("p95 {description} latency in milliseconds"),
                value,
            );
        }
        if let Some(value) = metrics.p99_ms {
            gauge_f64(
                buf,
                &format!("{metric_prefix}_p99_ms"),
                &format!("p99 {description} latency in milliseconds"),
                value,
            );
        }
        if let Some(value) = metrics.min_ms {
            gauge_f64(
                buf,
                &format!("{metric_prefix}_min_ms"),
                &format!("Minimum observed {description} latency in milliseconds"),
                value,
            );
        }
        if let Some(value) = metrics.max_ms {
            gauge_f64(
                buf,
                &format!("{metric_prefix}_max_ms"),
                &format!("Maximum observed {description} latency in milliseconds"),
                value,
            );
        }
        counter(
            buf,
            &format!("{metric_prefix}_samples_total"),
            &format!("Total samples recorded for {description} latency calculations"),
            metrics.sample_count,
        );
    }

    let mut buf = String::new();

    counter(
        &mut buf,
        "signal_fish_connections_total",
        "Total connections accepted since startup",
        snapshot.connections.total_connections,
    );
    gauge(
        &mut buf,
        "signal_fish_connections_active",
        "Number of currently active connections",
        snapshot.connections.active_connections,
    );
    counter(
        &mut buf,
        "signal_fish_connections_disconnections_total",
        "Total connection closures observed since startup",
        snapshot.connections.disconnections,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_messages_dropped_total",
        "Server messages that could not be delivered: abandoned together with a slow-consumer or already-closing connection, or replaced by an error frame because a binary payload could not be converted for the recipient",
        snapshot.connections.websocket_messages_dropped,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_backpressure_events_total",
        "Times a full outbound queue forced delivery to wait for capacity (message still delivered)",
        snapshot.connections.websocket_backpressure_events,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_slow_consumer_disconnects_total",
        "Connections force-closed because outbound delivery could not make accountable progress",
        snapshot.connections.websocket_slow_consumer_disconnects,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_ping_timeouts_total",
        "Server-initiated WebSocket pings that missed their matching Pong deadline",
        snapshot.connections.websocket_ping_timeouts,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_ping_probes_skipped_activity_total",
        "Scheduled WebSocket liveness probes skipped because inbound activity already proved liveness",
        snapshot.connections.websocket_ping_probes_skipped_activity,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_ping_probes_cancelled_activity_total",
        "Outstanding WebSocket liveness probes cancelled by inbound activity or completed outbound application writes",
        snapshot
            .connections
            .websocket_ping_probes_cancelled_activity,
    );
    emit_latency_metrics(
        &mut buf,
        "signal_fish_websocket_ping_rtt",
        "server-initiated WebSocket ping round-trip",
        &snapshot.connections.websocket_ping_rtt,
    );
    // Delivery conservation counters: together with the drop counter above,
    // enqueued + channel_closed + canceled <= attempts <=
    // enqueued + channel_closed + canceled + dropped at any quiescent point
    // (drops also cover messages abandoned after enqueue).
    counter(
        &mut buf,
        "signal_fish_websocket_delivery_attempts_total",
        "Delivery attempts routed through reliable server delivery paths (one per message per recipient)",
        snapshot.connections.websocket_delivery_attempts,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_deliveries_enqueued_total",
        "Delivery attempts enqueued on the recipient's outbound queue (fast path or after backpressure)",
        snapshot.connections.websocket_deliveries_enqueued,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_deliveries_channel_closed_total",
        "Delivery attempts that found the recipient's connection already closing (a normal disconnect race, not a delivery fault)",
        snapshot.connections.websocket_deliveries_channel_closed,
    );
    counter(
        &mut buf,
        "signal_fish_websocket_deliveries_canceled_total",
        "Conditional delivery attempts canceled before enqueue because the commit condition no longer held: shutdown drain, caller predicate, or recipient snapshot (not a delivery fault)",
        snapshot.connections.websocket_deliveries_canceled,
    );
    let _ = writeln!(
        buf,
        "# HELP signal_fish_websocket_delivery_class_outcomes_total Per-class accountable game-data outcomes; at quiescence attempted equals the sum of terminal outcomes for each class"
    );
    let _ = writeln!(
        buf,
        "# TYPE signal_fish_websocket_delivery_class_outcomes_total counter"
    );
    for (class, metrics) in [
        ("reliable", &snapshot.connections.delivery_by_class.reliable),
        ("latest", &snapshot.connections.delivery_by_class.latest),
        ("volatile", &snapshot.connections.delivery_by_class.volatile),
    ] {
        for (outcome, value) in [
            ("attempted", metrics.attempted),
            ("delivered", metrics.delivered),
            ("superseded", metrics.superseded),
            ("dropped_full", metrics.dropped_full),
            ("dropped", metrics.dropped),
            ("abandoned", metrics.abandoned),
            ("unsupported_format", metrics.unsupported_format),
        ] {
            let _ = writeln!(
                buf,
                "signal_fish_websocket_delivery_class_outcomes_total{{class=\"{class}\",outcome=\"{outcome}\"}} {value}"
            );
        }
    }

    counter(
        &mut buf,
        "signal_fish_rooms_created_total",
        "Total rooms created since startup",
        snapshot.rooms.rooms_created,
    );
    counter(
        &mut buf,
        "signal_fish_rooms_joined_total",
        "Total room joins processed since startup",
        snapshot.rooms.rooms_joined,
    );
    counter(
        &mut buf,
        "signal_fish_rooms_deleted_total",
        "Total rooms deleted since startup",
        snapshot.rooms.rooms_deleted,
    );
    counter(
        &mut buf,
        "signal_fish_room_cap_lock_acquisitions_total",
        "Successful acquisitions of the per-game room-cap distributed lock",
        snapshot.rooms.room_cap_lock_acquisitions,
    );
    counter(
        &mut buf,
        "signal_fish_room_cap_lock_failures_total",
        "Failed attempts to acquire the per-game room-cap distributed lock",
        snapshot.rooms.room_cap_lock_failures,
    );
    counter(
        &mut buf,
        "signal_fish_room_cap_denials_total",
        "Room creation attempts rejected because the per-game room cap was reached",
        snapshot.rooms.room_cap_denials,
    );

    counter(
        &mut buf,
        "signal_fish_rate_limit_rejections_total",
        "Total requests rejected by rate limiting",
        snapshot.rate_limiting.rate_limit_rejections,
    );
    counter(
        &mut buf,
        "signal_fish_rate_limit_resets_total",
        "Total rate limit resets processed",
        snapshot.rate_limiting.rate_limit_resets,
    );
    gauge(
        &mut buf,
        "signal_fish_rate_limit_minute_limit",
        "Configured per-minute request limit",
        snapshot.rate_limiting.minute_limit,
    );
    gauge(
        &mut buf,
        "signal_fish_rate_limit_minute_used",
        "Requests counted in the current minute window",
        snapshot.rate_limiting.minute_count,
    );
    gauge(
        &mut buf,
        "signal_fish_rate_limit_hour_limit",
        "Configured per-hour request limit",
        snapshot.rate_limiting.hour_limit,
    );
    gauge(
        &mut buf,
        "signal_fish_rate_limit_hour_used",
        "Requests counted in the current hour window",
        snapshot.rate_limiting.hour_count,
    );
    gauge(
        &mut buf,
        "signal_fish_rate_limit_day_limit",
        "Configured per-day request limit",
        snapshot.rate_limiting.day_limit,
    );
    gauge(
        &mut buf,
        "signal_fish_rate_limit_day_used",
        "Requests counted in the current day window",
        snapshot.rate_limiting.day_count,
    );

    counter(
        &mut buf,
        "signal_fish_queries_total",
        "Total queries issued via the signaling server",
        snapshot.performance.query_count,
    );
    emit_latency_metrics(
        &mut buf,
        "signal_fish_room_creation_latency",
        "room creation",
        &snapshot.performance.room_creation_latency,
    );
    emit_latency_metrics(
        &mut buf,
        "signal_fish_room_join_latency",
        "room join",
        &snapshot.performance.room_join_latency,
    );
    emit_latency_metrics(
        &mut buf,
        "signal_fish_query_latency",
        "query",
        &snapshot.performance.query_latency,
    );
    counter(
        &mut buf,
        "signal_fish_latency_clamped_samples_total",
        "Latency samples that exceeded the histogram tracking range",
        snapshot.performance.latency_histogram_clamped_samples,
    );

    counter(
        &mut buf,
        "signal_fish_errors_total",
        "Total errors encountered since startup",
        snapshot.errors.total_errors,
    );
    counter(
        &mut buf,
        "signal_fish_errors_internal_total",
        "Internal errors encountered since startup",
        snapshot.errors.internal_errors,
    );
    counter(
        &mut buf,
        "signal_fish_errors_websocket_total",
        "WebSocket errors encountered since startup",
        snapshot.errors.websocket_errors,
    );
    counter(
        &mut buf,
        "signal_fish_errors_validation_total",
        "Protocol validation errors encountered since startup",
        snapshot.errors.validation_errors,
    );

    gauge(
        &mut buf,
        "signal_fish_players_active",
        "Number of players currently marked as active",
        snapshot
            .players
            .players_joined
            .saturating_sub(snapshot.players.players_left),
    );
    counter(
        &mut buf,
        "signal_fish_players_joined_total",
        "Total players joined since startup",
        snapshot.players.players_joined,
    );
    counter(
        &mut buf,
        "signal_fish_players_left_total",
        "Total players disconnected since startup",
        snapshot.players.players_left,
    );
    counter(
        &mut buf,
        "signal_fish_game_data_messages_total",
        "Total game data messages forwarded through the relays",
        snapshot.players.game_data_messages,
    );
    counter(
        &mut buf,
        "signal_fish_reconnection_tokens_issued_total",
        "Total reconnection tokens minted for disconnected players",
        snapshot.reconnection.tokens_issued,
    );
    gauge(
        &mut buf,
        "signal_fish_reconnection_sessions_active",
        "Number of disconnected players awaiting reconnection",
        snapshot.reconnection.sessions_active,
    );
    counter(
        &mut buf,
        "signal_fish_reconnection_validation_failures_total",
        "Total reconnection attempts rejected due to invalid tokens or expirations",
        snapshot.reconnection.validations_failed,
    );
    counter(
        &mut buf,
        "signal_fish_reconnection_completions_total",
        "Total reconnections completed successfully",
        snapshot.reconnection.completions,
    );
    counter(
        &mut buf,
        "signal_fish_reconnection_events_buffered_total",
        "Total lobby events buffered for reconnecting players",
        snapshot.reconnection.events_buffered,
    );
    counter(
        &mut buf,
        "signal_fish_reconnection_events_evicted_total",
        "Control events evicted from a replay ring while a reconnection was pending (that player's missed_events arrives truncated)",
        snapshot.reconnection.events_evicted,
    );
    counter(
        &mut buf,
        "signal_fish_distributed_lock_release_failures_total",
        "Total distributed-lock release attempts that failed due to stale handles",
        snapshot.distributed_lock.release_failures,
    );
    counter(
        &mut buf,
        "signal_fish_distributed_lock_extend_failures_total",
        "Total distributed-lock extend attempts rejected due to stale handles",
        snapshot.distributed_lock.extend_failures,
    );
    counter(
        &mut buf,
        "signal_fish_distributed_lock_cleanup_runs_total",
        "Total cleanup executions for distributed locks",
        snapshot.distributed_lock.cleanup_runs,
    );
    counter(
        &mut buf,
        "signal_fish_distributed_lock_cleanup_removed_total",
        "Total expired distributed locks removed via cleanup",
        snapshot.distributed_lock.cleanup_removed,
    );

    counter(
        &mut buf,
        "signal_fish_cleanup_empty_rooms_total",
        "Total rooms deleted because they were empty past the configured timeout",
        snapshot.cleanup.empty_rooms_cleaned,
    );
    counter(
        &mut buf,
        "signal_fish_cleanup_inactive_rooms_total",
        "Total rooms deleted because they stayed inactive despite players",
        snapshot.cleanup.inactive_rooms_cleaned,
    );
    counter(
        &mut buf,
        "signal_fish_cleanup_expired_players_total",
        "Total players disconnected by the cleanup task after missing heartbeats",
        snapshot.cleanup.expired_players_cleaned,
    );

    counter(
        &mut buf,
        "signal_fish_cross_instance_messages_total",
        "Reserved remote-coordination envelopes processed (zero for the shipped in-memory backend)",
        snapshot.cross_instance.cross_instance_messages,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_dedup_hits_total",
        "Total deduplication cache hits",
        snapshot.cross_instance.dedup_cache_hits,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_dedup_misses_total",
        "Total deduplication cache misses",
        snapshot.cross_instance.dedup_cache_misses,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_dedup_evictions_total",
        "Total deduplication cache evictions",
        snapshot.cross_instance.dedup_cache_evictions,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_cache_hits_total",
        "Total membership cache hits within the message coordinator",
        snapshot.cross_instance.membership_cache_hits,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_cache_misses_total",
        "Total membership cache misses within the message coordinator",
        snapshot.cross_instance.membership_cache_misses,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_updates_published_total",
        "Reserved membership updates published to a remote-coordination backend",
        snapshot.cross_instance.remote_membership_updates_published,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_updates_received_total",
        "Reserved membership updates consumed from a remote-coordination backend",
        snapshot.cross_instance.remote_membership_updates_received,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_known_broadcasts_total",
        "Reserved room broadcasts sent to known remote members",
        snapshot.cross_instance.remote_membership_known_broadcasts,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_forced_broadcasts_total",
        "Reserved remote broadcasts sent without cached membership information",
        snapshot.cross_instance.remote_membership_forced_broadcasts,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_skipped_broadcasts_total",
        "Reserved remote broadcasts skipped because no listeners are known",
        snapshot.cross_instance.remote_membership_skipped_broadcasts,
    );

    counter(
        &mut buf,
        "signal_fish_relay_client_id_reuse_total",
        "Total times relay client IDs were recycled to service churn",
        snapshot.relay_health.client_id_reuse_events,
    );
    counter(
        &mut buf,
        "signal_fish_relay_client_id_exhaustion_total",
        "Total occasions where relay client IDs were exhausted",
        snapshot.relay_health.client_id_exhaustion_events,
    );

    counter(
        &mut buf,
        "signal_fish_transport_session_plans_emitted_total",
        "Finalization-time v3 SessionPlan publications, including relay-floor plans (one per room with v3 recipients)",
        snapshot.transport.session_plans_emitted,
    );
    counter(
        &mut buf,
        "signal_fish_transport_session_replans_emitted_total",
        "Mid-session host re-plan events (departure failover or late-join self-heal; one per event, not per recipient)",
        snapshot.transport.session_replans_emitted,
    );
    counter(
        &mut buf,
        "signal_fish_transport_session_plans_late_join_total",
        "SessionPlans delivered to late joiners or reconnectors of already-active sessions",
        snapshot.transport.session_plans_late_join,
    );
    counter(
        &mut buf,
        "signal_fish_transport_topology_mesh_selected_total",
        "Finalized rooms whose chosen session topology was mesh",
        snapshot.transport.topology_mesh_selected,
    );
    counter(
        &mut buf,
        "signal_fish_transport_topology_host_selected_total",
        "Finalized rooms whose chosen session topology was host",
        snapshot.transport.topology_host_selected,
    );
    counter(
        &mut buf,
        "signal_fish_transport_topology_relay_selected_total",
        "Finalized rooms that resolved to the relay floor topology",
        snapshot.transport.topology_relay_selected,
    );
    counter(
        &mut buf,
        "signal_fish_transport_webrtc_selected_total",
        "Finalized rooms whose chosen data-path transport was webrtc",
        snapshot.transport.transport_webrtc_selected,
    );
    counter(
        &mut buf,
        "signal_fish_transport_direct_selected_total",
        "Finalized rooms whose chosen data-path transport was direct",
        snapshot.transport.transport_direct_selected,
    );
    counter(
        &mut buf,
        "signal_fish_transport_relay_selected_total",
        "Finalized rooms that resolved to the relay floor transport",
        snapshot.transport.transport_relay_selected,
    );
    counter(
        &mut buf,
        "signal_fish_transport_p2p_established_total",
        "First TransportStatus reports or state transitions clients reported as established P2P",
        snapshot.transport.p2p_established,
    );
    counter(
        &mut buf,
        "signal_fish_transport_relay_fallback_total",
        "First TransportStatus reports or state transitions clients reported as relay fallback",
        snapshot.transport.relay_fallback,
    );
    counter(
        &mut buf,
        "signal_fish_transport_signals_relayed_total",
        "Opaque WebRTC Signal messages accepted for best-effort dispatch to same-room WebRTC peers",
        snapshot.transport.signals_relayed,
    );
    counter(
        &mut buf,
        "signal_fish_transport_turn_credentials_issued_total",
        "Ephemeral TURN credentials minted into SessionPlans and pre-gather RoomJoined/Reconnected ICE lists",
        snapshot.transport.turn_credentials_issued,
    );
    counter(
        &mut buf,
        "signal_fish_transport_status_fanout_total",
        "PeerTransportStatus fan-out events: accepted TransportStatus state changes from in-room clients fanned out to v3 room peers (one per event, not per recipient)",
        snapshot.transport.transport_status_fanout,
    );
    counter(
        &mut buf,
        "signal_fish_transport_ice_pregather_emitted_total",
        "RoomJoined/Reconnected payloads that carried a non-empty ICE pre-gather list (one per carrying payload)",
        snapshot.transport.ice_pregather_emitted,
    );

    let cache_age_seconds = {
        let last_refresh = snapshot.dashboard_cache.last_refresh_timestamp;
        if last_refresh == 0 {
            0
        } else {
            let now = Utc::now().timestamp().max(0) as u64;
            now.saturating_sub(last_refresh)
        }
    };
    gauge(
        &mut buf,
        "signal_fish_dashboard_cache_age_seconds",
        "Age of the cached dashboard metrics snapshot",
        cache_age_seconds,
    );
    counter(
        &mut buf,
        "signal_fish_dashboard_cache_refresh_failures_total",
        "Total dashboard metrics cache refresh failures",
        snapshot.dashboard_cache.refresh_failures,
    );

    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{RateLimitWindow, ServerMetrics};
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn prometheus_metrics_survive_promtool_check() {
        let promtool_path = match std::env::var_os("PROMTOOL") {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => {
                eprintln!(
                    "PROMTOOL environment variable not set; skipping promtool validation test."
                );
                return;
            }
        };

        let metrics = ServerMetrics::new();
        let snapshot = metrics.snapshot().await;
        let rendered = render_prometheus_metrics(&snapshot);

        let mut child = Command::new(promtool_path)
            .arg("check")
            .arg("metrics")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to start promtool");

        {
            let stdin = child.stdin.as_mut().expect("stdin missing for promtool");
            stdin
                .write_all(rendered.as_bytes())
                .expect("failed to write metrics payload to promtool stdin");
        }

        let output = child
            .wait_with_output()
            .expect("failed to wait for promtool result");

        assert!(
            output.status.success(),
            "promtool reported an invalid metrics payload\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_render_prometheus_metrics_includes_core_counters() {
        let metrics = ServerMetrics::new();
        metrics.increment_connections();
        metrics.increment_connections();
        metrics.decrement_active_connections();
        metrics.record_rate_limit_limit(RateLimitWindow::Minute, 120);
        metrics.record_rate_limit_usage(RateLimitWindow::Minute, 42);
        metrics.record_rate_limit_check(RateLimitWindow::Minute);
        metrics.record_rate_limit_rejection(RateLimitWindow::Minute);
        metrics.increment_query_count();

        let snapshot = metrics.snapshot().await;
        let rendered = render_prometheus_metrics(&snapshot);

        assert!(
            rendered.contains("signal_fish_connections_total 2"),
            "expected connections counter line"
        );
        assert!(
            rendered.contains("signal_fish_rate_limit_minute_limit 120"),
            "expected minute limit gauge"
        );
        assert!(
            rendered.contains("signal_fish_rate_limit_rejections_total 1"),
            "expected rate limit rejection counter"
        );
        assert!(
            rendered.contains("# TYPE signal_fish_queries_total counter"),
            "expected queries metric type"
        );
        assert!(
            rendered.contains("signal_fish_websocket_messages_dropped_total 0"),
            "expected websocket drop counter line"
        );
        assert!(
            rendered.contains("signal_fish_websocket_ping_timeouts_total 0"),
            "expected websocket ping timeout counter line"
        );
        assert!(
            rendered.contains("signal_fish_websocket_ping_probes_skipped_activity_total 0"),
            "expected activity-skipped websocket ping counter line"
        );
        assert!(
            rendered.contains("signal_fish_websocket_ping_probes_cancelled_activity_total 0"),
            "expected activity-cancelled websocket ping counter line"
        );
        assert!(
            rendered.contains("signal_fish_websocket_ping_rtt_samples_total 0"),
            "expected websocket ping RTT histogram sample line"
        );
        assert!(
            rendered.contains("signal_fish_dashboard_cache_age_seconds"),
            "expected dashboard cache age gauge"
        );
        assert!(
            rendered.contains("signal_fish_dashboard_cache_refresh_failures_total 0"),
            "expected dashboard cache failure counter"
        );
        assert!(
            rendered.contains("signal_fish_room_creation_latency_samples_total 0"),
            "expected room creation latency sample counter"
        );
        assert!(
            rendered.contains("signal_fish_room_join_latency_samples_total 0"),
            "expected room join latency sample counter"
        );
        assert!(
            rendered.contains("signal_fish_query_latency_samples_total 0"),
            "expected query latency sample counter"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_render_prometheus_metrics_includes_delivery_conservation_counters() {
        let metrics = ServerMetrics::new();
        // Drive each conservation counter to a distinct, non-default value so
        // the rendered lines are unambiguous, shaped like a real trace: six
        // attempts resolving as three enqueued, one channel-closed, one
        // canceled, and one slow-consumer drop.
        for _ in 0..6 {
            metrics.increment_websocket_delivery_attempts();
        }
        for _ in 0..3 {
            metrics.increment_websocket_deliveries_enqueued();
        }
        metrics.increment_websocket_deliveries_channel_closed();
        metrics.increment_websocket_deliveries_canceled();
        metrics.increment_websocket_messages_dropped();

        use crate::protocol::DeliveryClass;
        metrics.increment_delivery_class_attempted(DeliveryClass::Reliable);
        metrics.increment_delivery_class_unsupported_format(DeliveryClass::Reliable);
        for _ in 0..2 {
            metrics.increment_delivery_class_attempted(DeliveryClass::Latest);
        }
        metrics.increment_delivery_class_delivered(DeliveryClass::Latest);
        metrics.increment_delivery_class_superseded();
        metrics.increment_delivery_class_attempted(DeliveryClass::Volatile);
        metrics.increment_delivery_class_dropped(DeliveryClass::Volatile);

        let snapshot = metrics.snapshot().await;
        let rendered = render_prometheus_metrics(&snapshot);

        // Exact HELP assertions keep operator semantics from drifting: these
        // four counters (plus the drop counter) carry the delivery
        // conservation law, so their meanings must stay precise.
        let expectations = [
            (
                "signal_fish_websocket_delivery_attempts_total",
                "Delivery attempts routed through reliable server delivery paths (one per message per recipient)",
                6u64,
            ),
            (
                "signal_fish_websocket_deliveries_enqueued_total",
                "Delivery attempts enqueued on the recipient's outbound queue (fast path or after backpressure)",
                3,
            ),
            (
                "signal_fish_websocket_deliveries_channel_closed_total",
                "Delivery attempts that found the recipient's connection already closing (a normal disconnect race, not a delivery fault)",
                1,
            ),
            (
                "signal_fish_websocket_deliveries_canceled_total",
                "Conditional delivery attempts canceled before enqueue because the commit condition no longer held: shutdown drain, caller predicate, or recipient snapshot (not a delivery fault)",
                1,
            ),
        ];

        for (name, help, value) in expectations {
            assert!(
                rendered.contains(&format!("# HELP {name} {help}")),
                "missing exact HELP line for {name}"
            );
            assert!(
                rendered.contains(&format!("# TYPE {name} counter")),
                "missing TYPE counter line for {name}"
            );
            assert!(
                rendered.contains(&format!("{name} {value}")),
                "missing value line `{name} {value}`"
            );
        }

        for (class, outcome, value) in [
            ("reliable", "attempted", 1),
            ("reliable", "unsupported_format", 1),
            ("latest", "attempted", 2),
            ("latest", "delivered", 1),
            ("latest", "superseded", 1),
            ("volatile", "attempted", 1),
            ("volatile", "dropped", 1),
        ] {
            let sample = format!(
                "signal_fish_websocket_delivery_class_outcomes_total{{class=\"{class}\",outcome=\"{outcome}\"}} {value}"
            );
            assert!(rendered.contains(&sample), "missing sample `{sample}`");
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_render_prometheus_metrics_includes_reconnection_replay_counters() {
        let metrics = ServerMetrics::new();
        // Drive the replay-ring counters to distinct, non-default values shaped
        // like a real trace: five control events buffered while reconnections
        // were pending, two of which the ring later evicted.
        for _ in 0..5 {
            metrics.add_reconnection_events_buffered(1);
        }
        metrics.add_reconnection_events_evicted(2);

        let snapshot = metrics.snapshot().await;
        let rendered = render_prometheus_metrics(&snapshot);

        // Exact HELP assertions keep operator semantics from drifting: the
        // eviction counter is the capacity alarm for `event_buffer_size`, so
        // its meaning must stay precise.
        let expectations = [
            (
                "signal_fish_reconnection_events_buffered_total",
                "Total lobby events buffered for reconnecting players",
                5u64,
            ),
            (
                "signal_fish_reconnection_events_evicted_total",
                "Control events evicted from a replay ring while a reconnection was pending (that player's missed_events arrives truncated)",
                2,
            ),
        ];

        for (name, help, value) in expectations {
            assert!(
                rendered.contains(&format!("# HELP {name} {help}")),
                "missing exact HELP line for {name}"
            );
            assert!(
                rendered.contains(&format!("# TYPE {name} counter")),
                "missing TYPE counter line for {name}"
            );
            assert!(
                rendered.contains(&format!("{name} {value}")),
                "missing value line `{name} {value}`"
            );
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_render_prometheus_metrics_includes_transport_counters() {
        use crate::protocol::{Topology, Transport};

        let metrics = ServerMetrics::new();
        // Drive every P5 transport counter to a distinct, non-default value so the
        // rendered lines are unambiguous.
        metrics.record_topology_selected(Topology::Mesh);
        metrics.record_transport_selected(Transport::WebRtc);
        metrics.increment_session_plans_emitted();
        metrics.increment_session_replans_emitted();
        metrics.increment_session_replans_emitted();
        metrics.increment_session_plans_late_join();
        metrics.increment_session_plans_late_join();
        metrics.increment_session_plans_late_join();
        metrics.increment_session_plans_late_join();
        metrics.record_topology_selected(Topology::Host);
        metrics.record_transport_selected(Transport::Direct);
        metrics.record_topology_selected(Topology::Relay);
        metrics.record_transport_selected(Transport::Relay);
        metrics.record_p2p_established();
        metrics.record_relay_fallback();
        metrics.increment_signals_relayed();
        metrics.add_turn_credentials_issued(3);
        for _ in 0..5 {
            metrics.record_transport_status_fanout();
        }
        for _ in 0..6 {
            metrics.increment_ice_pregather_emitted();
        }

        let snapshot = metrics.snapshot().await;
        let rendered = render_prometheus_metrics(&snapshot);

        // Each new metric name must be present with its exact HELP, a TYPE
        // counter line, and its value. Exact HELP assertions keep operator
        // semantics from drifting (for example, best-effort dispatch is not
        // guaranteed end-to-end delivery).
        let expectations = [
            (
                "signal_fish_transport_session_plans_emitted_total",
                "Finalization-time v3 SessionPlan publications, including relay-floor plans (one per room with v3 recipients)",
                1u64,
            ),
            (
                "signal_fish_transport_session_replans_emitted_total",
                "Mid-session host re-plan events (departure failover or late-join self-heal; one per event, not per recipient)",
                2,
            ),
            (
                "signal_fish_transport_session_plans_late_join_total",
                "SessionPlans delivered to late joiners or reconnectors of already-active sessions",
                4,
            ),
            (
                "signal_fish_transport_topology_mesh_selected_total",
                "Finalized rooms whose chosen session topology was mesh",
                1,
            ),
            (
                "signal_fish_transport_topology_host_selected_total",
                "Finalized rooms whose chosen session topology was host",
                1,
            ),
            (
                "signal_fish_transport_topology_relay_selected_total",
                "Finalized rooms that resolved to the relay floor topology",
                1,
            ),
            (
                "signal_fish_transport_webrtc_selected_total",
                "Finalized rooms whose chosen data-path transport was webrtc",
                1,
            ),
            (
                "signal_fish_transport_direct_selected_total",
                "Finalized rooms whose chosen data-path transport was direct",
                1,
            ),
            (
                "signal_fish_transport_relay_selected_total",
                "Finalized rooms that resolved to the relay floor transport",
                1,
            ),
            (
                "signal_fish_transport_p2p_established_total",
                "First TransportStatus reports or state transitions clients reported as established P2P",
                1,
            ),
            (
                "signal_fish_transport_relay_fallback_total",
                "First TransportStatus reports or state transitions clients reported as relay fallback",
                1,
            ),
            (
                "signal_fish_transport_signals_relayed_total",
                "Opaque WebRTC Signal messages accepted for best-effort dispatch to same-room WebRTC peers",
                1,
            ),
            (
                "signal_fish_transport_turn_credentials_issued_total",
                "Ephemeral TURN credentials minted into SessionPlans and pre-gather RoomJoined/Reconnected ICE lists",
                3,
            ),
            (
                "signal_fish_transport_status_fanout_total",
                "PeerTransportStatus fan-out events: accepted TransportStatus state changes from in-room clients fanned out to v3 room peers (one per event, not per recipient)",
                5,
            ),
            (
                "signal_fish_transport_ice_pregather_emitted_total",
                "RoomJoined/Reconnected payloads that carried a non-empty ICE pre-gather list (one per carrying payload)",
                6,
            ),
        ];

        for (name, help, value) in expectations {
            assert!(
                rendered.contains(&format!("# HELP {name} {help}")),
                "missing exact HELP line for {name}"
            );
            assert!(
                rendered.contains(&format!("# TYPE {name} counter")),
                "missing TYPE counter line for {name}"
            );
            assert!(
                rendered.contains(&format!("{name} {value}")),
                "missing value line `{name} {value}`"
            );
        }

        // Sanity: the body lines (non-comment) are well-formed `name value` pairs.
        for line in rendered.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split(' ').collect();
            assert_eq!(
                parts.len(),
                2,
                "exposition body line must be `name value`: {line:?}"
            );
            assert!(
                parts[1].parse::<f64>().is_ok(),
                "metric value must be numeric: {line:?}"
            );
        }
    }
}
