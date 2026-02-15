use crate::server::EnhancedGameServer;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use std::sync::Arc;

use super::prometheus::render_prometheus_metrics;

async fn enforce_metrics_auth(
    headers: &HeaderMap,
    server: &EnhancedGameServer,
) -> Result<(), StatusCode> {
    let config = server.config();
    let Some(raw_header) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        tracing::warn!("Unauthorized metrics access attempt: missing Authorization header");
        return Err(StatusCode::UNAUTHORIZED);
    };

    let Some(token) = raw_header.strip_prefix("Bearer ") else {
        tracing::warn!("Unauthorized metrics access attempt: invalid Authorization scheme");
        return Err(StatusCode::UNAUTHORIZED);
    };

    if let Some(expected) = config.metrics_auth_token.as_deref() {
        if token == expected {
            tracing::debug!("Metrics access authorized via bearer token");
            return Ok(());
        }
    }

    tracing::warn!("Unauthorized metrics access attempt: token rejected");
    Err(StatusCode::UNAUTHORIZED)
}

/// Query parameters for metrics endpoint
#[derive(serde::Deserialize)]
pub struct MetricsQuery {
    #[serde(default = "default_time_range")]
    time_range: String,
    #[serde(default, rename = "includeSnapshot")]
    include_snapshot: bool,
}

fn default_time_range() -> String {
    "1h".to_string()
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
    let now = chrono::Utc::now();

    let dashboard_metrics = server.dashboard_metrics_view().await;
    let rooms_by_game = dashboard_metrics.rooms_by_game;
    let player_percentiles = dashboard_metrics.player_percentiles;
    let game_percentiles = dashboard_metrics.game_percentiles;
    let active_rooms = dashboard_metrics.active_rooms;
    let cache_fetched_at = dashboard_metrics.fetched_at.map(|ts| ts.to_rfc3339());
    let cache_age_seconds = dashboard_metrics
        .fetched_at
        .map(|ts| now.signed_duration_since(ts).num_seconds().max(0) as u64);
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
        "timeRange": query.time_range,
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
            "performance": {
                "queries": metrics_snapshot.performance.query_count,
                "room_creation_latency": metrics_snapshot.performance.room_creation_latency,
                "room_join_latency": metrics_snapshot.performance.room_join_latency,
                "query_latency": metrics_snapshot.performance.query_latency
            },
            "errors": {
                "internal": metrics_snapshot.errors.internal_errors,
                "websocket": metrics_snapshot.errors.websocket_errors,
                "total": metrics_snapshot.errors.total_errors
            },
            "rateLimiting": {
                "minute": {
                    "limit": metrics_snapshot.rate_limiting.minute_limit,
                    "used": metrics_snapshot.rate_limiting.minute_count,
                    "checks": metrics_snapshot.rate_limiting.minute_checks,
                    "rejections": metrics_snapshot.rate_limiting.minute_rejections
                },
                "hour": {
                    "limit": metrics_snapshot.rate_limiting.hour_limit,
                    "used": metrics_snapshot.rate_limiting.hour_count,
                    "checks": metrics_snapshot.rate_limiting.hour_checks,
                    "rejections": metrics_snapshot.rate_limiting.hour_rejections
                },
                "day": {
                    "limit": metrics_snapshot.rate_limiting.day_limit,
                    "used": metrics_snapshot.rate_limiting.day_count,
                    "checks": metrics_snapshot.rate_limiting.day_checks,
                    "rejections": metrics_snapshot.rate_limiting.day_rejections
                },
                "total_rejections": metrics_snapshot.rate_limiting.rate_limit_rejections,
                "resets": metrics_snapshot.rate_limiting.rate_limit_resets
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

    async fn build_metrics_test_server(mut config: ServerConfig) -> Arc<EnhancedGameServer> {
        config.require_metrics_auth = true;
        EnhancedGameServer::new(
            config,
            crate::config::ProtocolConfig::default(),
            crate::config::RelayTypeConfig::default(),
            DatabaseConfig::InMemory,
            crate::config::MetricsConfig::default(),
            crate::config::AuthMaintenanceConfig::default(),
            crate::config::CoordinationConfig::default(),
            crate::config::TransportSecurityConfig::default(),
            Vec::new(),
        )
        .await
        .expect("create test server")
    }

    #[tokio::test]
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
