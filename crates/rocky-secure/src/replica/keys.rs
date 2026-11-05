//! Key epochs of a reader scope: one current key that seals new writes and
//! the earlier ones kept to open retained ciphertext. See
//! `docs/runtime/credentials.md`.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    // Earlier epochs of a scope; `secure_keys` holds the current one.
    "CREATE TABLE IF NOT EXISTS secure_key_epochs(scope BLOB NOT NULL,generation BLOB NOT NULL,key_hash BLOB NOT NULL,PRIMARY KEY(scope,generation))",
    // Root escrow records, opaque here, by the key epoch whose frames they
    // travel with and the root-issued identity atop the vouching chain, so
    // an issuer fills one slot per epoch; empty scope and epoch for a
    // record of the whole domain, which stays local. `carrier` is the peer
    // that carried it, this node for its own.
    "CREATE TABLE IF NOT EXISTS secure_escrow(scope BLOB NOT NULL,generation BLOB NOT NULL,issuer BLOB NOT NULL,carrier BLOB NOT NULL,record BLOB NOT NULL,PRIMARY KEY(scope,generation,issuer))",
    // Carried records refused, by the peer that carried them.
    "CREATE TABLE IF NOT EXISTS secure_escrow_refused(peer BLOB PRIMARY KEY,records INTEGER NOT NULL,reason TEXT NOT NULL)",
];
use rocky_credentials::{RootEscrow, VouchedEscrow};
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
/// A stored record: vouched under a key epoch, or the domain's own.
enum Stored {
    Vouched(Box<VouchedEscrow>),
    Domain(RootEscrow),
}
impl Stored {
    fn record(&self) -> &RootEscrow {
        match self {
            Self::Vouched(vouched) => vouched.record(),
            Self::Domain(record) => record,
        }
    }
}
fn escrow_row(row: &[SqlValue]) -> Result<(Slot, Stored), Error> {
    let corrupt = || Error::Corrupt("escrow record".into());
    let [
        SqlValue::Blob(scope),
        SqlValue::Blob(generation),
        SqlValue::Blob(record),
    ] = row
    else {
        return Err(corrupt());
    };
    if scope.is_empty() && generation.is_empty() {
        let record = RootEscrow::from_bytes(record).map_err(|_| corrupt())?;
        return Ok((None, Stored::Domain(record)));
    }
    let vouched = VouchedEscrow::from_bytes(record).map_err(|_| corrupt())?;
    Ok((
        Some((decode(scope)?, decode(generation)?)),
        Stored::Vouched(Box::new(vouched)),
    ))
}

