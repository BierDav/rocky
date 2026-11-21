use rocky_sdk::{Failure, SqlFailure};
use std::sync::atomic::{AtomicU64, Ordering};

pub const ATTEMPTS: u64 = 20;
pub static RETRIES: AtomicU64 = AtomicU64::new(0);

pub fn retryable(failure: &Failure) -> bool {
    failure
        .downcast_ref::<SqlFailure>()
        .is_some_and(|failure| failure.retryable)
}

pub fn note() {
    RETRIES.fetch_add(1, Ordering::Relaxed);
}

pub fn count() -> u64 {
    RETRIES.load(Ordering::Relaxed)
}

macro_rules! retrying {
    ($body:expr) => {{
        let mut attempt = 0;
        loop {
            attempt += 1;
            match $body {
                Ok(value) => break Ok(value),
                Err(failure)
                    if attempt < $crate::retry::ATTEMPTS && $crate::retry::retryable(&failure) =>
                {
                    $crate::retry::note();
                }
                Err(failure) => break Err(failure),
            }
        }
    }};
}
pub(crate) use retrying;
