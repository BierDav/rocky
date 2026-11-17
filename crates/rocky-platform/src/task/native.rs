use std::future::Future;

pub use tokio::{
    spawn,
    task::{JoinError, JoinHandle, JoinSet, spawn_local, yield_now},
};

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

pub const THREAD_STACK: usize = 16 << 20;

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
        .stack_size(THREAD_STACK)
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            runtime.block_on(local.run_until(task()))
        })?;
    Ok(Dedicated(Running::Thread(thread)))
}

pub struct Dedicated<T>(Running<T>);
enum Running<T> {
    Thread(std::thread::JoinHandle<T>),
    #[cfg(feature = "simulation")]
    Local(JoinHandle<T>),
}
impl<T: Send + 'static> Dedicated<T> {
    pub async fn join(self) -> Result<T, Ended> {
        match self.0 {
            Running::Thread(thread) => tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|_| Ended)?
                .map_err(|_| Ended),
            #[cfg(feature = "simulation")]
            Running::Local(task) => task.await.map_err(|_| Ended),
        }
    }
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

#[derive(Debug)]
pub struct Ended;

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
    pub fn wait_released(&self, poll: std::time::Duration) {
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

pub fn backoff_blocking(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

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
