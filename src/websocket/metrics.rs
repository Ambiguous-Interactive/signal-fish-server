use crate::server::EnhancedGameServer;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::prometheus::render_prometheus_metrics;

/// Minimum quiet period between emitted metrics-endpoint warnings.
///
/// These endpoints are log-volume amplification vectors: every admitted log
/// line reaches the operator's sink(s), there is no HTTP rate limiter on the
/// routes, and one JSON log line per event would let a request loop grow log
/// volume indefinitely (anonymous rejection loops before any credential guess
/// matters; dashboard polls against a persistently oversized response). Sixty
/// seconds keeps the first signal and a periodic suppressed-count summary at
/// negligible volume.
const REJECTION_LOG_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// One decision to emit a throttled metrics-endpoint warning.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RejectionLogEmission<'a> {
    /// The first event after a quiet period (or ever).
    First { reason: &'a str },
    /// A quiet-period boundary reached while earlier same-reason events were
    /// suppressed; the count summarizes them.
    WithSuppressedCount { reason: &'a str, suppressed: u64 },
}

/// Emits at most one warning per [`REJECTION_LOG_MIN_INTERVAL`] per instance,
/// counting suppressed repeats so the next emission carries their number.
///
/// Instances exist for unauthorized-access rejections and for response
/// truncation events; each call supplies its own event message, so one
/// throttle type serves both without conflating their log lines.
///
/// The decision logic is pure (tests drive it with synthetic instants); the
/// handler maps a returned [`RejectionLogEmission`] to the actual `tracing::warn!`.
#[derive(Default)]
pub(crate) struct RejectionLogThrottle {
    state: Mutex<Option<(Instant, u64)>>,
}

impl RejectionLogThrottle {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record one rejected attempt at instant `now`.
    ///
    /// Returns the emission to log, or `None` when the rejection falls inside
    /// the quiet period following a previous emission. An elapsed quiet period
    /// is exactly `[min_interval, ∞)`; the comparison uses wall-clock-free
    /// monotonic arithmetic, so identical inputs give identical decisions.
    fn record_at<'a>(&self, reason: &'a str, now: Instant) -> Option<RejectionLogEmission<'a>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((last_emit, suppressed)) = state.as_mut() {
            if now.duration_since(*last_emit) < REJECTION_LOG_MIN_INTERVAL {
                // A u64 counter cannot saturate from log throttling alone.
                *suppressed = suppressed.saturating_add(1);
                return None;
            }
            let emission = RejectionLogEmission::WithSuppressedCount {
                reason,
                suppressed: *suppressed,
            };
            *state = Some((now, 0));
            return Some(emission);
        }
        *state = Some((now, 0));
        Some(RejectionLogEmission::First { reason })
    }

    /// Production entry point: decide and emit the warning in one step.
    ///
    /// `message` is the human-readable event line (it must fit every event
    /// class sharing this throttle instance); `reason` is the structured
    /// per-event field.
    pub(crate) fn record(&self, message: &'static str, reason: &'static str) {
        if let Some(emission) = self.record_at(reason, Instant::now()) {
            match emission {
                RejectionLogEmission::First { reason } => {
                    tracing::warn!(reason, message);
                }
                RejectionLogEmission::WithSuppressedCount { reason, suppressed } => {
                    tracing::warn!(reason, suppressed_repeats = suppressed, message);
                }
            }
        }
    }
}

async fn enforce_metrics_auth(
    headers: &HeaderMap,
    server: &EnhancedGameServer,
) -> Result<(), StatusCode> {
    let config = server.config();
    let Some(raw_header) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        server.metrics_rejection_log().record(
            "Unauthorized metrics access attempt",
            "missing Authorization header",
        );
        return Err(StatusCode::UNAUTHORIZED);
    };

    let Some(token) = raw_header.strip_prefix("Bearer ") else {
        server.metrics_rejection_log().record(
            "Unauthorized metrics access attempt",
            "invalid Authorization scheme",
        );
        return Err(StatusCode::UNAUTHORIZED);
    };

    if let Some(expected) = config.metrics_auth_token.as_deref() {
        // Constant-time compare so the bearer token is not recoverable via a
        // timing side-channel (shared crate-wide secret-comparison helper).
        if crate::security::constant_time_eq(token, expected) {
            tracing::debug!("Metrics access authorized via bearer token");
            return Ok(());
        }
    }

    server
        .metrics_rejection_log()
        .record("Unauthorized metrics access attempt", "token rejected");
    Err(StatusCode::UNAUTHORIZED)
}

