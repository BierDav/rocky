use std::future::Future;

pub use tokio::{
    spawn,
    task::{JoinError, JoinHandle, JoinSet, spawn_blocking, spawn_local, yield_now},
};

/// Runs `task` to completion on its own thread and current-thread runtime,
/// so its blocking SQLite work never stalls the caller's executor.
pub fn dedicated<F, T>(
    name: impl Into<String>,
    task: impl FnOnce() -> F + Send + 'static,
) -> std::io::Result<Dedicated<T>>
where
    F: Future<Output = T> + 'static,
    T: Send + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            runtime.block_on(local.run_until(task()))
        })?;
    Ok(Dedicated(thread))
}

/// A `dedicated` task. Joining waits for its thread to end.
pub struct Dedicated<T>(std::thread::JoinHandle<T>);
impl<T: Send + 'static> Dedicated<T> {
    pub async fn join(self) -> Result<T, Ended> {
        spawn_blocking(move || self.0.join())
            .await
            .map_err(|_| Ended)?
            .map_err(|_| Ended)
    }
    /// Lets a synchronous owner wait for the task's end: it joins the thread.
    pub fn release(self) {
        let _ = self.0.join();
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
