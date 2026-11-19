use crate::Failure;
use rocky_platform::time::Duration;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::watch;

const PREDECESSOR_TEARDOWN_BOUND: Duration = Duration::from_secs(30);
const IN_USE: &str = "data directory is already in use";

#[derive(Clone)]
struct Resident {
    claim: u64,
    handles: Weak<()>,
    departure: watch::Receiver<()>,
}

impl Resident {
    fn departed(&self) -> bool {
        self.departure.has_changed().is_err()
    }
}

static RESIDENTS: Mutex<BTreeMap<PathBuf, Resident>> = Mutex::new(BTreeMap::new());
static CLAIMS: AtomicU64 = AtomicU64::new(0);

pub(super) struct Claimed {
    pub(super) claim: Claim,
    pub(super) predecessor: Predecessor,
    pub(super) handles: Handles,
}

pub(super) struct Claim {
    directory: PathBuf,
    claim: u64,
    succeeding: Option<Resident>,
    _departing: watch::Sender<()>,
}

pub(super) struct Predecessor(Option<watch::Receiver<()>>);

pub(super) struct Handles {
    _alive: Arc<()>,
}

fn key(directory: &Path) -> PathBuf {
    std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf())
}

pub(super) fn claim(directory: &Path) -> Result<Claimed, Failure> {
    let directory = key(directory);
    let mut residents = RESIDENTS.lock().unwrap_or_else(PoisonError::into_inner);
    let succeeding = match residents.get(&directory) {
        Some(resident) if resident.handles.strong_count() > 0 => return Err(IN_USE.into()),
        Some(resident) if !resident.departed() => Some(resident.clone()),
        Some(_) | None => None,
    };
    let claim = CLAIMS.fetch_add(1, Ordering::Relaxed);
    let handles = Arc::new(());
    let (departing, departure) = watch::channel(());
    residents.insert(
        directory.clone(),
        Resident {
            claim,
            handles: Arc::downgrade(&handles),
            departure,
        },
    );
    Ok(Claimed {
        predecessor: Predecessor(
            succeeding
                .as_ref()
                .map(|resident| resident.departure.clone()),
        ),
        claim: Claim {
            directory,
            claim,
            succeeding,
            _departing: departing,
        },
        handles: Handles { _alive: handles },
    })
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut residents = RESIDENTS.lock().unwrap_or_else(PoisonError::into_inner);
        if !residents
            .get(&self.directory)
            .is_some_and(|resident| resident.claim == self.claim)
        {
            return;
        }
        match self.succeeding.take() {
            Some(predecessor) if !predecessor.departed() => {
                residents.insert(self.directory.clone(), predecessor);
            }
            Some(_) | None => {
                residents.remove(&self.directory);
            }
        }
    }
}

impl Predecessor {
    pub(super) async fn departed(self, directory: &Path) -> Result<(), Failure> {
        let Some(mut departure) = self.0 else {
            return Ok(());
        };
        crate::pause::at(directory, crate::pause::Point::Succeeding).await;
        match rocky_platform::time::timeout(PREDECESSOR_TEARDOWN_BOUND, departure.changed()).await {
            Ok(_) => Ok(()),
            Err(_) => Err(IN_USE.into()),
        }
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_successor_gives_up_with_in_use_once_its_predecessor_outlives_the_bound()
    -> Result<(), Failure> {
        let directory = Path::new("residence-stuck-predecessor");
        let stuck = claim(directory)?;
        drop(stuck.handles);
        let successor = claim(directory)?;
        let waited = successor.predecessor.departed(directory).await;
        assert_eq!(
            waited.map_err(|error| error.to_string()),
            Err(IN_USE.to_owned())
        );
        drop(stuck.claim);
        Ok(())
    }

    #[test]
    fn a_successor_dropped_before_waiting_hands_the_directory_back_to_its_predecessor()
    -> Result<(), Failure> {
        let directory = Path::new("residence-abandoned-successor");
        let tearing_down = claim(directory)?;
        drop(tearing_down.handles);
        let abandoned = claim(directory)?;
        drop(abandoned);
        let next = claim(&std::path::absolute(directory)?)?;
        assert!(
            next.predecessor.0.is_some(),
            "the next open, under any spelling of the path, still waits for the predecessor"
        );
        drop(tearing_down.claim);
        drop(next);
        Ok(())
    }
}