/// Query parameters for the metrics endpoints.
///
/// Every reported counter is a **lifetime-cumulative total** since process
/// start; there is deliberately no `timeRange` windowing parameter. Unknown
/// query parameters are accepted and ignored. Clients that need a window can
/// filter the `dashboardCache.history` samples client-side by their
/// `fetchedAt` timestamps instead.
#[derive(serde::Deserialize)]
pub struct MetricsQuery {
    #[serde(default, rename = "includeSnapshot")]
    include_snapshot: bool,
}

/// Maximum serialized size of the `metricsSnapshot` response field.
///
/// The raw snapshot includes per-identity maps (slow-consumer eviction
/// attributions, per-app relay bytes) whose size grows with live traffic. The
/// endpoint is bearer-token-gated, but if metrics auth is ever relaxed the
/// snapshot echo must not become an unbounded disclosure and response-size
/// amplifier (issue #518). A snapshot above this cap is replaced by a small
/// truncation marker. The rest of the response is bounded too: dashboard
/// history is capped in sample count by config, every game-name map
/// (current view and per-sample) is entry-capped below, and the whole
/// response carries a byte budget ([`METRICS_RESPONSE_MAX_BYTES`], issue
/// #551).
const METRICS_SNAPSHOT_MAX_BYTES: usize = 128 * 1024;

/// Whole-response byte budget for the `/metrics` JSON response (issue #551).
///
/// Entry caps bound cardinality, not bytes: `dashboardCache.history` holds up
/// to 720 samples with two game-name maps each, and a raised
/// `protocol.max_game_name_length` multiplies that into tens of megabytes per
/// poll. When the budget is exceeded, the oldest history samples are dropped
/// (the newest survive) and the loss is marked `historyTruncated` /
/// `historySamplesDropped`; a response that is still oversized is replaced by
/// the same fail-visible truncation marker the snapshot uses.
const METRICS_RESPONSE_MAX_BYTES: usize = 1024 * 1024;

/// Slack reserved for the truncation markers themselves when computing how
/// much history fits in [`METRICS_RESPONSE_MAX_BYTES`]. The markers carry a
/// boolean and a count, far below this bound.
const HISTORY_TRUNCATION_MARKER_SLACK_BYTES: usize = 128;

