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
        "Server messages dropped because the outbound WebSocket buffer was full",
        snapshot.connections.websocket_messages_dropped,
    );

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
        "Total cross-instance messages processed",
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
        "Total membership deltas/snapshots published to the cross-instance bus",
        snapshot.cross_instance.remote_membership_updates_published,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_updates_received_total",
        "Total membership deltas/snapshots consumed from the cross-instance bus",
        snapshot.cross_instance.remote_membership_updates_received,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_known_broadcasts_total",
        "Room broadcasts sent because remote members are known to be listening",
        snapshot.cross_instance.remote_membership_known_broadcasts,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_forced_broadcasts_total",
        "Room broadcasts sent without cached remote membership info (safety fallback)",
        snapshot.cross_instance.remote_membership_forced_broadcasts,
    );
    counter(
        &mut buf,
        "signal_fish_cross_instance_membership_skipped_broadcasts_total",
        "Room broadcasts skipped because no remote listeners are known",
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
}
