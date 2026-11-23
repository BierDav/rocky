use rocky_credentials::testing::MemoryKeys;
use rocky_sdk::SecretStore;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

static STORES: LazyLock<Mutex<HashMap<PathBuf, Arc<MemoryKeys>>>> = LazyLock::new(Default::default);

pub fn keys(directory: &Path) -> Arc<dyn SecretStore> {
    STORES
        .lock()
        .unwrap()
        .entry(directory.to_path_buf())
        .or_default()
        .clone()
}
