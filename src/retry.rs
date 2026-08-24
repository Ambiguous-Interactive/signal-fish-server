use crate::metrics::ServerMetrics;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, warn};

/// Configuration for retry logic with exponential backoff
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Initial delay between retries
    pub initial_delay: Duration,
    /// Maximum delay between retries
    pub max_delay: Duration,
    /// Multiplier for exponential backoff
    pub backoff_multiplier: f64,
    /// Amount of jitter to add (0.0 to 1.0)
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_secs(2),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
        }
    }
}

impl RetryConfig {
    #[allow(dead_code)]
    pub fn fast() -> Self {
        Self {
            max_attempts: 3,
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(500),
            backoff_multiplier: 2.0,
            jitter_factor: 0.1,
        }
    }

    #[allow(dead_code)]
    pub fn persistent() -> Self {
        Self {
            max_attempts: 10,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            backoff_multiplier: 1.5,
            jitter_factor: 0.2,
        }
    }

    pub fn storage() -> Self {
        Self {
            max_attempts: 7,
            initial_delay: Duration::from_millis(25),
            max_delay: Duration::from_millis(1000),
            backoff_multiplier: 1.8,
            jitter_factor: 0.15,
        }
    }

    /// Worst-case total time this budget spends sleeping across a full attempt
    /// sequence (every delay at its maximum jitter).
    ///
    /// The executor performs `max_attempts` attempts with exactly
    /// `max_attempts - 1` sleeps between them, so this sums precisely those
    /// sleeps.
    ///
    /// Callers that retry against TTL-bounded resources use this to prove the
    /// whole retry budget fits inside the lease they wait for: a waiter that
    /// can still be backing off *after* the key may have expired would give up
    /// on (or race) an already-free resource (issue #414).
    pub fn worst_case_total_backoff(&self) -> Duration {
        let sleeps = self.max_attempts.saturating_sub(1);
        let mut total = Duration::ZERO;
        let mut current = bounded_initial_delay(self);
        for _ in 0..sleeps {
            total = total.saturating_add(current);
            current = bounded_next_delay(self, current, 1.0);
        }
        total
    }

    /// Trim `max_attempts` so [`Self::worst_case_total_backoff`] stays strictly
    /// below `budget`, keeping every other knob unchanged.
    ///
    /// Used by lease-style callers whose retry window must never outlive the
    /// TTL they contend on.
    pub fn clamped_to_total_backoff(&self, budget: Duration) -> Self {
        let mut trimmed = self.clone();
        while trimmed.max_attempts > 1 && trimmed.worst_case_total_backoff() >= budget {
            trimmed.max_attempts = trimmed.max_attempts.saturating_sub(1);
        }
        trimmed
    }
}

/// Error types that can be retried
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum RetryableError {
    /// Constraint violation (race condition)
    ConstraintViolation(String),
    /// Connection error
    ConnectionError(String),
    /// Room capacity reached (race condition)
    RoomCapacity,
    /// Room code collision
    RoomCodeCollision,
    /// Authority conflict
    AuthorityConflict,
    /// Remote-coordination extension failure (no shipped remote backend)
    CrossInstanceFailure(String),
    /// Temporary resource unavailable
    ResourceUnavailable(String),
    /// Generic retryable error
    Generic(String),
}

impl std::fmt::Display for RetryableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConstraintViolation(msg) => write!(f, "Constraint violation: {msg}"),
            Self::ConnectionError(msg) => write!(f, "Connection error: {msg}"),
            Self::RoomCapacity => write!(f, "Room at capacity"),
            Self::RoomCodeCollision => write!(f, "Room code collision"),
            Self::AuthorityConflict => write!(f, "Authority conflict"),
            Self::CrossInstanceFailure(msg) => {
                write!(f, "Cross-instance failure: {msg}")
            }
            Self::ResourceUnavailable(msg) => write!(f, "Resource unavailable: {msg}"),
            Self::Generic(msg) => write!(f, "Generic error: {msg}"),
        }
    }
}

impl std::error::Error for RetryableError {}

/// Retry executor with exponential backoff and jitter
pub struct RetryExecutor {
    config: RetryConfig,
    metrics: Option<Arc<ServerMetrics>>,
}

impl RetryExecutor {
    pub fn new(config: RetryConfig) -> Self {
        Self {
            config,
            metrics: None,
        }
    }

    pub fn with_metrics(config: RetryConfig, metrics: Arc<ServerMetrics>) -> Self {
        Self {
            config,
            metrics: Some(metrics),
        }
    }

