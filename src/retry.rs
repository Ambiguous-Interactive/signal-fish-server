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
    /// Cross-instance communication failure
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
        let mut delay = self.config.initial_delay;

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

                    // Calculate next delay with exponential backoff and jitter
                    let next_delay = Duration::from_millis(
                        (delay.as_millis() as f64 * self.config.backoff_multiplier) as u64,
                    );

                    delay = std::cmp::min(next_delay, self.config.max_delay);

                    // Add jitter
                    if self.config.jitter_factor > 0.0 {
                        let jitter = (delay.as_millis() as f64 * self.config.jitter_factor) as u64;
                        let jitter_amount = fastrand::u64(0..=jitter);
                        delay = Duration::from_millis(delay.as_millis() as u64 + jitter_amount);
                    }

                    attempt += 1;
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
        let mut delay = self.config.initial_delay;

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

                    // Calculate next delay
                    let next_delay = Duration::from_millis(
                        (delay.as_millis() as f64 * self.config.backoff_multiplier) as u64,
                    );
                    delay = std::cmp::min(next_delay, self.config.max_delay);

                    // Add jitter
                    if self.config.jitter_factor > 0.0 {
                        let jitter = (delay.as_millis() as f64 * self.config.jitter_factor) as u64;
                        let jitter_amount = fastrand::u64(0..=jitter);
                        delay = Duration::from_millis(delay.as_millis() as u64 + jitter_amount);
                    }

                    attempt += 1;
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
    fn test_race_condition_detection() {
        let race_error = anyhow::anyhow!("unique_violation: room code already exists");
        assert!(is_race_condition_error(&race_error));

        let temp_error = anyhow::anyhow!("connection timeout");
        assert!(!is_race_condition_error(&temp_error));
        assert!(is_temporary_connection_error(&temp_error));
    }
}
