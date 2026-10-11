//! A simulated LLM for examples and tests.
//!
//! There is no model client yet (`agentbuild-providers` is not started). This
//! stand-in lets workflows that "call a model" be built and tested without a
//! network, API keys or cost. Like a real call, it takes time: by default each
//! reply arrives after 2–3 seconds. The delay and the reply are derived from
//! the prompt, so runs are reproducible.
//!
//! Shared by `tests/llm_simulation.rs` and `examples/llm_workflow.rs` via
//! `#[path]`; each uses a different subset, hence the `dead_code` allowance.

#![allow(dead_code)]

use std::sync::Mutex;
use std::time::Duration;

use agentbuild_core::NodeError;

/// A fake model: answers every prompt after a delay, or fails on request.
pub struct FakeLlm {
    min: Duration,
    max: Duration,
    fail_marker: Option<String>,
    prompts: Mutex<Vec<String>>,
}

impl Default for FakeLlm {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeLlm {
    /// Replies after 2–3 seconds.
    pub fn new() -> Self {
        Self::with_delay(Duration::from_secs(2), Duration::from_secs(3))
    }

    /// Replies after a delay between `min` and `max` (inclusive).
    pub fn with_delay(min: Duration, max: Duration) -> Self {
        assert!(min <= max, "min delay must not exceed max delay");
        Self {
            min,
            max,
            fail_marker: None,
            prompts: Mutex::new(Vec::new()),
        }
    }

    /// Prompts containing `marker` fail (after the delay) instead of replying.
    pub fn failing_on(mut self, marker: impl Into<String>) -> Self {
        self.fail_marker = Some(marker.into());
        self
    }

    /// How long the reply to `prompt` takes: fixed per prompt, within the range.
    pub fn delay_for(&self, prompt: &str) -> Duration {
        let span = (self.max - self.min).as_millis() as u64;
        let offset = if span == 0 {
            0
        } else {
            fnv1a(prompt) % (span + 1)
        };
        self.min + Duration::from_millis(offset)
    }

    /// Simulates one model call.
    pub async fn complete(&self, prompt: &str) -> Result<String, NodeError> {
        self.prompts.lock().unwrap().push(prompt.to_string());
        tokio::time::sleep(self.delay_for(prompt)).await;
        if let Some(marker) = &self.fail_marker
            && prompt.contains(marker.as_str())
        {
            return Err(NodeError::new("simulated model failure")
                .with_kind("fake_llm_error")
                .with_details(serde_json::json!({ "prompt": prompt })));
        }
        Ok(format!("[fake-llm] reply to: {prompt}"))
    }

    /// Number of calls started so far.
    pub fn calls(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }

    /// Every prompt received, in call order.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
