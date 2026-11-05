//! Key epochs of a reader scope: one current key that seals new writes and
//! the earlier ones kept to open retained ciphertext. See
//! `docs/runtime/credentials.md`.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    // Earlier epochs of a scope; `secure_keys` holds the current one.
    "CREATE TABLE IF NOT EXISTS secure_key_epochs(scope BLOB NOT NULL,generation BLOB NOT NULL,key_hash BLOB NOT NULL,PRIMARY KEY(scope,generation))",
    // Root escrow records, opaque here, by the key epoch whose frames they
    // travel with; empty scope and epoch for a record of the whole domain,
    // which stays local.
    "CREATE TABLE IF NOT EXISTS secure_escrow(scope BLOB NOT NULL,generation BLOB NOT NULL,tag BLOB NOT NULL,record BLOB NOT NULL,PRIMARY KEY(scope,generation,tag))",
];
use crate::wire::MAX_CARRIED_ESCROW;
type Slot = Option<(ResourceScope, rocky_core::KeyEpoch)>;
fn slot_values(slot: &Slot) -> Result<[SqlValue; 2], Error> {
    Ok(match slot {
        Some((scope, generation)) => [
            SqlValue::Blob(encode(scope)?),
            SqlValue::Blob(encode(generation)?),
        ],
        None => [SqlValue::Blob(Vec::new()), SqlValue::Blob(Vec::new())],
    })
}
fn escrow_row(row: &[SqlValue]) -> Result<(Slot, rocky_credentials::RootEscrow), Error> {
    let corrupt = || Error::Corrupt("escrow record".into());
    let [
        SqlValue::Blob(scope),
        SqlValue::Blob(generation),
        SqlValue::Blob(record),
    ] = row
    else {
        return Err(corrupt());
    };
    let slot = if scope.is_empty() && generation.is_empty() {
        None
    } else {
        Some((decode(scope)?, decode(generation)?))
    };
    let record = rocky_credentials::RootEscrow::from_bytes(record).map_err(|_| corrupt())?;
    Ok((slot, record))
}

fn key_hash(key: &[u8; 32]) -> Vec<u8> {
    rocky_core::commitment("rocky.scope-key.v1", key).to_vec()
}
/// The current key of every scope, and the earlier epochs of `keys`.
pub(super) fn split(keys: Vec<KeyMaterial>) -> (Vec<KeyMaterial>, Vec<KeyMaterial>) {
    let mut newest: BTreeMap<ResourceScope, rocky_core::KeyEpoch> = BTreeMap::new();
    for key in &keys {
        let generation = newest.entry(key.scope).or_insert(key.generation);
        *generation = (*generation).max(key.generation);
    }
    keys.into_iter()
        .partition(|key| newest.get(&key.scope) == Some(&key.generation))
}

