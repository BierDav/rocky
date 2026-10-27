use std::{
    ops::{Deref, DerefMut},
    panic::Location,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Class {
    Interactive,
    Background,
}

/// The raw mutex stays private so every acquisition has a class and a caller.
pub(crate) struct ReplicaLock<T> {
    mutex: Arc<Mutex<T>>,
    #[cfg(any(test, feature = "qualification"))]
    measurements: LockMeasurements,
}
impl<T> ReplicaLock<T> {
    pub(crate) fn new(
        value: T,
        #[cfg(any(test, feature = "qualification"))] measurements: LockMeasurements,
    ) -> Self {
        Self {
            mutex: Arc::new(Mutex::new(value)),
            #[cfg(any(test, feature = "qualification"))]
            measurements,
        }
    }

    #[track_caller]
    pub(crate) fn lock(&self, class: Class) -> impl Future<Output = Guard<MutexGuard<'_, T>>> {
        self.acquire(class, self.mutex.lock())
    }

    #[track_caller]
    pub(crate) fn lock_owned(
        &self,
        class: Class,
    ) -> impl Future<Output = Guard<OwnedMutexGuard<T>>> + use<T> {
        self.acquire(class, self.mutex.clone().lock_owned())
    }

    #[track_caller]
    fn acquire<G, F: Future<Output = G>>(
        &self,
        class: Class,
        future: F,
    ) -> impl Future<Output = Guard<G>> + use<T, G, F> {
        let site = Location::caller();
        // With no sink only acquisition and release need clock reads.
        let timing = cfg!(feature = "qualification")
            || tracing::enabled!(target: "replica", tracing::Level::DEBUG);
        let requested = timing.then(Instant::now);
        #[cfg(any(test, feature = "qualification"))]
        let measurements = self.measurements.clone();
        async move {
            let guard = future.await;
            let acquired = Instant::now();
            #[cfg(any(test, feature = "qualification"))]
            let work_start = measurements.work_snapshot();
            Guard {
                guard,
                #[cfg(any(test, feature = "qualification"))]
                work_start,
                _timing: Hold {
                    site,
                    class,
                    acquired,
                    wait: requested.map_or(Duration::ZERO, |start| acquired.duration_since(start)),
                    #[cfg(any(test, feature = "qualification"))]
                    measurements,
                    #[cfg(any(test, feature = "qualification"))]
                    connection: qualification::connection(),
                    #[cfg(any(test, feature = "qualification"))]
                    units: Vec::new(),
                    #[cfg(any(test, feature = "qualification"))]
                    normal_rows: super::group::APPLICATION_ROWS,
                    #[cfg(any(test, feature = "qualification"))]
                    input_rows: 1,
                },
            }
        }
    }

    #[cfg(any(test, feature = "qualification"))]
    pub(crate) fn measurements(&self) -> LockMeasurements {
        self.measurements.clone()
    }
}

