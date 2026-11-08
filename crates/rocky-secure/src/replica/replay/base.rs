//! A replayed scope's rows at a seal: its replay base. Replayed tables keep
//! no row records, so a checkpoint of one carries `(row id, image)` pairs
//! committed by an LtHash, and a rebuild starts from them. An image keys
//! each value by its field id, so it outlives the seal's schema. Stored
//! images name each field id by its ordinal in `secure_replayed_fields`;
//! digests and pages use the full image.
use super::*;
use crate::replica::checkpoint::Sets;

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // Rows at the newest finalized, uncompacted seals; `secure_replayed_seals`
    // marks the seals a scope's rows were taken at, empty ones included,
    // with the seal's encoded `SealedOutcomes`. They stay while the seal
    // leaves the path and comes back.
    "CREATE TABLE IF NOT EXISTS secure_replayed_seals(scope INTEGER NOT NULL,seal BLOB NOT NULL,outcomes BLOB NOT NULL,version INTEGER NOT NULL,PRIMARY KEY(scope,seal)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_replayed_snapshot_clock(id INTEGER PRIMARY KEY CHECK(id=1),version INTEGER NOT NULL)",
    "INSERT OR IGNORE INTO secure_replayed_snapshot_clock VALUES(1,0)",
    "CREATE TABLE IF NOT EXISTS secure_replayed_snapshots(scope INTEGER NOT NULL,seal BLOB NOT NULL,version INTEGER NOT NULL,PRIMARY KEY(scope,seal,version)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_replayed_seal_rows(scope INTEGER NOT NULL,seal BLOB NOT NULL,version INTEGER NOT NULL,row BLOB NOT NULL,image BLOB NOT NULL,PRIMARY KEY(scope,seal,version,row)) WITHOUT ROWID",
    // Rows at the scope's checkpoint, and those of an import in progress.
    "CREATE TABLE IF NOT EXISTS secure_replayed_base_rows(scope INTEGER NOT NULL,row BLOB NOT NULL,image BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    // A locally compacted base borrows immutable seal rows. Imported bases
    // own their rows instead. The selector changes with the signed checkpoint.
    "CREATE TABLE IF NOT EXISTS secure_replayed_base_source(scope INTEGER PRIMARY KEY,seal BLOB NOT NULL,version INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE VIEW IF NOT EXISTS secure_replayed_base AS SELECT r.scope,r.row,r.image FROM secure_replayed_base_rows r WHERE NOT EXISTS(SELECT 1 FROM secure_replayed_base_source s WHERE s.scope=r.scope) UNION ALL SELECT r.scope,r.row,r.image FROM secure_replayed_base_source s JOIN secure_replayed_seal_rows r ON r.scope=s.scope AND r.seal=s.seal AND r.version=s.version",
    "CREATE TRIGGER IF NOT EXISTS secure_replayed_base_delete INSTEAD OF DELETE ON secure_replayed_base BEGIN DELETE FROM secure_replayed_base_rows WHERE scope=OLD.scope AND row=OLD.row; DELETE FROM secure_replayed_seal_rows WHERE scope=OLD.scope AND row=OLD.row AND (seal,version)=(SELECT seal,version FROM secure_replayed_base_source WHERE scope=OLD.scope); END",
    "CREATE TABLE IF NOT EXISTS secure_replayed_staged(scope INTEGER NOT NULL,row BLOB NOT NULL,image BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    // The field ids stored images name; never deleted.
    "CREATE TABLE IF NOT EXISTS secure_replayed_fields(ordinal INTEGER PRIMARY KEY,field BLOB NOT NULL UNIQUE)",
    // While rows are taken at a seal: per lowercase table and encoded key,
    // the row's image before the earliest open member that changed it, and
    // that member's order; no image when the member created it.
    "CREATE TABLE IF NOT EXISTS secure_seal_undone(tbl TEXT NOT NULL,key BLOB NOT NULL,era INTEGER NOT NULL,clock INTEGER NOT NULL,author INTEGER NOT NULL,sequence INTEGER NOT NULL,image BLOB,PRIMARY KEY(tbl,key)) WITHOUT ROWID",
];

const ROW_LABEL: &str = "rocky.replayed-row.v1";

/// Each field id followed by its encoded value, in field order.
fn fields_blob(fields: &RowFields) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (field, value) in fields {
        bytes.extend_from_slice(field);
        bytes.extend_from_slice(value);
    }
    bytes
}

fn blob_fields(bytes: &[u8]) -> Result<RowFields, Error> {
    let mut fields = RowFields::new();
    for (field, value) in image_fields(bytes)? {
        if fields.insert(*field, value.to_vec()).is_some() {
            return Err(Error::Corrupt("replayed base image".into()));
        }
    }
    Ok(fields)
}

/// One field id of an image with its encoded value.
type ImageField<'a> = (&'a [u8; 32], &'a [u8]);

