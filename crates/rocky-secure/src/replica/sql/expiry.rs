use rocky_platform::time::{Duration, Instant};
use tokio::sync::oneshot;

pub(super) struct Expiry {
    deadline: Instant,
    _lapse_ends_with_the_transaction: oneshot::Sender<()>,
}
pub(super) struct Lapse {
    deadline: Instant,
    transaction_ended: oneshot::Receiver<()>,
}
impl Expiry {
    pub(super) fn start(timeout: Duration) -> (Self, Lapse) {
        let deadline = Instant::now() + timeout;
        let (ends, transaction_ended) = oneshot::channel();
        (
            Self {
                deadline,
                _lapse_ends_with_the_transaction: ends,
            },
            Lapse {
                deadline,
                transaction_ended,
            },
        )
    }
    pub(super) fn passed(&self) -> bool {
        Instant::now() >= self.deadline
    }
    pub(super) fn bound(&self, budget: web_time::Instant) -> web_time::Instant {
        let left = self.deadline.saturating_duration_since(Instant::now());
        budget.min(web_time::Instant::now() + left)
    }
}
impl Lapse {
    pub(super) async fn passed(self) -> bool {
        let Self {
            deadline,
            transaction_ended,
        } = self;
        tokio::select! {
            biased;
            _ = transaction_ended => false,
            () = rocky_platform::time::sleep_until(deadline) => true,
        }
    }
}
