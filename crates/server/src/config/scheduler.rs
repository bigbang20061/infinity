//! Deployment settings for durable `game.jobs` scheduling, not a client service.
//! Loaded from server configuration; lease renewal and cross-process exclusion
//! are unsupported.
use anyhow::{ensure, Result};
use serde::Deserialize;

#[derive(Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Server policy: maximum polling interval in seconds.
    pub poll_secs: u64,
    /// Server policy: claim lease in seconds, longer than the poll interval.
    pub lease_secs: u64,
    /// Server policy: maximum simultaneously executing handlers.
    pub max_concurrent: usize,
    /// Server policy: initial retry delay in seconds.
    pub retry_base_secs: u64,
    /// Server policy: failed non-periodic attempt at which a job is dead-lettered.
    pub dead_letter_attempts: i32,
    /// Server policy: shutdown drain timeout in seconds.
    pub drain_secs: u64,
}
impl Default for SchedulerConfig {
    fn default() -> Self { Self { poll_secs: 5, lease_secs: 60, max_concurrent: 8, retry_base_secs: 30, dead_letter_attempts: 25, drain_secs: 10 } }
}
impl SchedulerConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.poll_secs > 0 && self.lease_secs > self.poll_secs, "scheduler.lease_secs must exceed positive poll_secs");
        ensure!(self.max_concurrent > 0 && self.retry_base_secs > 0 && self.dead_letter_attempts > 0 && self.drain_secs > 0, "scheduler settings must be positive");
        Ok(())
    }
}