/// Each field id of an image with its encoded value.
fn image_fields(mut bytes: &[u8]) -> Result<Vec<ImageField<'_>>, Error> {
    let corrupt = || Error::Corrupt("replayed base image".into());
    let mut fields = Vec::new();
    while !bytes.is_empty() {
        let (field, rest) = bytes.split_first_chunk::<32>().ok_or_else(corrupt)?;
        let (value, rest) = split_value(rest).ok_or_else(corrupt)?;
        fields.push((field, value));
        bytes = rest;
    }
    Ok(fields)
}

/// One encoded value and what follows it.
fn split_value(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut rest = bytes;
    SqlValue::decode(&mut rest).ok()?;
    Some(bytes.split_at(bytes.len() - rest.len()))
}

/// The encoded primary key of a row image.
fn key_bytes(key: &[usize], image: &[SqlValue]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for index in key {
        if let Some(value) = image.get(*index) {
            value.encode(&mut bytes);
        }
    }
    bytes
}

/// The field ids `images` name.
pub(in crate::replica) fn fields_of<'a>(
    images: impl IntoIterator<Item = &'a [u8]>,
) -> Result<BTreeSet<[u8; 32]>, Error> {
    let mut fields = BTreeSet::new();
    for image in images {
        for (field, _) in image_fields(image)? {
            fields.insert(*field);
        }
    }
    Ok(fields)
}

/// The field dictionary of stored images, both ways.
#[derive(Default)]
pub(in crate::replica) struct FieldOrdinals {
    ordinals: BTreeMap<[u8; 32], u64>,
    fields: BTreeMap<u64, [u8; 32]>,
}

impl FieldOrdinals {
    /// `image` as stored: each field's ordinal as a LEB128 varint, then its
    /// value.
    pub(in crate::replica) fn pack(&self, image: &[u8]) -> Result<Vec<u8>, Error> {
        let mut packed = Vec::with_capacity(image.len() / 4);
        for (field, value) in image_fields(image)? {
            let mut ordinal = *self
                .ordinals
                .get(field)
                .ok_or_else(|| Error::Corrupt("replayed field ordinal".into()))?;
            loop {
                let byte = (ordinal & 0x7f) as u8;
                ordinal >>= 7;
                if ordinal == 0 {
                    packed.push(byte);
                    break;
                }
                packed.push(byte | 0x80);
            }
            packed.extend_from_slice(value);
        }
        Ok(packed)
    }
    /// The full image of a stored one.
    pub(in crate::replica) fn unpack(&self, packed: &[u8]) -> Result<Vec<u8>, Error> {
        let mut image = Vec::with_capacity(packed.len() * 4);
        for (ordinal, value) in packed_fields(packed)? {
            image.extend_from_slice(
                self.fields
                    .get(&ordinal)
                    .ok_or_else(|| Error::Corrupt("replayed stored image".into()))?,
            );
            image.extend_from_slice(value);
        }
        Ok(image)
    }
}

