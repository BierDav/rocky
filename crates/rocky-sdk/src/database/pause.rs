use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Point {
    Bootstrap,
    Teardown,
    Succeeding,
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
pub(crate) async fn at(directory: &Path, point: Point) {
    let armed = {
        let mut armed = ARMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        armed
            .iter()
            .position(|(at, armed, _, _)| at == directory && *armed == point)
            .map(|index| armed.remove(index))
    };
    if let Some((_, _, reached, released)) = armed {
        let _ = reached.send(());
        let _ = released.await;
    }
}

#[cfg(not(test))]
pub(crate) async fn at(_: &Path, _: Point) {}
