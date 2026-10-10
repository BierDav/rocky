//! Move provenance and field ordering, independent of source-key availability.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreationProof {
    version: u8,
    manifest: Commitment,
    table: Commitment,
    id: rocky_core::RowIdentity,
    stamp: Stamp,
    bindings: Vec<rocky_credentials::AuthorBinding>,
    signature: p2panda_core::Signature,
}
impl CreationProof {
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.row-creation-proof.v1",
            &(
                self.version,
                self.manifest,
                self.table,
                self.id,
                &self.stamp,
                &self.bindings,
            ),
        )
    }
    pub(super) fn sign(
        manifest: Commitment,
        table: Commitment,
        id: rocky_core::RowIdentity,
        stamp: Stamp,
        author: &identity::RowAuthor,
    ) -> Result<Self, Error> {
        let mut proof = Self {
            version: 1,
            manifest,
            table,
            id,
            stamp,
            bindings: author
                .credentials
                .0
                .iter()
                .map(|link| link.author_binding.clone())
                .collect(),
            signature: author.key.sign(b""),
        };
        proof.signature = author.key.sign(&proof.bytes()?);
        Ok(proof)
    }
    pub(crate) fn verify(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        table: Commitment,
        id: rocky_core::RowIdentity,
        ceiling: &Stamp,
    ) -> Result<(), Error> {
        if self.version != 1
            || self.manifest != manifest
            || self.table != table
            || self.id != id
            || self.stamp > *ceiling
            || !self.stamp.author.verify(&self.bytes()?, &self.signature)
        {
            return Err(denied("invalid signed row creation proof"));
        }
        rocky_credentials::AuthorBinding::verify_chain(
            &self.bindings,
            publisher,
            manifest,
            table,
            id.author(),
            self.stamp.author,
        )
        .map_err(credential)
    }
    pub(crate) fn stamp(&self) -> &Stamp {
        &self.stamp
    }
    pub(super) fn manifest(&self) -> Commitment {
        self.manifest
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Carried {
    pub(crate) proof: CreationProof,
    pub(crate) stamps: BTreeMap<Commitment, Stamp>,
    /// Present only on a destination insert; sealed with the source key.
    pub(crate) moved_from: Option<Vec<u8>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MoveSource {
    pub(super) scope: ResourceScope,
    pub(super) tuple: rocky_core::PartitionTuple,
    pub(super) creation: Hash,
    pub(super) generation: u64,
    pub(super) deletion_author: PublicKey,
    pub(super) deletion_sequence: u64,
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn check_carried(
        &self,
        write: &RowWrite,
        carried: &Carried,
        ceiling: &Stamp,
    ) -> Result<(), Error> {
        let id = self
            .app
            .row_identity(&write.change)
            .map_err(|error| denied(&error))?
            .ok_or_else(|| denied("carried fields require a numeric identity"))?;
        self.known(carried.proof.manifest())?;
        carried.proof.verify(
            self.config.publisher,
            carried.proof.manifest(),
            write.change.scope.table,
            id,
            ceiling,
        )?;
        let fields = write
            .change
            .after
            .as_ref()
            .ok_or_else(|| denied("cannot carry a deletion"))?;
        if carried
            .stamps
            .keys()
            .any(|field| !fields.contains_key(field))
            || carried.stamps.values().any(|stamp| stamp > ceiling)
            || (write.change.before.is_none()
                && (carried.moved_from.is_none()
                    || carried.stamps.keys().copied().collect::<BTreeSet<_>>()
                        != fields.keys().copied().collect()))
            || (write.change.before.is_some()
                && (carried.moved_from.is_some()
                    || carried.stamps.keys().copied().collect::<BTreeSet<_>>() != write.assigned))
        {
            return Err(denied("invalid carried field stamps or move link"));
        }
        if write.change.before.is_some()
            && carried
                .stamps
                .values()
                .any(|stamp| stamp.author != ceiling.author)
        {
            return Err(denied("only the original author can follow an update"));
        }
        Ok(())
    }
}

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_move_rows(scope BLOB NOT NULL,row BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_consumed(hash BLOB PRIMARY KEY,reason TEXT NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_followed(original BLOB NOT NULL,scope BLOB NOT NULL,field BLOB NOT NULL,PRIMARY KEY(original,scope,field)) WITHOUT ROWID",
];
impl<A: RowApplication> Inner<A> {
    pub(super) async fn consume(&self, hash: Hash, reason: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO secure_consumed VALUES(?,?)",
                bindings(vec![
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                    SqlValue::Text(reason.into()),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn consumed(&self, hash: Hash) -> Result<Option<String>, Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT reason FROM secure_consumed WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0))
            .transpose()
            .map_err(Error::from)
    }
}

impl<A: RowApplication> SecureSession<A> {
    /// Atomically moves a parent and its in-group cascade children.
    pub async fn move_row(
        &self,
        handle: Commitment,
        table: &str,
        id: rocky_core::RowIdentity,
        from: &[rocky_core::SqlValue],
        to: &[rocky_core::SqlValue],
    ) -> Result<Vec<EncryptedFrame>, Error> {
        let mut inner = self.replica.inner.lock().await;
        inner.check_authority(Some(self.authority))?;
        inner.local_grants(now()?)?;
        let frames = transaction!(inner, |inner| {
            inner
                .move_row(handle, table, id, from, to, &self.status.scopes)
                .await
        })?;
        inner.publish_revision().await?;
        inner.flush()?;
        Ok(frames)
    }
}

fn scope_for(
    typed: &typed::Typed,
    table: &typed::Projected,
    values: &[SqlValue],
) -> Result<(ResourceScope, rocky_core::PartitionTuple), Error> {
    let logical = typed
        .rows
        .schema()
        .table(&table.table.sql_name)
        .ok_or_else(|| denied("unknown move table"))?;
    if values.len() != logical.partition_by.len() || values.is_empty() {
        return Err(denied(&format!(
            "{}: move requires a partition tuple",
            logical.sql_name
        )));
    }
    let mut row = vec![SqlValue::Null; logical.columns.len()];
    for (column, value) in logical.partition_by.iter().zip(values) {
        let position = logical
            .columns
            .iter()
            .position(|held| held.sql_name == *column)
            .ok_or_else(|| denied("missing partition column"))?;
        row[position] = value.clone();
    }
    let tuple = typed
        .rows
        .partition(&table.table, &row)
        .map_err(|error| denied(&error))?
        .ok_or_else(|| denied("cannot move an unpartitioned row"))?;
    let scope = tuple
        .scope(typed.domain, table.table.id)
        .map_err(|error| denied(&error.to_string()))?;
    Ok((scope, tuple))
}
fn positions(table: &rocky_core::LogicalTable, names: &[String]) -> Result<Vec<usize>, Error> {
    names
        .iter()
        .map(|name| {
            table
                .columns
                .iter()
                .position(|column| column.sql_name.eq_ignore_ascii_case(name))
                .ok_or_else(|| denied("missing reference column"))
        })
        .collect()
}
fn matches_reference(
    child: &rocky_core::LogicalTable,
    child_values: &[SqlValue],
    foreign: &rocky_core::LogicalForeignKey,
    parent: &rocky_core::LogicalTable,
    parent_values: &[SqlValue],
) -> Result<bool, Error> {
    let child_columns = positions(child, &foreign.columns)?;
    let parent_columns = positions(parent, &foreign.references)?;
    let left: Vec<_> = child_columns
        .iter()
        .map(|position| child_values[*position].clone())
        .collect();
    if left.contains(&SqlValue::Null) {
        return Ok(false);
    }
    let right: Vec<_> = parent_columns
        .iter()
        .map(|position| parent_values[*position].clone())
        .collect();
    let key: Vec<_> = parent_columns
        .iter()
        .map(|position| rocky_core::KeyColumn {
            affinity: parent.columns[*position].affinity,
            collation: parent.columns[*position].collation,
        })
        .collect();
    Ok(
        rocky_core::key_components(&key, &left).map_err(|error| denied(&error.to_string()))?
            == rocky_core::key_components(&key, &right)
                .map_err(|error| denied(&error.to_string()))?,
    )
}

impl<A: RowApplication> Inner<A> {
    async fn move_holdings(
        &self,
        name: &str,
        scope: ResourceScope,
        allowed: &BTreeSet<ResourceScope>,
    ) -> Result<(), Error> {
        if !allowed.contains(&scope) || !self.holds(scope).await? || !self.readable(scope)? {
            return Err(denied(&format!(
                "{name}: move requires the held readable table partition"
            )));
        }
        self.ensure_horizon_writable(scope).await
    }
    async fn move_records(&self, scope: ResourceScope) -> Result<Vec<Materialized>, Error> {
        let rows = collect_rows(
            self.conn
                .query(
                    "SELECT fields FROM secure_rows WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?;
        rows.into_iter()
            .map(|row| match row.as_slice() {
                [SqlValue::Blob(bytes)] => decode::<Materialized>(bytes),
                _ => Err(Error::Corrupt("move source row".into())),
            })
            .filter(|row| row.as_ref().map_or(true, |row| !row.deleted))
            .collect()
    }
    pub(super) async fn move_row(
        &mut self,
        handle: Commitment,
        name: &str,
        id: rocky_core::RowIdentity,
        from: &[SqlValue],
        to: &[SqlValue],
        allowed: &BTreeSet<ResourceScope>,
    ) -> Result<Vec<EncryptedFrame>, Error> {
        let input = rocky_core::commitment(
            "rocky.row-move-input.v1",
            &encode(&(name, id, scalar_bytes(from), scalar_bytes(to)))?,
        );
        if let Some(previous) = self.lookup_bundle(handle, input).await? {
            return previous.map_err(|_| denied("move handle is behind pruning horizon"));
        }
        let typed = self
            .typed
            .clone()
            .ok_or_else(|| denied("move requires typed tables"))?;
        let parent = typed
            .by_name(name)
            .ok_or_else(|| denied("unknown move table"))?;
        let logical = typed
            .rows
            .schema()
            .table(name)
            .ok_or_else(|| denied("unknown move table"))?;
        if logical.partition_by == ["id"] {
            return Err(denied(&format!(
                "{name}: a table partitioned by its own id cannot move"
            )));
        }
        if from == to {
            return Err(denied("move source and destination are equal"));
        }
        let (source, _) = scope_for(&typed, parent, from)?;
        self.move_holdings(name, source, allowed).await?;
        let row = rocky_core::commitment("rocky.row-incarnation.v2", &id.integer().to_le_bytes());
        let record = self
            .row(source, row)
            .await?
            .filter(|row| !row.deleted)
            .ok_or_else(|| denied(&format!("{name}: move source row is absent")))?;
        let mut collected = BTreeMap::from([((source, id.integer()), record)]);
        let mut pending = vec![(source, id.integer())];
        while let Some(key) = pending.pop() {
            let record = collected
                .get(&key)
                .ok_or_else(|| Error::Corrupt("move closure".into()))?;
            let parent = typed
                .by_scope(key.0)
                .ok_or_else(|| denied("unknown move parent"))?;
            let parent_logical = typed
                .rows
                .schema()
                .table(&parent.table.sql_name)
                .ok_or_else(|| denied("unknown move parent"))?;
            let parent_values = typed
                .rows
                .values(&parent.table, &record.fields)
                .map_err(|error| denied(&error))?;
            for child in &typed.tables {
                let child_logical = typed
                    .rows
                    .schema()
                    .table(&child.table.sql_name)
                    .ok_or_else(|| denied("unknown move child"))?;
                if child_logical.partition_group.is_none()
                    || child_logical.partition_group != parent_logical.partition_group
                {
                    continue;
                }
                for foreign in child_logical.foreign_keys.iter().filter(|foreign| {
                    foreign.table.eq_ignore_ascii_case(&parent.table.sql_name)
                        && foreign.on_delete == "CASCADE"
                }) {
                    let (source, _) = scope_for(&typed, child, from)?;
                    let (destination, _) = scope_for(&typed, child, to)?;
                    self.move_holdings(&child.table.sql_name, source, allowed)
                        .await?;
                    self.move_holdings(&child.table.sql_name, destination, allowed)
                        .await?;
                    for record in self.move_records(source).await? {
                        let values = typed
                            .rows
                            .values(&child.table, &record.fields)
                            .map_err(|error| denied(&error))?;
                        if !matches_reference(
                            child_logical,
                            &values,
                            foreign,
                            parent_logical,
                            &parent_values,
                        )? {
                            continue;
                        }
                        let identity = record
                            .identity
                            .ok_or_else(|| denied("child has no row identity"))?;
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            collected.entry((source, identity))
                        {
                            entry.insert(record);
                            pending.push((source, identity));
                            if collected.len() * 2 > super::bundle::MAX_BUNDLE_ROWS {
                                return Err(denied(&format!(
                                    "{}: move exceeds bundle row limit",
                                    child.table.sql_name
                                )));
                            }
                        }
                    }
                }
            }
        }
        let mut writes = Vec::new();
        let mut carried = BTreeMap::new();
        let mut deletion_counts = BTreeMap::<ResourceScope, u64>::new();
        for ((source, identity), record) in &collected {
            let table = typed
                .by_scope(*source)
                .ok_or_else(|| denied("unknown moved table"))?;
            let logical = typed
                .rows
                .schema()
                .table(&table.table.sql_name)
                .ok_or_else(|| denied("unknown moved table"))?;
            let (destination, _) = scope_for(&typed, table, to)?;
            let (_, tuple) = scope_for(&typed, table, from)?;
            self.move_holdings(&table.table.sql_name, destination, allowed)
                .await?;
            let mut values = typed
                .rows
                .values(&table.table, &record.fields)
                .map_err(|error| denied(&error))?;
            let before_values = values.clone();
            let mut nulled = BTreeSet::new();
            for foreign in &logical.foreign_keys {
                let target = typed
                    .by_name(&foreign.table)
                    .ok_or_else(|| denied("unknown referenced table"))?;
                let target_logical = typed
                    .rows
                    .schema()
                    .table(&target.table.sql_name)
                    .ok_or_else(|| denied("unknown referenced table"))?;
                if target_logical.partition_by.is_empty()
                    || foreign
                        .columns
                        .iter()
                        .all(|column| logical.partition_by.contains(column))
                {
                    continue;
                }
                let follows = collected.iter().any(|((scope, _), parent)| {
                    scope.table == target.table.id
                        && typed
                            .rows
                            .values(&target.table, &parent.fields)
                            .ok()
                            .and_then(|parent_values| {
                                matches_reference(
                                    logical,
                                    &before_values,
                                    foreign,
                                    target_logical,
                                    &parent_values,
                                )
                                .ok()
                            })
                            == Some(true)
                });
                let reference_columns = positions(logical, &foreign.columns)?;
                if follows
                    || reference_columns
                        .iter()
                        .any(|position| values[*position] == SqlValue::Null)
                {
                    continue;
                }
                for position in reference_columns.into_iter().filter(|position| {
                    !logical
                        .partition_by
                        .contains(&logical.columns[*position].sql_name)
                }) {
                    if foreign.on_delete != "SET NULL" || logical.columns[position].not_null {
                        return Err(denied(&format!(
                            "{}: non-nullable outgoing reference to {}",
                            table.table.sql_name, target.table.sql_name
                        )));
                    }
                    values[position] = SqlValue::Null;
                    nulled.insert(table.table.fields[position]);
                }
            }
            for (position, value) in positions(logical, &logical.partition_by)?
                .into_iter()
                .zip(to)
            {
                values[position] = value.clone();
            }
            let fields = typed
                .rows
                .fields(&table.table, &values)
                .map_err(|error| denied(&error))?;
            let key = typed
                .rows
                .row_id(typed.domain.domain, &table.table, &values)
                .map_err(|error| denied(&error))?;
            let insert = RowWrite::changed(RowChange {
                scope: destination,
                row: key,
                before: None,
                after: Some(fields),
            });
            let delete = RowWrite::changed(RowChange {
                scope: *source,
                row: record.key.ok_or_else(|| denied("move source has no key"))?,
                before: Some(record.fields.clone()),
                after: None,
            });
            let proof = record
                .proof
                .clone()
                .ok_or_else(|| denied("move source lacks creation proof"))?;
            let source_key = self
                .config
                .keys
                .iter()
                .find(|key| key.scope == *source)
                .ok_or_else(|| denied("source key missing"))?;
            let source_author = self
                .row_authors
                .get(&source.table)
                .ok_or_else(|| denied("source table author missing"))?
                .key
                .public_key();
            let source_number = self.scope_id(*source).await?;
            let deletion_base = self
                .log(source_number, source_author)
                .await?
                .map_or(0, |log| log.sequence as u64 + 1);
            let count = deletion_counts.entry(*source).or_default();
            let deletion_sequence = deletion_base + *count / super::bundle::ROWS_PER_FRAME as u64;
            *count += 1;
            let moved_from = super::super::wire::encrypt(
                &source_key.key,
                &encode(&(destination, identity))?,
                &encode(&MoveSource {
                    scope: *source,
                    tuple,
                    creation: record.creation,
                    generation: source_key.generation,
                    deletion_author: source_author,
                    deletion_sequence,
                })?,
            )?;
            let mut stamps = record.stamps.clone();
            // Fields added by an application generation inherit the birth's ordering.
            for field in insert.change.after.iter().flat_map(|fields| fields.keys()) {
                stamps
                    .entry(*field)
                    .or_insert_with(|| proof.stamp().clone());
            }
            let author = self.row_authors.get(&destination.table).ok_or_else(|| {
                denied(&format!(
                    "{}: move requires a table pseudonym",
                    table.table.sql_name
                ))
            })?;
            for field in nulled {
                stamps.insert(
                    field,
                    Stamp {
                        clock: record
                            .stamps
                            .values()
                            .map(|stamp| stamp.clock)
                            .max()
                            .unwrap_or(0)
                            + 1,
                        author: author.key.public_key(),
                        sequence: 0,
                    },
                );
            }
            carried.insert(
                (destination, key),
                Carried {
                    proof,
                    stamps,
                    moved_from: Some(moved_from),
                },
            );
            writes.extend([delete, insert]);
        }
        let time = now()?;
        let mut scope_rows = BTreeMap::<ResourceScope, usize>::new();
        for write in &writes {
            let scope = write.change.scope;
            let table = typed
                .by_scope(scope)
                .ok_or_else(|| denied("move table missing"))?;
            let table_name = &table.table.sql_name;
            let author = self
                .row_authors
                .get(&scope.table)
                .ok_or_else(|| denied(&format!("{table_name}: move needs a table author")))?;
            let tuple = self
                .partitions
                .get(&scope)
                .ok_or_else(|| denied(&format!("{table_name}: move partition missing")))?;
            let grants = author
                .credentials
                .verify_partition_author(
                    self.config.publisher,
                    self.manifest,
                    author.key.public_key(),
                    author.number,
                    scope,
                    tuple,
                    &self.config.revocations,
                    time,
                )
                .map_err(|error| denied(&format!("{table_name}: {error}")))?;
            if !write.permitted_by(&grants) {
                return Err(denied(&format!(
                    "{table_name}: move needs delete in the source and insert of every destination field"
                )));
            }
            *scope_rows.entry(scope).or_default() += 1;
        }
        if scope_rows
            .values()
            .map(|rows| rows.div_ceil(super::bundle::ROWS_PER_FRAME))
            .sum::<usize>()
            > super::bundle::MAX_BUNDLE_FRAMES
        {
            return Err(denied(&format!("{name}: move exceeds bundle frame limit")));
        }
        if encode(&(&writes, &carried))?.len() > super::bundle::MAX_BUNDLE_BYTES {
            return Err(denied(&format!("{name}: move exceeds bundle byte limit")));
        }
        Box::pin(self.author_bundle_carried(handle, input, &writes, &[], now()?, &carried)).await
    }
}

fn scalar_bytes(values: &[SqlValue]) -> Vec<Vec<u8>> {
    values
        .iter()
        .map(|value| {
            let mut bytes = Vec::new();
            value.encode(&mut bytes);
            bytes
        })
        .collect()
}

impl<A: RowApplication> Inner<A> {
    fn open_move_source(
        &self,
        destination: ResourceScope,
        identity: i64,
        sealed: &[u8],
    ) -> Result<Option<MoveSource>, Error> {
        let aad = encode(&(destination, identity))?;
        for key in self.opening_keys() {
            if let Ok(bytes) = decrypt(&key.key, &aad, sealed) {
                let source: MoveSource = decode(&bytes)?;
                if source.scope != key.scope
                    || source.generation != key.generation
                    || source
                        .tuple
                        .scope(source.scope.domain, source.scope.table)
                        .ok()
                        != Some(source.scope)
                    || source.scope.table != destination.table
                {
                    return Err(denied("move source differs from authenticated key"));
                }
                return Ok(Some(source));
            }
        }
        Ok(None)
    }
    async fn move_finalized(&self, source: &MoveSource) -> Result<bool, Error> {
        let Some(id) = self.find_scope_id(source.scope).await? else {
            return Ok(false);
        };
        Ok(self
            .checkpoint_record(id, true)
            .await?
            .and_then(|checkpoint| checkpoint.signed)
            .and_then(|checkpoint| checkpoint.statement.finalized)
            .is_some_and(|horizon| {
                horizon.statement.logs.iter().any(|log| {
                    log.signer == source.deletion_author && log.sequence >= source.deletion_sequence
                })
            }))
    }
    pub(super) async fn follow_moves(&mut self, time: u64) -> Result<(), Error> {
        if self.row_authors.is_empty() || self.local_grants(time).is_err() {
            return Ok(());
        }
        let rows = collect_rows(
            self.conn
                .query("SELECT r.scope,r.row,r.fields FROM secure_move_rows m JOIN secure_rows r ON r.scope=m.scope AND r.row=m.row", ())
                .await?,
        )
        .await?;
        for row in rows {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(storage_row),
                SqlValue::Blob(bytes),
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("move scan".into()));
            };
            let destination: ResourceScope = decode(scope)?;
            let record: Materialized = decode(bytes)?;
            let Some(sealed) = record.moved_from.as_ref().filter(|_| !record.deleted) else {
                continue;
            };
            let Some(identity) = record.identity else {
                continue;
            };
            let Some(author) = self.row_authors.get(&destination.table) else {
                continue;
            };
            let signer = author.key.public_key();
            let Some(proof) = record.proof.clone() else {
                return Err(denied("moved row lacks creation proof"));
            };
            if !self.holds(destination).await?
                || !self.readable(destination).unwrap_or(false)
                || self.ensure_horizon_writable(destination).await.is_err()
            {
                continue;
            }
            let mut sources = Vec::new();
            let mut visited = BTreeSet::from([destination]);
            let mut link = self.open_move_source(destination, identity, sealed)?;
            while let Some(source) = link {
                if !visited.insert(source.scope) {
                    return Err(denied("cyclic move links"));
                }
                if self.move_finalized(&source).await? {
                    break;
                }
                link = match self
                    .row(
                        source.scope,
                        storage_row
                            .as_slice()
                            .try_into()
                            .map_err(|_| denied("invalid incarnation"))?,
                    )
                    .await?
                {
                    Some(row) => row
                        .moved_from
                        .as_ref()
                        .map(|sealed| self.open_move_source(source.scope, identity, sealed))
                        .transpose()?
                        .flatten(),
                    None => None,
                };
                sources.push(source);
            }
            let mut candidates = BTreeMap::<Commitment, (Stamp, Vec<u8>, Hash)>::new();
            for source in sources {
                let frames = collect_rows(self.conn.query("SELECT header,body FROM secure_operations WHERE scope=? AND author=? AND status='applied' ORDER BY sequence",
                    [encode(&source.scope)?, signer.as_bytes().to_vec()]).await?).await?;
                for row in frames {
                    let [SqlValue::Blob(header), SqlValue::Blob(body)] = row.as_slice() else {
                        return Err(Error::Corrupt("own updates".into()));
                    };
                    let frame = EncryptedFrame {
                        header: header.clone(),
                        body: body.clone(),
                    };
                    let operation = frame.operation()?;
                    let payload = self.row_payload(&frame)?;
                    for write in payload.clone().into_writes() {
                        if write.change.before.is_none()
                            || write.change.after.is_none()
                            || self
                                .app
                                .row_identity(&write.change)
                                .map_err(|error| denied(&error))?
                                .map(|id| id.integer())
                                != Some(identity)
                        {
                            continue;
                        }
                        let default_stamp = Stamp {
                            clock: operation.header.timestamp,
                            author: signer,
                            sequence: operation.header.seq_num,
                        };
                        for field in write.published_fields() {
                            let Some(value) = write
                                .change
                                .after
                                .as_ref()
                                .and_then(|fields| fields.get(&field))
                            else {
                                continue;
                            };
                            let stamp = payload
                                .carried(&write.change.row)
                                .and_then(|carried| carried.stamps.get(&field))
                                .unwrap_or(&default_stamp);
                            if stamp.author != signer
                                || record.stamps.get(&field).is_some_and(|old| old >= stamp)
                            {
                                continue;
                            }
                            if candidates.get(&field).is_none_or(|(old, _, _)| old < stamp) {
                                candidates
                                    .insert(field, (stamp.clone(), value.clone(), operation.hash));
                            }
                        }
                    }
                }
            }
            if candidates.is_empty() {
                continue;
            }
            // A followed stamp must also remain mergeable at B's finalized cut.
            if let Some(id) = self.find_scope_id(destination).await?
                && let Some(checkpoint) = self
                    .checkpoint_record(id, true)
                    .await?
                    .and_then(|checkpoint| checkpoint.signed)
                && checkpoint.statement.finalized.is_some()
            {
                candidates.retain(|_, (stamp, _, _)| stamp.clock > checkpoint.statement.clock);
            }
            let mut after = record.fields.clone();
            let mut stamps = BTreeMap::new();
            let mut originals = Vec::new();
            for (field, (stamp, value, original)) in candidates {
                let seen = collect_rows(self.conn.query("SELECT 1 FROM secure_followed WHERE original=? AND scope=? AND field=?",
                    [original.as_bytes().to_vec(), scope.clone(), field.to_vec()]).await?).await?;
                if !seen.is_empty() {
                    continue;
                }
                stamps.insert(field, stamp);
                after.insert(field, value);
                originals.push((original, field));
            }
            if stamps.is_empty() {
                continue;
            }
            let change = RowChange {
                scope: destination,
                row: record.key.ok_or_else(|| denied("moved row key missing"))?,
                before: Some(record.fields.clone()),
                after: Some(after),
            };
            let write = RowWrite {
                change,
                assigned: stamps.keys().copied().collect(),
            };
            let author = self
                .row_authors
                .get(&destination.table)
                .ok_or_else(|| denied("follow author missing"))?;
            let tuple = self
                .partitions
                .get(&destination)
                .ok_or_else(|| denied("follow partition missing"))?;
            let grants = author.credentials.verify_partition_author(
                self.config.publisher,
                self.manifest,
                signer,
                author.number,
                destination,
                tuple,
                &self.config.revocations,
                time,
            );
            let Ok(grants) = grants else {
                continue;
            };
            if !write.permitted_by(&grants) {
                continue;
            }
            let carried = BTreeMap::from([(
                (destination, write.change.row),
                Carried {
                    proof,
                    stamps,
                    moved_from: None,
                },
            )]);
            let input = rocky_core::commitment(
                "rocky.follow-input.v1",
                &encode(&(&originals, destination, &write, &carried))?,
            );
            let handle = rocky_core::commitment(
                "rocky.follow-handle.v1",
                &encode(&(&originals, destination))?,
            );
            self.conn.execute("SAVEPOINT follow", ()).await?;
            let authored =
                Box::pin(self.author_bundle_carried(handle, input, &[write], &[], time, &carried))
                    .await;
            if let Err(error) = authored {
                self.conn.execute("ROLLBACK TO follow", ()).await?;
                self.conn.execute("RELEASE follow", ()).await?;
                return Err(error);
            }
            for (original, field) in originals {
                self.conn
                    .execute(
                        "INSERT OR IGNORE INTO secure_followed VALUES(?,?,?)",
                        [original.as_bytes().to_vec(), scope.clone(), field.to_vec()],
                    )
                    .await?;
            }
            self.conn.execute("RELEASE follow", ()).await?;
        }
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn checkpoint_moves(
        &self,
        scope: i64,
    ) -> Result<checkpoint::SetSummary, Error> {
        self.move_summary("SELECT row,fields FROM secure_checkpoint_rows WHERE scope=?1 AND row NOT IN(SELECT row FROM secure_checkpoint_overlay WHERE scope=?1) UNION ALL SELECT row,fields FROM secure_checkpoint_overlay WHERE scope=?1", scope).await
    }
    pub(super) async fn rebootstrap_moves(
        &self,
        scope: i64,
    ) -> Result<checkpoint::SetSummary, Error> {
        self.move_summary(
            "SELECT row,fields FROM secure_rebootstrap_rows WHERE scope=?",
            scope,
        )
        .await
    }
    pub(super) async fn reindex_move_scope(&self, scope: ResourceScope) -> Result<(), Error> {
        let blob = encode(&scope)?;
        self.conn
            .execute("DELETE FROM secure_move_rows WHERE scope=?", [blob.clone()])
            .await?;
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT row,fields FROM secure_rows WHERE scope=?",
                    [blob.clone()],
                )
                .await?,
        )
        .await?;
        for values in records {
            let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = values.as_slice() else {
                return Err(Error::Corrupt("move index row".into()));
            };
            let record: Materialized = decode(bytes)?;
            if record.moved_from.is_some() && !record.deleted {
                self.conn
                    .execute(
                        "INSERT INTO secure_move_rows VALUES(?,?)",
                        [blob.clone(), row.clone()],
                    )
                    .await?;
            }
        }
        Ok(())
    }
    async fn move_summary(&self, sql: &str, scope: i64) -> Result<checkpoint::SetSummary, Error> {
        let mut rows = self.conn.query(sql, [scope]).await?;
        let mut summary = checkpoint::SetSummary {
            count: 0,
            digest: [0; 32],
        };
        while let Some(row) = rows.next().await? {
            let id: Vec<u8> = row.get(0)?;
            let bytes: Vec<u8> = row.get(1)?;
            let row: Materialized = decode(&bytes)?;
            if let Some(link) = row.moved_from {
                summary.count += 1;
                let hash =
                    rocky_core::commitment("rocky.checkpoint.move.v1", &encode(&(id, link))?);
                for (digest, byte) in summary.digest.iter_mut().zip(hash) {
                    *digest ^= byte;
                }
            }
        }
        Ok(summary)
    }
}

#[cfg(test)]
impl<A: RowApplication> Inner<A> {
    pub(crate) fn move_authors(&self) -> Vec<(PrivateKey, CredentialChain)> {
        self.row_authors
            .values()
            .map(|author| (author.key.clone(), author.credentials.clone()))
            .collect()
    }
}
