//! Budgets fixed in the coordinator's certified genesis.
use rocky_core::*;
use serde::{Deserialize, Serialize};

pub const DAY: u64 = 86_400;

/// Tenant storage and bounded metadata retry budgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub tenant_domains: usize,
    pub tenant_bytes: usize,
    pub total_bytes: usize,
    pub retry_window: u64,
    pub retry_records: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            tenant_domains: 16,
            tenant_bytes: 4 * 1024 * 1024,
            total_bytes: 24 * 1024 * 1024,
            retry_window: 3600,
            retry_records: 64,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<(), CoordinationError> {
        if self.tenant_domains == 0
            || self.tenant_bytes == 0
            || self.retry_window == 0
            || self.retry_records == 0
            || self.total_bytes > crate::certified::MAX_STORE / 4 * 3
        {
            return Err(CoordinationError::Configuration);
        }
        Ok(())
    }
}