pub(crate) struct Guard<G> {
    // Field order releases the mutex before recording its completed hold.
    guard: G,
    _timing: Hold,
    #[cfg(any(test, feature = "qualification"))]
    work_start: [u64; 16],
}
struct Hold {
    site: &'static Location<'static>,
    class: Class,
    acquired: Instant,
    wait: Duration,
    #[cfg(any(test, feature = "qualification"))]
    measurements: LockMeasurements,
    #[cfg(any(test, feature = "qualification"))]
    connection: Option<u64>,
    #[cfg(any(test, feature = "qualification"))]
    units: Vec<(&'static str, u64)>,
    #[cfg(any(test, feature = "qualification"))]
    normal_rows: u64,
    #[cfg(any(test, feature = "qualification"))]
    input_rows: u64,
}
#[cfg(any(test, feature = "qualification"))]
impl<G> Guard<G> {
    pub(crate) fn row_work(&mut self, bound: u64, inputs: u64) {
        self._timing.normal_rows = bound;
        self._timing.input_rows = inputs;
    }
}
#[cfg(any(test, feature = "qualification"))]
impl<G> Guard<G> {
    pub(crate) fn units(&mut self, unit: &'static str, count: u64) {
        if count > 0 {
            self._timing.units.push((unit, count));
        }
    }
}
#[cfg(any(test, feature = "qualification"))]
impl<G> Drop for Guard<G> {
    fn drop(&mut self) {
        let end = self._timing.measurements.work_snapshot();
        for (index, unit) in [
            "replayed_frame",
            "base_row",
            "checkpoint_row",
            "constraint_dependent_row",
            "maintenance_record",
            "held_refresh_record",
            "held_refresh_frame",
            "held_member_frame",
            "held_member_row",
            "held_token_record",
            "held_page_record",
            "received_input_row",
            "held_listing_row",
            "migration_storage_record",
            "prune_candidate",
            "reclaimed_page",
        ]
        .into_iter()
        .enumerate()
        {
            let count = end[index].saturating_sub(self.work_start[index]);
            if count > 0 {
                self._timing.units.push((unit, count));
            }
        }
    }
}
impl<G: Deref> Deref for Guard<G> {
    type Target = G::Target;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}
impl<G: DerefMut> DerefMut for Guard<G> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
impl Drop for Hold {
    fn drop(&mut self) {
        let hold = self.acquired.elapsed();
        if hold > Duration::from_millis(1) {
            tracing::debug!(target: "replica", site = %self.site, class = ?self.class,
                wait_us = self.wait.as_micros() as u64, hold_us = hold.as_micros() as u64,
                "replica lock hold");
        }
        #[cfg(any(test, feature = "qualification"))]
        self.measurements.record(
            self.site,
            self.class,
            (self.wait, hold),
            self.connection,
            &self.units,
            (self.normal_rows, self.input_rows),
        );
    }
}

#[cfg(any(test, feature = "qualification"))]
mod qualification {
    use super::*;
    use std::collections::BTreeMap;

    tokio::task_local! { static CONNECTION: u64; }
    pub(super) fn connection() -> Option<u64> {
        CONNECTION.try_with(|id| *id).ok()
    }
    /// Carries an SDK SQL connection identity across its server-side task.
    #[cfg(feature = "qualification")]
    pub async fn with_measurement_connection<T>(id: u64, future: impl Future<Output = T>) -> T {
        CONNECTION.scope(id, future).await
    }
    /// Opt-in, replica-local histograms. Disabled until `start` is called.
    #[derive(Clone, Default)]
    pub struct LockMeasurements(Arc<Recording>);
    #[derive(Default)]
    struct Recording {
        data: std::sync::Mutex<Option<Measurements>>,
        work: [std::sync::atomic::AtomicU64; 16],
    }
    pub(crate) enum WorkUnit {
        #[cfg(feature = "qualification")]
        ReplayedFrame = 0,
        BaseRow = 1,
        #[cfg(feature = "qualification")]
        CheckpointRow = 2,
        DependentRow = 3,
        MaintenanceRecord = 4,
        HeldRefreshRecord = 5,
        HeldRefreshFrame = 6,
        HeldMemberFrame = 7,
        HeldMemberRow = 8,
        HeldTokenRecord = 9,
        HeldPageRecord = 10,
        ReceivedInputRow = 11,
        HeldListingRow = 12,
        MigrationStorageRecord = 13,
        PruneCandidate = 14,
        ReclaimedPage = 15,
    }

