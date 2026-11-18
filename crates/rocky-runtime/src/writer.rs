use crate::{
    Command, Durability, QueuedCommand, Reply, RuntimeOptions,
    database::DatabaseOwner,
    scoped_connection::ScopedConnection,
    sqlite,
    storage::{self, Error, retryable, rows},
    worker::{Notifications, Worker},
};
use rocky_core::{SqlValue, StoredApplication};
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    task::Poll,
};
use tokio::sync::mpsc;
use web_time::Instant;

pub(crate) const MAX_GROUP_BYTES: usize = 4 * crate::MAX_FRAME_BYTES;
pub(crate) const CHECKPOINT_FRAMES: u32 = 8_192;
const WAL_SIZE_LIMIT: i64 = 4 * 1024 * 1024;
#[cfg(target_os = "android")]
const WRITER_CACHE_KIB: i64 = -8 * 1024;
#[cfg(not(target_os = "android"))]
const WRITER_CACHE_KIB: i64 = -32 * 1024;
const RESET_WAIT: std::time::Duration = std::time::Duration::from_millis(50);
const MAX_RESET_BACKOFF: u32 = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriterStats {
    pub groups: u64,
    pub steps: u64,
    pub flushes: u64,
    pub step_micros: u64,
    pub commit_micros: u64,
    pub read_micros: u64,
    pub admission_micros: u64,
    pub reduction_micros: u64,
    pub encoding_micros: u64,
    pub sql_micros: u64,
    pub authoring_steps: u64,
    pub incoming_steps: u64,
    pub authoring_step_micros: u64,
    pub incoming_step_micros: u64,
    pub checkpoints: u64,
    pub checkpoint_micros: u64,
    pub checkpoint_max_micros: u64,
    pub checkpoint_wal_flush_micros: u64,
    pub checkpoint_copy_micros: u64,
    pub checkpoint_flush_micros: u64,
    pub truncations: u64,
    pub truncate_micros: u64,
    pub truncate_waits: u64,
    pub wal_frames: u64,
    pub wal_frames_max: u64,
    pub deferred_follows: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepKind {
    Authoring,
    Incoming,
    Other,
}
#[derive(Default)]
pub(crate) struct Metrics {
    groups: AtomicU64,
    steps: AtomicU64,
    flushes: AtomicU64,
    step_micros: AtomicU64,
    commit_micros: AtomicU64,
    read_micros: AtomicU64,
    admission_nanos: AtomicU64,
    reduction_nanos: AtomicU64,
    encoding_nanos: AtomicU64,
    sql_nanos: AtomicU64,
    authoring_steps: AtomicU64,
    incoming_steps: AtomicU64,
    authoring_step_micros: AtomicU64,
    incoming_step_micros: AtomicU64,
    checkpoints: AtomicU64,
    checkpoint_micros: AtomicU64,
    checkpoint_max_micros: AtomicU64,
    checkpoint_wal_flush_micros: AtomicU64,
    checkpoint_copy_micros: AtomicU64,
    checkpoint_flush_micros: AtomicU64,
    truncations: AtomicU64,
    truncate_micros: AtomicU64,
    truncate_waits: AtomicU64,
    wal_frames: AtomicU64,
    wal_frames_max: AtomicU64,
    deferred_follows: AtomicU64,
    checkpoint_log: std::sync::Mutex<CheckpointLog>,
}
pub(crate) const MAX_CHECKPOINT_INTERVALS: usize = 4096;
#[derive(Default)]
struct CheckpointLog {
    intervals: VecDeque<CheckpointInterval>,
    dropped: u64,
}
impl Metrics {
    pub(crate) fn add_admission(&self, nanos: u64) {
        self.admission_nanos.fetch_add(nanos, Relaxed);
    }
    pub(crate) fn add_reduction(&self, nanos: u64) {
        self.reduction_nanos.fetch_add(nanos, Relaxed);
    }
    pub(crate) fn add_encoding(&self, nanos: u64) {
        self.encoding_nanos.fetch_add(nanos, Relaxed);
    }
    pub(crate) fn add_sql(&self, nanos: u64) {
        self.sql_nanos.fetch_add(nanos, Relaxed);
    }
    pub(crate) fn deferred_follow(&self) {
        self.deferred_follows.fetch_add(1, Relaxed);
    }
    pub(crate) fn add_step(&self, kind: StepKind, micros: u64) {
        match kind {
            StepKind::Authoring => {
                self.authoring_steps.fetch_add(1, Relaxed);
                self.authoring_step_micros.fetch_add(micros, Relaxed);
            }
            StepKind::Incoming => {
                self.incoming_steps.fetch_add(1, Relaxed);
                self.incoming_step_micros.fetch_add(micros, Relaxed);
            }
            StepKind::Other => {}
        }
    }
    pub(crate) fn snapshot(&self) -> WriterStats {
        WriterStats {
            groups: self.groups.load(Relaxed),
            steps: self.steps.load(Relaxed),
            flushes: self.flushes.load(Relaxed),
            step_micros: self.step_micros.load(Relaxed),
            commit_micros: self.commit_micros.load(Relaxed),
            read_micros: self.read_micros.load(Relaxed),
            admission_micros: self.admission_nanos.load(Relaxed) / 1_000,
            reduction_micros: self.reduction_nanos.load(Relaxed) / 1_000,
            encoding_micros: self.encoding_nanos.load(Relaxed) / 1_000,
            sql_micros: self.sql_nanos.load(Relaxed) / 1_000,
            authoring_steps: self.authoring_steps.load(Relaxed),
            incoming_steps: self.incoming_steps.load(Relaxed),
            authoring_step_micros: self.authoring_step_micros.load(Relaxed),
            incoming_step_micros: self.incoming_step_micros.load(Relaxed),
            checkpoints: self.checkpoints.load(Relaxed),
            checkpoint_micros: self.checkpoint_micros.load(Relaxed),
            checkpoint_max_micros: self.checkpoint_max_micros.load(Relaxed),
            checkpoint_wal_flush_micros: self.checkpoint_wal_flush_micros.load(Relaxed),
            checkpoint_copy_micros: self.checkpoint_copy_micros.load(Relaxed),
            checkpoint_flush_micros: self.checkpoint_flush_micros.load(Relaxed),
            truncations: self.truncations.load(Relaxed),
            truncate_micros: self.truncate_micros.load(Relaxed),
            truncate_waits: self.truncate_waits.load(Relaxed),
            wal_frames: self.wal_frames.load(Relaxed),
            wal_frames_max: self.wal_frames_max.load(Relaxed),
            deferred_follows: self.deferred_follows.load(Relaxed),
        }
    }
    pub(crate) fn record_checkpoint(&self, interval: CheckpointInterval) {
        let Ok(mut log) = self.checkpoint_log.lock() else {
            return;
        };
        if log.intervals.len() >= MAX_CHECKPOINT_INTERVALS {
            log.intervals.pop_front();
            log.dropped += 1;
        }
        log.intervals.push_back(interval);
    }
    pub(crate) fn checkpoint_intervals(&self) -> (Vec<CheckpointInterval>, u64) {
        let Ok(log) = self.checkpoint_log.lock() else {
            return (Vec::new(), 0);
        };
        (log.intervals.iter().copied().collect(), log.dropped)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointKind {
    Passive,
    Truncate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointInterval {
    pub start: Instant,
    pub end: Instant,
    pub kind: CheckpointKind,
    pub copy_micros: u64,
    pub flush_micros: u64,
}
pub(crate) fn micros(since: Instant) -> u64 {
    since.elapsed().as_micros() as u64
}
pub(crate) fn nanos(since: Instant) -> u64 {
    since.elapsed().as_nanos() as u64
}

const FLUSH_TO_DEVICE: i64 = 1;
const NO_AUTOCHECKPOINT: i64 = 0;
pub(crate) async fn configure(
    conn: &ScopedConnection,
    durability: Durability,
) -> Result<(), Error> {
    let synchronous = durability.synchronous();
    let settings = [
        (
            "journal_mode",
            "wal".to_string(),
            SqlValue::Text("wal".into()),
        ),
        (
            "synchronous",
            synchronous.to_string(),
            SqlValue::Integer(synchronous),
        ),
        (
            "fullfsync",
            FLUSH_TO_DEVICE.to_string(),
            SqlValue::Integer(FLUSH_TO_DEVICE),
        ),
        (
            "checkpoint_fullfsync",
            FLUSH_TO_DEVICE.to_string(),
            SqlValue::Integer(FLUSH_TO_DEVICE),
        ),
        (
            "wal_autocheckpoint",
            NO_AUTOCHECKPOINT.to_string(),
            SqlValue::Integer(NO_AUTOCHECKPOINT),
        ),
        (
            "cache_size",
            WRITER_CACHE_KIB.to_string(),
            SqlValue::Integer(WRITER_CACHE_KIB),
        ),
        (
            "journal_size_limit",
            WAL_SIZE_LIMIT.to_string(),
            SqlValue::Integer(WAL_SIZE_LIMIT),
        ),
    ];
    for (name, value, _) in &settings {
        rows(conn, &format!("PRAGMA {name}={value}")).await?;
    }
    for (name, _, expected) in settings {
        let sql = format!("PRAGMA {name}");
        if rows(conn, &sql).await? != vec![vec![expected]] {
            return Err(Error::Durability(sql));
        }
    }
    conn.connection().track_wal()?;
    Ok(())
}

pub(crate) trait Answer {
    fn fatal(&self) -> bool;
    fn send(self: Box<Self>, flushed: bool);
    fn fail(self: Box<Self>, error: Error);
}
pub(crate) struct Answered<T> {
    pub(crate) reply: Reply<T>,
    pub(crate) result: Result<T, Error>,
}
impl<T> Answer for Answered<T> {
    fn fatal(&self) -> bool {
        matches!(self.result, Err(Error::Rollback { .. }))
    }
    fn send(self: Box<Self>, _: bool) {
        let _ = self.reply.send(self.result);
    }
    fn fail(self: Box<Self>, error: Error) {
        let _ = self.reply.send(Err(error));
    }
}
pub(crate) struct Committing {
    pub(crate) reply: Reply<crate::CommittedMutation>,
    pub(crate) result: Result<crate::CommittedMutation, Error>,
}
impl Answer for Committing {
    fn fatal(&self) -> bool {
        matches!(self.result, Err(Error::Rollback { .. }))
    }
    fn send(self: Box<Self>, flushed: bool) {
        let result = self.result.map(|mut committed| {
            if flushed {
                committed.class = crate::CommitClass::Flushed;
            }
            committed
        });
        let _ = self.reply.send(result);
    }
    fn fail(self: Box<Self>, error: Error) {
        let _ = self.reply.send(Err(error));
    }
}
pub(crate) struct Unanswered;
impl Answer for Unanswered {
    fn fatal(&self) -> bool {
        false
    }
    fn send(self: Box<Self>, _: bool) {}
    fn fail(self: Box<Self>, _: Error) {}
}

struct Slot<A: StoredApplication> {
    worker: Option<Worker<A>>,
    receiver: mpsc::Receiver<QueuedCommand<A::Row>>,
    deferred: VecDeque<QueuedCommand<A::Row>>,
    drained: bool,
    closing: Option<Reply<()>>,
}
struct File {
    conn: Arc<sqlite::Connection>,
    _owner: DatabaseOwner,
    pool: Arc<crate::read::ReadPool>,
}
pub(crate) struct Writer<A: StoredApplication> {
    slots: Vec<Slot<A>>,
    file: Option<File>,
    group: usize,
    cursor: usize,
    durability: Durability,
    flush_wanted: bool,
    waiting: Vec<usize>,
    checkpoint_frames: i64,
    reset_backoff: u32,
    reset_skip: u32,
    instrument: bool,
    metrics: Arc<Metrics>,
    #[cfg(test)]
    fault: Option<crate::worker::Fault>,
    #[cfg(test)]
    groups: Option<crate::worker::GroupLog>,
    #[cfg(test)]
    snapshots: Option<std::path::PathBuf>,
}
type Work<R> = (usize, QueuedCommand<R>);
pub(crate) type Domain<A> = (
    Worker<A>,
    mpsc::Receiver<QueuedCommand<<A as StoredApplication>::Row>>,
);

impl<A> Writer<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    pub(crate) fn new(
        owner: DatabaseOwner,
        conn: Arc<sqlite::Connection>,
        workers: Vec<Domain<A>>,
        options: RuntimeOptions,
        pool: Arc<crate::read::ReadPool>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, Error> {
        let reader = Arc::new(owner.connect_read_only()?);
        pool.attach(owner.path().to_path_buf());
        reader.set_query_only()?;
        let slots = workers
            .into_iter()
            .map(|(mut worker, receiver)| {
                worker.attach_reader(reader.clone());
                Slot {
                    worker: Some(worker),
                    receiver,
                    deferred: VecDeque::new(),
                    drained: false,
                    closing: None,
                }
            })
            .collect();
        Ok(Self {
            slots,
            file: Some(File {
                conn,
                _owner: owner,
                pool,
            }),
            group: options.group_commit.max(1),
            cursor: 0,
            durability: options.durability,
            flush_wanted: true,
            waiting: Vec::new(),
            checkpoint_frames: i64::from(options.checkpoint_frames),
            reset_backoff: 0,
            reset_skip: 0,
            instrument: options.instrument,
            metrics,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            groups: None,
            #[cfg(test)]
            snapshots: None,
        })
    }
    pub(crate) async fn run(mut self) {
        let mut stopped = self.flush().await;
        while !stopped {
            self.retire().await;
            if self.slots.iter().all(|slot| slot.worker.is_none()) {
                break;
            }
            if self.slots.iter().all(|slot| slot.deferred.is_empty()) {
                self.wait().await;
            }
            let (group, exposing) = self.collect().await;
            if group.is_empty() {
                stopped = self.flush_wanted && self.flush().await;
                continue;
            }
            self.cursor = (self.cursor + 1) % self.slots.len();
            let full = exposing || self.flush_wanted || self.durability == Durability::Full;
            stopped = self.commit(group, full).await;
        }
        let notifications: Vec<_> = self
            .slots
            .iter_mut()
            .filter_map(|slot| slot.worker.take())
            .map(Worker::into_notifications)
            .collect();
        self.close_file().await;
        drop(notifications);
    }
    async fn wait(&mut self) {
        std::future::poll_fn(|context| {
            let mut ready = true;
            for slot in &mut self.slots {
                if slot.worker.is_none() || slot.drained {
                    continue;
                }
                match slot.receiver.poll_recv(context) {
                    Poll::Ready(Some(command)) => {
                        slot.deferred.push_back(command);
                        return Poll::Ready(());
                    }
                    Poll::Ready(None) => {
                        slot.drained = true;
                        return Poll::Ready(());
                    }
                    Poll::Pending => ready = false,
                }
            }
            if ready {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
    fn next(&mut self, index: usize) -> Option<QueuedCommand<A::Row>> {
        let slot = &mut self.slots[index];
        slot.worker.as_ref()?;
        if let Some(command) = slot.deferred.pop_front() {
            return Some(command);
        }
        if slot.drained {
            return None;
        }
        match slot.receiver.try_recv() {
            Ok(command) => Some(command),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                slot.drained = true;
                None
            }
            Err(mpsc::error::TryRecvError::Empty) => None,
        }
    }
    async fn collect(&mut self) -> (Vec<Work<A::Row>>, bool) {
        let count = self.slots.len();
        let start = self.cursor % count;
        let mut exposing = false;
        let mut group = Vec::new();
        let mut bytes = 0usize;
        let mut writing = vec![false; count];
        let mut done = vec![false; count];
        loop {
            let mut taken = false;
            for index in (0..count).map(|offset| (start + offset) % count) {
                while !done[index] {
                    let Some(queued) = self.next(index) else {
                        done[index] = true;
                        break;
                    };
                    if !queued.command.writes() {
                        if writing[index] {
                            self.slots[index].deferred.push_front(queued);
                            done[index] = true;
                        } else if queued.command.exposes() && self.dirty(index) {
                            self.slots[index].deferred.push_front(queued);
                            done[index] = true;
                            self.flush_wanted = true;
                            self.waiting.push(index);
                        } else {
                            let started = Instant::now();
                            self.serve(index, queued.command).await;
                            self.metrics.read_micros.fetch_add(micros(started), Relaxed);
                        }
                        continue;
                    }
                    let weight = queued._budget.num_permits();
                    if group.len() >= self.group
                        || (!group.is_empty() && bytes.saturating_add(weight) > MAX_GROUP_BYTES)
                    {
                        self.slots[index].deferred.push_front(queued);
                        return (group, exposing);
                    }
                    exposing |= queued.command.exposes();
                    bytes = bytes.saturating_add(weight);
                    writing[index] = true;
                    group.push((index, queued));
                    taken = true;
                    break;
                }
            }
            if !taken {
                return (group, exposing);
            }
        }
    }
    async fn serve(&mut self, index: usize, command: Command<A::Row>) {
        let closing = self.slots[index].closing.is_some();
        match command {
            Command::Close(reply) if closing => drop(reply.send(Err(Error::Closed))),
            Command::Close(reply) => {
                let slot = &mut self.slots[index];
                slot.receiver.close();
                slot.closing = Some(reply);
            }
            command @ (Command::Query(_) | Command::Decode(..)) if closing => {
                command.refuse(Error::Closed)
            }
            #[cfg(test)]
            Command::Fault(_, reply) if closing => drop(reply.send(Err(Error::Closed))),
            #[cfg(test)]
            Command::Fault(crate::worker::Fault::AbortInCheckpoint, reply) => {
                let _ = reply.send(Ok(()));
                self.abort_in_checkpoint().await;
            }
            #[cfg(test)]
            Command::Fault(crate::worker::Fault::RecordGroups(log), reply) => {
                self.groups = Some(log);
                let _ = reply.send(Ok(()));
            }
            #[cfg(test)]
            Command::Fault(crate::worker::Fault::SnapshotAtFlush(directory), reply) => {
                self.snapshots = Some(directory);
                let _ = reply.send(Ok(()));
            }
            #[cfg(test)]
            Command::Fault(crate::worker::Fault::CheckpointFrames(frames), reply) => {
                self.checkpoint_frames = frames;
                let _ = reply.send(Ok(()));
            }
            #[cfg(test)]
            Command::Fault(crate::worker::Fault::VmSteps(steps), reply) => {
                let _ = steps.send(sqlite::vm_steps());
                let _ = reply.send(Ok(()));
            }
            #[cfg(test)]
            Command::Fault(fault, reply) => {
                if fault.file_level() {
                    self.fault = Some(fault);
                } else if let Some(worker) = self.slots[index].worker.as_mut() {
                    worker.fault = Some(fault);
                }
                let _ = reply.send(Ok(()));
            }
            command => match self.slots[index].worker.as_mut() {
                Some(worker) => worker.read(command).await,
                None => command.refuse(Error::Closed),
            },
        }
    }
    fn dirty(&self, index: usize) -> bool {
        self.durability == Durability::Buffered
            && self.slots[index].worker.as_ref().is_some_and(Worker::dirty)
    }
    async fn synchronous(&self, conn: &sqlite::Connection, full: bool) -> Result<(), Error> {
        if self.durability == Durability::Full {
            return Ok(());
        }
        let level = if full {
            2
        } else {
            self.durability.synchronous()
        };
        conn.execute(format!("PRAGMA synchronous={level}"), ())
            .await?;
        Ok(())
    }
    fn flushed(&mut self) {
        self.flush_wanted = false;
        self.waiting.clear();
        for worker in self
            .slots
            .iter_mut()
            .filter_map(|slot| slot.worker.as_mut())
        {
            worker.flushed();
        }
        #[cfg(test)]
        self.snapshot();
    }
    fn flush_failed(&mut self, error: &Error) {
        self.flush_wanted = false;
        for index in std::mem::take(&mut self.waiting) {
            if let Some(queued) = self.slots[index].deferred.pop_front() {
                queued.command.refuse(storage::group_error(error));
            }
        }
    }
    async fn flush(&mut self) -> bool {
        let Some(conn) = self.conn() else {
            return true;
        };
        let result = async {
            self.synchronous(&conn, true).await?;
            self.begin(&conn).await?;
            let written = change_header_page(&conn).await;
            storage::finish(&conn, written).await
        }
        .await;
        let restored = self.synchronous(&conn, false).await;
        match result.and(restored) {
            Ok(_) => {
                self.flushed();
                false
            }
            Err(error) => {
                self.flush_failed(&error);
                matches!(error, Error::Rollback { .. })
            }
        }
    }
    #[cfg(test)]
    fn snapshot(&self) {
        let (Some(directory), Some(file)) = (&self.snapshots, &self.file) else {
            return;
        };
        let path = file._owner.path();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let name = path.file_name().unwrap_or_default();
        let mut saved_wal = name.to_owned();
        saved_wal.push("-wal");
        let _ = rocky_platform::fs::create_dir_all(directory);
        let _ = rocky_platform::fs::copy(path, directory.join(name));
        if rocky_platform::fs::copy(&wal, directory.join(&saved_wal)).is_err() {
            let _ = rocky_platform::fs::remove_file(directory.join(saved_wal));
        }
    }
    fn conn(&self) -> Option<Arc<sqlite::Connection>> {
        self.file.as_ref().map(|file| file.conn.clone())
    }
    async fn begin(&self, conn: &sqlite::Connection) -> Result<(), Error> {
        self.begin_as(conn, "BEGIN IMMEDIATE").await
    }
    async fn begin_as(&self, conn: &sqlite::Connection, begin: &str) -> Result<(), Error> {
        for attempt in 0..storage::TRANSACTION_ATTEMPTS {
            match conn.execute(begin, ()).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    let error = Error::from(error);
                    if !retryable(&error) {
                        return Err(error);
                    }
                    if attempt + 1 < storage::TRANSACTION_ATTEMPTS {
                        rocky_platform::time::sleep(storage::backoff(attempt)).await;
                    }
                }
            }
        }
        Err(Error::RetryExhausted)
    }
    async fn commit(&mut self, group: Vec<Work<A::Row>>, full: bool) -> bool {
        let Some(conn) = self.conn() else {
            for (_, queued) in group {
                queued.command.refuse(Error::Closed);
            }
            return true;
        };
        if let Err(error) = self.synchronous(&conn, full).await {
            self.flush_failed(&error);
            fail(
                group
                    .into_iter()
                    .map(|(_, queued)| queued.command)
                    .collect(),
                error,
            );
            return false;
        }
        let instrument = self.instrument;
        let metrics = self.metrics.clone();
        #[cfg(test)]
        let composition: Vec<_> = group
            .iter()
            .map(|(index, queued)| crate::worker::LoggedStep {
                domain: *index,
                queue_weight: queued._budget.num_permits(),
            })
            .collect();
        if let Err(error) = self.begin(&conn).await {
            let _ = self.synchronous(&conn, false).await;
            if full {
                self.flush_failed(&error);
            }
            fail(
                group
                    .into_iter()
                    .map(|(_, queued)| queued.command)
                    .collect(),
                error,
            );
            return false;
        }
        let started = Instant::now();
        let mut answers = Vec::new();
        let mut touched = vec![0usize; self.slots.len()];
        let mut changed_rows = vec![false; self.slots.len()];
        let mut failure = None;
        let mut fatal = false;
        let mut skipped = Vec::new();
        for (index, queued) in group {
            let Some(worker) = self.slots[index].worker.as_mut() else {
                queued.command.refuse(Error::Closed);
                continue;
            };
            if failure.is_some() {
                skipped.push(queued.command);
                continue;
            }
            touched[index] += 1;
            let before = conn.total_changes().ok();
            let timed = instrument.then(|| (queued.command.step_kind(), Instant::now()));
            worker.in_group = true;
            let answer = worker.step(queued.command).await;
            worker.in_group = false;
            if let Some((kind, step_started)) = timed {
                metrics.add_step(kind, micros(step_started));
            }
            changed_rows[index] |= changed_since(&conn, before);
            if answer.fatal() {
                fatal = true;
                failure = Some(Error::Corrupt("a step could not roll back".into()));
            } else if conn.is_autocommit().unwrap_or(true) {
                failure = Some(storage::group_lost());
            }
            answers.push((answer, queued._budget));
        }
        for (index, writes) in touched.iter().enumerate() {
            let Some(worker) = self.slots[index].worker.as_mut() else {
                continue;
            };
            if failure.is_some() || *writes == 0 || !worker.maintenance_due() {
                continue;
            }
            let before = conn.total_changes().ok();
            worker.in_group = true;
            let result = worker.maintain(*writes).await;
            worker.in_group = false;
            changed_rows[index] |= changed_since(&conn, before);
            if matches!(result, Err(Error::Rollback { .. })) {
                fatal = true;
                failure = Some(Error::Corrupt("a step could not roll back".into()));
            } else if conn.is_autocommit().unwrap_or(true) {
                failure = Some(storage::group_lost());
            }
        }
        if self
            .slots
            .iter()
            .filter_map(|slot| slot.worker.as_ref())
            .any(Worker::wants_flush)
        {
            self.flush_wanted = true;
        }
        #[cfg(test)]
        if matches!(self.fault, Some(crate::worker::Fault::AbortBeforeCommit)) {
            std::process::abort();
        }
        #[cfg(test)]
        if failure.is_none() && matches!(self.fault, Some(crate::worker::Fault::FailCommit)) {
            self.fault = None;
            failure = Some(Error::InjectedFailure);
        }
        self.metrics.step_micros.fetch_add(micros(started), Relaxed);
        let committing = Instant::now();
        let committed = match failure {
            None => conn.execute("COMMIT", ()).await.map_err(Error::from),
            Some(error) => Err(error),
        };
        self.metrics
            .commit_micros
            .fetch_add(micros(committing), Relaxed);
        if committed.is_err()
            && !conn.is_autocommit().unwrap_or(false)
            && conn.execute("ROLLBACK", ()).await.is_err()
        {
            fatal = true;
        }
        let _ = self.synchronous(&conn, false).await;
        match committed {
            Ok(_) => {
                #[cfg(test)]
                if matches!(self.fault, Some(crate::worker::Fault::AbortAfterCommit)) {
                    std::process::abort();
                }
                #[cfg(test)]
                if let Some(log) = &self.groups
                    && let Ok(mut log) = log.lock()
                {
                    log.push(composition);
                }
                self.metrics.groups.fetch_add(1, Relaxed);
                self.metrics
                    .steps
                    .fetch_add(touched.iter().sum::<usize>() as u64, Relaxed);
                if full {
                    self.metrics.flushes.fetch_add(1, Relaxed);
                }
                for (index, slot) in self.slots.iter_mut().enumerate() {
                    if let (true, Some(worker)) = (touched[index] > 0, slot.worker.as_mut()) {
                        worker.publish();
                        if changed_rows[index] {
                            worker.buffered();
                        }
                    }
                }
                if full {
                    self.flushed();
                }
                for (answer, budget) in answers {
                    answer.send(full);
                    drop(budget);
                }
                self.checkpoint(&conn).await || fatal
            }
            Err(error) => {
                if full {
                    self.flush_failed(&error);
                }
                for (slot, touched) in self.slots.iter_mut().zip(&touched) {
                    if let (true, Some(worker)) = (*touched > 0, slot.worker.as_mut())
                        && worker.recover().await.is_err()
                    {
                        fatal = true;
                    }
                }
                let mut errors = copies(error, answers.len() + skipped.len());
                for (answer, budget) in answers {
                    answer.fail(errors.pop().unwrap_or(Error::Closed));
                    drop(budget);
                }
                for command in skipped {
                    command.refuse(errors.pop().unwrap_or(Error::Closed));
                }
                fatal
            }
        }
    }
    async fn checkpoint(&mut self, conn: &sqlite::Connection) -> bool {
        let frames = sqlite::wal_frames();
        self.metrics.wal_frames.store(frames.max(0) as u64, Relaxed);
        self.metrics
            .wal_frames_max
            .fetch_max(frames.max(0) as u64, Relaxed);
        if frames < 2 * self.checkpoint_frames {
            self.reset_backoff = 0;
            self.reset_skip = 0;
        }
        if frames < self.checkpoint_frames {
            return false;
        }
        let started = Instant::now();
        let mut stop = false;
        let mut kind = CheckpointKind::Passive;
        let mut copy_micros = 0u64;
        let mut flush_micros = 0u64;
        if frames < 2 * self.checkpoint_frames {
            let (passive_stop, copy, flush) = self.passive(conn).await;
            stop = passive_stop;
            copy_micros = copy;
            flush_micros = flush;
        } else {
            kind = CheckpointKind::Truncate;
            self.reset(conn).await;
            self.metrics.truncations.fetch_add(1, Relaxed);
            self.metrics
                .truncate_micros
                .fetch_add(micros(started), Relaxed);
        }
        let elapsed = micros(started);
        self.metrics.checkpoints.fetch_add(1, Relaxed);
        self.metrics.checkpoint_micros.fetch_add(elapsed, Relaxed);
        self.metrics
            .checkpoint_max_micros
            .fetch_max(elapsed, Relaxed);
        if self.instrument {
            self.metrics.record_checkpoint(CheckpointInterval {
                start: started,
                end: Instant::now(),
                kind,
                copy_micros,
                flush_micros,
            });
        }
        stop
    }
    async fn reset(&mut self, conn: &sqlite::Connection) {
        let mut reset = truncate(conn).await;
        if !reset && self.reset_skip > 0 {
            self.reset_skip -= 1;
        } else if !reset {
            self.metrics.truncate_waits.fetch_add(1, Relaxed);
            if conn.set_busy_timeout(RESET_WAIT).is_ok() {
                reset = truncate(conn).await;
            }
            let _ = conn.set_busy_timeout(std::time::Duration::ZERO);
            if !reset {
                self.reset_backoff = (self.reset_backoff * 2).clamp(1, MAX_RESET_BACKOFF);
                self.reset_skip = self.reset_backoff;
            }
        }
        if reset {
            self.reset_backoff = 0;
            self.reset_skip = 0;
        }
    }
    async fn passive(&self, conn: &sqlite::Connection) -> (bool, u64, u64) {
        let Some(file) = &self.file else {
            return (false, 0, 0);
        };
        let path = file._owner.path();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let flush = |path: &std::path::Path, spent: &AtomicU64| {
            let started = Instant::now();
            let result = rocky_platform::fs::sync(path).and_then(|found| {
                found
                    .then_some(())
                    .ok_or_else(|| std::io::ErrorKind::NotFound.into())
            });
            let elapsed = micros(started);
            spent.fetch_add(elapsed, Relaxed);
            (result, elapsed)
        };
        let (wal_result, _) = flush(
            std::path::Path::new(&wal),
            &self.metrics.checkpoint_wal_flush_micros,
        );
        if wal_result.is_err() || conn.execute("PRAGMA synchronous=0", ()).await.is_err() {
            return (false, 0, 0);
        }
        let copying = Instant::now();
        let copied = conn.execute("PRAGMA wal_checkpoint(PASSIVE)", ()).await;
        let copy_micros = micros(copying);
        self.metrics
            .checkpoint_copy_micros
            .fetch_add(copy_micros, Relaxed);
        let (flush_result, flush_micros) = flush(path, &self.metrics.checkpoint_flush_micros);
        let unflushed = copied.is_ok() && flush_result.is_err();
        let level = format!("PRAGMA synchronous={}", self.durability.synchronous());
        (
            conn.execute(level, ()).await.is_err() || unflushed,
            copy_micros,
            flush_micros,
        )
    }
    #[cfg(test)]
    async fn abort_in_checkpoint(&self) {
        if let Some(conn) = self.conn() {
            let _ = conn.abort_when_busy();
            let _ = conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", ()).await;
        }
    }
    async fn retire(&mut self) {
        let ready: Vec<_> = (0..self.slots.len())
            .filter(|index| {
                let slot = &self.slots[*index];
                slot.worker.is_some() && slot.drained && slot.deferred.is_empty()
            })
            .collect();
        if ready.is_empty() {
            return;
        }
        let mut retired: Vec<(Notifications, Option<Reply<()>>)> = Vec::new();
        for index in ready {
            let slot = &mut self.slots[index];
            if let Some(worker) = slot.worker.take() {
                retired.push((worker.into_notifications(), slot.closing.take()));
            }
        }
        if self.slots.iter().all(|slot| slot.worker.is_none()) {
            self.close_file().await;
        }
        for (notifications, closing) in retired {
            drop(notifications);
            if let Some(reply) = closing {
                let _ = reply.send(Ok(()));
            }
        }
    }
    async fn close_file(&mut self) {
        if let Some(file) = self.file.take() {
            file.pool.close();
            let _ = file.conn.execute("PRAGMA optimize", ()).await;
            let _ = file
                .conn
                .execute("PRAGMA wal_checkpoint(TRUNCATE)", ())
                .await;
            drop(file);
        }
    }
}
fn changed_since(conn: &sqlite::Connection, before: Option<u64>) -> bool {
    before.is_none() || before != conn.total_changes().ok()
}
async fn change_header_page(conn: &sqlite::Connection) -> Result<(), Error> {
    let current = rows_raw(conn, "PRAGMA user_version").await?;
    let next = match current.first().map(Vec::as_slice) {
        Some([SqlValue::Integer(value)]) => (value + 1).rem_euclid(i64::from(i32::MAX)),
        _ => 1,
    };
    conn.execute(format!("PRAGMA user_version={next}"), ())
        .await?;
    Ok(())
}
async fn truncate(conn: &sqlite::Connection) -> bool {
    matches!(
        rows_raw(conn, "PRAGMA wal_checkpoint(TRUNCATE)").await.as_deref(),
        Ok([row]) if row.first() == Some(&SqlValue::Integer(0))
    )
}
async fn rows_raw(conn: &sqlite::Connection, sql: &str) -> Result<Vec<Vec<SqlValue>>, Error> {
    storage::collect_rows(conn.query(sql, ()).await?).await
}
fn fail<R>(commands: Vec<Command<R>>, error: Error) {
    let mut errors = copies(error, commands.len());
    for command in commands {
        command.refuse(errors.pop().unwrap_or(Error::Closed));
    }
}
fn copies(error: Error, count: usize) -> Vec<Error> {
    let mut errors: Vec<_> = (1..count.max(1))
        .map(|_| storage::group_error(&error))
        .collect();
    errors.push(error);
    errors
}
