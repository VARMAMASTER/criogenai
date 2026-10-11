//! Per-node execution policies: timeout and retry (design 0005 §4).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How the engine executes one node. Declared with
/// [`WorkflowBuilder::policy`](crate::WorkflowBuilder::policy).
///
/// Plain data, so SDKs can pass it through as JSON.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodePolicy {
    /// Time limit for each attempt, in milliseconds. Exceeding it fails the
    /// step with `node_timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Retries for errors the node marks [retryable](crate::NodeError::retryable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryPolicy>,
}

impl NodePolicy {
    /// No timeout, no retries.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the per-attempt time limit (whole milliseconds).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        self
    }

    /// Enables retries.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.timeout_ms == Some(0) {
            return Err("timeout must be at least 1 ms".into());
        }
        if let Some(retry) = &self.retry {
            if retry.max_attempts == 0 {
                return Err("retry.max_attempts must be at least 1".into());
            }
            if !(retry.backoff_factor.is_finite() && retry.backoff_factor >= 1.0) {
                return Err("retry.backoff_factor must be a finite number >= 1".into());
            }
        }
        Ok(())
    }
}

/// Exponential backoff with full jitter (0003 §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// Total attempts, including the first. Default 3.
    pub max_attempts: u32,
    /// Backoff before the second attempt, before jitter. Default 500 ms.
    pub initial_backoff_ms: u64,
    /// Multiplier for each further attempt. Default 2.
    pub backoff_factor: f64,
    /// Upper bound on any backoff. Default 20 s.
    pub max_backoff_ms: u64,
    /// Full jitter: wait a uniformly spread fraction of the backoff. Default on.
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 500,
            backoff_factor: 2.0,
            max_backoff_ms: 20_000,
            jitter: true,
        }
    }
}

impl RetryPolicy {
    /// The defaults: 3 attempts, 0.5 s → 1 s backoff with jitter, 20 s cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets [`max_attempts`](Self::max_attempts).
    pub fn max_attempts(mut self, attempts: u32) -> Self {
        self.max_attempts = attempts;
        self
    }

    /// Sets the initial backoff, the factor and the cap.
    pub fn backoff(mut self, initial: Duration, factor: f64, max: Duration) -> Self {
        self.initial_backoff_ms = u64::try_from(initial.as_millis()).unwrap_or(u64::MAX);
        self.backoff_factor = factor;
        self.max_backoff_ms = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
        self
    }

    /// Turns jitter on or off. Off gives exact, predictable delays.
    pub fn jitter(mut self, jitter: bool) -> Self {
        self.jitter = jitter;
        self
    }

    /// The wait after attempt `attempt` (1-based) failed. `seed` makes the
    /// jitter reproducible (the engine passes the task id).
    pub fn delay(&self, attempt: u32, seed: &str) -> Duration {
        let exponent = attempt.saturating_sub(1).min(64) as i32;
        let raw = self.initial_backoff_ms as f64 * self.backoff_factor.powi(exponent);
        let capped = raw.min(self.max_backoff_ms as f64);
        let ms = if self.jitter {
            capped * unit_fraction(seed, attempt)
        } else {
            capped
        };
        Duration::from_millis(ms as u64)
    }
}

/// A deterministic value in `[0, 1]` from `seed` and `attempt` (FNV-1a).
fn unit_fraction(seed: &str, attempt: u32) -> f64 {
    let hash = seed
        .bytes()
        .chain(attempt.to_le_bytes())
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    (hash >> 11) as f64 / ((1_u64 << 53) - 1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_without_jitter_is_capped() {
        let retry = RetryPolicy::new()
            .backoff(Duration::from_millis(100), 3.0, Duration::from_millis(1000))
            .jitter(false);
        let delays: Vec<u64> = (1..=5)
            .map(|a| retry.delay(a, "t").as_millis() as u64)
            .collect();
        assert_eq!(delays, [100, 300, 900, 1000, 1000]);
    }

    #[test]
    fn jitter_stays_within_bounds_and_is_reproducible() {
        let retry = RetryPolicy::new();
        for attempt in 1..=10 {
            let delay = retry.delay(attempt, "run:3:node");
            let bound = (500.0 * 2f64.powi(attempt as i32 - 1)).min(20_000.0);
            assert!(delay.as_millis() as f64 <= bound, "{delay:?} > {bound}");
            assert_eq!(delay, retry.delay(attempt, "run:3:node"));
        }
        assert_ne!(retry.delay(1, "a"), retry.delay(1, "b"));
    }

    #[test]
    fn huge_attempt_numbers_do_not_overflow() {
        let retry = RetryPolicy::new().jitter(false);
        assert_eq!(retry.delay(u32::MAX, "t"), Duration::from_secs(20));
    }

    #[test]
    fn validation() {
        assert!(NodePolicy::new().validate().is_ok());
        assert!(
            NodePolicy::new()
                .timeout(Duration::ZERO)
                .validate()
                .is_err()
        );
        assert!(
            NodePolicy::new()
                .timeout(Duration::from_micros(500))
                .validate()
                .is_err()
        );
        let zero = RetryPolicy::new().max_attempts(0);
        assert!(NodePolicy::new().retry(zero).validate().is_err());
        let shrinking =
            RetryPolicy::new().backoff(Duration::from_secs(1), 0.5, Duration::from_secs(5));
        assert!(NodePolicy::new().retry(shrinking).validate().is_err());
        let nan =
            RetryPolicy::new().backoff(Duration::from_secs(1), f64::NAN, Duration::from_secs(5));
        assert!(NodePolicy::new().retry(nan).validate().is_err());
    }

    #[test]
    fn json_form_uses_defaults_for_missing_fields() {
        let policy: NodePolicy =
            serde_json::from_str(r#"{"timeout_ms": 10000, "retry": {"max_attempts": 5}}"#).unwrap();
        assert_eq!(policy.timeout_ms, Some(10_000));
        let retry = policy.retry.unwrap();
        assert_eq!(retry.max_attempts, 5);
        assert_eq!(retry.initial_backoff_ms, 500);
        assert!(retry.jitter);
    }
}