impl<A: RowApplication> Inner<A> {
    /// The key that opens ciphertext of `scope` sealed at `generation`:
    /// the current key or a retained earlier epoch.
    pub(crate) fn opening_key(
        &self,
        scope: ResourceScope,
        generation: rocky_core::KeyEpoch,
    ) -> Option<&KeyMaterial> {
        self.config
            .keys
            .iter()
            .chain(&self.history)
            .find(|key| key.scope == scope && key.generation == generation)
    }
    /// Every installed key, current epochs first.
    pub(super) fn opening_keys(&self) -> impl Iterator<Item = &KeyMaterial> {
        self.config.keys.iter().chain(&self.history)
    }
    /// Remembers which key material an epoch was used with, inside the
    /// caller's transaction.
    pub(crate) async fn record_key(&self, key: &KeyMaterial) -> Result<(), Error> {
        let current = self
            .config
            .keys
            .iter()
            .any(|current| current.scope == key.scope && current.generation == key.generation);
        if !current {
            // Members given only the current epoch need a later seal for it.
            self.earlier_epoch.store(true, Ordering::Relaxed);
        }
        let sql = if current {
            "INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO NOTHING"
        } else {
            "INSERT INTO secure_key_epochs VALUES(?,?,?) ON CONFLICT(scope,generation) DO NOTHING"
        };
        self.conn
            .execute(
                sql,
                bindings(vec![
                    SqlValue::Blob(encode(&key.scope)?),
                    SqlValue::Blob(encode(&key.generation)?),
                    SqlValue::Blob(key_hash(&key.key)),
                ]),
            )
            .await?;
        Ok(())
    }
    /// The retained record of a scope's current epoch.
    async fn current_record(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(rocky_core::KeyEpoch, Vec<u8>)>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT generation,key_hash FROM secure_keys WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            None => Ok(None),
            Some([SqlValue::Blob(generation), SqlValue::Blob(hash)]) => {
                Ok(Some((decode(generation)?, hash.clone())))
            }
            _ => Err(denied("corrupt key metadata")),
        }
    }
    /// Checks a key against what its epoch was used with before.
    async fn matches_record(&self, key: &KeyMaterial) -> Result<(), Error> {
        let mismatch = || denied("key differs from retained crypto state");
        let hash = key_hash(&key.key);
        if let Some((generation, retained)) = self.current_record(key.scope).await?
            && generation == key.generation
            && retained != hash
        {
            return Err(mismatch());
        }
        let earlier = collect_rows(
            self.conn
                .query(
                    "SELECT key_hash FROM secure_key_epochs WHERE scope=? AND generation=?",
                    bindings(vec![
                        SqlValue::Blob(encode(&key.scope)?),
                        SqlValue::Blob(encode(&key.generation)?),
                    ]),
                )
                .await?,
        )
        .await?;
        match earlier.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(retained)]) if *retained != hash => Err(mismatch()),
            _ => Ok(()),
        }
    }
    /// Makes `key` the scope's recorded current epoch and keeps the record
    /// of the one before, inside the caller's transaction.
    async fn record_rotation(&self, key: &KeyMaterial) -> Result<(), Error> {
        let scope = encode(&key.scope)?;
        self.conn
            .execute(
                "INSERT INTO secure_key_epochs SELECT scope,generation,key_hash FROM secure_keys WHERE scope=? AND generation<>? ON CONFLICT(scope,generation) DO NOTHING",
                bindings(vec![
                    SqlValue::Blob(scope.clone()),
                    SqlValue::Blob(encode(&key.generation)?),
                ]),
            )
            .await?;
        self.conn
            .execute(
                "INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO UPDATE SET generation=excluded.generation,key_hash=excluded.key_hash",
                bindings(vec![
                    SqlValue::Blob(scope),
                    SqlValue::Blob(encode(&key.generation)?),
                    SqlValue::Blob(key_hash(&key.key)),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Checks the installed keys against the retained key records at open.
    /// A current key newer than the record completes a rotation that was
    /// interrupted before the replica learned it; an older one is refused.
    pub(super) async fn adopt_keys(&mut self) -> Result<(), Error> {
        let mut rotated = Vec::new();
        for key in self.opening_keys() {
            self.matches_record(key).await?;
        }
        for (index, key) in self.config.keys.iter().enumerate() {
            match self.current_record(key.scope).await? {
                Some((generation, _)) if generation > key.generation => {
                    return Err(denied("key is older than the scope's retained key epoch"));
                }
                Some((generation, _)) if generation < key.generation => rotated.push(index),
                _ => {}
            }
        }
        if rotated.is_empty() {
            return Ok(());
        }
        transaction!(self, |inner| {
            for index in &rotated {
                inner.record_rotation(&inner.config.keys[*index]).await?;
            }
            Ok(())
        })
    }
    /// Whether any retained operation of `scope` was sealed at `generation`.
    async fn epoch_in_use(
        &self,
        scope: ResourceScope,
        generation: rocky_core::KeyEpoch,
    ) -> Result<bool, Error> {
        let mut cursor = Vec::new();
        loop {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT hash,header FROM secure_operations WHERE scope=? AND hash>? ORDER BY hash LIMIT 64",
                        [encode(&scope)?, cursor.clone()],
                    )
                    .await?,
            )
            .await?;
            if records.is_empty() {
                return Ok(false);
            }
            for record in records {
                let [SqlValue::Blob(hash), SqlValue::Blob(header)] = record.as_slice() else {
                    return Err(denied("corrupt retained operation"));
                };
                cursor = hash.clone();
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                if header.extensions.generation == generation {
                    return Ok(true);
                }
            }
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Installs another key epoch of a scope. A generation above the
    /// current one rotates: it seals every later write, the previous key
    /// stays to open what it sealed, and every session ends. A generation
    /// below it is an earlier epoch, kept to open retained ciphertext. The
    /// same key again does nothing. Original signed bytes are never
    /// rewritten.
    pub async fn install_key_epoch(&self, key: KeyMaterial) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let time = now()?;
        let scope = key.scope;
        if let Some(held) = inner.opening_key(key.scope, key.generation) {
            return if held.key == key.key {
                Ok(())
            } else {
                Err(denied("key differs from retained crypto state"))
            };
        }
        if inner.opening_keys().any(|held| held.key == key.key) {
            return Err(denied(
                "key material is already installed under another scope or epoch",
            ));
        }
        if !key.generation.valid() || !inner.can_read_key(key.scope, &inner.local_grants(time)?) {
            return Err(denied(
                "key requires read permission for every row in its encryption scope",
            ));
        }
        if inner
            .opening_keys()
            .filter(|held| held.scope == scope)
            .count()
            >= 256
        {
            return Err(denied("key epoch capacity reached"));
        }
        inner.matches_record(&key).await?;
        let recorded = inner.current_record(key.scope).await?;
        let current = inner
            .config
            .keys
            .iter()
            .position(|held| held.scope == key.scope);
        match current {
            Some(index) if inner.config.keys[index].generation > key.generation => {
                inner.history.push(key);
            }
            None if recorded.is_some_and(|(generation, _)| generation > key.generation) => {
                return Err(denied("key is older than the scope's retained key epoch"));
            }
            _ => {
                transaction!(inner, |inner| inner.record_rotation(&key).await)?;
                if let Some(index) = current {
                    let previous = inner.config.keys.swap_remove(index);
                    inner.history.push(previous);
                }
                inner.config.keys.push(key);
            }
        }
        inner.bump_authority();
        drop(inner);
        self.settle().await?;
        Ok(())
    }
    /// Drops an earlier key epoch from memory once no retained operation
    /// was sealed with it: a checkpoint covers them and they were pruned.
    /// The current epoch is evicted with `evict_key` instead.
    pub async fn retire_key_epoch(
        &self,
        scope: ResourceScope,
        generation: rocky_core::KeyEpoch,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(index) = inner
            .history
            .iter()
            .position(|key| key.scope == scope && key.generation == generation)
        else {
            return Err(denied("no earlier key epoch to retire"));
        };
        if inner.epoch_in_use(scope, generation).await? {
            return Err(denied(
                "key epoch still opens retained operations; checkpoint and prune first",
            ));
        }
        inner.history.swap_remove(index);
        inner.bump_authority();
        Ok(())
    }
    /// Every registered partition with the key epochs this file recorded
    /// using there, ascending. An owner that lost its record of a group
    /// finds which secrets to look for.
    pub async fn retained_partitions(
        &self,
    ) -> Result<
        Vec<(
            ResourceScope,
            rocky_core::PartitionTuple,
            rocky_core::PartitionName,
            Vec<rocky_core::KeyEpoch>,
        )>,
        Error,
    > {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut retained = Vec::new();
        for (scope, tuple) in &inner.partitions {
            let mut epochs = Vec::new();
            for sql in [
                "SELECT generation FROM secure_keys WHERE scope=?",
                "SELECT generation FROM secure_key_epochs WHERE scope=?",
            ] {
                for row in collect_rows(inner.conn.query(sql, [encode(scope)?]).await?).await? {
                    let [SqlValue::Blob(generation)] = row.as_slice() else {
                        return Err(denied("corrupt key metadata"));
                    };
                    epochs.push(decode::<rocky_core::KeyEpoch>(generation)?);
                }
            }
            epochs.sort_unstable();
            epochs.dedup();
            let name = *inner
                .partition_names
                .get(scope)
                .ok_or_else(|| Error::Corrupt("partition name".into()))?;
            retained.push((*scope, tuple.clone(), name, epochs));
        }
        Ok(retained)
    }
    /// The root escrow records held, by key epoch and tag.
    pub async fn escrowed(&self) -> Result<BTreeSet<(Slot, Commitment)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        rows(
            &inner.conn,
            "SELECT scope,generation,record FROM secure_escrow",
        )
        .await?
        .iter()
        .map(|row| escrow_row(row).map(|(slot, record)| (slot, record.tag())))
        .collect()
    }
    /// Keeps root escrow records this node sealed of the keys it holds,
    /// each with the frames of its key epoch, or locally for `None`. One
    /// replaces a carried record of its tag.
    pub async fn keep_escrow(
        &self,
        records: Vec<(Slot, rocky_credentials::RootEscrow)>,
    ) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(@cache_neutral inner, |inner| {
            for (slot, record) in &records {
                let [scope, generation] = slot_values(slot)?;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_escrow VALUES(?,?,?,?) ON CONFLICT(scope,generation,tag) DO UPDATE SET record=excluded.record",
                        bindings(vec![
                            scope,
                            generation,
                            SqlValue::Blob(record.tag().to_vec()),
                            SqlValue::Blob(record.to_bytes().map_err(credential)?),
                        ]),
                    )
                    .await?;
            }
            Ok(())
        })
    }
    /// Keeps records a peer carried with frames of their key epoch, up to
    /// `MAX_CARRIED_ESCROW` distinct ones per epoch; a held tag stays.
    pub(crate) async fn keep_carried_escrow(
        &self,
        records: Vec<(
            (ResourceScope, rocky_core::KeyEpoch),
            rocky_credentials::RootEscrow,
        )>,
    ) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(@cache_neutral inner, |inner| {
            for (slot, record) in &records {
                let [scope, generation] = slot_values(&Some(*slot))?;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_escrow SELECT ?1,?2,?3,?4 WHERE (SELECT count(*) FROM secure_escrow WHERE scope=?1 AND generation=?2)<?5 ON CONFLICT(scope,generation,tag) DO NOTHING",
                        bindings(vec![
                            scope,
                            generation,
                            SqlValue::Blob(record.tag().to_vec()),
                            SqlValue::Blob(record.to_bytes().map_err(credential)?),
                            SqlValue::Integer(MAX_CARRIED_ESCROW as i64),
                        ]),
                    )
                    .await?;
            }
            Ok(())
        })
    }
    /// The records of `slots`, which a session carries before their frames.
    pub(crate) async fn escrow_of(
        &self,
        slots: &BTreeSet<(ResourceScope, rocky_core::KeyEpoch)>,
    ) -> Result<
        Vec<(
            (ResourceScope, rocky_core::KeyEpoch),
            rocky_credentials::RootEscrow,
        )>,
        Error,
    > {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut found = Vec::new();
        for slot in slots {
            let [scope, generation] = slot_values(&Some(*slot))?;
            let records = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT scope,generation,record FROM secure_escrow WHERE scope=? AND generation=? LIMIT ?",
                        bindings(vec![
                            scope,
                            generation,
                            SqlValue::Integer(MAX_CARRIED_ESCROW as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            for row in records {
                found.push((*slot, escrow_row(&row)?.1));
            }
        }
        Ok(found)
    }
    /// Reads a closed file, a backup's copy: its root escrow records and
    /// the key epochs of the ciphertext it holds, which they must cover.
    pub async fn archived_escrow(
        path: &Path,
    ) -> Result<
        (
            Vec<(Slot, rocky_credentials::RootEscrow)>,
            BTreeSet<(ResourceScope, rocky_core::KeyEpoch)>,
        ),
        Error,
    > {
        check_replica_format(path).await?;
        let conn = ScopedConnection::new(
            crate::sqlite::Connection::open_read_only(path)?,
            None,
            crate::store::SCHEMA,
        );
        let records = rows(&conn, "SELECT scope,generation,record FROM secure_escrow")
            .await?
            .iter()
            .map(|row| escrow_row(row))
            .collect::<Result<Vec<_>, Error>>()?;
        let mut held = BTreeSet::new();
        for row in rows(&conn, "SELECT header FROM secure_operations").await? {
            let [SqlValue::Blob(header)] = row.as_slice() else {
                return Err(Error::Corrupt("retained operation".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(header)?;
            held.insert((header.extensions.scope, header.extensions.generation));
        }
        Ok((records, held))
    }
    /// The installed key epochs of a scope, ascending; the last is current.
    pub async fn key_epochs(&self, scope: ResourceScope) -> Vec<rocky_core::KeyEpoch> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut epochs: Vec<rocky_core::KeyEpoch> = inner
            .opening_keys()
            .filter(|key| key.scope == scope)
            .map(|key| key.generation)
            .collect();
        epochs.sort_unstable();
        epochs
    }
}