    #[derive(Default, Debug)]
    pub struct Measurements {
        pub holders: BTreeMap<(String, String), HolderMeasurements>,
        pub flush_us: Vec<u64>,
        pub connections: BTreeMap<u64, ConnectionMeasurements>,
        pub checkpoints: BTreeMap<String, Vec<(u64, u64)>>,
    }
    #[derive(Default, Debug)]
    pub struct HolderMeasurements {
        pub wait_us: Vec<u64>,
        pub hold_us: Vec<u64>,
        /// Oversized dependent fan-out: (induced typed rows, full hold us).
        /// These acquisitions are excluded from the ordinary hold histogram.
        pub dependent_fanout_us: Vec<(u64, u64)>,
        /// Existing indivisible-request exception, separate from induced fan-out.
        pub oversized_request_us: Vec<u64>,
        /// (work count, full acquisition hold in microseconds).
        pub units: BTreeMap<String, Vec<(u64, u64)>>,
    }
    #[derive(Default, Debug)]
    pub struct ConnectionMeasurements {
        pub wait_us: u64,
        pub hold_us: u64,
        pub flush_us: u64,
    }
    impl LockMeasurements {
        pub(crate) fn work(&self, unit: WorkUnit, count: u64) {
            self.0.work[unit as usize].fetch_add(count, std::sync::atomic::Ordering::Relaxed);
        }
        pub(super) fn work_snapshot(&self) -> [u64; 16] {
            std::array::from_fn(|index| {
                self.0.work[index].load(std::sync::atomic::Ordering::Relaxed)
            })
        }

