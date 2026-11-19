use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    Bootstrap,
    Teardown,
    Succeeding,
    BootstrapIntentLoaded,
    BootstrapIntentContended,
}

#[cfg(test)]
type Armed = (
    std::path::PathBuf,
    Point,
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

#[cfg(test)]
static ARMED: std::sync::Mutex<Vec<Armed>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn arm(
    directory: &Path,
    point: Point,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (reached, reaching) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    ARMED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((directory.to_path_buf(), point, reached, released));
    (reaching, release)
}

#[cfg(test)]
fn reach(directory: &Path, point: Point) -> Option<tokio::sync::oneshot::Receiver<()>> {
    let (_, _, reached, released) = {
        let mut armed = ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = armed
            .iter()
            .position(|(at, armed, _, _)| at == directory && *armed == point)?;
        armed.remove(index)
    };
    let _ = reached.send(());
    Some(released)
}

#[cfg(test)]
pub(crate) async fn at(directory: &Path, point: Point) {
    if let Some(released) = reach(directory, point) {
        let _ = released.await;
    }
}

#[cfg(test)]
pub(crate) fn blocking_at(directory: &Path, point: Point) {
    if let Some(released) = reach(directory, point) {
        let _ = released.blocking_recv();
    }
}

#[cfg(test)]
pub(crate) fn passing(directory: &Path, point: Point) {
    let _ = reach(directory, point);
}

#[cfg(not(test))]
pub(crate) async fn at(_: &Path, _: Point) {}

#[cfg(not(test))]
pub(crate) fn blocking_at(_: &Path, _: Point) {}

#[cfg(not(test))]
pub(crate) fn passing(_: &Path, _: Point) {}
