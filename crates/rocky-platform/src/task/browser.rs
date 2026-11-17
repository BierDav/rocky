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

#[derive(Debug)]
pub struct JoinHandle<T> {
    output: oneshot::Receiver<T>,
    abort: AbortHandle,
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

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    spawn_local(future)
}

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

pub async fn yield_now() {
    crate::time::platform::turn().await;
}

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
    pub fn release(self) {}
}

#[derive(Debug)]
pub struct Ended;

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

pub fn backoff_blocking(_duration: std::time::Duration) {}

#[derive(Clone, Debug)]
pub struct Home(());
impl Home {
    pub fn current() -> Option<Self> {
        None
    }
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        spawn_local(future)
    }
}
