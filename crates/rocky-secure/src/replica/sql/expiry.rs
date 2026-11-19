use rocky_platform::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};

#[derive(Clone, Default)]
pub(crate) enum LeaseClock {
    #[default]
    Wall,
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        expect(
            dead_code,
            reason = "only test support opens a replica on a manual lease clock"
        )
    )]
    Manual(watch::Receiver<Duration>),
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone)]
pub struct ManualLeaseClock(std::sync::Arc<watch::Sender<Duration>>);
#[cfg(any(test, feature = "test-support"))]
impl Default for ManualLeaseClock {
    fn default() -> Self {
        Self(std::sync::Arc::new(watch::Sender::new(Duration::ZERO)))
    }
}
#[cfg(any(test, feature = "test-support"))]
impl ManualLeaseClock {
    pub fn advance(&self, by: Duration) {
        self.0.send_modify(|now| *now = now.saturating_add(by));
    }
    pub(crate) fn lease_clock(&self) -> LeaseClock {
        LeaseClock::Manual(self.0.subscribe())
    }
    #[cfg(all(test, not(target_family = "wasm")))]
    pub(crate) fn followers(&self) -> usize {
        self.0.receiver_count()
    }
}

#[derive(Clone)]
enum Deadline {
    Wall(Instant),
    Manual {
        at: Duration,
        clock: watch::Receiver<Duration>,
    },
}
impl Deadline {
    fn passed(&self) -> bool {
        match self {
            Self::Wall(at) => Instant::now() >= *at,
            Self::Manual { at, clock } => *clock.borrow() >= *at,
        }
    }
    async fn reached(self) -> bool {
        match self {
            Self::Wall(at) => {
                rocky_platform::time::sleep_until(at).await;
                true
            }
            Self::Manual { at, mut clock } => clock.wait_for(|now| *now >= at).await.is_ok(),
        }
    }
}

pub(super) struct Expiry {
    deadline: Deadline,
    _lapse_ends_with_the_transaction: oneshot::Sender<()>,
}
pub(super) struct Lapse {
    deadline: Deadline,
    transaction_ended: oneshot::Receiver<()>,
}
impl Expiry {
    pub(super) fn start(clock: &LeaseClock, timeout: Duration) -> (Self, Lapse) {
        let deadline = match clock {
            LeaseClock::Wall => Deadline::Wall(Instant::now() + timeout),
            LeaseClock::Manual(clock) => Deadline::Manual {
                at: clock.borrow().saturating_add(timeout),
                clock: clock.clone(),
            },
        };
        let (ends, transaction_ended) = oneshot::channel();
        (
            Self {
                deadline: deadline.clone(),
                _lapse_ends_with_the_transaction: ends,
            },
            Lapse {
                deadline,
                transaction_ended,
            },
        )
    }
    pub(super) fn passed(&self) -> bool {
        self.deadline.passed()
    }
    pub(super) fn bound(&self, budget: web_time::Instant) -> web_time::Instant {
        match &self.deadline {
            Deadline::Wall(at) => {
                let left = at.saturating_duration_since(Instant::now());
                budget.min(web_time::Instant::now() + left)
            }
            Deadline::Manual { .. } if self.passed() => budget.min(web_time::Instant::now()),
            Deadline::Manual { .. } => budget,
        }
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
            reached = deadline.reached() => reached,
        }
    }
}