/// Enforce the whole-response byte budget (see [`METRICS_RESPONSE_MAX_BYTES`]).
///
/// Truncation is deterministic and fail-visible: the newest history samples
/// survive, the dropped count is reported in the response, and every
/// truncation goes through the throttled truncation log.
fn bounded_metrics_response(
    server: &EnhancedGameServer,
    mut response: serde_json::Value,
) -> serde_json::Value {
    // Serializing a `serde_json::Value` cannot fail; the `0` fallback is dead
    // and degrades to pass-through.
    let measured =
        |value: &serde_json::Value| serde_json::to_vec(value).map_or(0, |bytes| bytes.len());
    if measured(&response) <= METRICS_RESPONSE_MAX_BYTES {
        return response;
    }

    // First lever: drop the oldest dashboard-history samples. Per-sample
    // sizes are measured once, so the keep point is computed, not searched.
    let history_taken = response
        .get_mut("dashboardCache")
        .and_then(|cache| cache.get_mut("history"))
        .map(std::mem::take);
    if let Some(serde_json::Value::Array(samples)) = history_taken {
        let sample_sizes: Vec<usize> = samples.iter().map(&measured).collect();
        // The fixed size is measured with an empty history plus the markers,
        // so the kept suffix always lands strictly inside the budget.
        if let Some(cache) = response
            .get_mut("dashboardCache")
            .and_then(|cache| cache.as_object_mut())
        {
            cache.insert("history".to_string(), serde_json::Value::Array(Vec::new()));
            cache.insert(
                "historyTruncated".to_string(),
                serde_json::Value::Bool(true),
            );
        }
        let fixed_size = measured(&response) + HISTORY_TRUNCATION_MARKER_SLACK_BYTES;
        let budget_for_history = METRICS_RESPONSE_MAX_BYTES.saturating_sub(fixed_size);
        let mut kept_from = samples.len();
        let mut suffix_bytes = 0usize;
        while kept_from > 0 {
            let size = sample_sizes[kept_from - 1];
            if suffix_bytes + size > budget_for_history {
                break;
            }
            suffix_bytes += size;
            kept_from -= 1;
        }
        let dropped = kept_from;
        let kept: Vec<serde_json::Value> = samples.into_iter().skip(dropped).collect();
        if let Some(cache) = response
            .get_mut("dashboardCache")
            .and_then(|cache| cache.as_object_mut())
        {
            cache.insert("history".to_string(), serde_json::Value::Array(kept));
            cache.insert(
                "historySamplesDropped".to_string(),
                serde_json::json!(dropped),
            );
        }
        server.metrics_truncation_log().record(
            "Metrics response history truncated",
            "response exceeded the byte budget",
        );
    }

    if measured(&response) <= METRICS_RESPONSE_MAX_BYTES {
        return response;
    }

    // Still oversized (pathological per-sample size): fail visibly, the same
    // way an oversized snapshot does.
    let size_bytes = measured(&response);
    server.metrics_truncation_log().record(
        "Metrics response truncated",
        "response exceeded the byte budget even without history",
    );
    serde_json::json!({
        "truncated": true,
        "sizeBytes": size_bytes,
        "capBytes": METRICS_RESPONSE_MAX_BYTES,
    })
}

/// Maximum number of game-name entries kept in a game-name-keyed map of the
/// `/metrics` response (`roomsByGame`, `gamePercentiles`, and the same fields
/// in each `dashboardCache.history` sample).
///
/// Game names are client-chosen, so these maps can grow to one entry per live
/// game name. When more entries exist, the map keeps the first
/// [`METRICS_GAME_MAP_ENTRY_CAP`] entries in ascending game-name order and the
/// containing object gains a `<field>Truncated: true` marker (issue #518).
const METRICS_GAME_MAP_ENTRY_CAP: usize = 256;

/// Bound a game-name-keyed response map (see [`METRICS_GAME_MAP_ENTRY_CAP`]).
///
/// Returns the (possibly smaller) map and whether entries were dropped.
/// Survivors are the lexicographically first game names, so truncation is
/// deterministic across requests.
fn bounded_game_name_map(
    mut map: serde_json::Map<String, serde_json::Value>,
) -> (serde_json::Map<String, serde_json::Value>, bool) {
    if map.len() <= METRICS_GAME_MAP_ENTRY_CAP {
        return (map, false);
    }
    let mut names: Vec<String> = map.keys().cloned().collect();
    names.sort_unstable();
    let bounded = names
        .into_iter()
        .take(METRICS_GAME_MAP_ENTRY_CAP)
        .filter_map(|name| map.remove(&name).map(|value| (name, value)))
        .collect();
    (bounded, true)
}

/// Replace `map_field` in `response` with its bounded form, inserting
/// `marker_field: true` when entries were dropped and throttled-logging the
/// truncation with `reason` through `log`.
///
/// The handler always passes a top-level object and map-typed fields; any
/// other shape is restored verbatim (fail-open to the previous behavior).
fn bound_response_game_map(
    response: &mut serde_json::Value,
    map_field: &str,
    marker_field: &str,
    reason: &'static str,
    log: &RejectionLogThrottle,
) {
    let Some(taken) = response.get_mut(map_field).map(std::mem::take) else {
        return;
    };
    let serde_json::Value::Object(map) = taken else {
        // Not a map (the handler always builds objects here); restore it.
        if let Some(obj) = response.as_object_mut() {
            obj.insert(map_field.to_string(), taken);
        }
        return;
    };
    let (bounded, truncated) = bounded_game_name_map(map);
    if let Some(obj) = response.as_object_mut() {
        obj.insert(map_field.to_string(), serde_json::Value::Object(bounded));
        if truncated {
            obj.insert(marker_field.to_string(), serde_json::Value::Bool(true));
            log.record("Metrics response game-name map truncated", reason);
        }
    }
}

