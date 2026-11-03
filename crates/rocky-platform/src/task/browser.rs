//! Tasks on the Worker's event loop. Handles are `Send` like tokio's, so
//! shared code that stores them in `Send` state compiles unchanged; the
//! tasks themselves need not be `Send`.
use futures_util::{
    FutureExt,
    future::{AbortHandle, Abortable},
    stream::{FuturesUnordered, StreamExt},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::oneshot;

/// The task was aborted before it finished. A panic aborts the Worker's
/// wasm instance, so it is never reported here.
#[derive(Debug)]
pub struct JoinError(());
impl JoinError {
    pub fn is_cancelled(&self) -> bool {
        true
    }
    pub fn is_panic(&self) -> bool {
        false
    }
}
impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("task was cancelled")
    }
}
impl std::error::Error for JoinError {}

/// Awaits a spawned task's output; dropping it detaches the task.
#[derive(Debug)]
pub struct JoinHandle<T> {
    output: oneshot::Receiver<T>,
    abort: AbortHandle,
    /// Set when the task ends, finished or aborted.
    ended: Arc<AtomicBool>,
}
impl<T> JoinHandle<T> {
    pub fn abort(&self) {
        self.abort.abort();
    }
    pub fn is_finished(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}
impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.output)
            .poll(cx)
            .map_err(|_| JoinError(()))
    }
}

pub fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let (sender, output) = oneshot::channel();
    let (abort, registration) = AbortHandle::new_pair();
    let ended = Arc::new(AtomicBool::new(false));
    let ending = ended.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result = Abortable::new(future, registration).await;
        ending.store(true, Ordering::SeqCst);
        if let Ok(value) = result {
            let _ = sender.send(value);
        }
    });
    JoinHandle {
        output,
        abort,
        ended,
    }
}

/// A Worker has one thread, so `spawn` is `spawn_local`.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    spawn_local(future)
}

/// Runs `work` now, on the Worker's only thread: it is bounded CPU or
/// SQLite work that natively moves off the async threads.
pub fn spawn_blocking<T: 'static>(work: impl FnOnce() -> T) -> JoinHandle<T> {
    let (sender, output) = oneshot::channel();
    let _ = sender.send(work());
    let (abort, _) = AbortHandle::new_pair();
    JoinHandle {
        output,
        abort,
        ended: Arc::new(AtomicBool::new(true)),
    }
}

/// Lets timers, network events and other tasks run: a macrotask, unlike a
/// waker, also yields to the browser's event loop.
pub async fn yield_now() {
    crate::time::platform::turn().await;
}

/// Runs `task` on the Worker's event loop, where natively it would get its
/// own thread.
pub fn dedicated<F, T>(
    _name: impl Into<String>,
    task: impl FnOnce() -> F + 'static,
) -> std::io::Result<Dedicated<T>>
where
    F: Future<Output = T> + 'static,
    T: 'static,
{
    Ok(Dedicated(spawn_local(async move { task().await })))
}

pub struct Dedicated<T>(JoinHandle<T>);
impl<T> Dedicated<T> {
    pub async fn join(self) -> Result<T, Ended> {
        self.0.await.map_err(|_| Ended)
    }
    /// A synchronous owner cannot wait in a Worker: the task ends on its
    /// own turn of the event loop.
    pub fn release(self) {}
}

/// A dedicated task was aborted.
#[derive(Debug)]
pub struct Ended;

/// Tasks whose outputs arrive as they finish; dropping the set aborts them.
pub struct JoinSet<T> {
    tasks: FuturesUnordered<JoinHandle<T>>,
}
impl<T: 'static> Default for JoinSet<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T: 'static> JoinSet<T> {
    pub fn new() -> Self {
        Self {
            tasks: FuturesUnordered::new(),
        }
    }
    pub fn spawn_local(&mut self, task: impl Future<Output = T> + 'static) -> AbortHandle {
        let handle = spawn_local(task);
        let abort = handle.abort.clone();
        self.tasks.push(handle);
        abort
    }
    /// A Worker has one thread, so `spawn` is `spawn_local`.
    pub fn spawn(&mut self, task: impl Future<Output = T> + 'static) -> AbortHandle {
        self.spawn_local(task)
    }
    pub fn len(&self) -> usize {
        self.tasks.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        self.tasks.next().await
    }
    pub fn try_join_next(&mut self) -> Option<Result<T, JoinError>> {
        self.tasks.next().now_or_never().flatten()
    }
    pub fn abort_all(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
    pub async fn shutdown(&mut self) {
        self.abort_all();
        while self.join_next().await.is_some() {}
    }
}
impl<T> Drop for JoinSet<T> {
    fn drop(&mut self) {
        self.tasks.iter().for_each(JoinHandle::abort);
    }
}

/// Whether a worker still holds something its owner would wait for. A
/// Worker's thread cannot block, so `wait_released` returns at once and the
/// worker task releases on its next turn of the event loop.
#[derive(Default)]
pub struct Held(std::sync::atomic::AtomicBool);
impl Held {
    pub fn new(held: bool) -> Self {
        Self(std::sync::atomic::AtomicBool::new(held))
    }
    pub fn set(&self, held: bool) {
        self.0.store(held, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn wait_released(&self, _poll: std::time::Duration) {}
}

/// Synchronous work cannot wait in a Worker: the caller retries at once.
/// Only connections in this Worker can hold its files, and none of them
/// can make progress while this work runs.
pub fn backoff_blocking(_duration: std::time::Duration) {}
