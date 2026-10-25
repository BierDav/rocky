//! Retry deadlines use Tokio's clock; successful idle peers have no deadline.
use tokio::time::{Duration, Instant};

pub(crate) struct Retry {
    random: u64,
    failures: u32,
    deadline: Option<Instant>,
    epoch_millis: Option<u64>,
}
impl Default for Retry {
    fn default() -> Self {
        Self::seeded(rand::random(), &[])
    }
}
impl Retry {
    pub fn seeded(seed: u64, identity: &[u8]) -> Self {
        let mut bytes = seed.to_le_bytes().to_vec();
        bytes.extend_from_slice(identity);
        let hash = rocky_core::commitment("rocky.retry.v1", &bytes);
        let mut initial = [0; 8];
        initial.copy_from_slice(&hash[..8]);
        Self {
            random: u64::from_le_bytes(initial),
            failures: 0,
            deadline: None,
            epoch_millis: None,
        }
    }
    pub fn failed(&mut self) {
        let seconds = (1_u64 << self.failures.min(6)).min(60);
        self.failures = self.failures.saturating_add(1);
        // SplitMix64 keeps a reproducible stream for each node/peer pair.
        self.random = self.random.wrapping_add(0x9e3779b97f4a7c15);
        let mut mixed = self.random;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d049bb133111eb);
        mixed ^= mixed >> 31;
        let millis = seconds * (750 + mixed % 501);
        self.deadline = Some(Instant::now() + Duration::from_millis(millis));
        self.epoch_millis = crate::status::epoch_millis().map(|time| time.saturating_add(millis));
    }
    /// Work that failed after it made progress retries shortly, without
    /// growing the backoff.
    pub fn soon(&mut self) {
        self.deadline = Some(Instant::now() + Duration::from_secs(1));
        self.epoch_millis = crate::status::epoch_millis().map(|time| time.saturating_add(1000));
    }
    pub fn reset(&mut self) {
        self.failures = 0;
        self.wake();
    }
    pub fn survived(&mut self, age: Duration) {
        if age >= Duration::from_secs(60) {
            self.reset();
        }
    }
    pub fn wake(&mut self) {
        self.deadline = None;
        self.epoch_millis = None;
    }
    pub fn ready(&self) -> bool {
        self.deadline
            .is_none_or(|deadline| deadline <= Instant::now())
    }
    pub fn deadline_epoch_millis(&self) -> Option<u64> {
        self.epoch_millis
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
    #[cfg(test)]
    pub async fn wait(&self) {
        match self.deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn jitter_spreads_nodes_with_the_same_seed_and_preserves_failure_history() {
        let mut a = Retry::seeded(7, b"node a");
        let mut b = Retry::seeded(7, b"node b");
        let mut replay = Retry::seeded(7, b"node a");
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            a.failed();
            b.failed();
            replay.failed();
            let delay = a.deadline.unwrap() - Instant::now();
            assert!(delay >= Duration::from_millis(seconds * 750));
            assert!(delay <= Duration::from_millis(seconds * 1250));
            assert_ne!(a.deadline, b.deadline);
            assert_eq!(a.deadline, replay.deadline);
            tokio::time::advance(delay).await;
            assert!(a.ready());
        }
        a.survived(Duration::from_secs(59));
        assert_eq!(a.failures, 8);
        a.soon();
        assert_eq!(a.failures, 8);
        a.wake();
        assert_eq!(a.failures, 8);
        a.survived(Duration::from_secs(60));
        assert_eq!(a.failures, 0);
        assert!(a.deadline.is_none());
        assert!(
            tokio::time::timeout(Duration::ZERO, a.wait())
                .await
                .is_err()
        );
    }
}