/// Bound the serialized `metricsSnapshot` response field.
///
/// Returns `value` unchanged when it serializes within
/// [`METRICS_SNAPSHOT_MAX_BYTES`]; otherwise returns a small truncation marker
/// carrying the measured size, so oversized snapshots fail visibly instead of
/// silently disappearing.
fn bounded_metrics_snapshot(
    server: &EnhancedGameServer,
    value: serde_json::Value,
) -> serde_json::Value {
    // Serializing a `serde_json::Value` cannot fail; the `0` fallback is dead
    // and degrades to pass-through.
    let size_bytes = serde_json::to_vec(&value).map_or(0, |bytes| bytes.len());
    if size_bytes <= METRICS_SNAPSHOT_MAX_BYTES {
        return value;
    }
    server.metrics_truncation_log().record(
        "Metrics snapshot truncated",
        "metricsSnapshot exceeded the response cap",
    );
    serde_json::json!({
        "truncated": true,
        "sizeBytes": size_bytes,
        "capBytes": METRICS_SNAPSHOT_MAX_BYTES,
    })
}

/// Metrics API endpoint - returns real data from server metrics
pub async fn metrics_handler(
    headers: axum::http::HeaderMap,
    State(server): State<Arc<EnhancedGameServer>>,
    axum::extract::Query(query): axum::extract::Query<MetricsQuery>,
) -> axum::response::Result<axum::response::Json<serde_json::Value>> {
    // Check authentication if required
    if server.config().require_metrics_auth {
        enforce_metrics_auth(&headers, server.as_ref()).await?;
    }
    // Get current time
    // Wall clock (durable record): the age readout is derived from the
    // snapshot's durable wall stamp for API consumers; the staleness
    // decision itself runs on monotonic time (see DashboardMetricsCache).
    let now = chrono::Utc::now();

    let dashboard_metrics = server.dashboard_metrics_view().await;
    let rooms_by_game = dashboard_metrics.rooms_by_game;
    let player_percentiles = dashboard_metrics.player_percentiles;
    let game_percentiles = dashboard_metrics.game_percentiles;
    let active_rooms = dashboard_metrics.active_rooms;
    let cache_fetched_at = dashboard_metrics.fetched_at.map(|ts| ts.to_rfc3339());
    let cache_age_seconds = dashboard_metrics
        .fetched_at
        .map(|ts| u64::try_from(now.signed_duration_since(ts).num_seconds()).unwrap_or(0));
    let cache_history: Vec<serde_json::Value> = dashboard_metrics
        .history
        .into_iter()
        .map(|entry| {
            let mut sample = serde_json::json!({
                "fetchedAt": entry.fetched_at.to_rfc3339(),
                "activeRooms": entry.active_rooms,
                "roomsByGame": entry.rooms_by_game,
                "playerPercentiles": entry.player_percentiles,
                "gamePercentiles": entry.game_percentiles,
            });
            // Each history sample carries the same client-chosen game-name
            // maps as the current view; bound them identically (issue #518).
            bound_response_game_map(
                &mut sample,
                "roomsByGame",
                "roomsByGameTruncated",
                "roomsByGame history map truncated",
                server.metrics_truncation_log(),
            );
            bound_response_game_map(
                &mut sample,
                "gamePercentiles",
                "gamePercentilesTruncated",
                "gamePercentiles history map truncated",
                server.metrics_truncation_log(),
            );
            sample
        })
        .collect();

    // Get server metrics
    let metrics_snapshot = server.metrics.snapshot().await;

    // Create response with real data
    let mut response = serde_json::json!({
        "playerPercentiles": player_percentiles,
        "roomsByGame": rooms_by_game,
        "gamePercentiles": game_percentiles,
        "activeRooms": active_rooms,
        "timestamp": now.to_rfc3339(),
        "dashboardCache": {
            "fetchedAt": cache_fetched_at,
            "ageSeconds": cache_age_seconds,
            "stale": dashboard_metrics.stale,
            "lastError": dashboard_metrics.last_error,
            "refreshIntervalSeconds": dashboard_metrics.refresh_interval_secs,
            "history": cache_history,
        },
        "serverMetrics": {
            "connections": {
                "total": metrics_snapshot.connections.total_connections,
                "active": metrics_snapshot.connections.active_connections,
                "disconnections": metrics_snapshot.connections.disconnections
            },
            "rooms": {
                "created": metrics_snapshot.rooms.rooms_created,
                "joined": metrics_snapshot.rooms.rooms_joined,
                "deleted": metrics_snapshot.rooms.rooms_deleted
            },
            "rateLimiting": {
                "total_rejections": metrics_snapshot.rate_limiting.rate_limit_rejections,
                "auth_rejections": metrics_snapshot.rate_limiting.auth_rejections,
                "room_creation_rejections":
                    metrics_snapshot.rate_limiting.room_creation_rejections,
                "join_attempt_rejections":
                    metrics_snapshot.rate_limiting.join_attempt_rejections,
                "signal_rejections": metrics_snapshot.rate_limiting.signal_rejections,
                "signal_error_rejections":
                    metrics_snapshot.rate_limiting.signal_error_rejections,
                "relay_bandwidth_rejections":
                    metrics_snapshot.rate_limiting.relay_bandwidth_rejections,
                "relay_room_bandwidth_rejections":
                    metrics_snapshot.rate_limiting.relay_room_bandwidth_rejections,
                "inbound_error_reply_rejections":
                    metrics_snapshot.rate_limiting.inbound_error_reply_rejections
            }
        }
    });

    if query.include_snapshot {
        if let Ok(snapshot_value) = serde_json::to_value(&metrics_snapshot) {
            if let Some(obj) = response.as_object_mut() {
                obj.insert(
                    "metricsSnapshot".to_string(),
                    bounded_metrics_snapshot(&server, snapshot_value),
                );
            }
        }
    }

    // Game names are client-chosen, so both maps can grow to one entry per
    // live game name; bound them before the response leaves the server.
    bound_response_game_map(
        &mut response,
        "roomsByGame",
        "roomsByGameTruncated",
        "roomsByGame map truncated",
        server.metrics_truncation_log(),
    );
    bound_response_game_map(
        &mut response,
        "gamePercentiles",
        "gamePercentilesTruncated",
        "gamePercentiles map truncated",
        server.metrics_truncation_log(),
    );

    // Whole-response byte budget (issue #551): entry caps bound cardinality,
    // not bytes; this is the fail-visible backstop.
    let response = bounded_metrics_response(&server, response);

    Ok(axum::response::Json(response))
}