fn packed_fields(mut packed: &[u8]) -> Result<Vec<(u64, &[u8])>, Error> {
    let corrupt = || Error::Corrupt("replayed stored image".into());
    let mut fields = Vec::new();
    while !packed.is_empty() {
        let mut ordinal = 0u64;
        let mut shift = 0;
        loop {
            let (&byte, rest) = packed.split_first().ok_or_else(corrupt)?;
            packed = rest;
            if shift > 56 {
                return Err(corrupt());
            }
            ordinal |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        let (value, rest) = split_value(packed).ok_or_else(corrupt)?;
        fields.push((ordinal, value));
        packed = rest;
    }
    Ok(fields)
}

/// Stored `(row, packed image)` records with their full images.
fn unpack_records(
    records: Vec<Vec<SqlValue>>,
    ordinals: &FieldOrdinals,
) -> Result<Vec<(Commitment, Vec<u8>)>, Error> {
    records
        .into_iter()
        .map(|record| match record.as_slice() {
            [SqlValue::Blob(row), SqlValue::Blob(image)] => {
                Ok((checkpoint::row_of(row)?, ordinals.unpack(image)?))
            }
            _ => Err(Error::Corrupt("replayed base row".into())),
        })
        .collect()
}

/// Adds one base row to `sets`.
pub(in crate::replica) fn add_row(sets: &mut Sets, row: &Commitment, image: &[u8]) {
    sets.rows += 1;
    sets.digest
        .add(ROW_LABEL, &[row.as_slice(), image].concat());
}

/// The summary of `rows`, as a checkpoint statement commits it.
pub(in crate::replica) fn base_sets(rows: &[(Commitment, Vec<u8>)]) -> Sets {
    let mut sets = Sets::new();
    for (row, image) in rows {
        add_row(&mut sets, row, image);
    }
    sets
}

impl<A: RowApplication> Inner<A> {
    /// Only the dictionary entries referenced by a bounded chunk of stored images.
    pub(in crate::replica) async fn chunk_field_ordinals<'a>(
        &self,
        images: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<FieldOrdinals, Error> {
        let mut needed = BTreeSet::new();
        for image in images {
            needed.extend(
                packed_fields(image)?
                    .into_iter()
                    .map(|(ordinal, _)| ordinal),
            );
        }
        let mut ordinals = FieldOrdinals::default();
        for ordinal in needed {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT field FROM secure_replayed_fields WHERE ordinal=?",
                        [ordinal as i64],
                    )
                    .await?,
            )
            .await?;
            let Some([SqlValue::Blob(field)]) = records.first().map(Vec::as_slice) else {
                return Err(Error::Corrupt("replayed field ordinal".into()));
            };
            ordinals.fields.insert(ordinal, checkpoint::row_of(field)?);
        }
        #[cfg(any(test, feature = "qualification"))]
        self.flusher.measurements.work(
            lock::WorkUnit::TransitionDictionaryField,
            ordinals.fields.len() as u64,
        );
        Ok(ordinals)
    }
    /// The stored images' field dictionary, with every field of `fields`.
    pub(in crate::replica) async fn field_ordinals(
        &self,
        fields: BTreeSet<[u8; 32]>,
    ) -> Result<FieldOrdinals, Error> {
        let ordinals = self.stored_field_ordinals().await?;
        let missing: Vec<_> = fields
            .into_iter()
            .filter(|field| !ordinals.ordinals.contains_key(field))
            .collect();
        if missing.is_empty() {
            return Ok(ordinals);
        }
        for field in missing {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_replayed_fields(field) VALUES(?)",
                    [field.to_vec()],
                )
                .await?;
        }
        self.stored_field_ordinals().await
    }
    /// The stored images' field dictionary.
    pub(in crate::replica) async fn stored_field_ordinals(&self) -> Result<FieldOrdinals, Error> {
        let mut ordinals = FieldOrdinals::default();
        for record in rows(
            &self.conn,
            "SELECT ordinal,field FROM secure_replayed_fields",
        )
        .await?
        {
            let [SqlValue::Integer(ordinal), SqlValue::Blob(field)] = record.as_slice() else {
                return Err(Error::Corrupt("replayed field ordinal".into()));
            };
            let ordinal = u64::try_from(*ordinal)
                .map_err(|_| Error::Corrupt("replayed field ordinal".into()))?;
            let field: [u8; 32] = field
                .as_slice()
                .try_into()
                .map_err(|_| Error::Corrupt("replayed field ordinal".into()))?;
            ordinals.ordinals.insert(field, ordinal);
            ordinals.fields.insert(ordinal, field);
        }
        Ok(ordinals)
    }
    /// The live rows of replayed scope `scope`, by row id.
    pub(in crate::replica) async fn replayed_images(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<(Commitment, Vec<u8>)>, Error> {
        self.scanned_images(scope, None)
            .await?
            .ok_or_else(|| Error::Corrupt("replayed scope".into()))
    }
    /// The rows of replayed scope `scope` at kept `seal`, marked `mark`,
    /// by row id; none when the seal kept its table under other columns.
    pub(in crate::replica) async fn kept_images(
        &self,
        scope: ResourceScope,
        kept: (Commitment, i64),
    ) -> Result<Option<Vec<(Commitment, Vec<u8>)>>, Error> {
        self.scanned_images(scope, Some(kept)).await
    }
    /// The rows of `scope` from one scan of its table, live or at `kept`.
    async fn scanned_images(
        &self,
        scope: ResourceScope,
        kept: Option<(Commitment, i64)>,
    ) -> Result<Option<Vec<(Commitment, Vec<u8>)>>, Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        let table = typed
            .plan(&BTreeSet::from([scope]))?
            .tables
            .into_iter()
            .find(|table| table.name.eq_ignore_ascii_case(&projected.table.sql_name))
            .ok_or_else(corrupt)?;
        let scan = match kept {
            None => table.scan.clone(),
            Some(kept) => {
                let logical = typed
                    .rows
                    .schema()
                    .table(&projected.table.sql_name)
                    .ok_or_else(corrupt)?;
                let columns: Vec<String> =
                    logical.columns.iter().map(|c| c.sql_name.clone()).collect();
                match self.kept_scan(kept, &logical.sql_name, &columns).await? {
                    Some(scan) => scan,
                    None => return Ok(None),
                }
            }
        };
        let records = self
            .conn
            .connection()
            .rows(&scan, Vec::new())
            .map_err(Error::from)?;
        #[cfg(test)]
        crate::replica::held_at_once(records.len());
        let mut rows = Vec::new();
        for values in records {
            let image = table
                .image(&values)
                .map_err(|_| Error::Corrupt("replayed row image".into()))?;
            if typed.row_scope(projected, &image, &self.partitions)? != scope {
                continue;
            }
            let row = typed
                .rows
                .row_id(typed.domain.domain, &projected.table, &image)
                .map_err(|_| Error::Corrupt("replayed row key".into()))?;
            let fields = typed
                .rows
                .fields(&projected.table, &image)
                .map_err(|_| Error::Corrupt("replayed row image".into()))?;
            rows.push((row, fields_blob(&fields)));
        }
        rows.sort_unstable();
        Ok(Some(rows))
    }

    /// Takes the rows of every replayed scope of `scopes` at `seal`, just
    /// finalized at `depth`: the live rows with every member after the seal
    /// undone, and the seal's outcomes. A scope the seal leaves undecided
    /// (`leaves_open`) takes none. Rows move a page at a time, so the
    /// table and the open members' undo never sit in memory whole.
    pub(in crate::replica) async fn take_seal_rows(
        &self,
        seal: Commitment,
        depth: i64,
        scopes: &[(ResourceScope, i64)],
    ) -> Result<(), Error> {
        let mut decided = Vec::new();
        for (scope, id) in scopes {
            if self.replayed_scope(*scope)
                && self.has_scope_key(*scope)
                && !self.leaves_open(*id, depth).await?
            {
                decided.push((*scope, *id));
            }
        }
        if decided.is_empty() {
            return Ok(());
        }
        // Each scope's snapshot header first: its rows name its version.
        let mut versions = BTreeMap::new();
        for (scope, id) in &decided {
            let outcomes = self.sealed_outcomes(*id).await?;
            self.conn
                .execute(
                    "UPDATE secure_replayed_snapshot_clock SET version=version+1 WHERE id=1",
                    (),
                )
                .await?;
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_replayed_seals SELECT ?,?,?,version FROM secure_replayed_snapshot_clock WHERE id=1",
                    bindings(vec![
                        SqlValue::Integer(*id),
                        SqlValue::Blob(seal.to_vec()),
                        SqlValue::Blob(encode(&outcomes)?),
                    ]),
                )
                .await?;
            self.conn.execute(
                "INSERT INTO secure_replayed_snapshots SELECT scope,seal,version FROM secure_replayed_seals WHERE scope=? AND seal=?",
                bindings(vec![SqlValue::Integer(*id), SqlValue::Blob(seal.to_vec())]),
            ).await?;
            let version = match rows(
                &self.conn,
                "SELECT version FROM secure_replayed_snapshot_clock WHERE id=1",
            )
            .await?
            .first()
            .map(Vec::as_slice)
            {
                Some([SqlValue::Integer(version)]) => *version,
                _ => return Err(Error::Corrupt("replayed snapshot clock".into())),
            };
            versions.insert(*scope, (*id, version));
        }
        self.undo_open_members().await?;
        let mut tables: BTreeMap<_, Vec<ResourceScope>> = BTreeMap::new();
        for (scope, _) in &decided {
            tables.entry(scope.table).or_default().push(*scope);
        }
        for table in tables.values() {
            self.take_table_rows(seal, table, &versions).await?;
        }
        self.conn
            .execute("DELETE FROM secure_seal_undone", ())
            .await?;
        Ok(())
    }
    /// Fills `secure_seal_undone`, a page of open members at a time: per
    /// row an open member changed, its image before the earliest of them.
    /// The seal leaves none of the members it covers open, so these all
    /// follow it.
    async fn undo_open_members(&self) -> Result<(), Error> {
        let mut after = None;
        loop {
            let (filter, mut params) = match &after {
                Some(after) => (
                    "AND (scope,era,clock,author,sequence)>(?,?,?,?,?)",
                    Vec::clone(after),
                ),
                None => ("", Vec::new()),
            };
            params.push(SqlValue::Integer(checkpoint::STEP_OPERATIONS as i64));
            let records = collect_rows(
                self.conn
                    .query(
                        format!(
                            "SELECT scope,era,clock,author,sequence,undo FROM secure_replayed WHERE sealed=0 AND undo IS NOT NULL {filter} ORDER BY scope,era,clock,author,sequence LIMIT ?"
                        ),
                        bindings(params),
                    )
                    .await?,
            )
            .await?;
            #[cfg(test)]
            crate::replica::held_at_once(records.len());
            let Some(last) = records.last() else {
                return Ok(());
            };
            let order = last
                .get(..5)
                .ok_or_else(|| Error::Corrupt("replayed member".into()))?;
            after = Some(order.to_vec());
            for record in &records {
                let [_, era, clock, author, sequence, SqlValue::Blob(undo)] = record.as_slice()
                else {
                    return Err(Error::Corrupt("replayed member".into()));
                };
                for undo in super::undo_of(undo)? {
                    // The earliest member's image wins, whatever the page order.
                    self.conn
                        .execute(
                            "INSERT INTO secure_seal_undone VALUES(?,?,?,?,?,?,?) ON CONFLICT(tbl,key) DO UPDATE SET era=excluded.era,clock=excluded.clock,author=excluded.author,sequence=excluded.sequence,image=excluded.image WHERE (excluded.era,excluded.clock,excluded.author,excluded.sequence)<(era,clock,author,sequence)",
                            bindings(vec![
                                SqlValue::Text(undo.table.to_ascii_lowercase()),
                                SqlValue::Blob(super::values_blob(&undo.key)),
                                era.clone(),
                                clock.clone(),
                                author.clone(),
                                sequence.clone(),
                                undo.before
                                    .as_deref()
                                    .map_or(SqlValue::Null, |before| {
                                        SqlValue::Blob(super::values_blob(before))
                                    }),
                            ]),
                        )
                        .await?;
                }
            }
        }
    }
    /// Takes the rows of `scopes`, partitions of one table, at `seal`: its
    /// live rows a page at a time, each one an open member changed replaced
    /// by its image in `secure_seal_undone`.
    async fn take_table_rows(
        &self,
        seal: Commitment,
        scopes: &[ResourceScope],
        versions: &BTreeMap<ResourceScope, (i64, i64)>,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let first = *scopes.first().ok_or_else(corrupt)?;
        let projected = typed.by_scope(first).ok_or_else(corrupt)?;
        let table = typed
            .plan(&scopes.iter().copied().collect())?
            .tables
            .into_iter()
            .find(|table| table.name.eq_ignore_ascii_case(&projected.table.sql_name))
            .ok_or_else(corrupt)?;
        let logical = typed
            .rows
            .schema()
            .table(&projected.table.sql_name)
            .ok_or_else(corrupt)?;
        let name = table.name.to_ascii_lowercase();
        let columns: Vec<String> = logical
            .columns
            .iter()
            .map(|column| typed::quoted(&column.sql_name))
            .collect();
        // Pages follow the rowid, or the primary key of a table without
        // one, which SQLite keeps NOT NULL.
        let without_rowid = crate::scoped_connection::tables(&[&logical.create_sql])
            .first()
            .is_some_and(|(_, without_rowid)| *without_rowid);
        let cursor: Vec<String> = match without_rowid {
            true => table
                .key
                .iter()
                .map(|key| columns.get(*key).cloned().ok_or_else(corrupt))
                .collect::<Result<_, _>>()?,
            false => vec!["rowid".into()],
        };
        if cursor.is_empty() {
            return Err(corrupt());
        }
        let select = format!(
            "SELECT {},{} FROM {}",
            columns.join(","),
            cursor.join(","),
            typed::quoted(&table.physical)
        );
        let order = format!(
            "ORDER BY {} LIMIT {}",
            cursor.join(","),
            checkpoint::STEP_ROWS
        );
        let next = format!(
            "{select} WHERE ({})>({}) {order}",
            cursor.join(","),
            vec!["?"; cursor.len()].join(",")
        );
        let first = format!("{select} {order}");
        let mut after: Option<Vec<crate::sqlite::Value>> = None;
        loop {
            let records = match &after {
                Some(after) => self.conn.connection().rows(&next, after.clone()),
                None => self.conn.connection().rows(&first, Vec::new()),
            }
            .map_err(Error::from)?;
            #[cfg(test)]
            crate::replica::held_at_once(records.len());
            let Some(last) = records.last() else {
                break;
            };
            after = Some(last.get(columns.len()..).ok_or_else(corrupt)?.to_vec());
            let mut images = Vec::with_capacity(records.len());
            for values in &records {
                let image = table
                    .image(values.get(..columns.len()).ok_or_else(corrupt)?)
                    .map_err(|_| Error::Corrupt("replayed row image".into()))?;
                let undone = collect_rows(
                    self.conn
                        .query(
                            "SELECT 1 FROM secure_seal_undone WHERE tbl=? AND key=?",
                            bindings(vec![
                                SqlValue::Text(name.clone()),
                                SqlValue::Blob(key_bytes(&table.key, &image)),
                            ]),
                        )
                        .await?,
                )
                .await?;
                if undone.is_empty() {
                    images.push(image);
                }
            }
            self.store_seal_images(seal, projected, images, versions)
                .await?;
        }
        // Then each row as open members found it, in key order.
        let mut after = Vec::new();
        loop {
            let records = collect_rows(
                self.conn
                    .query(
                        "SELECT key,image FROM secure_seal_undone WHERE tbl=? AND key>? AND image IS NOT NULL ORDER BY key LIMIT ?",
                        bindings(vec![
                            SqlValue::Text(name.clone()),
                            SqlValue::Blob(after.clone()),
                            SqlValue::Integer(checkpoint::STEP_ROWS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            #[cfg(test)]
            crate::replica::held_at_once(records.len());
            if records.is_empty() {
                return Ok(());
            }
            let mut images = Vec::with_capacity(records.len());
            for record in records {
                let [SqlValue::Blob(key), SqlValue::Blob(image)] = record.as_slice() else {
                    return Err(Error::Corrupt("replayed member".into()));
                };
                after.clone_from(key);
                images.push(super::blob_values(image)?);
            }
            self.store_seal_images(seal, projected, images, versions)
                .await?;
        }
    }
    /// Stores `images` of `projected`'s table at `seal` under the snapshot
    /// version of their scope; rows of scopes not taken are skipped.
    async fn store_seal_images(
        &self,
        seal: Commitment,
        projected: &typed::Projected,
        images: Vec<Vec<SqlValue>>,
        versions: &BTreeMap<ResourceScope, (i64, i64)>,
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let mut taken = Vec::with_capacity(images.len());
        for image in images {
            let Some((id, version)) =
                versions.get(&typed.row_scope(projected, &image, &self.partitions)?)
            else {
                continue;
            };
            let row = typed
                .rows
                .row_id(typed.domain.domain, &projected.table, &image)
                .map_err(|_| Error::Corrupt("replayed row key".into()))?;
            let fields = typed
                .rows
                .fields(&projected.table, &image)
                .map_err(|_| Error::Corrupt("replayed row image".into()))?;
            taken.push((*id, *version, row, fields_blob(&fields)));
        }
        let ordinals = self
            .field_ordinals(fields_of(
                taken.iter().map(|(_, _, _, image)| image.as_slice()),
            )?)
            .await?;
        for (id, version, row, image) in taken {
            self.conn
                .execute(
                    "INSERT INTO secure_replayed_seal_rows VALUES(?,?,?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(seal.to_vec()),
                        SqlValue::Integer(version),
                        SqlValue::Blob(row.to_vec()),
                        SqlValue::Blob(ordinals.pack(&image)?),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    /// Without compaction, keeps scope `id` the rows of only the newest
    /// `SEAL_SNAPSHOTS` seals of `domain` finalized through `depth`, besides
    /// those its base, a base in preparation or a kept seal names.
    pub(in crate::replica) async fn release_seal_rows(
        &self,
        domain: rocky_core::Scope,
        id: i64,
        depth: i64,
    ) -> Result<(), Error> {
        // Headers first: no reader takes a partly released snapshot.
        for table in [
            "secure_replayed_seals",
            "secure_replayed_snapshots",
            "secure_replayed_seal_rows",
        ] {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE scope=?1 AND seal IN(SELECT seal FROM secure_finalized WHERE domain=?2 AND compacted=0 AND depth<=?3) AND seal NOT IN(SELECT seal FROM secure_replayed_base_source WHERE scope=?1) AND seal NOT IN(SELECT seal FROM secure_base_transition WHERE scope=?1) AND seal NOT IN(SELECT seal FROM secure_kept_marks)"),
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(encode(&domain)?),
                        SqlValue::Integer(depth - super::SEAL_SNAPSHOTS),
                    ]),
                )
                .await?;
        }
        Ok(())
    }

    /// The rows taken at `seal`; none when they were not taken.
    pub(in crate::replica) async fn seal_rows(
        &self,
        id: i64,
        seal: Commitment,
    ) -> Result<Option<Vec<(Commitment, Vec<u8>)>>, Error> {
        if collect_rows(
            self.conn
                .query(
                    "SELECT 1 FROM secure_replayed_seals WHERE scope=? AND seal=?",
                    bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())]),
                )
                .await?,
        )
        .await?
        .is_empty()
        {
            return Ok(None);
        }
        self.stored_images("secure_replayed_seal_rows", id, Some(seal))
            .await
            .map(Some)
    }
    /// The outcomes taken at `seal`; none when they were not taken.
    pub(in crate::replica) async fn seal_outcomes(
        &self,
        id: i64,
        seal: Commitment,
    ) -> Result<Option<crate::replica::checkpoint::SealedOutcomes>, Error> {
        match collect_rows(
            self.conn
                .query(
                    "SELECT outcomes FROM secure_replayed_seals WHERE scope=? AND seal=?",
                    bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())]),
                )
                .await?,
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some([SqlValue::Blob(outcomes)]) => {
                let outcomes: checkpoint::SealedOutcomes = decode(outcomes)?;
                outcomes
                    .check()
                    .map_err(|reason| Error::Corrupt(reason.into()))?;
                #[cfg(any(test, feature = "qualification"))]
                self.flusher.measurements.work(
                    lock::WorkUnit::TransitionOutcomeRecord,
                    outcomes.listed.len() as u64,
                );
                Ok(Some(outcomes))
            }
            Some(_) => Err(Error::Corrupt("sealed outcomes".into())),
            None => Ok(None),
        }
    }
    /// The rows of `table`, `secure_replayed_base` or
    /// `secure_replayed_staged`, or of a seal, ordered by row id.
    pub(in crate::replica) async fn stored_images(
        &self,
        table: &str,
        id: i64,
        seal: Option<Commitment>,
    ) -> Result<Vec<(Commitment, Vec<u8>)>, Error> {
        let (sql, params) = match seal {
            Some(seal) => (
                format!(
                    "SELECT row,image FROM {table} WHERE scope=?1 AND seal=?2 AND version=(SELECT version FROM secure_replayed_seals WHERE scope=?1 AND seal=?2) ORDER BY row"
                ),
                vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())],
            ),
            None => (
                format!("SELECT row,image FROM {table} WHERE scope=? ORDER BY row"),
                vec![SqlValue::Integer(id)],
            ),
        };
        let ordinals = self.stored_field_ordinals().await?;
        let records = collect_rows(self.conn.query(sql, bindings(params)).await?).await?;
        #[cfg(test)]
        crate::replica::held_at_once(records.len());
        unpack_records(records, &ordinals)
    }
    /// Up to `STEP_ROWS` rows of scope `id` in `table`, as `stored_images`
    /// without a seal, after row `after`.
    pub(in crate::replica) async fn stored_page(
        &self,
        table: &str,
        id: i64,
        after: Option<Commitment>,
        ordinals: &FieldOrdinals,
    ) -> Result<Vec<(Commitment, Vec<u8>)>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    format!(
                        "SELECT row,image FROM {table} WHERE scope=? AND row>? ORDER BY row LIMIT ?"
                    ),
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(after.map(|row| row.to_vec()).unwrap_or_default()),
                        SqlValue::Integer(checkpoint::STEP_ROWS as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        #[cfg(test)]
        crate::replica::held_at_once(records.len());
        unpack_records(records, ordinals)
    }
    /// Makes `rows` the scope's replay base and forgets the seal rows of
    /// seals not finalized after `depth`.
    pub(in crate::replica) async fn store_base(
        &self,
        id: i64,
        rows: &[(Commitment, Vec<u8>)],
        depth: Option<i64>,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM secure_replayed_base_source WHERE scope=?",
                [id],
            )
            .await?;
        self.conn
            .execute("DELETE FROM secure_base_transition WHERE scope=?", [id])
            .await?;
        self.conn
            .execute("DELETE FROM secure_replayed_base_rows WHERE scope=?", [id])
            .await?;
        let fields = fields_of(rows.iter().map(|(_, image)| image.as_slice()))?;
        let ordinals = self.field_ordinals(fields).await?;
        for (row, image) in rows {
            self.conn
                .execute(
                    "INSERT INTO secure_replayed_base_rows VALUES(?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(row.to_vec()),
                        SqlValue::Blob(ordinals.pack(image)?),
                    ]),
                )
                .await?;
        }
        self.forget_seal_rows(id, depth.unwrap_or(i64::MAX)).await
    }
    /// `store_base` of scope `id`'s staged rows, copied as stored: both
    /// tables pack images with the same dictionary.
    pub(in crate::replica) async fn store_staged_base(&self, id: i64) -> Result<(), Error> {
        for sql in [
            "DELETE FROM secure_replayed_base_source WHERE scope=?",
            "DELETE FROM secure_base_transition WHERE scope=?",
            "DELETE FROM secure_replayed_base_rows WHERE scope=?",
            "INSERT INTO secure_replayed_base_rows SELECT scope,row,image FROM secure_replayed_staged WHERE scope=?",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        self.forget_seal_rows(id, i64::MAX).await
    }
    pub(in crate::replica) async fn forget_seal_rows(
        &self,
        id: i64,
        through: i64,
    ) -> Result<(), Error> {
        for table in [
            "secure_replayed_seals",
            "secure_replayed_seal_rows",
            "secure_replayed_snapshots",
        ] {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE scope=?1 AND seal NOT IN (SELECT seal FROM secure_finalized WHERE depth>?2) AND seal NOT IN (SELECT seal FROM secure_replayed_base_source WHERE scope=?1) AND seal NOT IN (SELECT seal FROM secure_base_transition WHERE scope=?1)"),
                    [id, through],
                )
                .await?;
        }
        Ok(())
    }
    /// `rows`, a base at a seal of tip `source`, as the scope's current
    /// typed rows: a field the table lacks, or one some tip since `source`
    /// lacked, takes its default.
    fn current_images(
        &self,
        scope: ResourceScope,
        source: TipId,
        rows: &[(Commitment, Vec<u8>)],
    ) -> Result<Vec<(Commitment, Vec<SqlValue>)>, Error> {
        let corrupt = || Error::Corrupt("replayed base image".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        let columns = self.restored_columns(scope, source)?;
        rows.iter()
            .map(|(row, image)| {
                let fields = blob_fields(image)?;
                let image = columns
                    .iter()
                    .map(
                        |(id, kept, default)| match fields.get(id).filter(|_| *kept) {
                            Some(bytes) => {
                                SqlValue::decode(&mut bytes.as_slice()).map_err(|_| corrupt())
                            }
                            None => Ok(default.clone()),
                        },
                    )
                    .collect::<Result<Vec<_>, Error>>()?;
                if typed.row_scope(projected, &image, &self.partitions)? != scope
                    || typed
                        .rows
                        .row_id(typed.domain.domain, &projected.table, &image)
                        .map_err(|_| corrupt())?
                        != *row
                {
                    return Err(denied("replayed base row differs from its scope"));
                }
                Ok((*row, image))
            })
            .collect()
    }
    /// Each column of scope `scope`'s table with its field id, whether rows
    /// at a seal of tip `source` keep it, and its default.
    fn restored_columns(
        &self,
        scope: ResourceScope,
        source: TipId,
    ) -> Result<Vec<(Commitment, bool, SqlValue)>, Error> {
        let corrupt = || Error::Corrupt("replayed base image".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        let schema = typed.rows.schema();
        let logical = schema
            .table(&projected.table.sql_name)
            .ok_or_else(corrupt)?;
        let path = self
            .path_fields_to(scope.domain, source, self.workspace.tip)
            .ok();
        let declared: BTreeSet<_> = self
            .definition
            .fields
            .iter()
            .map(|field| field.id)
            .collect();
        Ok(logical
            .columns
            .iter()
            .map(|column| {
                let id = schema.field_id(logical, column);
                let kept = !declared.contains(&id)
                    || path.as_ref().is_none_or(|path| path.fields.contains(&id));
                let default = rocky_core::default_value(column).unwrap_or(SqlValue::Null);
                (id, kept, default)
            })
            .collect())
    }
    /// Whether `rows`, at a seal of tip `source`, carry every field the
    /// current table keeps from them. Rows taken under a schema off the
    /// path, as after an owner's rollback, may lack one.
    pub(in crate::replica) fn images_serve(
        &self,
        scope: ResourceScope,
        source: TipId,
        rows: &[(Commitment, Vec<u8>)],
    ) -> Result<bool, Error> {
        let kept: Vec<_> = self
            .restored_columns(scope, source)?
            .into_iter()
            .filter_map(|(id, kept, _)| kept.then_some(id))
            .collect();
        for (_, image) in rows {
            let fields = blob_fields(image)?;
            if kept.iter().any(|id| !fields.contains_key(id)) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Refuses base rows, at a seal of tip `source`, whose identity a
    /// signed mark does not cover.
    pub(in crate::replica) fn check_base_identities(
        &self,
        scope: ResourceScope,
        source: TipId,
        rows: &[(Commitment, Vec<u8>)],
        marks: &[crate::replica::checkpoint::CoveredMark],
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        for (row, image) in self.current_images(scope, source, rows)? {
            let change = RowChange {
                scope,
                row,
                before: None,
                after: Some(
                    typed
                        .rows
                        .fields(&projected.table, &image)
                        .map_err(|_| corrupt())?,
                ),
            };
            if let Some(identity) = self
                .app
                .row_identity(&change)
                .map_err(|error| denied(&error))?
                && !marks.iter().any(|mark| {
                    mark.author == identity.author()
                        && mark.sequence >= identity.sequence()
                        && mark.own >= identity.sequence()
                })
            {
                return Err(denied("checkpoint identity exceeds its permanent mark"));
            }
        }
        Ok(())
    }
    /// Writes `rows`, a base at a seal of tip `source`, into the scope's
    /// replayed table, emptied first.
    pub(in crate::replica) async fn restore_images(
        &self,
        scope: ResourceScope,
        source: TipId,
        rows: &[(Commitment, Vec<u8>)],
    ) -> Result<(), Error> {
        // Bases of linked scopes restore in any order; their references
        // settle at commit. Deferral lasts until the transaction ends, so it
        // stops here: members replayed after the restore check their keys.
        self.conn
            .execute("PRAGMA defer_foreign_keys=ON", ())
            .await?;
        self.insert_images(scope, source, rows).await?;
        self.conn
            .execute("PRAGMA defer_foreign_keys=OFF", ())
            .await?;
        Ok(())
    }
    /// `restore_images` of scope `id`'s staged rows, a page at a time.
    pub(in crate::replica) async fn restore_staged(
        &self,
        scope: ResourceScope,
        source: TipId,
        id: i64,
    ) -> Result<(), Error> {
        let ordinals = self.stored_field_ordinals().await?;
        self.conn
            .execute("PRAGMA defer_foreign_keys=ON", ())
            .await?;
        let mut after = None;
        loop {
            let rows = self
                .stored_page("secure_replayed_staged", id, after, &ordinals)
                .await?;
            let Some((last, _)) = rows.last() else {
                break;
            };
            after = Some(*last);
            self.insert_images(scope, source, &rows).await?;
        }
        self.conn
            .execute("PRAGMA defer_foreign_keys=OFF", ())
            .await?;
        Ok(())
    }
    async fn insert_images(
        &self,
        scope: ResourceScope,
        source: TipId,
        rows: &[(Commitment, Vec<u8>)],
    ) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("replayed scope".into());
        let typed = self.workspace.typed.as_ref().ok_or_else(corrupt)?;
        let projected = typed.by_scope(scope).ok_or_else(corrupt)?;
        let table = typed
            .plan(&BTreeSet::from([scope]))?
            .tables
            .into_iter()
            .find(|table| table.name.eq_ignore_ascii_case(&projected.table.sql_name))
            .ok_or_else(corrupt)?;
        let images = self.current_images(scope, source, rows)?;
        self.touch(&table.name);
        for (_, image) in images {
            self.conn
                .execute(&table.insert_image(), bindings(table.stored(&image)))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_stored_image_unpacks_to_the_full_image() {
        let mut ordinals = FieldOrdinals::default();
        // Ordinals past one varint byte.
        for (ordinal, byte) in [(1u64, 1u8), (300, 2), (1 << 20, 3)] {
            ordinals.ordinals.insert([byte; 32], ordinal);
            ordinals.fields.insert(ordinal, [byte; 32]);
        }
        let fields = RowFields::from([
            ([1; 32], encoded(SqlValue::Integer(-7))),
            ([2; 32], encoded(SqlValue::Text("list".into()))),
            ([3; 32], encoded(SqlValue::Null)),
        ]);
        let image = fields_blob(&fields);
        let packed = ordinals.pack(&image).unwrap();
        assert!(packed.len() < image.len());
        assert_eq!(ordinals.unpack(&packed).unwrap(), image);
        assert_eq!(blob_fields(&image).unwrap(), fields);
        // A field outside the dictionary is refused, never dropped.
        let unknown = fields_blob(&RowFields::from([([9; 32], encoded(SqlValue::Null))]));
        assert!(ordinals.pack(&unknown).is_err());
    }

    fn encoded(value: SqlValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        value.encode(&mut bytes);
        bytes
    }
}
