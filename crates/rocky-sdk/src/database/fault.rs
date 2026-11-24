use crate::Failure;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Started,
    RightsChangeRecorded,
    RightsChangeCommitted,
    RevocationBegun,
}
#[cfg(test)]
pub(crate) static STOPS: std::sync::Mutex<Vec<(std::path::PathBuf, Stage)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
pub(crate) fn check(directory: &Path, stage: Stage) -> Result<(), Failure> {
    let mut stops = STOPS.lock().map_err(|_| "poisoned fault")?;
    match stops
        .iter()
        .position(|(at, stop)| at == directory && *stop == stage)
    {
        Some(index) => {
            stops.remove(index);
            Err(format!("injected stop at {stage:?}").into())
        }
        None => Ok(()),
    }
}
#[cfg(not(test))]
pub(crate) fn check(_: &Path, _: Stage) -> Result<(), Failure> {
    Ok(())
}
