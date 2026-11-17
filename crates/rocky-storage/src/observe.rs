use crate::Error;
use rocky_core::{Commitment, Scope};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cause {
    Local,
    Sync,
    Rebuild,
    Authority,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableChange {
    pub tables: BTreeSet<String>,
    pub revisions: BTreeMap<Scope, u64>,
    pub causes: BTreeSet<Cause>,
    pub dropped: BTreeSet<Commitment>,
    pub sealed: BTreeSet<Commitment>,
    pub reconcile: bool,
}
impl TableChange {
    fn merge(
        &mut self,
        tables: impl Iterator<Item = String>,
        scope: Scope,
        revision: u64,
        causes: &[Cause],
    ) {
        self.tables.extend(tables);
        let current = self.revisions.entry(scope).or_default();
        *current = (*current).max(revision);
        self.causes.extend(causes);
    }
    fn merge_outcomes(&mut self, outcomes: &Outcomes) {
        if self.reconcile {
            return;
        }
        self.dropped.extend(&outcomes.dropped);
        self.sealed.extend(&outcomes.sealed);
        if self.dropped.len() + self.sealed.len() > MAX_PENDING_OUTCOMES {
            self.dropped.clear();
            self.sealed.clear();
            self.reconcile = true;
        }
    }
}
pub const MAX_PENDING_OUTCOMES: usize = 4096;

#[derive(Default)]
struct Pending {
    change: Option<TableChange>,
    closed: bool,
}
#[derive(Default)]
struct Slot {
    pending: Mutex<Pending>,
    notify: Notify,
}
impl Slot {
    fn offer(&self, labels: Vec<String>, scope: Scope, revision: u64, causes: &[Cause]) {
        if let Ok(mut pending) = self.pending.lock() {
            pending
                .change
                .get_or_insert_with(TableChange::default)
                .merge(labels.into_iter(), scope, revision, causes);
        }
        self.notify.notify_one();
    }
    fn offer_outcomes(&self, scope: Scope, revision: u64, outcomes: &Outcomes) {
        if let Ok(mut pending) = self.pending.lock() {
            let change = pending.change.get_or_insert_with(TableChange::default);
            change.merge(std::iter::empty(), scope, revision, &[Cause::Sync]);
            change.merge_outcomes(outcomes);
        }
        self.notify.notify_one();
    }
    fn close(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.closed = true;
        }
        self.notify.notify_one();
    }
}

struct Entry {
    slot: Weak<Slot>,
    label: String,
}
#[derive(Default)]
struct Registry {
    by_table: HashMap<String, Vec<Entry>>,
    every: Vec<Weak<Slot>>,
    added: usize,
}
impl Registry {
    fn prune(&mut self) {
        self.by_table.retain(|_, entries| {
            entries.retain(|entry| entry.slot.strong_count() > 0);
            !entries.is_empty()
        });
        self.every.retain(|slot| slot.strong_count() > 0);
        self.added = 0;
    }
    fn slots(&self) -> impl Iterator<Item = Arc<Slot>> + '_ {
        self.by_table
            .values()
            .flatten()
            .map(|entry| &entry.slot)
            .chain(&self.every)
            .filter_map(Weak::upgrade)
    }
}

