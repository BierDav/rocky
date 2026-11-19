use crate::Failure;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::watch;

struct Resident {
    claim: u64,
    handles: Weak<()>,
    departure: watch::Receiver<()>,
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
    _departing: watch::Sender<()>,
}

pub(super) struct Predecessor(Option<watch::Receiver<()>>);

pub(super) struct Handles {
    _alive: Arc<()>,
}

pub(super) fn claim(directory: &Path) -> Result<Claimed, Failure> {
    let mut residents = RESIDENTS.lock().unwrap_or_else(PoisonError::into_inner);
    let predecessor = match residents.get(directory) {
        Some(resident) if resident.handles.strong_count() > 0 => {
            return Err("data directory is already in use".into());
        }
        Some(resident) => Some(resident.departure.clone()),
        None => None,
    };
    let claim = CLAIMS.fetch_add(1, Ordering::Relaxed);
    let handles = Arc::new(());
    let (departing, departure) = watch::channel(());
    residents.insert(
        directory.to_path_buf(),
        Resident {
            claim,
            handles: Arc::downgrade(&handles),
            departure,
        },
    );
    Ok(Claimed {
        claim: Claim {
            directory: directory.to_path_buf(),
            claim,
            _departing: departing,
        },
        predecessor: Predecessor(predecessor),
        handles: Handles { _alive: handles },
    })
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut residents = RESIDENTS.lock().unwrap_or_else(PoisonError::into_inner);
        if residents
            .get(&self.directory)
            .is_some_and(|resident| resident.claim == self.claim)
        {
            residents.remove(&self.directory);
        }
    }
}

impl Predecessor {
    pub(super) async fn departed(self, directory: &Path) {
        let Some(mut departure) = self.0 else {
            return;
        };
        super::pause::at(directory, super::pause::Point::Succeeding).await;
        let _ = departure.changed().await;
    }
}
