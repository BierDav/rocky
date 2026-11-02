//! Rocky: SQLite's file locks and WAL shared memory between the connections
//! one worker opens on pool files.
//!
//! OPFS sync access handles already exclude every other worker and tab, so
//! only connections in this worker can share a file. This is the in-process
//! part of SQLite's unix locking protocol (`os_unix.c`, `unixLock` and
//! `unixShmLock`) with the wal-index kept in heap memory, so WAL and several
//! connections per file work as they do natively. Safe code only: the VFS
//! callbacks translate pointers and result codes.
use std::collections::HashMap;

/// A connection's file: the address of its `sqlite3_file`, unique while open.
pub(crate) type Handle = usize;

/// SQLite's `SQLITE_LOCK_*` levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    None,
    Shared,
    Reserved,
    Pending,
    Exclusive,
}
impl Level {
    pub(crate) fn from_sqlite(level: i32) -> Option<Self> {
        Some(match level {
            0 => Self::None,
            1 => Self::Shared,
            2 => Self::Reserved,
            3 => Self::Pending,
            4 => Self::Exclusive,
            _ => return None,
        })
    }
}

/// A wal-index lock request (`SQLITE_SHM_*` flags).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShmLock {
    Unlock,
    Shared,
    Exclusive,
}

/// Another connection holds a conflicting lock (`SQLITE_BUSY`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Busy;

/// A wal-index slot: readers or one writer.
#[derive(Clone, Copy, Default)]
struct Slot {
    shared: usize,
    exclusive: Option<Handle>,
}

/// The state every connection to one file shares.
#[derive(Default)]
struct File {
    /// Connections holding at least `SHARED`.
    shared: usize,
    reserved: Option<Handle>,
    pending: Option<Handle>,
    exclusive: Option<Handle>,
    /// The wal-index. Regions are boxed, so their addresses stay fixed while
    /// any connection maps them.
    regions: Vec<Box<[u8]>>,
    mapped: usize,
    slots: [Slot; 8],
}

struct Open {
    file: String,
    level: Level,
    mapped: bool,
    shm_shared: u8,
    shm_exclusive: u8,
}

#[derive(Default)]
pub(crate) struct Locks {
    files: HashMap<String, File>,
    handles: HashMap<Handle, Open>,
}

impl Locks {
    pub(crate) fn open(&mut self, handle: Handle, file: &str) {
        self.files.entry(file.to_owned()).or_default();
        self.handles.insert(
            handle,
            Open {
                file: file.to_owned(),
                level: Level::None,
                mapped: false,
                shm_shared: 0,
                shm_exclusive: 0,
            },
        );
    }

