//! Passive checkpoints own a connection, never the replica or reactor.
use super::*;

#[derive(Default)]
struct State {
    running: bool,
    mark: u64,
    epoch: u64,
}
pub(crate) struct Passive {
    conn: crate::sqlite::Connection,
    _owner: Arc<DatabaseOwner>,
    state: std::sync::Mutex<State>,
    idle: tokio::sync::Notify,
    #[cfg(test)]
    pause: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            std::sync::mpsc::Receiver<()>,
        )>,
    >,
    #[cfg(test)]
    completed: std::sync::atomic::AtomicUsize,
}
impl Passive {
    pub(super) fn open(owner: Arc<DatabaseOwner>) -> Result<Arc<Self>, Error> {
        let conn = owner.connect()?;
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
            conn,
            _owner: owner,
            state: Default::default(),
            idle: Default::default(),
            #[cfg(test)]
            pause: Default::default(),
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
    pub(super) fn schedule(self: &Arc<Self>, size: u64, flusher: Arc<Flusher>) {
        let epoch = {
            let mut state = self.state();
            if state.running || size <= state.mark {
                return;
            }
            state.running = true;
            state.epoch
        };
        let checkpoint = self.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some((started, resume)) = checkpoint
                .pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = started.send(());
                let _ = resume.recv();
            }
            #[cfg(feature = "qualification")]
            let started = std::time::Instant::now();
            let result = checkpoint.conn.rows("PRAGMA wal_checkpoint(PASSIVE)", ());
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
            {
                let mut state = checkpoint.state();
                if result.is_ok() {
                    // A truncation must not inherit the previous WAL's size mark.
                    if state.epoch == epoch {
                        state.mark = size;
                    }
                    #[cfg(test)]
                    checkpoint.completed.fetch_add(1, Ordering::Relaxed);
                }
                state.running = false;
            }
            checkpoint.idle.notify_waiters();
        });
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
    pub(crate) fn pause(
        &self,
        started: tokio::sync::oneshot::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) {
        *self
            .pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((started, resume));
    }
    #[cfg(test)]
    pub(crate) fn completed(&self) -> usize {
        self.completed.load(Ordering::Relaxed)
    }
}
