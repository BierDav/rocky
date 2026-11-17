use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
    time::Duration,
};
use wasm_bindgen::{JsCast, closure::Closure};

pub use web_time::Instant;

thread_local! {
    static TIMERS: RefCell<Timers> = RefCell::default();
}

#[derive(Default)]
struct Timers {
    next: u64,
    waiting: BTreeMap<(Instant, u64), Waker>,
    armed: Option<(Instant, i32)>,
}

fn global() -> web_sys::WorkerGlobalScope {
    js_sys::global().unchecked_into()
}

fn fire() {
    let wakers = TIMERS.with(|timers| {
        let mut timers = timers.borrow_mut();
        timers.armed = None;
        let now = Instant::now();
        let later = timers.waiting.split_off(&(now, u64::MAX));
        let expired = std::mem::replace(&mut timers.waiting, later);
        arm(&mut timers);
        expired.into_values().collect::<Vec<_>>()
    });
    wakers.into_iter().for_each(Waker::wake);
}

fn arm(timers: &mut Timers) {
    let Some(&(deadline, _)) = timers.waiting.keys().next() else {
        return;
    };
    if timers.armed.is_some_and(|(armed, _)| armed <= deadline) {
        return;
    }
    if let Some((_, handle)) = timers.armed.take() {
        global().clear_timeout_with_handle(handle);
    }
    let millis = browser_timeout_millis(deadline.saturating_duration_since(Instant::now()));
    let callback = Closure::once_into_js(fire);
    if let Ok(handle) = global()
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), millis)
    {
        timers.armed = Some((deadline, handle));
    }
}

fn browser_timeout_millis(delay: Duration) -> i32 {
    i32::try_from(delay.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
}

#[derive(Debug)]
pub struct Sleep {
    deadline: Instant,
    key: Option<u64>,
    turn: bool,
}
impl Sleep {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn is_elapsed(&self) -> bool {
        Instant::now() >= self.deadline
    }
    pub fn reset(mut self: Pin<&mut Self>, deadline: Instant) {
        self.cancel();
        self.deadline = deadline;
    }
    fn cancel(&mut self) {
        if let Some(key) = self.key.take() {
            TIMERS.with(|timers| timers.borrow_mut().waiting.remove(&(self.deadline, key)));
        }
    }
}
impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.is_elapsed() && !(self.turn && self.key.is_none()) {
            self.cancel();
            return Poll::Ready(());
        }
        let deadline = self.deadline;
        let key = self.key;
        let key = TIMERS.with(|timers| {
            let mut timers = timers.borrow_mut();
            let key = key.unwrap_or_else(|| {
                timers.next += 1;
                timers.next
            });
            timers.waiting.insert((deadline, key), cx.waker().clone());
            arm(&mut timers);
            key
        });
        self.key = Some(key);
        Poll::Pending
    }
}
impl Drop for Sleep {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep {
        deadline,
        key: None,
        turn: false,
    }
}
pub(crate) fn turn() -> Sleep {
    Sleep {
        deadline: Instant::now(),
        key: None,
        turn: true,
    }
}
pub fn sleep(duration: Duration) -> Sleep {
    sleep_until(Instant::now() + duration)
}

pub mod error {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Elapsed(());
    impl std::fmt::Display for Elapsed {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("deadline has elapsed")
        }
    }
    impl std::error::Error for Elapsed {}
    impl From<Elapsed> for std::io::Error {
        fn from(_: Elapsed) -> Self {
            std::io::ErrorKind::TimedOut.into()
        }
    }
    pub(super) const ELAPSED: Elapsed = Elapsed(());
}

#[derive(Debug)]
pub struct Timeout<F> {
    future: Pin<Box<F>>,
    sleep: Sleep,
}
impl<F: Future> Future for Timeout<F> {
    type Output = Result<F::Output, error::Elapsed>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(output) = self.future.as_mut().poll(cx) {
            return Poll::Ready(Ok(output));
        }
        match Pin::new(&mut self.sleep).poll(cx) {
            Poll::Ready(()) => Poll::Ready(Err(error::ELAPSED)),
            Poll::Pending => Poll::Pending,
        }
    }
}
pub fn timeout_at<F: Future>(deadline: Instant, future: F) -> Timeout<F> {
    Timeout {
        future: Box::pin(future),
        sleep: sleep_until(deadline),
    }
}
pub fn timeout<F: Future>(duration: Duration, future: F) -> Timeout<F> {
    timeout_at(Instant::now() + duration, future)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissedTickBehavior {
    Burst,
    Skip,
}

#[derive(Debug)]
pub struct Interval {
    next: Instant,
    period: Duration,
    missed: MissedTickBehavior,
}
impl Interval {
    pub async fn tick(&mut self) -> Instant {
        sleep_until(self.next).await;
        let tick = self.next;
        let now = Instant::now();
        self.next = match self.missed {
            MissedTickBehavior::Burst => tick + self.period,
            MissedTickBehavior::Skip => {
                let mut next = tick + self.period;
                while next <= now {
                    next += self.period;
                }
                next
            }
        };
        tick
    }
    pub fn set_missed_tick_behavior(&mut self, missed: MissedTickBehavior) {
        self.missed = missed;
    }
    pub fn reset(&mut self) {
        self.next = Instant::now() + self.period;
    }
}
pub fn interval(period: Duration) -> Interval {
    Interval {
        next: Instant::now(),
        period,
        missed: MissedTickBehavior::Burst,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn browser_timeouts_round_up_so_a_timer_never_fires_early() {
        assert_eq!(browser_timeout_millis(Duration::ZERO), 0);
        assert_eq!(browser_timeout_millis(Duration::from_micros(1)), 1);
        assert_eq!(browser_timeout_millis(Duration::from_millis(1)), 1);
        assert_eq!(browser_timeout_millis(Duration::from_micros(1001)), 2);
        assert_eq!(browser_timeout_millis(Duration::MAX), i32::MAX);
    }
}
