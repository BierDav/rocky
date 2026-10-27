use std::future::Future;
use tokio::task::{JoinHandle, JoinSet};

pub(crate) fn spawn_local<T: 'static>(
    name: &'static str,
    future: impl Future<Output = T> + 'static,
) -> JoinHandle<T> {
    tokio::task::spawn_local(task(name, future))
}

pub(crate) fn spawn_in<T: 'static>(
    tasks: &mut JoinSet<T>,
    name: &'static str,
    future: impl Future<Output = T> + 'static,
) {
    tasks.spawn_local(task(name, future));
}

#[cfg(not(feature = "qualification"))]
fn task<T>(_: &'static str, future: impl Future<Output = T>) -> impl Future<Output = T> {
    future
}

#[cfg(feature = "qualification")]
use qualification::task;
#[cfg(feature = "qualification")]
pub(crate) use qualification::{CallTimer, current, scope};
#[cfg(feature = "qualification")]
pub use qualification::{ReactorMeasurements, ReactorSnapshot, SqlCallTiming};

#[cfg(feature = "qualification")]
mod qualification {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    tokio::task_local! { static RECORDING: ReactorMeasurements; }

    #[derive(Clone, Default)]
    pub struct ReactorMeasurements(Arc<Mutex<Option<ReactorSnapshot>>>);

    #[derive(Debug, Default)]
    pub struct ReactorSnapshot {
        /// Polls longer than 1 ms, in microseconds, grouped by spawn-site name.
        pub polls: BTreeMap<String, Vec<u64>>,
        pub calls: BTreeMap<u64, Vec<SqlCallTiming>>,
    }

    #[derive(Debug)]
    pub struct SqlCallTiming {
        /// Process-local bounds for measuring overlapping calls as one interval.
        pub queued_at: Instant,
        pub replied_at: Instant,
        pub queue_us: u64,
        pub in_call_us: u64,
    }

    impl ReactorMeasurements {
        pub fn start(&self) {
            *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(ReactorSnapshot::default());
        }
        pub fn take(&self) -> ReactorSnapshot {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default()
        }
        pub fn stop(&self) -> ReactorSnapshot {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or_default()
        }
        fn record(&self, update: impl FnOnce(&mut ReactorSnapshot)) {
            if let Some(snapshot) = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
            {
                update(snapshot);
            }
        }
    }

    pub(crate) fn current() -> Option<ReactorMeasurements> {
        RECORDING.try_with(Clone::clone).ok()
    }

    pub(crate) async fn scope<T>(
        measurements: ReactorMeasurements,
        future: impl Future<Output = T>,
    ) -> T {
        RECORDING
            .scope(measurements, async move { task("database", future).await })
            .await
    }

    pub(super) fn task<T>(
        name: &'static str,
        future: impl Future<Output = T>,
    ) -> impl Future<Output = T> {
        let recording = current();
        async move {
            let Some(recording) = recording else {
                return future.await;
            };
            let measured = recording.clone();
            let mut future = std::pin::pin!(future);
            let polling = std::future::poll_fn(move |cx| {
                let start = Instant::now();
                let result = future.as_mut().poll(cx);
                let elapsed = start.elapsed();
                if elapsed > Duration::from_millis(1) {
                    measured.record(|snapshot| {
                        snapshot
                            .polls
                            .entry(name.into())
                            .or_default()
                            .push(elapsed.as_micros() as u64)
                    });
                }
                result
            });
            RECORDING.scope(recording, polling).await
        }
    }

    pub(crate) struct CallTimer {
        queued: Instant,
        started: Instant,
    }
    impl CallTimer {
        pub(crate) fn start(queued: Instant) -> Self {
            Self {
                queued,
                started: Instant::now(),
            }
        }
        pub(crate) fn finish(self, connection: u64) {
            let replied_at = Instant::now();
            let timing = SqlCallTiming {
                queued_at: self.queued,
                replied_at,
                queue_us: self.started.duration_since(self.queued).as_micros() as u64,
                in_call_us: replied_at.duration_since(self.started).as_micros() as u64,
            };
            if let Some(recording) = current() {
                recording
                    .record(|snapshot| snapshot.calls.entry(connection).or_default().push(timing));
            }
        }
    }
}
