//! A replayed scope's rows at a seal: its replay base. Replayed tables keep
//! no row records, so a checkpoint of one carries `(row id, image)` pairs
//! committed by an LtHash, and a rebuild starts from them. An image keys
//! each value by its field id, so it outlives the seal's schema. Stored
//! images name each field id by its ordinal in `secure_replayed_fields`;
//! digests and pages use the full image.
use super::*;
use crate::replica::checkpoint::Sets;

pub(in crate::replica) const SCHEMA: &[&str] = &[
    // Rows at each finalized, uncompacted seal; `secure_replayed_seals`
    // marks the seals a scope's rows were taken at, empty ones included,
    // with the seal's encoded `SealedOutcomes`. They stay while the seal
    // leaves the path and comes back.
    "CREATE TABLE IF NOT EXISTS secure_replayed_seals(scope INTEGER NOT NULL,seal BLOB NOT NULL,outcomes BLOB NOT NULL,PRIMARY KEY(scope,seal)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_replayed_seal_rows(scope INTEGER NOT NULL,seal BLOB NOT NULL,row BLOB NOT NULL,image BLOB NOT NULL,PRIMARY KEY(scope,seal,row)) WITHOUT ROWID",
    // Rows at the scope's checkpoint, and those of an import in progress.
    "CREATE TABLE IF NOT EXISTS secure_replayed_base(scope INTEGER NOT NULL,row BLOB NOT NULL,image BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_replayed_staged(scope INTEGER NOT NULL,row BLOB NOT NULL,image BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    // The field ids stored images name; never deleted.
    "CREATE TABLE IF NOT EXISTS secure_replayed_fields(ordinal INTEGER PRIMARY KEY,field BLOB NOT NULL UNIQUE)",
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

/// Per lowercase table name, each row key an open member changed with the
/// row's image before the earliest of them; none when it was created.
type Undone = BTreeMap<String, BTreeMap<Vec<u8>, Option<Vec<SqlValue>>>>;

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
    pub(in crate::replica) fn unpack(&self, mut packed: &[u8]) -> Result<Vec<u8>, Error> {
        let corrupt = || Error::Corrupt("replayed stored image".into());
        let mut image = Vec::with_capacity(packed.len() * 4);
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
            image.extend_from_slice(self.fields.get(&ordinal).ok_or_else(corrupt)?);
            image.extend_from_slice(value);
            packed = rest;
        }
        Ok(image)
    }
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
        self.scanned_images(&[scope], None, &Undone::new())
            .await?
            .and_then(|mut scanned| scanned.remove(&scope))
            .ok_or_else(|| Error::Corrupt("replayed scope".into()))
    }
    /// The rows of replayed scope `scope` at kept `seal`, marked `mark`,
    /// by row id; none when the seal kept its table under other columns.
    pub(in crate::replica) async fn kept_images(
        &self,
        scope: ResourceScope,
        kept: (Commitment, i64),
    ) -> Result<Option<Vec<(Commitment, Vec<u8>)>>, Error> {
        Ok(self
            .scanned_images(&[scope], Some(kept), &Undone::new())
            .await?
            .and_then(|mut scanned| scanned.remove(&scope)))
    }
    /// The rows of each of `scopes`, partitions of one table, from a single
    /// scan of it, each row of `undone` replaced by its image there.
    #[allow(clippy::type_complexity)]
    async fn scanned_images(
        &self,
        scopes: &[ResourceScope],
        kept: Option<(Commitment, i64)>,
        undone: &Undone,
    ) -> Result<Option<BTreeMap<ResourceScope, Vec<(Commitment, Vec<u8>)>>>, Error> {
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
        let mut images: BTreeMap<_, Vec<_>> =
            scopes.iter().map(|scope| (*scope, Vec::new())).collect();
        let undone = undone.get(&table.name.to_ascii_lowercase());
        let mut live = Vec::new();
        for values in self
            .conn
            .connection()
            .rows(&scan, Vec::new())
            .map_err(Error::from)?
        {
            let image = table
                .image(&values)
                .map_err(|_| Error::Corrupt("replayed row image".into()))?;
            if undone.is_some_and(|undone| undone.contains_key(&key_bytes(&table.key, &image))) {
                continue;
            }
            live.push(image);
        }
        let before = undone
            .into_iter()
            .flat_map(|undone| undone.values().flatten().cloned());
        for image in live.into_iter().chain(before) {
            let Some(rows) =
                images.get_mut(&typed.row_scope(projected, &image, &self.partitions)?)
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
            rows.push((row, fields_blob(&fields)));
        }
        for rows in images.values_mut() {
            rows.sort_unstable();
        }
        Ok(Some(images))
    }

    /// Takes the rows of every replayed scope of `scopes` at `seal`, just
    /// finalized at `depth`: the live rows with every member after the seal
    /// undone, and the seal's outcomes. A scope the seal leaves undecided
    /// (`leaves_open`) takes none.
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
        let scopes = decided;
        if scopes.is_empty() {
            return Ok(());
        }
        let taken = self.images_at_seal(&scopes).await?;
        let fields = fields_of(
            taken
                .iter()
                .flat_map(|(_, images)| images.iter().map(|(_, image)| image.as_slice())),
        )?;
        let ordinals = self.field_ordinals(fields).await?;
        for (id, images) in taken {
            let outcomes = self.sealed_outcomes(id).await?;
            self.conn
                .execute(
                    "INSERT OR REPLACE INTO secure_replayed_seals VALUES(?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(seal.to_vec()),
                        SqlValue::Blob(encode(&outcomes)?),
                    ]),
                )
                .await?;
            self.conn
                .execute(
                    "DELETE FROM secure_replayed_seal_rows WHERE scope=? AND seal=?",
                    bindings(vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())]),
                )
                .await?;
            for (row, image) in images {
                self.conn
                    .execute(
                        "INSERT INTO secure_replayed_seal_rows VALUES(?,?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(seal.to_vec()),
                            SqlValue::Blob(row.to_vec()),
                            SqlValue::Blob(ordinals.pack(&image)?),
                        ]),
                    )
                    .await?;
            }
        }
        Ok(())
    }
    /// The rows of each of `scopes` with every open member undone, one
    /// scan per table. A row an open member changed takes its image before
    /// the earliest of them; no row is written. The seal leaves none of the
    /// members it covers open, so these all follow it.
    #[allow(clippy::type_complexity)]
    async fn images_at_seal(
        &self,
        scopes: &[(ResourceScope, i64)],
    ) -> Result<Vec<(i64, Vec<(Commitment, Vec<u8>)>)>, Error> {
        let mut undone = Undone::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT undo FROM secure_replayed WHERE sealed=0 AND undo IS NOT NULL ORDER BY era,clock,author,sequence",
                    (),
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(undo)] = record.as_slice() else {
                return Err(Error::Corrupt("replayed member".into()));
            };
            for undo in super::undo_of(undo)? {
                let mut key = Vec::new();
                for value in &undo.key {
                    value.encode(&mut key);
                }
                undone
                    .entry(undo.table.to_ascii_lowercase())
                    .or_default()
                    .entry(key)
                    .or_insert(undo.before);
            }
        }
        let mut tables: BTreeMap<_, Vec<ResourceScope>> = BTreeMap::new();
        for (scope, _) in scopes {
            tables.entry(scope.table).or_default().push(*scope);
        }
        let mut scanned = BTreeMap::new();
        for table in tables.values() {
            scanned.extend(
                self.scanned_images(table, None, &undone)
                    .await?
                    .ok_or_else(|| Error::Corrupt("replayed scope".into()))?,
            );
        }
        scopes
            .iter()
            .map(|(scope, id)| {
                scanned
                    .remove(scope)
                    .map(|rows| (*id, rows))
                    .ok_or_else(|| Error::Corrupt("replayed scope".into()))
            })
            .collect()
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
            Some([SqlValue::Blob(outcomes)]) => Ok(Some(decode(outcomes)?)),
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
                format!("SELECT row,image FROM {table} WHERE scope=? AND seal=? ORDER BY row"),
                vec![SqlValue::Integer(id), SqlValue::Blob(seal.to_vec())],
            ),
            None => (
                format!("SELECT row,image FROM {table} WHERE scope=? ORDER BY row"),
                vec![SqlValue::Integer(id)],
            ),
        };
        let ordinals = self.stored_field_ordinals().await?;
        collect_rows(self.conn.query(sql, bindings(params)).await?)
            .await?
            .into_iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(row), SqlValue::Blob(image)] => Ok((
                    super::super::checkpoint::row_of(row)?,
                    ordinals.unpack(image)?,
                )),
                _ => Err(Error::Corrupt("replayed base row".into())),
            })
            .collect()
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
            .execute("DELETE FROM secure_replayed_base WHERE scope=?", [id])
            .await?;
        let fields = fields_of(rows.iter().map(|(_, image)| image.as_slice()))?;
        let ordinals = self.field_ordinals(fields).await?;
        for (row, image) in rows {
            self.conn
                .execute(
                    "INSERT INTO secure_replayed_base VALUES(?,?,?)",
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
    pub(in crate::replica) async fn forget_seal_rows(
        &self,
        id: i64,
        through: i64,
    ) -> Result<(), Error> {
        for table in ["secure_replayed_seals", "secure_replayed_seal_rows"] {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE scope=? AND seal NOT IN (SELECT seal FROM secure_finalized WHERE depth>?)"),
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
        // Bases of linked scopes restore in any order; their references
        // settle at commit. Deferral lasts until the transaction ends, so it
        // stops here: members replayed after the restore check their keys.
        self.conn
            .execute("PRAGMA defer_foreign_keys=ON", ())
            .await?;
        for (_, image) in images {
            self.conn
                .execute(&table.insert_image(), bindings(table.stored(&image)))
                .await?;
        }
        self.conn
            .execute("PRAGMA defer_foreign_keys=OFF", ())
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
