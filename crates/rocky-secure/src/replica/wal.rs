use super::*;

pub(crate) struct OwnedFile {
    pub(crate) passive: Arc<Passive>,
    path: PathBuf,
    quiet_retry_from: u64,
}
impl OwnedFile {
    pub(super) fn open(database: DatabaseOwner) -> Result<Self, Error> {
        Ok(Self {
            path: database.path().to_path_buf(),
            passive: Passive::open(database)?,
            quiet_retry_from: 0,
        })
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn quiet_truncation_failed_below(&self, size: u64) -> bool {
        size < self.quiet_retry_from
    }
    pub(super) fn quiet_truncation_failed_at(&mut self, size: u64, growth: u64) {
        self.quiet_retry_from = size.saturating_add(growth);
    }
    pub(super) fn truncated(&mut self) {
        self.quiet_retry_from = 0;
        self.passive.reset();
    }
}
impl Drop for OwnedFile {
    fn drop(&mut self) {
        self.passive.stop();
    }
}

struct CheckpointFile {
    conn: crate::sqlite::Connection,
    _database_released_after_conn: DatabaseOwner,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hold {
    Queued,
    Checkpointing,
}

#[derive(Default)]
struct State {
    file: Option<CheckpointFile>,
    running: bool,
    stopped: bool,
    mark: u64,
    epoch: u64,
    #[cfg(test)]
    hold: Option<(
        Hold,
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
}
pub(crate) struct Passive {
    state: std::sync::Mutex<State>,
    idle: tokio::sync::Notify,
    #[cfg(test)]
    completed: std::sync::atomic::AtomicUsize,
}
impl Passive {
    fn open(database: DatabaseOwner) -> Result<Arc<Self>, Error> {
        let conn = database.connect()?;
        for (set, read, expected) in [
            ("PRAGMA synchronous=NORMAL", "PRAGMA synchronous", 1),
            ("PRAGMA fullfsync=1", "PRAGMA fullfsync", 1),
            (
                "PRAGMA checkpoint_fullfsync=1",
                "PRAGMA checkpoint_fullfsync",
                1,
            ),
            (
                "PRAGMA wal_autocheckpoint=0",
                "PRAGMA wal_autocheckpoint",
                0,
            ),
        ] {
            conn.rows(set, ())?;
            if conn.rows(read, ())? != vec![vec![crate::sqlite::Value::Integer(expected)]] {
                return Err(Error::Durability(format!("{read} is not {expected}")));
            }
        }
        Ok(Arc::new(Self {
            state: std::sync::Mutex::new(State {
                file: Some(CheckpointFile {
                    conn,
                    _database_released_after_conn: database,
                }),
                ..Default::default()
            }),
            idle: Default::default(),
            #[cfg(test)]
            completed: Default::default(),
        }))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) fn reset(&self) {
        let mut state = self.state();
        state.mark = 0;
        state.epoch = state.epoch.wrapping_add(1);
    }
    #[cfg(all(test, not(target_family = "wasm")))]
    pub(crate) fn schedule_past_every_mark(self: &Arc<Self>, flusher: Arc<Flusher>) {
        self.schedule(u64::MAX, flusher);
    }
    pub(super) fn schedule(self: &Arc<Self>, size: u64, flusher: Arc<Flusher>) {
        let epoch = {
            let mut state = self.state();
            if state.running || state.stopped || size <= state.mark {
                return;
            }
            state.running = true;
            state.epoch
        };
        let run = Run {
            passive: self.clone(),
            file: None,
            size,
            epoch,
            checkpointed: false,
        };
        rocky_platform::task::spawn_blocking(move || run.checkpoint(&flusher));
    }
    fn finish(&self, file: Option<CheckpointFile>, checkpointed: bool, size: u64, epoch: u64) {
        let released = {
            let mut state = self.state();
            if checkpointed {
                let wal_not_truncated_since_measured = state.epoch == epoch;
                if wal_not_truncated_since_measured {
                    state.mark = size;
                }
                #[cfg(test)]
                self.completed.fetch_add(1, Ordering::Relaxed);
            }
            state.running = false;
            match file {
                Some(file) if !state.stopped => {
                    state.file = Some(file);
                    None
                }
                released => released,
            }
        };
        drop(released);
        self.idle.notify_waiters();
    }
    fn stop(&self) {
        let released = {
            let mut state = self.state();
            state.stopped = true;
            state.file.take()
        };
        drop(released);
    }
    pub(crate) async fn wait(&self) {
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if !self.state().running {
                return;
            }
            idle.await;
        }
    }
    #[cfg(test)]
    fn hold(&self, at: Hold) {
        let resume = {
            let mut state = self.state();
            match state.hold.take() {
                Some((stage, started, resume)) if stage == at => {
                    let _ = started.send(());
                    Some(resume)
                }
                other => {
                    state.hold = other;
                    None
                }
            }
        };
        if let Some(resume) = resume {
            let _ = resume.recv();
        }
    }
    #[cfg(test)]
    #[cfg_attr(
        target_family = "wasm",
        expect(
            dead_code,
            reason = "Checkpoint pause hooks are exercised only by native thread tests."
        )
    )]
    pub(crate) fn pause(
        &self,
        at: Hold,
        started: tokio::sync::oneshot::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) {
        self.state().hold = Some((at, started, resume));
    }
    #[cfg(test)]
    #[cfg_attr(
        target_family = "wasm",
        expect(
            dead_code,
            reason = "Checkpoint pause hooks are exercised only by native thread tests."
        )
    )]
    pub(crate) fn stopped(&self) -> bool {
        self.state().stopped
    }
    #[cfg(test)]
    #[cfg_attr(
        target_family = "wasm",
        expect(
            dead_code,
            reason = "Checkpoint pause hooks are exercised only by native thread tests."
        )
    )]
    pub(crate) fn completed(&self) -> usize {
        self.completed.load(Ordering::Relaxed)
    }
}

struct Run {
    passive: Arc<Passive>,
    file: Option<CheckpointFile>,
    size: u64,
    epoch: u64,
    checkpointed: bool,
}
impl Run {
    fn checkpoint(mut self, flusher: &Flusher) {
        #[cfg(test)]
        self.passive.hold(Hold::Queued);
        self.file = self.passive.state().file.take();
        let Some(file) = &self.file else {
            return;
        };
        #[cfg(test)]
        self.passive.hold(Hold::Checkpointing);
        #[cfg(feature = "qualification")]
        let started = web_time::Instant::now();
        let result = file.conn.rows("PRAGMA wal_checkpoint(PASSIVE)", ());
        #[cfg(feature = "qualification")]
        if let Ok(result) = &result {
            let pages = match result.first().map(Vec::as_slice) {
                Some([_, _, crate::sqlite::Value::Integer(done)]) => (*done).max(0) as u64,
                _ => 0,
            };
            flusher
                .measurements
                .checkpoint("passive", pages, started.elapsed());
        }
        #[cfg(not(feature = "qualification"))]
        let _ = flusher;
        self.checkpointed = result.is_ok();
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        self.passive
            .finish(self.file.take(), self.checkpointed, self.size, self.epoch);
    }
}