/// Prometheus metrics endpoint (text format, version 0.0.4)
pub async fn prometheus_metrics_handler(
    headers: axum::http::HeaderMap,
    State(server): State<Arc<EnhancedGameServer>>,
) -> axum::response::Result<axum::response::Response> {
    use axum::http::header::{HeaderValue, CONTENT_TYPE};
    use axum::response::IntoResponse;

    if server.config().require_metrics_auth {
        enforce_metrics_auth(&headers, server.as_ref()).await?;
    }

    let snapshot = server.metrics.snapshot().await;
    let body = render_prometheus_metrics(&snapshot);
    let headers = [(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    )];

    Ok((headers, body).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::DatabaseConfig;
    use crate::server::ServerConfig;
    use axum::http::header::AUTHORIZATION;
    use axum::http::HeaderMap;

    /// Data-driven: a snapshot within the cap passes through byte-identical;
    /// an oversized snapshot is replaced by a truncation marker that is small,
    /// fail-visible (carries the measured size), and itself within the cap.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn bounded_metrics_snapshot_passes_through_or_truncates_visibly() {
        let server = build_metrics_test_server(ServerConfig::default()).await;
        let small = serde_json::json!({ "connections": { "total": 7_u64 } });
        assert_eq!(
            bounded_metrics_snapshot(&server, small.clone()),
            small,
            "a snapshot within the cap must be returned unchanged"
        );

        let oversized = serde_json::Value::Array(vec![
            serde_json::json!({ "padding": "x".repeat(64) });
            (METRICS_SNAPSHOT_MAX_BYTES / 32) + 1
        ]);
        let serialized_size = serde_json::to_vec(&oversized)
            .expect("a constructed value always serializes")
            .len();
        assert!(serialized_size > METRICS_SNAPSHOT_MAX_BYTES);

        let bounded = bounded_metrics_snapshot(&server, oversized);
        let bounded_size = serde_json::to_vec(&bounded)
            .expect("a constructed value always serializes")
            .len();
        assert!(
            bounded_size <= METRICS_SNAPSHOT_MAX_BYTES,
            "the replacement marker must itself respect the cap"
        );
        assert_eq!(bounded["truncated"], serde_json::Value::Bool(true));
        assert_eq!(
            bounded["sizeBytes"],
            serde_json::json!(serialized_size),
            "the marker must report the measured pre-truncation size"
        );
        assert_eq!(
            bounded["capBytes"],
            serde_json::json!(METRICS_SNAPSHOT_MAX_BYTES)
        );
    }

    /// Issue #551: a response within the budget passes through byte-identical.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn bounded_metrics_response_passes_small_responses_through() {
        let server = build_metrics_test_server(ServerConfig::default()).await;
        let small = serde_json::json!({
            "activeRooms": 3,
            "dashboardCache": { "history": [ { "activeRooms": 2 } ] },
        });
        assert_eq!(
            bounded_metrics_response(&server, small.clone()),
            small,
            "a response within the budget must be returned unchanged"
        );
    }

    /// Issue #551: an oversized response keeps the NEWEST history samples,
    /// reports the dropped count, and lands strictly inside the budget.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn bounded_metrics_response_drops_oldest_history_until_within_budget() {
        let server = build_metrics_test_server(ServerConfig::default()).await;
        // ~300 samples x ~8 KB each far exceeds the 1 MiB budget.
        let fat_sample = |index: usize| {
            serde_json::json!({
                "fetchedAt": format!("2026-09-08T00:00:{index:02}Z"),
                "padding": "x".repeat(8 * 1024),
            })
        };
        let sample_count = 300;
        let samples: Vec<serde_json::Value> = (0..sample_count).map(fat_sample).collect();
        let response = serde_json::json!({
            "activeRooms": 1,
            "dashboardCache": { "history": samples },
        });
        let oversized = serde_json::to_vec(&response)
            .expect("a constructed value always serializes")
            .len();
        assert!(oversized > METRICS_RESPONSE_MAX_BYTES);

        let bounded = bounded_metrics_response(&server, response);
        let bounded_size = serde_json::to_vec(&bounded)
            .expect("a constructed value always serializes")
            .len();
        assert!(
            bounded_size <= METRICS_RESPONSE_MAX_BYTES,
            "the bounded response must respect the budget"
        );
        assert_eq!(
            bounded["dashboardCache"]["historyTruncated"],
            serde_json::Value::Bool(true),
            "history loss must be fail-visible"
        );
        let dropped = bounded["dashboardCache"]["historySamplesDropped"]
            .as_u64()
            .expect("dropped count must be a number");
        let dropped = usize::try_from(dropped).expect("dropped count fits in usize");
        assert!(
            dropped > 0 && dropped < sample_count,
            "some but not all samples must survive, got dropped={dropped}"
        );
        let kept = bounded["dashboardCache"]["history"]
            .as_array()
            .expect("history stays an array");
        assert_eq!(
            dropped + kept.len(),
            sample_count,
            "kept + dropped must account for every sample"
        );
        // The NEWEST samples survive: the last original sample is the last
        // kept one.
        assert_eq!(
            kept.last().and_then(|s| s.get("fetchedAt")),
            Some(&serde_json::json!(format!(
                "2026-09-08T00:00:{:02}Z",
                sample_count - 1
            ))),
            "the newest sample must be the last survivor"
        );
    }

    /// Issue #551: a response that stays oversized even without history (a
    /// pathologically large non-history field) is replaced by the fail-visible
    /// marker.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn bounded_metrics_response_replaces_still_oversized_responses_with_marker() {
        let server = build_metrics_test_server(ServerConfig::default()).await;
        let response = serde_json::json!({
            // The current-view maps are NOT history: dropping history cannot
            // shrink them, so the response stays oversized.
            "roomsByGame": { "game": "x".repeat(2 * METRICS_RESPONSE_MAX_BYTES) },
            "dashboardCache": {
                "history": [ { "fetchedAt": "2026-09-08T00:00:00Z" } ],
            },
        });

        let bounded = bounded_metrics_response(&server, response);
        assert_eq!(bounded["truncated"], serde_json::Value::Bool(true));
        let reported = bounded["sizeBytes"]
            .as_u64()
            .expect("size must be a number");
        assert!(
            reported > METRICS_RESPONSE_MAX_BYTES as u64,
            "the marker must report the measured (over-budget) size it replaces, got {reported}"
        );
        assert_eq!(
            bounded["capBytes"],
            serde_json::json!(METRICS_RESPONSE_MAX_BYTES)
        );
        let bounded_size = serde_json::to_vec(&bounded)
            .expect("a constructed value always serializes")
            .len();
        assert!(
            bounded_size <= METRICS_RESPONSE_MAX_BYTES,
            "the replacement marker must itself respect the budget"
        );
    }

    /// Data-driven: a map within the entry cap passes through unchanged; a
    /// larger map keeps the lexicographically first names (deterministic
    /// across requests) and reports that entries were dropped.
    #[test]
    fn bounded_game_name_map_keeps_deterministic_prefix_and_reports_truncation() {
        let small: serde_json::Map<String, serde_json::Value> = [("b", 1), ("a", 2)]
            .into_iter()
            .map(|(name, rooms)| (name.to_string(), serde_json::json!(rooms)))
            .collect();
        let (bounded, truncated) = bounded_game_name_map(small.clone());
        assert!(!truncated);
        assert_eq!(bounded, small, "a map within the cap must be unchanged");

        let oversized: serde_json::Map<String, serde_json::Value> = (0
            ..(METRICS_GAME_MAP_ENTRY_CAP + 1))
            .map(|index| {
                let name = format!("game-{index:04}");
                (name, serde_json::json!(index))
            })
            .collect();
        let (bounded, truncated) = bounded_game_name_map(oversized);
        assert!(truncated, "an over-cap map must report truncation");
        assert_eq!(bounded.len(), METRICS_GAME_MAP_ENTRY_CAP);
        assert_eq!(
            bounded.get("game-0000"),
            Some(&serde_json::json!(0)),
            "the lexicographic prefix must survive"
        );
        assert!(
            !bounded.contains_key(&format!("game-{:04}", METRICS_GAME_MAP_ENTRY_CAP)),
            "entries beyond the cap must be dropped"
        );
    }

    /// `bound_response_game_map` must replace the named field in place, add
    /// the `Truncated` marker only when entries were dropped, and leave
    /// unrelated response fields untouched.
    #[test]
    fn bound_response_game_map_trims_field_and_sets_marker_only_when_truncated() {
        let small_map = serde_json::json!({ "game": 1 });
        let mut response = serde_json::json!({
            "roomsByGame": small_map,
            "gamePercentiles": small_map,
            "activeRooms": 3,
        });
        let log = RejectionLogThrottle::new();
        bound_response_game_map(
            &mut response,
            "roomsByGame",
            "roomsByGameTruncated",
            "roomsByGame map truncated",
            &log,
        );
        bound_response_game_map(
            &mut response,
            "gamePercentiles",
            "gamePercentilesTruncated",
            "gamePercentiles map truncated",
            &log,
        );
        assert_eq!(response["roomsByGame"], small_map);
        assert_eq!(response["gamePercentiles"], small_map);
        assert!(response.get("roomsByGameTruncated").is_none());
        assert!(response.get("gamePercentilesTruncated").is_none());
        assert_eq!(response["activeRooms"], 3, "unrelated fields are untouched");

        let oversized: serde_json::Map<String, serde_json::Value> = (0
            ..(METRICS_GAME_MAP_ENTRY_CAP + 1))
            .map(|index| (format!("game-{index:04}"), serde_json::json!(index)))
            .collect();
        let mut response = serde_json::json!({ "roomsByGame": oversized });
        bound_response_game_map(
            &mut response,
            "roomsByGame",
            "roomsByGameTruncated",
            "roomsByGame map truncated",
            &log,
        );
        assert_eq!(
            response["roomsByGame"]
                .as_object()
                .map(serde_json::Map::len),
            Some(METRICS_GAME_MAP_ENTRY_CAP)
        );
        assert_eq!(
            response["roomsByGameTruncated"],
            serde_json::Value::Bool(true)
        );
    }

    /// Data-driven, sleep-free: the throttle emits the first rejection,
    /// suppresses everything inside the quiet period (counting the
    /// suppressions), then summarizes them at the next boundary and starts a
    /// fresh quiet period.
    #[test]
    fn rejection_log_throttle_emits_first_and_window_summaries() {
        let throttle = RejectionLogThrottle::new();
        let start = Instant::now();
        let step = REJECTION_LOG_MIN_INTERVAL / 4;

        assert_eq!(
            throttle.record_at("token rejected", start),
            Some(RejectionLogEmission::First {
                reason: "token rejected"
            }),
            "the first rejection is always emitted"
        );

        // Three suppressed rejections inside the quiet period...
        for offset in [1 * step, 2 * step, 3 * step] {
            assert_eq!(
                throttle.record_at("token rejected", start + offset),
                None,
                "rejections inside the quiet period must be suppressed"
            );
        }

        // ...then the boundary emission carries their count.
        let boundary = start + REJECTION_LOG_MIN_INTERVAL;
        assert_eq!(
            throttle.record_at("missing Authorization header", boundary),
            Some(RejectionLogEmission::WithSuppressedCount {
                reason: "missing Authorization header",
                suppressed: 3
            })
        );

        // The quiet period restarts after a summary emission; an immediate
        // repeat is suppressed again rather than double-counted.
        assert_eq!(
            throttle.record_at("token rejected", boundary + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            throttle.record_at("token rejected", boundary + REJECTION_LOG_MIN_INTERVAL),
            Some(RejectionLogEmission::WithSuppressedCount {
                reason: "token rejected",
                suppressed: 1
            })
        );
    }

    async fn build_metrics_test_server(mut config: ServerConfig) -> Arc<EnhancedGameServer> {
        config.require_metrics_auth = true;
        EnhancedGameServer::new(
            config,
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            crate::config::SessionConfig::default(),
            crate::config::TurnConfig::default(),
            DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("create test server")
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_metrics_auth_missing_header_rejected() {
        let server = build_metrics_test_server(ServerConfig::default()).await;
        let headers = HeaderMap::new();
        assert_eq!(
            enforce_metrics_auth(&headers, server.as_ref())
                .await
                .unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_metrics_auth_accepts_static_token() {
        let config = ServerConfig {
            metrics_auth_token: Some("shared-token".to_string()),
            ..ServerConfig::default()
        };
        let server = build_metrics_test_server(config).await;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            "Bearer shared-token".parse().expect("header parse failed"),
        );

        assert!(enforce_metrics_auth(&headers, server.as_ref())
            .await
            .is_ok());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_metrics_auth_wrong_token_rejected() {
        let config = ServerConfig {
            metrics_auth_token: Some("correct-token".to_string()),
            ..ServerConfig::default()
        };
        let server = build_metrics_test_server(config).await;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            "Bearer wrong-token".parse().expect("header parse failed"),
        );

        assert_eq!(
            enforce_metrics_auth(&headers, server.as_ref())
                .await
                .unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_metrics_auth_invalid_scheme_rejected() {
        let config = ServerConfig {
            metrics_auth_token: Some("some-token".to_string()),
            ..ServerConfig::default()
        };
        let server = build_metrics_test_server(config).await;

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            "Basic some-token".parse().expect("header parse failed"),
        );

        assert_eq!(
            enforce_metrics_auth(&headers, server.as_ref())
                .await
                .unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }
}
