use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Validity {
    pub issued_at: u64,
    pub lifetime: Duration,
    pub clock_skew: Duration,
}