    /// Execute an operation with retry logic
    pub async fn execute<T, F, Fut, E>(&self, operation_name: &str, operation: F) -> Result<T, E>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: From<RetryableError> + std::fmt::Debug,
    {
        let mut attempt = 1;
        let mut delay = bounded_initial_delay(&self.config);

        loop {
            if let Some(metrics) = &self.metrics {
                metrics.increment_retry_attempts();
            }

            debug!(
                operation = operation_name,
                attempt = attempt,
                max_attempts = self.config.max_attempts,
                "Executing operation attempt"
            );

            match operation().await {
                Ok(result) => {
                    if attempt > 1 {
                        debug!(
                            operation = operation_name,
                            attempt = attempt,
                            "Operation succeeded after retry"
                        );
                        if let Some(metrics) = &self.metrics {
                            metrics.increment_retry_successes();
                        }
                    }
                    return Ok(result);
                }
                Err(error) => {
                    if attempt >= self.config.max_attempts {
                        error!(
                            operation = operation_name,
                            attempt = attempt,
                            error = ?error,
                            "Operation failed after all retry attempts"
                        );
                        return Err(error);
                    }

                    // Check if error is retryable
                    if !Self::is_retryable_error(&error) {
                        debug!(
                            operation = operation_name,
                            error = ?error,
                            "Error is not retryable, failing immediately"
                        );
                        return Err(error);
                    }

                    warn!(
                        operation = operation_name,
                        attempt = attempt,
                        max_attempts = self.config.max_attempts,
                        error = ?error,
                        delay_ms = delay.as_millis(),
                        "Operation failed, retrying after delay"
                    );

                    sleep(delay).await;

                    delay = bounded_next_delay(&self.config, delay, fastrand::f64());

                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }

    /// Execute operation with custom retry condition
    #[allow(dead_code)]
    pub async fn execute_with_condition<T, F, Fut, E, R>(
        &self,
        operation_name: &str,
        operation: F,
        retry_condition: R,
    ) -> Result<T, E>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        R: Fn(&E) -> bool,
        E: std::fmt::Debug,
    {
        let mut attempt = 1;
        let mut delay = bounded_initial_delay(&self.config);

        loop {
            if let Some(metrics) = &self.metrics {
                metrics.increment_retry_attempts();
            }

            debug!(
                operation = operation_name,
                attempt = attempt,
                max_attempts = self.config.max_attempts,
                "Executing operation attempt with custom condition"
            );

            match operation().await {
                Ok(result) => {
                    if attempt > 1 {
                        debug!(
                            operation = operation_name,
                            attempt = attempt,
                            "Operation succeeded after retry"
                        );
                        if let Some(metrics) = &self.metrics {
                            metrics.increment_retry_successes();
                        }
                    }
                    return Ok(result);
                }
                Err(error) => {
                    if attempt >= self.config.max_attempts {
                        error!(
                            operation = operation_name,
                            attempt = attempt,
                            error = ?error,
                            "Operation failed after all retry attempts"
                        );
                        return Err(error);
                    }

                    // Check custom retry condition
                    if !retry_condition(&error) {
                        debug!(
                            operation = operation_name,
                            error = ?error,
                            "Custom retry condition failed, not retrying"
                        );
                        return Err(error);
                    }

                    warn!(
                        operation = operation_name,
                        attempt = attempt,
                        max_attempts = self.config.max_attempts,
                        error = ?error,
                        delay_ms = delay.as_millis(),
                        "Operation failed, retrying after delay (custom condition)"
                    );

                    sleep(delay).await;

                    delay = bounded_next_delay(&self.config, delay, fastrand::f64());

                    attempt = attempt.saturating_add(1);
                }
            }
        }
    }

    fn is_retryable_error<E>(error: &E) -> bool
    where
        E: std::fmt::Debug,
    {
        // Check if the error message contains known retryable patterns
        let error_str = format!("{error:?}").to_lowercase();

        // Storage-related retryable errors
        if error_str.contains("unique_violation")
            || error_str.contains("foreign_key_violation")
            || error_str.contains("connection")
            || error_str.contains("timeout")
            || error_str.contains("capacity")
            || error_str.contains("collision")
            || error_str.contains("conflict")
            || error_str.contains("deadlock")
            || error_str.contains("serialization_failure")
            || error_str.contains("could not serialize")
            || error_str.contains("room at capacity")
        {
            return true;
        }

        // Network-related retryable errors
        if error_str.contains("io error")
            || error_str.contains("broken pipe")
            || error_str.contains("connection reset")
            || error_str.contains("connection refused")
        {
            return true;
        }

        false
    }
}

/// Calculate one backoff step while treating `max_delay` as a strict bound on
/// the complete sleep, including jitter. Invalid public configuration factors
/// degrade to a bounded zero/cap value instead of panicking or overflowing.
fn bounded_next_delay(
    config: &RetryConfig,
    current_delay: Duration,
    jitter_fraction: f64,
) -> Duration {
    let base = scale_duration_capped(current_delay, config.backoff_multiplier, config.max_delay);
    let jitter_factor = if config.jitter_factor.is_finite() {
        config.jitter_factor.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let available = config.max_delay.saturating_sub(base);
    let jitter_limit = scale_duration_capped(base, jitter_factor, available);
    let fraction = if jitter_fraction.is_finite() {
        jitter_fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let jitter = scale_duration_capped(jitter_limit, fraction, jitter_limit);
    base.saturating_add(jitter).min(config.max_delay)
}

fn bounded_initial_delay(config: &RetryConfig) -> Duration {
    std::cmp::min(config.initial_delay, config.max_delay)
}

fn scale_duration_capped(duration: Duration, factor: f64, cap: Duration) -> Duration {
    if cap.is_zero() || factor.is_nan() || factor <= 0.0 {
        return Duration::ZERO;
    }
    if factor.is_infinite() {
        return cap;
    }

    let scaled_secs = duration.as_secs_f64() * factor;
    if !scaled_secs.is_finite() || scaled_secs >= cap.as_secs_f64() {
        cap
    } else {
        Duration::try_from_secs_f64(scaled_secs).unwrap_or(cap)
    }
}

/// Convenience functions for common retry scenarios
pub async fn retry_storage_operation<T, F, Fut>(
    operation_name: &str,
    operation: F,
    metrics: Option<Arc<ServerMetrics>>,
) -> Result<T, anyhow::Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, anyhow::Error>>,
{
    let executor = if let Some(metrics) = metrics {
        RetryExecutor::with_metrics(RetryConfig::storage(), metrics)
    } else {
        RetryExecutor::new(RetryConfig::storage())
    };

    executor.execute(operation_name, operation).await
}

#[allow(dead_code)]
pub async fn retry_room_operation<T, F, Fut>(
    operation_name: &str,
    operation: F,
    metrics: Option<Arc<ServerMetrics>>,
) -> Result<T, anyhow::Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, anyhow::Error>>,
{
    let executor = if let Some(metrics) = metrics {
        RetryExecutor::with_metrics(RetryConfig::fast(), metrics)
    } else {
        RetryExecutor::new(RetryConfig::fast())
    };

    executor.execute(operation_name, operation).await
}

#[allow(dead_code)]
pub async fn retry_cross_instance_operation<T, F, Fut>(
    operation_name: &str,
    operation: F,
    metrics: Option<Arc<ServerMetrics>>,
) -> Result<T, anyhow::Error>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, anyhow::Error>>,
{
    let executor = if let Some(metrics) = metrics {
        RetryExecutor::with_metrics(RetryConfig::persistent(), metrics)
    } else {
        RetryExecutor::new(RetryConfig::persistent())
    };

    executor.execute(operation_name, operation).await
}

/// Utility to determine if an error indicates a race condition
pub fn is_race_condition_error(error: &anyhow::Error) -> bool {
    let error_str = format!("{error}").to_lowercase();

    error_str.contains("unique_violation")
        || error_str.contains("room at capacity")
        || error_str.contains("room code")
        || error_str.contains("already exists")
        || error_str.contains("constraint")
        || error_str.contains("deadlock")
        || error_str.contains("serialization_failure")
}

/// Utility to determine if an error is a temporary connection issue
#[allow(dead_code)]
pub fn is_temporary_connection_error(error: &anyhow::Error) -> bool {
    let error_str = format!("{error}").to_lowercase();

    error_str.contains("connection")
        || error_str.contains("timeout")
        || error_str.contains("io error")
        || error_str.contains("broken pipe")
        || error_str.contains("connection reset")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc as StdArc;

    #[tokio::test]
    async fn test_successful_operation() {
        let executor = RetryExecutor::new(RetryConfig::default());

        let result = executor
            .execute("test", || async { Ok::<i32, anyhow::Error>(42) })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_retry_until_success() {
        let counter = StdArc::new(AtomicU32::new(0));
        let executor = RetryExecutor::new(RetryConfig::fast());

        let counter_clone = counter.clone();
        let result = executor
            .execute("test_retry", move || {
                let counter = counter_clone.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    if attempt < 3 {
                        Err(anyhow::anyhow!("unique_violation: test error"))
                    } else {
                        Ok(attempt)
                    }
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 3);
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn test_max_attempts_exceeded() {
        let executor = RetryExecutor::new(RetryConfig {
            max_attempts: 2,
            ..RetryConfig::fast()
        });

        let result = executor
            .execute("test_fail", || async {
                Err::<i32, anyhow::Error>(anyhow::anyhow!("unique_violation: persistent error"))
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_non_retryable_error() {
        let executor = RetryExecutor::new(RetryConfig::fast());

        let result = executor
            .execute("test_non_retryable", || async {
                Err::<i32, anyhow::Error>(anyhow::anyhow!("validation error: not retryable"))
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_custom_retry_condition() {
        let counter = StdArc::new(AtomicU32::new(0));
        let executor = RetryExecutor::new(RetryConfig::fast());

        let counter_clone = counter.clone();
        let result = executor
            .execute_with_condition(
                "test_custom",
                move || {
                    let counter = counter_clone.clone();
                    async move {
                        let attempt = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        if attempt < 2 {
                            Err(anyhow::anyhow!("custom retryable error"))
                        } else {
                            Ok(attempt)
                        }
                    }
                },
                |error| error.to_string().contains("custom retryable"),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);
    }

    #[test]
    fn retry_delay_caps_the_complete_jittered_sleep() {
        let cases = [
            (
                "backoff reaches cap before jitter",
                RetryConfig {
                    max_delay: Duration::from_secs(5),
                    backoff_multiplier: 2.0,
                    jitter_factor: 0.2,
                    ..RetryConfig::persistent()
                },
                Duration::from_secs(4),
                1.0,
                Duration::from_secs(5),
            ),
            (
                "jitter consumes only remaining headroom",
                RetryConfig {
                    max_delay: Duration::from_secs(1),
                    backoff_multiplier: 1.0,
                    jitter_factor: 1.0,
                    ..RetryConfig::default()
                },
                Duration::from_millis(750),
                1.0,
                Duration::from_secs(1),
            ),
            (
                "jitter factor applies before remaining headroom cap",
                RetryConfig {
                    max_delay: Duration::from_secs(5),
                    backoff_multiplier: 1.0,
                    jitter_factor: 0.2,
                    ..RetryConfig::default()
                },
                Duration::from_secs(4),
                1.0,
                Duration::from_millis(4_800),
            ),
            (
                "ordinary jitter remains additive",
                RetryConfig {
                    max_delay: Duration::from_secs(1),
                    backoff_multiplier: 2.0,
                    jitter_factor: 0.5,
                    ..RetryConfig::default()
                },
                Duration::from_millis(100),
                1.0,
                Duration::from_millis(300),
            ),
            (
                "sub-millisecond precision is preserved",
                RetryConfig {
                    max_delay: Duration::from_secs(1),
                    backoff_multiplier: 2.0,
                    jitter_factor: 0.0,
                    ..RetryConfig::default()
                },
                Duration::from_micros(250),
                0.0,
                Duration::from_micros(500),
            ),
            (
                "fractional backoff may decrease a delay at the cap",
                RetryConfig {
                    max_delay: Duration::from_secs(5),
                    backoff_multiplier: 0.5,
                    jitter_factor: 0.0,
                    ..RetryConfig::default()
                },
                Duration::from_secs(5),
                0.0,
                Duration::from_millis(2_500),
            ),
            (
                "duration overflow saturates at cap",
                RetryConfig {
                    max_delay: Duration::from_secs(5),
                    backoff_multiplier: f64::MAX,
                    jitter_factor: 1.0,
                    ..RetryConfig::default()
                },
                Duration::MAX,
                1.0,
                Duration::from_secs(5),
            ),
        ];

        for (context, config, current, fraction, expected) in cases {
            assert_eq!(
                bounded_next_delay(&config, current, fraction),
                expected,
                "{context}"
            );
        }
    }

    #[test]
    fn retry_initial_delay_respects_the_configured_maximum() {
        let config = RetryConfig {
            initial_delay: Duration::from_secs(6),
            max_delay: Duration::from_secs(5),
            ..RetryConfig::persistent()
        };
        assert_eq!(bounded_initial_delay(&config), Duration::from_secs(5));
    }

    #[test]
    fn test_race_condition_detection() {
        let race_error = anyhow::anyhow!("unique_violation: room code already exists");
        assert!(is_race_condition_error(&race_error));

        let temp_error = anyhow::anyhow!("connection timeout");
        assert!(!is_race_condition_error(&temp_error));
        assert!(is_temporary_connection_error(&temp_error));
    }
}
