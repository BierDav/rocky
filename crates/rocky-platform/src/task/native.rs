use std::future::Future;

pub use tokio::{
    spawn,
    task::{JoinError, JoinHandle, JoinSet, spawn_local, yield_now},
};

/// Runs synchronous `work` on tokio's blocking pool. On a thread running a
/// seeded simulation (`entropy::simulation`) it runs in place, as in a
/// browser, so no thread's timing reaches the simulation's order.
pub fn spawn_blocking<F, R>(work: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    #[cfg(feature = "simulation")]
    if crate::entropy::simulation::installed() {
        let value = work();
        return tokio::spawn(async move { value });
    }
    tokio::task::spawn_blocking(work)
}

/// Runs `task` to completion on its own thread and current-thread runtime,
/// so its blocking SQLite work never stalls the caller's executor. A seeded
/// simulation's thread runs it as a local task, as a browser Worker does.
pub fn dedicated<F, T>(
    name: impl Into<String>,
    task: impl FnOnce() -> F + Send + 'static,
) -> std::io::Result<Dedicated<T>>
where
    F: Future<Output = T> + 'static,
    T: Send + 'static,
{
    #[cfg(feature = "simulation")]
    if crate::entropy::simulation::installed() {
        return Ok(Dedicated(Running::Local(spawn_local(async move {
            task().await
        }))));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            runtime.block_on(local.run_until(task()))
        })?;
    Ok(Dedicated(Running::Thread(thread)))
}

/// A `dedicated` task. Joining waits for its thread to end.
pub struct Dedicated<T>(Running<T>);
enum Running<T> {
    Thread(std::thread::JoinHandle<T>),
    #[cfg(feature = "simulation")]
    Local(JoinHandle<T>),
}
impl<T: Send + 'static> Dedicated<T> {
    pub async fn join(self) -> Result<T, Ended> {
        match self.0 {
            // Never in place: the thread may still need this executor.
            Running::Thread(thread) => tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|_| Ended)?
                .map_err(|_| Ended),
            #[cfg(feature = "simulation")]
            Running::Local(task) => task.await.map_err(|_| Ended),
        }
    }
    /// Lets a synchronous owner wait for the task's end: it joins the thread.
    /// A local task ends on its own turn instead.
    pub fn release(self) {
        match self.0 {
            Running::Thread(thread) => {
                let _ = thread.join();
            }
            #[cfg(feature = "simulation")]
            Running::Local(_) => {}
        }
    }
}

/// A dedicated task panicked or could not be joined.
#[derive(Debug)]
pub struct Ended;

/// Whether a worker still holds something, such as a connection, that its
/// owner waits for before going on.
#[derive(Default)]
pub struct Held {
    held: std::sync::Mutex<bool>,
    released: std::sync::Condvar,
}
impl Held {
    pub fn new(held: bool) -> Self {
        Self {
            held: std::sync::Mutex::new(held),
            released: std::sync::Condvar::new(),
        }
    }
    pub fn set(&self, held: bool) {
        if let Ok(mut current) = self.held.lock() {
            *current = held;
        }
        self.released.notify_all();
    }
    /// Waits until the worker releases, rechecking every `poll` in case it
    /// missed a signal such as a disconnected channel.
    pub fn wait_released(&self, poll: std::time::Duration) {
        // A local worker cannot run while this thread waits for it.
        #[cfg(feature = "simulation")]
        if crate::entropy::simulation::installed() {
            return;
        }
        let Ok(mut held) = self.held.lock() else {
            return;
        };
        while *held {
            match self.released.wait_timeout(held, poll) {
                Ok((next, _)) => held = next,
                Err(_) => return,
            }
        }
    }
}

/// Waits `duration` inside synchronous work that `spawn_blocking` runs,
/// such as a retry after another process held a file.
pub fn backoff_blocking(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

/// The runtime something was made on, which outlives the caller's: work
/// spawned there keeps running after a short-lived caller's runtime ends.
#[derive(Clone, Debug)]
pub struct Home(tokio::runtime::Handle);
impl Home {
    pub fn current() -> Option<Self> {
        tokio::runtime::Handle::try_current().ok().map(Self)
    }
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.0.spawn(future)
    }
}
