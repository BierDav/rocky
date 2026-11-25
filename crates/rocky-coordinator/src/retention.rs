use crate::CoordinationError;
use serde::{Deserialize, Serialize};

/// Seconds in a day, the period of the registration budget.
#[cfg(feature = "voter")]
pub const DAY: u64 = 86_400;

/// Storage and retry limits a coordinator enforces; the defaults suit a small shared service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Databases one tenant may register; 16 by default.
    pub tenant_domains: usize,
    /// Bytes of state one tenant may hold; 4 MiB by default.
    pub tenant_bytes: usize,
    /// Bytes of state all tenants may hold together; 24 MiB by default.
    pub total_bytes: usize,
    /// Seconds a request's outcome stays remembered for retries; one hour by default.
    pub retry_window: u64,
    /// Remembered request outcomes per tenant; 64 by default.
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
    /// Refuses zero limits and a total above three quarters of the store.
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