/// The root-issued identity atop `chain`, whose escrow slot its records fill.
fn issuer_of(chain: &CredentialChain) -> Option<Vec<u8>> {
    chain
        .0
        .first()
        .map(|link| link.capability.subject.as_bytes().to_vec())
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
    /// The issuer slot a carried record is kept in, or why it is refused:
    /// it must be signed by a node whose chain, at the signing time and
    /// under the current revocations, let it read the whole scope, as a
    /// frame's author is judged.
    fn judge_escrow(&self, vouched: &VouchedEscrow, time: u64) -> Result<Vec<u8>, &'static str> {
        if vouched.verify().is_err() {
            return Err("its vouch signature does not verify");
        }
        if vouched.signed_at() > time.saturating_add(rocky_core::MAX_CLOCK_AHEAD.as_secs()) {
            return Err("it is vouched for in the future");
        }
        let (scope, _) = vouched.slot();
        match self.resolve_chain(
            vouched.chain(),
            vouched.signer(),
            &self.config.revocations,
            vouched.signed_at(),
        ) {
            Ok(grants) if self.can_read_key(scope, &grants) => {
                issuer_of(vouched.chain()).ok_or("its signer could not read the scope's key")
            }
            _ => Err("its signer could not read the scope's key"),
        }
    }
    /// Keeps a record in its issuer's slot of `slot`. A carried record
    /// never replaces one this node sealed.
    async fn store_escrow(
        &self,
        slot: &Slot,
        issuer: Vec<u8>,
        carrier: PublicKey,
        record: Vec<u8>,
    ) -> Result<(), Error> {
        let [scope, generation] = slot_values(slot)?;
        self.conn
            .execute(
                "INSERT INTO secure_escrow VALUES(?1,?2,?3,?4,?5) ON CONFLICT(scope,generation,issuer) DO UPDATE SET carrier=excluded.carrier,record=excluded.record WHERE excluded.carrier=?6 OR secure_escrow.carrier<>?6",
                bindings(vec![
                    scope,
                    generation,
                    SqlValue::Blob(issuer),
                    SqlValue::Blob(carrier.as_bytes().to_vec()),
                    SqlValue::Blob(record),
                    SqlValue::Blob(self.config.node.public_key().as_bytes().to_vec()),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Counts `records` refused against the peer that carried them.
    async fn refuse_escrow(&self, peer: &[u8], records: u64, reason: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_escrow_refused VALUES(?,?,?) ON CONFLICT(peer) DO UPDATE SET records=records+excluded.records,reason=excluded.reason",
                bindings(vec![
                    SqlValue::Blob(peer.to_vec()),
                    SqlValue::Integer(i64::try_from(records).unwrap_or(i64::MAX)),
                    SqlValue::Text(reason.into()),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Re-judges the kept carried records under the current revocations,
    /// inside the caller's transaction: a refused one is deleted, freeing
    /// its issuer's slot, and counted against the peer that carried it.
    /// The last records of an epoch stay when none other is accepted:
    /// revoking a lost holder must not leave the epoch unrecoverable.
    pub(super) async fn drop_revoked_escrow(&self) -> Result<(), Error> {
        let time = now()?;
        let own = self.config.node.public_key().as_bytes().to_vec();
        let kept = collect_rows(
            self.conn
                .query(
                    "SELECT scope,generation,issuer,carrier,record FROM secure_escrow WHERE scope<>x''",
                    (),
                )
                .await?,
        )
        .await?;
        type Epoch = (Vec<u8>, Vec<u8>);
        let mut epochs: BTreeMap<Epoch, (bool, Vec<Epoch>)> = BTreeMap::new();
        for row in kept {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(generation),
                SqlValue::Blob(issuer),
                SqlValue::Blob(carrier),
                record,
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("escrow record".into()));
            };
            let (_, Stored::Vouched(vouched)) = escrow_row(&[
                SqlValue::Blob(scope.clone()),
                SqlValue::Blob(generation.clone()),
                record.clone(),
            ])?
            else {
                return Err(Error::Corrupt("escrow record".into()));
            };
            let (accepted, refused) = epochs
                .entry((scope.clone(), generation.clone()))
                .or_default();
            if *carrier == own || self.judge_escrow(&vouched, time).is_ok() {
                *accepted = true;
            } else {
                refused.push((issuer.clone(), carrier.clone()));
            }
        }
        let mut dropped: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
        for ((scope, generation), (accepted, refused)) in epochs {
            if !accepted {
                continue;
            }
            for (issuer, carrier) in refused {
                self.conn
                    .execute(
                        "DELETE FROM secure_escrow WHERE scope=? AND generation=? AND issuer=?",
                        bindings(vec![
                            SqlValue::Blob(scope.clone()),
                            SqlValue::Blob(generation.clone()),
                            SqlValue::Blob(issuer),
                        ]),
                    )
                    .await?;
                *dropped.entry(carrier).or_default() += 1;
            }
        }
        for (carrier, records) in dropped {
            self.refuse_escrow(
                &carrier,
                records,
                "a revocation refused its signer after it was kept",
            )
            .await?;
        }
        Ok(())
    }
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
    /// The root escrow records this node sealed, by key epoch and tag.
    pub async fn escrowed(&self) -> Result<BTreeSet<(Slot, Commitment)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        collect_rows(
            inner
                .conn
                .query(
                    "SELECT scope,generation,record FROM secure_escrow WHERE carrier=?",
                    [inner.config.node.public_key().as_bytes().to_vec()],
                )
                .await?,
        )
        .await?
        .iter()
        .map(|row| escrow_row(row).map(|(slot, stored)| (slot, stored.record().tag())))
        .collect()
    }
    /// Keeps root escrow records this node sealed of the keys it holds,
    /// each vouched for by this node under its key epoch, which carries it
    /// with its frames, or local for `None`.
    pub async fn keep_escrow(&self, records: Vec<(Slot, RootEscrow)>) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let time = now()?;
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(@cache_neutral inner, |inner| {
            let own = inner.config.node.public_key();
            let issuer = issuer_of(&inner.config.credentials)
                .unwrap_or_else(|| own.as_bytes().to_vec());
            for (slot, record) in &records {
                let bytes = match slot {
                    Some(slot) => VouchedEscrow::sign(
                        &inner.config.node,
                        inner.config.credentials.clone(),
                        *slot,
                        record.clone(),
                        time,
                    )
                    .and_then(|vouched| vouched.to_bytes()),
                    None => record.to_bytes(),
                }
                .map_err(credential)?;
                inner.store_escrow(slot, issuer.clone(), own, bytes).await?;
            }
            Ok(())
        })
    }
    /// Keeps the vouched records `peer` carried with frames of their key
    /// epoch: one per issuer, the root-issued identity atop the vouching
    /// chain, which a record under the same issuer replaces. Any other
    /// record is refused and counted against `peer` (`escrow_refusals`).
    pub(crate) async fn keep_carried_escrow(
        &self,
        peer: PublicKey,
        records: Vec<VouchedEscrow>,
    ) -> Result<(), Error> {
        if records.is_empty() {
            return Ok(());
        }
        let time = now()?;
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let own = inner.config.node.public_key();
        transaction!(@cache_neutral inner, |inner| {
            let mut refused: Option<(u64, &'static str)> = None;
            for vouched in &records {
                match inner.judge_escrow(vouched, time) {
                    Err(reason) => {
                        refused = Some((refused.map_or(0, |(count, _)| count) + 1, reason));
                    }
                    Ok(_) if vouched.signer() == own => {}
                    Ok(issuer) => {
                        let bytes = vouched.to_bytes().map_err(credential)?;
                        inner
                            .store_escrow(&Some(vouched.slot()), issuer, peer, bytes)
                            .await?;
                    }
                }
            }
            if let Some((count, reason)) = refused {
                inner.refuse_escrow(peer.as_bytes(), count, reason).await?;
            }
            Ok(())
        })
    }
    /// The carried root escrow records refused, by peer: how many and the
    /// latest reason.
    pub async fn escrow_refusals(&self) -> Result<Vec<(PublicKey, u64, String)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        rows(
            &inner.conn,
            "SELECT peer,records,reason FROM secure_escrow_refused",
        )
        .await?
        .into_iter()
        .map(|row| match row.as_slice() {
            [
                SqlValue::Blob(peer),
                SqlValue::Integer(records),
                SqlValue::Text(reason),
            ] => Ok((
                PublicKey::try_from(peer.as_slice())
                    .map_err(|_| Error::Corrupt("escrow refusal peer".into()))?,
                u64::try_from(*records).unwrap_or_default(),
                reason.clone(),
            )),
            _ => Err(Error::Corrupt("escrow refusal".into())),
        })
        .collect()
    }
    /// The vouched records of `slots`, which a session carries before
    /// their frames.
    pub(crate) async fn escrow_of(
        &self,
        slots: &BTreeSet<(ResourceScope, rocky_core::KeyEpoch)>,
    ) -> Result<Vec<VouchedEscrow>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut found = Vec::new();
        for slot in slots {
            let [scope, generation] = slot_values(&Some(*slot))?;
            let records = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT scope,generation,record FROM secure_escrow WHERE scope=? AND generation=?",
                        bindings(vec![scope, generation]),
                    )
                    .await?,
            )
            .await?;
            for row in records {
                if let (_, Stored::Vouched(vouched)) = escrow_row(&row)? {
                    found.push(*vouched);
                }
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
            Vec<(Slot, RootEscrow)>,
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
            .map(|row| escrow_row(row).map(|(slot, stored)| (slot, stored.record().clone())))
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