pub struct TableEvents {
    scope: Scope,
    tables: BTreeSet<String>,
    registry: Mutex<Registry>,
}
impl Drop for TableEvents {
    fn drop(&mut self) {
        if let Ok(registry) = self.registry.lock() {
            registry.slots().for_each(|slot| slot.close());
        }
    }
}
impl TableEvents {
    pub fn new(scope: Scope, tables: impl IntoIterator<Item = String>) -> Arc<Self> {
        Arc::new(Self {
            scope,
            tables: tables
                .into_iter()
                .map(|table| table.to_ascii_lowercase())
                .collect(),
            registry: Mutex::default(),
        })
    }
    pub fn follow(&self, tables: impl IntoIterator<Item = String>) -> Arc<Self> {
        let next = Self::new(self.scope, tables);
        if let (Ok(mut from), Ok(mut to)) = (self.registry.lock(), next.registry.lock()) {
            *to = std::mem::take(&mut *from);
        }
        next
    }
    pub fn scope(&self) -> Scope {
        self.scope
    }
    pub fn tables(&self) -> &BTreeSet<String> {
        &self.tables
    }
    pub fn register(
        &self,
        observer: &TableObserver,
        tables: Option<&[(String, String)]>,
    ) -> Result<(), Error> {
        let mut registry = self.registry.lock().map_err(|_| Error::Closed)?;
        let slot = Arc::downgrade(&observer.slot);
        match tables {
            Some(tables) => {
                if let Some((unknown, _)) = tables
                    .iter()
                    .find(|(table, _)| !self.tables.contains(&table.to_ascii_lowercase()))
                {
                    return Err(Error::Rejected(format!(
                        "{unknown} is not a table to observe"
                    )));
                }
                for (table, label) in tables {
                    registry
                        .by_table
                        .entry(table.to_ascii_lowercase())
                        .or_default()
                        .push(Entry {
                            slot: slot.clone(),
                            label: label.to_ascii_lowercase(),
                        });
                }
            }
            None => registry.every.push(slot),
        }
        registry.added += 1;
        if registry.added >= 1024 {
            registry.prune();
        }
        Ok(())
    }
    pub fn publish(&self, tables: Option<&BTreeSet<String>>, revision: u64, causes: &[Cause]) {
        if causes.is_empty() {
            return;
        }
        let Ok(mut registry) = self.registry.lock() else {
            return;
        };
        let tables = tables.unwrap_or(&self.tables);
        let mut dead = false;
        let mut offer_per_observer: HashMap<usize, (Arc<Slot>, Vec<String>)> = HashMap::new();
        let mut offer = |slot: &Weak<Slot>, label: &str| match slot.upgrade() {
            Some(slot) => offer_per_observer
                .entry(Arc::as_ptr(&slot) as usize)
                .or_insert_with(|| (slot, Vec::new()))
                .1
                .push(label.to_string()),
            None => dead = true,
        };
        for table in tables {
            for entry in registry.by_table.get(table).into_iter().flatten() {
                offer(&entry.slot, &entry.label);
            }
            for slot in &registry.every {
                offer(slot, table);
            }
        }
        for (slot, labels) in offer_per_observer.into_values() {
            slot.offer(labels, self.scope, revision, causes);
        }
        if dead {
            registry.prune();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcomes {
    pub dropped: BTreeSet<Commitment>,
    pub sealed: BTreeSet<Commitment>,
}

impl TableEvents {
    pub fn publish_outcomes(&self, revision: u64, outcomes: &Outcomes) {
        if outcomes.dropped.is_empty() && outcomes.sealed.is_empty() {
            return;
        }
        let Ok(registry) = self.registry.lock() else {
            return;
        };
        let mut slots: Vec<_> = registry.slots().collect();
        slots.sort_by_key(|slot| Arc::as_ptr(slot) as usize);
        slots.dedup_by_key(|slot| Arc::as_ptr(slot) as usize);
        for slot in slots {
            slot.offer_outcomes(self.scope, revision, outcomes);
        }
    }
}

#[derive(Default)]
pub struct TableObserver {
    slot: Arc<Slot>,
    registered: BTreeMap<Scope, u64>,
}
impl TableObserver {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn registered_at(&mut self, scope: Scope, revision: u64) {
        self.registered.insert(scope, revision);
    }
    pub fn registered(&self) -> &BTreeMap<Scope, u64> {
        &self.registered
    }
    pub fn try_next(&mut self) -> Result<Option<TableChange>, Error> {
        let mut pending = self.slot.pending.lock().map_err(|_| Error::Closed)?;
        match pending.change.take() {
            Some(change) => Ok(Some(change)),
            None if pending.closed => Err(Error::Closed),
            None => Ok(None),
        }
    }
    pub async fn next(&mut self) -> Result<TableChange, Error> {
        loop {
            if let Some(change) = self.try_next()? {
                return Ok(change);
            }
            self.slot.notify.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocky_core::{AppId, DomainId, MeshId};

    fn scope(byte: u8) -> Scope {
        Scope {
            mesh: MeshId([byte; 16]),
            app: AppId([byte; 16]),
            domain: DomainId([byte; 16]),
        }
    }
    fn set(tables: &[&str]) -> BTreeSet<String> {
        tables.iter().map(|table| table.to_string()).collect()
    }
    fn own(tables: &[&str]) -> Vec<(String, String)> {
        tables
            .iter()
            .map(|t| (t.to_string(), t.to_string()))
            .collect()
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn every_observer_receives_changed_outcomes_once() {
        let events = TableEvents::new(scope(1), ["items".into(), "tags".into()]);
        let mut items = TableObserver::new();
        events
            .register(&items, Some(&own(&["items", "tags"])))
            .unwrap();
        let mut tags = TableObserver::new();
        events.register(&tags, Some(&own(&["tags"]))).unwrap();
        events.publish_outcomes(
            5,
            &Outcomes {
                dropped: [[7; 32]].into(),
                sealed: [[8; 32]].into(),
            },
        );
        events.publish_outcomes(6, &Outcomes::default());
        for observer in [&mut items, &mut tags] {
            let change = observer.next().await.unwrap();
            assert_eq!(change.dropped, [[7; 32]].into());
            assert_eq!(change.sealed, [[8; 32]].into());
            assert!(change.tables.is_empty());
            assert_eq!(change.revisions, [(scope(1), 5)].into());
            assert_eq!(observer.try_next().unwrap(), None);
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn observers_receive_coalesced_changes_of_their_tables_only() {
        let events = TableEvents::new(scope(1), ["Items".into(), "tags".into(), "notes".into()]);
        let mut items = TableObserver::new();
        let mut every = TableObserver::new();
        events
            .register(&items, Some(&own(&["ITEMS", "tags"])))
            .unwrap();
        events.register(&every, None).unwrap();
        assert!(
            events
                .register(&TableObserver::new(), Some(&own(&["absent"])))
                .is_err()
        );
        events.publish(Some(&set(&["notes"])), 3, &[Cause::Local]);
        assert_eq!(items.try_next().unwrap(), None);
        events.publish(Some(&set(&["items"])), 4, &[Cause::Sync]);
        events.publish(Some(&set(&["items", "tags"])), 6, &[Cause::Local]);
        let change = items.next().await.unwrap();
        assert_eq!(change.tables, set(&["items", "tags"]));
        assert_eq!(change.revisions, [(scope(1), 6)].into());
        assert_eq!(change.causes, [Cause::Local, Cause::Sync].into());
        assert_eq!(items.try_next().unwrap(), None);
        events.publish(None, 7, &[Cause::Rebuild]);
        assert_eq!(items.next().await.unwrap().tables, set(&["items", "tags"]));
        assert_eq!(
            every.next().await.unwrap().tables,
            set(&["items", "notes", "tags"])
        );
        drop(events);
        assert!(matches!(items.next().await, Err(Error::Closed)));
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn one_observer_follows_tenants_under_their_labels_and_dropped_ones_are_pruned() {
        let first = TableEvents::new(scope(1), ["items".into()]);
        let second = TableEvents::new(scope(2), ["items".into()]);
        let mut observer = TableObserver::new();
        let label = |label: &str| vec![("items".to_string(), label.to_string())];
        first.register(&observer, Some(&label("a_items"))).unwrap();
        second.register(&observer, Some(&label("b_items"))).unwrap();
        for _ in 0..2000 {
            first
                .register(&TableObserver::new(), Some(&label("x")))
                .unwrap();
        }
        first.publish(Some(&set(&["items"])), 2, &[Cause::Local]);
        second.publish(Some(&set(&["items"])), 9, &[Cause::Sync]);
        let change = observer.next().await.unwrap();
        assert_eq!(change.tables, set(&["a_items", "b_items"]));
        assert_eq!(change.revisions, [(scope(1), 2), (scope(2), 9)].into());
        let registry = first.registry.lock().unwrap();
        assert_eq!(registry.by_table["items"].len(), 1);
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_burst_of_outcomes_past_the_cap_coalesces_into_a_reconcile() {
        let events = TableEvents::new(scope(1), ["items".into()]);
        let mut observer = TableObserver::new();
        events.register(&observer, None).unwrap();
        let held = |observer: &TableObserver| {
            let pending = observer.slot.pending.lock().unwrap();
            pending
                .change
                .as_ref()
                .map_or(0, |change| change.dropped.len() + change.sealed.len())
        };
        let mut most = 0;
        for index in 0..(3 * MAX_PENDING_OUTCOMES as u32) {
            let mut id = [0; 32];
            id[..4].copy_from_slice(&index.to_le_bytes());
            let outcomes = if index % 2 == 0 {
                Outcomes {
                    dropped: [id].into(),
                    ..Outcomes::default()
                }
            } else {
                Outcomes {
                    sealed: [id].into(),
                    ..Outcomes::default()
                }
            };
            events.publish_outcomes(u64::from(index) + 1, &outcomes);
            most = most.max(held(&observer));
        }
        assert!(most <= MAX_PENDING_OUTCOMES, "{most} held");
        events.publish(Some(&set(&["items"])), 1, &[Cause::Local]);
        let change = observer.next().await.unwrap();
        assert!(change.reconcile);
        assert!(change.dropped.is_empty() && change.sealed.is_empty());
        assert_eq!(change.tables, set(&["items"]));
        assert_eq!(
            change.revisions,
            [(scope(1), 3 * MAX_PENDING_OUTCOMES as u64)].into()
        );
        assert_eq!(change.causes, [Cause::Local, Cause::Sync].into());
        events.publish_outcomes(
            1,
            &Outcomes {
                dropped: [[9; 32]].into(),
                ..Outcomes::default()
            },
        );
        let change = observer.next().await.unwrap();
        assert!(
            !change.reconcile,
            "the next change names its outcomes again"
        );
        assert_eq!(change.dropped, [[9; 32]].into());
    }
}
