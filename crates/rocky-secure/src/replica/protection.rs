//! Immutable finalized keys and causal proofs that release them.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Protected {
    at: Commitment,
    fields: RowFields,
    releases: BTreeMap<Commitment, BTreeMap<Commitment, u64>>,
}
impl Protected {
    pub(super) fn new(at: Commitment, fields: RowFields) -> Self {
        Self {
            at,
            fields,
            releases: BTreeMap::new(),
        }
    }
}
type Locks = BTreeMap<Commitment, Vec<SqlValue>>;

impl<A: RowApplication> Inner<A> {
    pub(super) fn row_payload(&self, frame: &EncryptedFrame) -> Result<RowPayload, Error> {
        let operation = frame.operation()?;
        let extension = &operation.header.extensions;
        let key = self
            .opening_key(extension.scope, extension.generation)
            .ok_or_else(|| denied("row payload key unavailable"))?;
        let payload = RowPayload::decode(&decrypt(&key.key, &encode(extension)?, &frame.body)?)?;
        for hash in payload.protection().1 {
            let hash = Hash::from_bytes(*hash);
            if operation.header.backlink != Some(hash) && !operation.header.previous.contains(&hash)
            {
                return Err(denied("horizon release lacks signed causal dependency"));
            }
        }
        Ok(payload)
    }
    fn protected_locks(&self, scope: ResourceScope, fields: &RowFields) -> Result<Locks, Error> {
        let Some(typed) = &self.typed else {
            return Ok(BTreeMap::new());
        };
        let projected = typed
            .by_scope(scope)
            .ok_or_else(|| denied("protected table missing"))?;
        let table = typed
            .rows
            .schema()
            .table(&projected.table.sql_name)
            .ok_or_else(|| denied("protected schema missing"))?;
        let values = typed
            .rows
            .values(&projected.table, fields)
            .map_err(Error::Corrupt)?;
        let mut locks = BTreeMap::new();
        for (name, columns) in std::iter::once(("primary", table.primary_key.clone())).chain(
            table
                .indexes
                .iter()
                .filter(|index| index.unique)
                .map(|index| {
                    (
                        index.name.as_str(),
                        index
                            .columns
                            .iter()
                            .map(|column| column.name.clone())
                            .collect(),
                    )
                }),
        ) {
            let mut tuple = Vec::new();
            for name in columns {
                let position = table
                    .columns
                    .iter()
                    .position(|column| column.sql_name == name)
                    .ok_or_else(|| denied("protected index column missing"))?;
                let column = &table.columns[position];
                let value = column
                    .affinity
                    .store(values[position].clone())
                    .map_err(|error| denied(&error.to_string()))?;
                if value == SqlValue::Null {
                    tuple.clear();
                    break;
                }
                tuple.push(match value {
                    SqlValue::Text(value) => SqlValue::Blob(column.collation.fold(&value)),
                    value => value,
                });
            }
            if !tuple.is_empty() {
                locks.insert(
                    rocky_core::commitment("rocky.horizon-lock.v1", &encode(&(scope.table, name))?),
                    tuple,
                );
            }
        }
        Ok(locks)
    }
    pub(super) fn release_protection(
        &self,
        record: &mut Materialized,
        write: &RowWrite,
        operation: &p2panda_core::Operation<Extensions>,
    ) -> Result<(), Error> {
        let Some(protected) = &mut record.protected else {
            return Ok(());
        };
        let locks = self.protected_locks(write.change.scope, &protected.fields)?;
        let after = write
            .change
            .after
            .as_ref()
            .map(|fields| {
                let mut published = protected.fields.clone();
                for field in write.published_fields() {
                    if let Some(value) = fields.get(&field) {
                        published.insert(field, value.clone());
                    } else {
                        published.remove(&field);
                    }
                }
                self.protected_locks(write.change.scope, &published)
            })
            .transpose()?;
        // Union signed release attempts, including concurrent register losers.
        for (lock, tuple) in locks {
            if after.as_ref().and_then(|locks| locks.get(&lock)) != Some(&tuple) {
                protected
                    .releases
                    .entry(lock)
                    .or_default()
                    .insert(*operation.hash.as_bytes(), operation.header.timestamp);
            }
        }
        Ok(())
    }
    pub(super) async fn protection_stubs(
        &self,
        id: i64,
        record: &Materialized,
    ) -> Result<(), Error> {
        if let Some(protected) = &record.protected {
            for releases in protected.releases.values() {
                for (hash, clock) in releases {
                    self.put_stub(Hash::from_bytes(*hash), id, *clock, false)
                        .await?;
                }
            }
        }
        Ok(())
    }
    pub(super) async fn restore_protection_stubs(
        &self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<(), Error> {
        let mut after = Vec::new();
        loop {
            let rows = collect_rows(self.conn.query(
                "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Blob(after.clone()), SqlValue::Integer(checkpoint::STEP_ROWS as i64)]),
            ).await?).await?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let [SqlValue::Blob(key), SqlValue::Blob(fields)] = row.as_slice() else {
                    return Err(Error::Corrupt("protected row".into()));
                };
                after = key.clone();
                self.protection_stubs(id, &decode(fields)?).await?;
            }
        }
        Ok(())
    }
    async fn protected_collisions(
        &self,
        change: &RowChange,
    ) -> Result<Vec<(Commitment, Option<(Hash, u64)>, BTreeSet<Commitment>)>, Error> {
        let Some(fields) = &change.after else {
            return Ok(Vec::new());
        };
        let candidate = self.protected_locks(change.scope, fields)?;
        let mut after = Vec::new();
        let mut collisions = Vec::new();
        loop {
            let rows = collect_rows(self.conn.query(
                "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                bindings(vec![SqlValue::Blob(encode(&change.scope)?), SqlValue::Blob(after.clone()), SqlValue::Integer(checkpoint::STEP_ROWS as i64)]),
            ).await?).await?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let [SqlValue::Blob(key), SqlValue::Blob(fields)] = row.as_slice() else {
                    return Err(Error::Corrupt("protected row".into()));
                };
                after = key.clone();
                let record: Materialized = decode(fields)?;
                let Some(protected) = record.protected else {
                    continue;
                };
                for (lock, tuple) in self.protected_locks(change.scope, &protected.fields)? {
                    if candidate.get(&lock) == Some(&tuple) {
                        let releases = protected.releases.get(&lock);
                        let proof = releases
                            .and_then(|proofs| proofs.first_key_value())
                            .map(|(hash, clock)| (Hash::from_bytes(*hash), *clock));
                        collisions.push((
                            protected.at,
                            proof,
                            releases
                                .map(|proofs| proofs.keys().copied().collect())
                                .unwrap_or_default(),
                        ));
                        if collisions.len() > 64 {
                            return Err(denied("protected conflict quota"));
                        }
                    }
                }
            }
        }
        Ok(collisions)
    }
    pub(super) async fn protection_dependencies(
        &self,
        change: &RowChange,
    ) -> Result<Vec<(Hash, u64)>, Error> {
        Ok(self
            .protected_collisions(change)
            .await?
            .into_iter()
            .filter_map(|(_, proof, _)| proof)
            .collect())
    }
    pub(super) async fn protected_conflict(
        &self,
        change: &RowChange,
        payload: &RowPayload,
    ) -> Result<bool, Error> {
        let (basis, releases) = payload.protection();
        for (at, _, proofs) in self.protected_collisions(change).await? {
            if basis != Some(at) || !releases.iter().any(|hash| proofs.contains(hash)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
