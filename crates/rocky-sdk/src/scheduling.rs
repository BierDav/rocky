//! Retry deadlines use Tokio's clock; successful idle peers have no deadline.
use tokio::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Retry {
    failures: u32,
    deadline: Option<Instant>,
}
impl Retry {
    pub fn failed(&mut self) {
        let seconds = (1_u64 << self.failures.min(6)).min(60);
        self.failures = self.failures.saturating_add(1);
        self.deadline = Some(Instant::now() + Duration::from_secs(seconds));
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn ready(&self) -> bool {
        self.deadline
            .is_none_or(|deadline| deadline <= Instant::now())
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
    async fn failed_work_backs_off_and_idle_has_no_timer() {
        let mut retry = Retry::default();
        for seconds in [1, 2, 4, 8, 16, 32, 60, 60] {
            retry.failed();
            assert_eq!(
                retry.deadline,
                Some(Instant::now() + Duration::from_secs(seconds))
            );
            tokio::time::advance(Duration::from_secs(seconds - 1)).await;
            assert!(
                tokio::time::timeout(Duration::ZERO, retry.wait())
                    .await
                    .is_err()
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            retry.wait().await;
        }
        retry.reset();
        assert!(retry.deadline.is_none());
        tokio::time::advance(Duration::from_secs(86400)).await;
        assert!(
            tokio::time::timeout(Duration::ZERO, retry.wait())
                .await
                .is_err()
        );
        retry.failed();
        assert_eq!(
            retry.deadline,
            Some(Instant::now() + Duration::from_secs(1))
        );
    }
}