        pub fn start(&self) {
            *self
                .0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Measurements::default());
        }
        /// Drains the interval while continuing to record the next one.
        pub fn take(&self) -> Measurements {
            self.0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
                .map(std::mem::take)
                .unwrap_or_default()
        }
        pub fn stop(&self) -> Measurements {
            self.0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or_default()
        }
        pub(crate) fn record(
            &self,
            site: &Location<'_>,
            class: Class,
            (wait, hold): (Duration, Duration),
            connection: Option<u64>,
            units: &[(&str, u64)],
            (normal_rows, input_rows): (u64, u64),
        ) {
            if let Some(measurements) = self
                .0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
            {
                let holder = measurements
                    .holders
                    .entry((site.to_string(), format!("{class:?}")))
                    .or_default();
                holder.wait_us.push(wait.as_micros() as u64);
                let dependent = units
                    .iter()
                    .filter(|(unit, _)| *unit == "constraint_dependent_row")
                    .map(|(_, count)| count)
                    .sum::<u64>();
                let oversized = units.iter().any(|(unit, count)| {
                    (*unit == "oversized_request" && *count > 0)
                        || (*unit == "held_member_row"
                            && *count > super::super::group::APPLICATION_ROWS)
                        || (*unit == "received_input_row" && *count > normal_rows)
                });
                if dependent > 0 && dependent.saturating_add(input_rows) > normal_rows {
                    holder
                        .dependent_fanout_us
                        .push((dependent, hold.as_micros() as u64));
                } else if oversized {
                    holder.oversized_request_us.push(hold.as_micros() as u64);
                } else {
                    holder.hold_us.push(hold.as_micros() as u64);
                }
                for (unit, count) in units {
                    holder
                        .units
                        .entry((*unit).into())
                        .or_default()
                        .push((*count, hold.as_micros() as u64));
                }
                if let Some(id) = connection {
                    let work = measurements.connections.entry(id).or_default();
                    work.wait_us += wait.as_micros() as u64;
                    work.hold_us += hold.as_micros() as u64;
                }
            }
        }
        #[cfg(feature = "qualification")]
        pub(crate) fn checkpoint(&self, mode: &str, pages: u64, elapsed: Duration) {
            if let Some(measurements) = self
                .0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
            {
                measurements
                    .checkpoints
                    .entry(mode.into())
                    .or_default()
                    .push((pages, elapsed.as_micros() as u64));
            }
        }
        #[cfg(feature = "qualification")]
        pub(crate) fn flush(&self, elapsed: Duration) {
            if let Some(measurements) = self
                .0
                .data
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
            {
                measurements.flush_us.push(elapsed.as_micros() as u64);
                if let Some(id) = connection() {
                    measurements.connections.entry(id).or_default().flush_us +=
                        elapsed.as_micros() as u64;
                }
            }
        }
    }
}
#[cfg(any(test, feature = "qualification"))]
pub use qualification::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, sync::Mutex as StdMutex};
    use tracing::{
        Subscriber,
        field::{Field, Visit},
        span::{Attributes, Id, Record},
    };

    #[derive(Clone, Default)]
    struct Events(Arc<StdMutex<Vec<BTreeMap<String, String>>>>);
    struct Fields(BTreeMap<String, String>);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.insert(field.name().into(), format!("{value:?}"));
        }
    }
    impl Subscriber for Events {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            assert_eq!(event.metadata().target(), "replica");
            assert_eq!(event.metadata().level(), &tracing::Level::DEBUG);
            let mut fields = Fields(BTreeMap::new());
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0);
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[tokio::test]
    async fn slow_holds_report_the_request_site_and_class() {
        let events = Events::default();
        let _subscriber = tracing::subscriber::set_default(events.clone());
        let mutex = ReplicaLock::new(
            (),
            #[cfg(any(test, feature = "qualification"))]
            LockMeasurements::default(),
        );
        let line = line!() + 1;
        let future = mutex.lock(Class::Interactive);
        // Polling somewhere else must not change the attributed caller.
        let mut guard = future.await;
        guard._timing.acquired -= Duration::from_millis(2);
        drop(guard);
        let events = events.0.lock().unwrap();
        let fields = &events[0];
        assert!(
            fields["site"].contains(&format!("lock.rs:{line}:")),
            "{fields:?}"
        );
        assert_eq!(fields["class"], "Interactive");
        assert!(fields["hold_us"].parse::<u64>().unwrap() >= 2000);
        assert!(fields["wait_us"].parse::<u64>().is_ok());
    }

    #[cfg(any(test, feature = "qualification"))]
    #[tokio::test]
    async fn histogram_records_both_classes_and_owned_guards() {
        let measurements = LockMeasurements::default();
        let mutex = ReplicaLock::new((), measurements.clone());
        measurements.start();
        let mut guard = mutex.lock(Class::Background).await;
        guard._timing.acquired -= Duration::from_millis(2);
        measurements.work(WorkUnit::BaseRow, 7);
        measurements.work(WorkUnit::DependentRow, 4);
        drop(guard);
        let line = line!() + 1;
        let future = mutex.lock_owned(Class::Interactive);
        drop(future.await);
        let snapshot = measurements.stop();
        assert_eq!(snapshot.holders.len(), 2);
        let background = snapshot
            .holders
            .iter()
            .find(|((_, class), _)| class == "Background")
            .unwrap()
            .1;
        let samples = &background.units["base_row"];
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].0, 7);
        assert!(samples[0].1 >= 2000);
        assert!(background.hold_us.is_empty());
        assert_eq!(background.dependent_fanout_us.len(), 1);
        assert_eq!(background.dependent_fanout_us[0].0, 4);
        let interactive = snapshot
            .holders
            .iter()
            .find(|((_, class), _)| class == "Interactive")
            .unwrap();
        assert!(interactive.0.0.contains(&format!("lock.rs:{line}:")));
        assert_eq!(interactive.1.hold_us.len(), 1);
        assert!(interactive.1.dependent_fanout_us.is_empty());
        assert!(measurements.take().holders.is_empty());
    }
    #[tokio::test]
    async fn indivisible_requests_keep_their_own_histogram() {
        let measurements = LockMeasurements::default();
        let mutex = ReplicaLock::new((), measurements.clone());
        measurements.start();
        for rows in [0, 2] {
            let guard = mutex.lock(Class::Background).await;
            measurements.work(WorkUnit::HeldMemberRow, rows);
            drop(guard);
        }
        let snapshot = measurements.stop();
        assert_eq!(snapshot.holders.len(), 1);
        let holder = snapshot.holders.values().next().unwrap();
        assert_eq!(holder.wait_us.len(), 2);
        assert_eq!(holder.hold_us.len(), 1);
        assert_eq!(holder.oversized_request_us.len(), 1);
        assert!(holder.dependent_fanout_us.is_empty());
    }
}
