use crate::server::EnhancedGameServer;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::prometheus::render_prometheus_metrics;

/// Minimum quiet period between emitted unauthorized-metrics-access warnings.
///
/// Unauthenticated endpoints are a log-disk amplification vector: file logging
/// defaults on, there is no HTTP rate limiter on these routes, and one JSON
/// log line per rejection would let an anonymous request loop grow the log
/// indefinitely before any credential guess matters. Sixty seconds keeps the
/// first signal and a periodic suppressed-count summary at negligible volume.
const REJECTION_LOG_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// One decision to emit a rejected-metrics-access warning.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RejectionLogEmission<'a> {
    /// The first rejection after a quiet period (or ever).
    First { reason: &'a str },
    /// A quiet-period boundary reached while earlier rejections were
    /// suppressed; the count summarizes them.
    WithSuppressedCount { reason: &'a str, suppressed: u64 },
}

/// Emits at most one unauthorized-access warning per [`REJECTION_LOG_MIN_INTERVAL`],
/// counting suppressed repeats so the next emission carries their number.
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
    pub(crate) fn record(&self, reason: &'static str) {
        if let Some(emission) = self.record_at(reason, Instant::now()) {
            match emission {
                RejectionLogEmission::First { reason } => {
                    tracing::warn!(reason, "Unauthorized metrics access attempt");
                }
                RejectionLogEmission::WithSuppressedCount { reason, suppressed } => {
                    tracing::warn!(
                        reason,
                        suppressed_repeats = suppressed,
                        "Unauthorized metrics access attempts (throttled)"
                    );
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
        server
            .metrics_rejection_log()
            .record("missing Authorization header");
        return Err(StatusCode::UNAUTHORIZED);
    };

    let Some(token) = raw_header.strip_prefix("Bearer ") else {
        server
            .metrics_rejection_log()
            .record("invalid Authorization scheme");
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

    server.metrics_rejection_log().record("token rejected");
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
            serde_json::json!({
                "fetchedAt": entry.fetched_at.to_rfc3339(),
                "activeRooms": entry.active_rooms,
                "roomsByGame": entry.rooms_by_game,
                "playerPercentiles": entry.player_percentiles,
                "gamePercentiles": entry.game_percentiles,
            })
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
                    metrics_snapshot.rate_limiting.relay_bandwidth_rejections
            }
        }
    });

    if query.include_snapshot {
        if let Ok(snapshot_value) = serde_json::to_value(&metrics_snapshot) {
            if let Some(obj) = response.as_object_mut() {
                obj.insert("metricsSnapshot".to_string(), snapshot_value);
            }
        }
    }

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