    /// Releases everything `handle` holds; the last connection to a file
    /// discards its wal-index, which the next open rebuilds from the WAL.
    pub(crate) fn close(&mut self, handle: Handle) {
        self.unlock(handle, Level::None);
        self.shm_unmap(handle);
        if let Some(open) = self.handles.remove(&handle) {
            if !self.handles.values().any(|other| other.file == open.file) {
                self.files.remove(&open.file);
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    fn state(&mut self, handle: Handle) -> Option<(&mut Open, &mut File)> {
        let open = self.handles.get_mut(&handle)?;
        let file = self.files.get_mut(&open.file)?;
        Some((open, file))
    }

    /// Raises `handle` to `want`. A refused `EXCLUSIVE` keeps `PENDING`, so
    /// new readers wait while existing ones finish, as on unix.
    pub(crate) fn lock(&mut self, handle: Handle, want: Level) -> Result<(), Busy> {
        let Some((open, file)) = self.state(handle) else {
            return Err(Busy);
        };
        if open.level >= want {
            return Ok(());
        }
        let other = |holder: Option<Handle>| holder.is_some_and(|holder| holder != handle);
        match want {
            Level::None => Ok(()),
            Level::Shared => {
                if other(file.pending) || other(file.exclusive) {
                    return Err(Busy);
                }
                file.shared += 1;
                open.level = Level::Shared;
                Ok(())
            }
            Level::Reserved => {
                if other(file.reserved) || other(file.pending) || other(file.exclusive) {
                    return Err(Busy);
                }
                file.reserved = Some(handle);
                open.level = Level::Reserved;
                Ok(())
            }
            Level::Pending | Level::Exclusive => {
                if other(file.pending) || other(file.exclusive) {
                    return Err(Busy);
                }
                file.pending = Some(handle);
                open.level = open.level.max(Level::Pending);
                if want == Level::Pending {
                    return Ok(());
                }
                // Every reader but this connection must be gone.
                if file.shared > 1 {
                    return Err(Busy);
                }
                file.exclusive = Some(handle);
                open.level = Level::Exclusive;
                Ok(())
            }
        }
    }

    /// Lowers `handle` to `want`, which is `SHARED` or `NONE`.
    pub(crate) fn unlock(&mut self, handle: Handle, want: Level) {
        let Some((open, file)) = self.state(handle) else {
            return;
        };
        if open.level <= want {
            return;
        }
        let release = |holder: &mut Option<Handle>| {
            if *holder == Some(handle) {
                *holder = None;
            }
        };
        if open.level > Level::Shared {
            release(&mut file.reserved);
            release(&mut file.pending);
            release(&mut file.exclusive);
            open.level = Level::Shared;
        }
        if want == Level::None && open.level == Level::Shared {
            file.shared -= 1;
            open.level = Level::None;
        }
    }

    /// Whether any connection to the file holds `RESERVED` or more.
    pub(crate) fn reserved(&self, handle: Handle) -> bool {
        self.handles
            .get(&handle)
            .and_then(|open| self.files.get(&open.file))
            .is_some_and(|file| {
                file.reserved.is_some() || file.pending.is_some() || file.exclusive.is_some()
            })
    }

    /// The wal-index region `index` of `size` bytes, zeroed when `extend`
    /// adds it; `None` if it does not exist and `extend` is false.
    pub(crate) fn shm_map(
        &mut self,
        handle: Handle,
        index: usize,
        size: usize,
        extend: bool,
    ) -> Result<Option<*mut u8>, String> {
        let Some((open, file)) = self.state(handle) else {
            return Err("unknown file".into());
        };
        if !open.mapped {
            open.mapped = true;
            file.mapped += 1;
        }
        if let Some(region) = file.regions.first() {
            if region.len() != size {
                return Err(format!(
                    "wal-index region size {size} differs from {}",
                    region.len()
                ));
            }
        }
        if file.regions.len() <= index {
            if !extend {
                return Ok(None);
            }
            file.regions
                .resize_with(index + 1, || vec![0; size].into_boxed_slice());
        }
        Ok(Some(file.regions[index].as_mut_ptr()))
    }

    pub(crate) fn shm_lock(
        &mut self,
        handle: Handle,
        offset: usize,
        count: usize,
        request: ShmLock,
    ) -> Result<(), Busy> {
        let Some((open, file)) = self.state(handle) else {
            return Err(Busy);
        };
        let range = offset..offset + count;
        if range.end > file.slots.len() {
            return Err(Busy);
        }
        let bit = |slot: usize| 1u8 << slot;
        match request {
            ShmLock::Unlock => {
                for slot in range {
                    if open.shm_shared & bit(slot) != 0 {
                        file.slots[slot].shared -= 1;
                    }
                    if open.shm_exclusive & bit(slot) != 0 {
                        file.slots[slot].exclusive = None;
                    }
                    open.shm_shared &= !bit(slot);
                    open.shm_exclusive &= !bit(slot);
                }
                Ok(())
            }
            ShmLock::Shared => {
                for slot in range.clone() {
                    if open.shm_shared & bit(slot) == 0
                        && file.slots[slot]
                            .exclusive
                            .is_some_and(|holder| holder != handle)
                    {
                        return Err(Busy);
                    }
                }
                for slot in range {
                    if open.shm_shared & bit(slot) == 0 {
                        file.slots[slot].shared += 1;
                        open.shm_shared |= bit(slot);
                    }
                }
                Ok(())
            }
            ShmLock::Exclusive => {
                for slot in range.clone() {
                    let own = usize::from(open.shm_shared & bit(slot) != 0);
                    let state = file.slots[slot];
                    if state.exclusive.is_some_and(|holder| holder != handle) || state.shared > own
                    {
                        return Err(Busy);
                    }
                }
                for slot in range {
                    file.slots[slot].exclusive = Some(handle);
                    open.shm_exclusive |= bit(slot);
                }
                Ok(())
            }
        }
    }

    /// Stops `handle` using the wal-index; the last user discards it.
    pub(crate) fn shm_unmap(&mut self, handle: Handle) {
        let _ = self.shm_lock(handle, 0, 8, ShmLock::Unlock);
        let Some((open, file)) = self.state(handle) else {
            return;
        };
        if open.mapped {
            open.mapped = false;
            file.mapped -= 1;
            if file.mapped == 0 {
                file.regions.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two() -> Locks {
        let mut locks = Locks::default();
        locks.open(1, "a.db");
        locks.open(2, "a.db");
        locks
    }

    #[test]
    fn writers_exclude_each_other_and_readers() {
        let mut locks = two();
        locks.lock(1, Level::Shared).unwrap();
        locks.lock(2, Level::Shared).unwrap();
        locks.lock(1, Level::Reserved).unwrap();
        assert_eq!(locks.lock(2, Level::Reserved), Err(Busy));
        assert!(locks.reserved(2));
        // A reader blocks the writer, which keeps PENDING and so blocks
        // new readers until the old one leaves.
        assert_eq!(locks.lock(1, Level::Exclusive), Err(Busy));
        locks.unlock(2, Level::None);
        assert_eq!(locks.lock(2, Level::Shared), Err(Busy));
        locks.lock(1, Level::Exclusive).unwrap();
        locks.unlock(1, Level::None);
        assert!(!locks.reserved(2));
        locks.lock(2, Level::Shared).unwrap();
    }

    #[test]
    fn wal_index_is_shared_and_locked_per_slot() {
        let mut locks = two();
        assert_eq!(locks.shm_map(1, 0, 32768, false), Ok(None));
        let first = locks.shm_map(1, 0, 32768, true).unwrap().unwrap();
        assert_eq!(locks.shm_map(2, 0, 32768, false).unwrap(), Some(first));
        locks.shm_lock(1, 3, 1, ShmLock::Shared).unwrap();
        locks.shm_lock(2, 3, 1, ShmLock::Shared).unwrap();
        assert_eq!(locks.shm_lock(1, 3, 1, ShmLock::Exclusive), Err(Busy));
        locks.shm_lock(2, 3, 1, ShmLock::Unlock).unwrap();
        locks.shm_lock(1, 3, 1, ShmLock::Exclusive).unwrap();
        assert_eq!(locks.shm_lock(2, 0, 8, ShmLock::Exclusive), Err(Busy));
        locks.shm_lock(2, 0, 3, ShmLock::Exclusive).unwrap();
        // Closing releases the closer's locks.
        locks.close(1);
        locks.shm_lock(2, 3, 1, ShmLock::Exclusive).unwrap();
        locks.close(2);
        assert!(locks.is_empty());
        assert!(locks.files.is_empty());
    }
}
