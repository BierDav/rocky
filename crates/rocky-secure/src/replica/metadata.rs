//! Stored frame metadata, invalidated by table writes and rollback.
use super::*;

#[derive(Default)]
pub(super) struct Cached {
    pub(super) role: Option<roles::RoleConfig>,
    pub(super) holdings: BTreeMap<ResourceScope, Option<roles::Holding>>,
    pub(super) checkpoints: BTreeMap<i64, Option<checkpoint::Record>>,
}
pub(super) struct Metadata {
    writes: Arc<crate::sqlite::Writes>,
    #[cfg(test)]
    disabled: std::sync::atomic::AtomicBool,
    tables: [String; 3],
    cached: std::sync::Mutex<([u64; 3], Cached)>,
}
impl Metadata {
    pub(super) fn new(conn: &ScopedConnection) -> Result<Self, Error> {
        let names = conn.names();
        let tables = ["secure_roles", "secure_subscriptions", "secure_checkpoints"]
            .map(|table| names.physical(table).unwrap_or(table).to_owned());
        Ok(Self {
            writes: conn
                .connection()
                .watch_writes(tables.iter().cloned().map(|table| (table, true)))?,
            tables,
            #[cfg(test)]
            disabled: Default::default(),
            cached: Default::default(),
        })
    }
    pub(super) fn with<T>(&self, f: impl FnOnce(&mut Cached) -> T) -> T {
        #[cfg(test)]
        if self.disabled.load(Ordering::Relaxed) {
            return f(&mut Cached::default());
        }
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let versions = self
            .tables
            .each_ref()
            .map(|table| self.writes.table_count(table));
        if cached.0[0] != versions[0] {
            cached.1.role = None;
        }
        if cached.0[1] != versions[1] {
            cached.1.holdings.clear();
        }
        if cached.0[2] != versions[2] {
            cached.1.checkpoints.clear();
        }
        cached.0 = versions;
        f(&mut cached.1)
    }
    pub(super) fn clear(&self) {
        self.with(|cached| *cached = Cached::default());
    }
}

#[cfg(test)]
impl<A> Inner<A> {
    pub(crate) fn disable_metadata_cache_for_test(&self) {
        self.metadata.disabled.store(true, Ordering::Relaxed);
    }
}
