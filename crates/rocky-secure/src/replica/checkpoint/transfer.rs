use super::*;

const UNJOURNALED_BIRTH: &str = "an applied birth has no admission evidence";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrunedPage {
    pub scope: ResourceScope,
    pub logs: Vec<(PublicKey, u64)>,
    pub next: Option<PublicKey>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointPage {
    pub scope: ResourceScope,
    pub checkpoint: Commitment,
    pub after: Option<Commitment>,
    #[serde(with = "sealed")]
    pub rows: EncryptedPackage,
    pub next: Option<Commitment>,
}
mod sealed {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Sealed {
        scope: ResourceScope,
        generation: rocky_core::KeyEpoch,
        kind: PackageKind,
        #[serde(with = "serde_bytes")]
        bytes: Vec<u8>,
    }
    pub(super) fn serialize<S: serde::Serializer>(
        package: &EncryptedPackage,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Sealed {
            scope: package.scope,
            generation: package.generation,
            kind: package.kind.clone(),
            bytes: package.bytes.clone(),
        }
        .serialize(serializer)
    }
    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<EncryptedPackage, D::Error> {
        let sealed = Sealed::deserialize(deserializer)?;
        Ok(EncryptedPackage {
            scope: sealed.scope,
            generation: sealed.generation,
            kind: sealed.kind,
            bytes: sealed.bytes,
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct PageRow {
    #[serde(with = "serde_bytes")]
    row: Vec<u8>,
    #[serde(with = "serde_bytes")]
    fields: Vec<u8>,
    #[serde(deserialize_with = "rocky_core::bounded_clock")]
    clock: u64,
}
const ROW_OVERHEAD: usize = 64;
#[derive(Serialize, Deserialize)]
struct PagePlain {
    checkpoint: Commitment,
    after: Option<Commitment>,
    next: Option<Commitment>,
    rows: Vec<PageRow>,
    legend: Vec<String>,
    #[serde(with = "super::origin_legend")]
    origins: Vec<serde_bytes::ByteBuf>,
    creations: Vec<Hash>,
    authors: stamp_authors::Authors,
    bindings: Vec<serde_bytes::ByteBuf>,
    headers: Vec<serde_bytes::ByteBuf>,
    signature: Signature,
}
fn page_signing_bytes(plain: &PagePlain) -> Result<Vec<u8>, Error> {
    let rows = rocky_core::commitment(
        "rocky.checkpoint-page-rows.v2",
        &encode(&(
            plain.checkpoint,
            plain.after,
            plain.next,
            &plain.legend,
            &plain.origins,
            &plain.creations,
            &plain.authors,
            &plain.rows,
        ))?,
    );
    signing_bytes("rocky.checkpoint-page.v2", &rows)
}
const PAGE_ROWS: usize = 256;
const STORE_ROWS: usize = if cfg!(test) { 8 } else { 128 };
#[cfg(test)]
thread_local! {
    pub(crate) static STORE_ROWS_FOR_TEST: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}
fn store_rows() -> Result<usize, Error> {
    #[cfg(test)]
    if let Some(rows) = STORE_ROWS_FOR_TEST.with(std::cell::Cell::get) {
        return Ok(rows);
    }
    #[cfg(feature = "qualification")]
    return crate::qualification_bound("ROCKY_CHECKPOINT_STORE_ROWS", STORE_ROWS, PAGE_ROWS);
    #[cfg(not(feature = "qualification"))]
    Ok(STORE_ROWS)
}
pub(crate) const STORE_METADATA: usize = 8;
const EXPORT_ROWS: usize = 8;
const EXPORT_LOGS: usize = 1;

struct OpenedPage {
    rows: Vec<(Commitment, PageRow, Materialized)>,
    bindings: Vec<Vec<u8>>,
    headers: Vec<Arc<crypto::CreationHeader>>,
    authors: stamp_authors::Authors,
    next: Option<Commitment>,
    signature: Signature,
}
struct ExportSource {
    authority: u64,
    epoch: u64,
    version: u64,
    writes: Arc<crate::sqlite::Writes>,
    table: String,
}
enum PageMetadata<'a> {
    Header(&'a Arc<crypto::CreationHeader>),
    Bindings(&'a Vec<u8>),
    Author(&'a WriterId, &'a PublicKey),
}
struct PageStep {
    rows: Vec<(Commitment, PageRow, Materialized)>,
    next: Option<Commitment>,
    signature: Signature,
    delta: Sets,
}
fn open_page(
    page: CheckpointPage,
    plain: PagePlain,
    signed: &SignedCheckpoint,
    cursor: Option<Commitment>,
    publisher: [u8; 32],
    known_headers: BTreeMap<Commitment, Vec<u8>>,
    crypto: &crypto::Cache,
) -> Result<OpenedPage, Error> {
    if plain.checkpoint != page.checkpoint
        || plain.after != page.after
        || plain.next != page.next
        || plain.rows.is_empty() && plain.next.is_some()
        || plain.rows.len() > PAGE_ROWS
    {
        return Err(denied("checkpoint page binding mismatch"));
    }
    if plain.legend.iter().any(String::is_empty)
        || plain.legend.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(denied("checkpoint field legend is not strictly ordered"));
    }
    if plain.origins.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(denied("checkpoint origin legend is not strictly ordered"));
    }
    let names: BTreeMap<_, _> = plain
        .legend
        .iter()
        .enumerate()
        .map(|(id, name)| {
            (
                id as i64,
                rocky_core::definition_item_id(
                    publisher,
                    signed.statement.scope.domain.app,
                    "field",
                    name,
                ),
            )
        })
        .collect();
    let mut available_headers: BTreeMap<_, _> = known_headers
        .iter()
        .map(|(digest, bytes)| (*digest, bytes.as_slice()))
        .collect();
    for header in &plain.headers {
        available_headers.insert(moves::header_digest(header), header.as_ref());
    }
    let available_headers: BTreeMap<_, _> = available_headers
        .into_iter()
        .map(|(digest, bytes)| Ok((digest, crypto.creation_header(bytes)?)))
        .collect::<Result<_, Error>>()?;
    let origins: Vec<moves::Origin> = plain
        .origins
        .iter()
        .map(|origin| {
            let digest = moves::Origin::checkpoint_header(origin)?;
            let header = available_headers
                .get(&digest)
                .ok_or_else(|| denied("checkpoint page lacks a creation header"))?;
            moves::Origin::from_checkpoint(origin, header)
        })
        .collect::<Result<_, Error>>()?;
    if plain.creations.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(denied("checkpoint creation legend is not strictly ordered"));
    }
    let mut used_creations = BTreeSet::new();
    let mut used_names = BTreeSet::new();
    let mut used_origins = BTreeSet::new();
    let mut used_authors = BTreeSet::new();
    let signing = page_signing_bytes(&plain)?;
    if !signed.signer().verify(&signing, &plain.signature) {
        return Err(denied("checkpoint page signature mismatch"));
    }
    let mut sets = BTreeMap::new();
    for set in &plain.bindings {
        decode::<Vec<rocky_credentials::AuthorBinding>>(set)?;
        if sets.insert(moves::bindings_digest(set), false).is_some() {
            return Err(denied("checkpoint page repeats author bindings"));
        }
    }
    let mut headers = BTreeMap::new();
    for header in &plain.headers {
        if headers
            .insert(moves::header_digest(header), false)
            .is_some()
        {
            return Err(denied("checkpoint page repeats a creation header"));
        }
    }
    let mut last = cursor;
    let mut clocks = BTreeMap::new();
    let mut rows = Vec::with_capacity(plain.rows.len());
    for row in plain.rows {
        let id = row_of(&row.row)?;
        if last.is_some_and(|last| id <= last) {
            return Err(denied("checkpoint rows out of order"));
        }
        last = Some(id);
        let packed: records::Record = decode(&row.fields)?;
        used_names.extend(packed.field_ids());
        let origin = match packed.origin_id() {
            Some(id) => {
                used_origins.insert(id);
                Some(
                    origins
                        .get(
                            usize::try_from(id)
                                .map_err(|_| denied("invalid checkpoint origin index"))?,
                        )
                        .cloned()
                        .ok_or_else(|| denied("checkpoint page lacks a row origin"))?,
                )
            }
            None => None,
        };
        if packed.author_set_id() != 0 {
            return Err(denied("checkpoint row names a local author set"));
        }
        let creation = packed.creation_index();
        let packed_creation = *plain
            .creations
            .get(creation as usize)
            .ok_or_else(|| denied("checkpoint page lacks a row creation"))?;
        used_creations.insert(creation);
        let record = packed.unpack(&names, origin, &plain.authors, packed_creation)?;
        for stamp in record.stamps.values().chain(&record.created) {
            if !plain.authors.contains_key(&stamp.author) {
                return Err(denied("checkpoint page lacks a stamp author"));
            }
            used_authors.insert(stamp.author);
        }
        if let Some(proof) = &record.proof {
            if let std::collections::btree_map::Entry::Vacant(entry) = headers.entry(proof.header())
            {
                let bytes = known_headers
                    .get(&proof.header())
                    .ok_or_else(|| denied("checkpoint page lacks a creation header"))?;
                if moves::header_digest(bytes) != proof.header() {
                    return Err(denied("checkpoint header digest mismatch"));
                }
                entry.insert(true);
            }
            *headers
                .get_mut(&proof.header())
                .ok_or_else(|| denied("checkpoint page lacks a creation header"))? = true;
            *sets
                .get_mut(&proof.bindings())
                .ok_or_else(|| denied("checkpoint page lacks a row's author bindings"))? = true;
        }
        if clocks
            .insert(record.creation, row.clock)
            .is_some_and(|clock| clock != row.clock)
        {
            return Err(denied("checkpoint creation clocks disagree"));
        }
        rows.push((id, row, record));
    }
    if plain.next.is_some() && plain.next != last {
        return Err(denied("checkpoint page binding mismatch"));
    }
    if used_creations.len() != plain.creations.len()
        || used_names.len() != plain.legend.len()
        || used_origins.len() != origins.len()
        || used_authors.len() != plain.authors.len()
    {
        return Err(denied("checkpoint page carries unused legend entries"));
    }
    if headers.values().any(|named| !named) {
        return Err(denied("checkpoint page carries unnamed creation headers"));
    }
    if sets.values().any(|named| !named) {
        return Err(denied("checkpoint page carries unnamed author bindings"));
    }
    Ok(OpenedPage {
        rows,
        authors: plain.authors,
        headers: plain
            .headers
            .into_iter()
            .map(|header| crypto.creation_header(&header))
            .collect::<Result<_, Error>>()?,
        bindings: plain
            .bindings
            .into_iter()
            .map(|set| set.into_vec())
            .collect(),
        next: plain.next,
        signature: plain.signature,
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootstrapReport {
    pub checkpoint: Commitment,
    pub superseded: u64,
    pub reapplied: Vec<Hash>,
    pub rejected: Vec<(Hash, String)>,
    pub pending: Vec<Hash>,
}
pub(in crate::replica) struct Staging {
    pub(in crate::replica) signed: SignedCheckpoint,
    pub(super) cursor: Option<Commitment>,
    pub(in crate::replica) complete: bool,
    pub(super) sets: Sets,
    pub(super) direct: bool,
}

impl<A: RowApplication> Inner<A> {
    async fn store_page_metadata(&self, items: &[PageMetadata<'_>]) -> Result<(), Error> {
        for item in items {
            match item {
                PageMetadata::Header(header) => self.put_verified_creation_header(header).await?,
                PageMetadata::Bindings(set) => {
                    self.put_bindings(set).await?;
                }
                PageMetadata::Author(author, signer) => {
                    self.keep_stamp_author(**author, **signer)?
                }
            }
        }
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    pub(crate) fn authorize_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.same_tip_peer(peer)?;
        self.authorize_seal_peer(peer, credentials, scope)
    }
    pub(crate) fn authorize_seal_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &[scope])?;
        if !crate::replica::roles::holds_whole(&grants, scope) {
            return Err(denied("ciphertext custody denied"));
        }
        Ok(())
    }
    async fn authorize_checkpoint_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<(), Error> {
        if self
            .served_seal_checkpoint(scope, checkpoint)
            .await?
            .is_some()
        {
            self.authorize_seal_peer(peer, credentials, scope)
        } else {
            self.authorize_peer(peer, credentials, scope)
        }
    }
    pub(in crate::replica) fn descends(
        graph: &rocky_credentials::TipGraph,
        tip: TipId,
        ancestor: TipId,
    ) -> Result<bool, Error> {
        let mut cursor = tip;
        let mut visited = BTreeSet::new();
        while let Some((parent, _)) = Self::tip_parent(graph, cursor)? {
            if parent == ancestor {
                return Ok(true);
            }
            if !visited.insert(parent) {
                break;
            }
            cursor = parent;
        }
        Ok(false)
    }
    pub(in crate::replica) async fn staging(&self, id: i64) -> Result<Option<Staging>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT signed,cursor,complete,sets,direct FROM secure_rebootstrap WHERE scope=?",
                [id],
            )
            .await?;
        let Some(row) = result.next().await? else {
            return Ok(None);
        };
        Ok(Some(Staging {
            signed: SignedCheckpoint::from_bytes(&row.get::<Vec<u8>>(0)?)?,
            cursor: match row.get_value(1)? {
                crate::sqlite::Value::Blob(bytes) => Some(row_of(&bytes)?),
                _ => None,
            },
            complete: row.get::<i64>(2)? == 1,
            sets: Sets::from_bytes(&row.get::<Vec<u8>>(3)?)?,
            direct: row.get::<i64>(4)? == 1,
        }))
    }
    pub(in crate::replica) async fn verify_import_statement(
        &self,
        signed: &SignedCheckpoint,
    ) -> Result<(), Error> {
        signed.verify_authority(self, now()?)?;
        let statement = &signed.statement;
        let scope = statement.scope;
        if self.graphs.contains_key(&scope.domain) && statement.seal.is_none() {
            return Err(denied(
                "with a tip graph an imported checkpoint sits at a seal",
            ));
        }
        let generation = self.generation_of(self.manifest_of_tip(statement.tip)?, true)?;
        if statement.schema != generation.schema
            || (!statement.no_frames() && Some(self.scope_key(scope)?.0) != statement.key_epoch())
        {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
        if statement.no_frames() {
            self.check_no_frames(signed).await?;
        }
        Ok(())
    }
    pub(in crate::replica) async fn authenticated_direct_import(
        &self,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(false);
        };
        let Some(staging) = self.staging(id).await?.filter(|staging| staging.direct) else {
            return Ok(false);
        };
        let statement = &staging.signed.statement;
        if statement.scope != scope || self.verify_import_statement(&staging.signed).await.is_err()
        {
            return Ok(false);
        }
        if !self.graphs.contains_key(&scope.domain) {
            return Ok(true);
        }
        let Some(sealed) = statement.seal else {
            return Ok(false);
        };
        let graph = self.graph_for(scope.domain)?;
        Ok(graph
            .seal_by_id(sealed)
            .is_ok_and(|seal| seal.statement.tip == statement.tip)
            && self.path_index(scope.domain, sealed)?.is_some())
    }
    pub(crate) async fn importing(&self, scope: ResourceScope) -> Result<bool, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(self.importing.contains(&scope));
        };
        if self.importing.contains(&scope) {
            let mut held = match self.replayed_scope(scope) {
                true => {
                    self.conn
                        .query("SELECT 1 FROM secure_applied WHERE scope=? LIMIT 1", [id])
                        .await?
                }
                false => {
                    self.conn
                        .query(
                            "SELECT 1 FROM secure_row_records WHERE scope=? LIMIT 1",
                            [encode(&scope)?],
                        )
                        .await?
                }
            };
            if held.next().await?.is_none() {
                return Ok(true);
            }
        }
        let mut staged = self
            .conn
            .query("SELECT 1 FROM secure_rebootstrap WHERE scope=?", [id])
            .await?;
        Ok(staged.next().await?.is_some())
    }
    async fn expected_page(&self, page: &CheckpointPage) -> Result<([u8; 32], Staging), Error> {
        let scope = page.scope;
        let (_, key) = self.scope_key(scope)?;
        let id = self
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        let staging = self
            .staging(id)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        if staging.signed.id()? != page.checkpoint
            || staging.complete
            || staging.cursor != page.after
            || page.rows.scope != scope
            || page.rows.kind != PackageKind::Checkpoint
            || Some(page.rows.generation) != staging.signed.statement.key_epoch()
        {
            return Err(denied("unexpected checkpoint page"));
        }
        Ok((key, staging))
    }
    async fn store_page(
        &mut self,
        scope: ResourceScope,
        id: i64,
        staging: &Staging,
        page: &PageStep,
        finish: Option<Option<Commitment>>,
    ) -> Result<(), Error> {
        let after = staging.cursor;
        let finished = finish.is_some();
        #[cfg(feature = "qualification")]
        self.flusher.measurements.work(
            super::super::lock::WorkUnit::CheckpointRow,
            page.rows.len() as u64,
        );
        let statement = &staging.signed.statement;
        self.check_statement_marks(scope, &statement.identities)
            .await?;
        if let Some((allocation, _)) = self.allocation_binding(scope).await? {
            for (_, _, record) in &page.rows {
                self.check_record_provenance(allocation, record).await?;
            }
        }
        let mut sets = staging.sets.clone();
        sets.merge_rows(&page.delta);
        let blob = encode(&scope)?;
        let mut creations = BTreeMap::new();
        for (_, PageRow { clock, .. }, record) in &page.rows {
            if creations
                .insert(record.creation, *clock)
                .is_some_and(|known| known != *clock)
            {
                return Err(denied("checkpoint creation clocks disagree"));
            }
        }
        let mut fresh = creations.clone();
        if !creations.is_empty() {
            let placeholders = vec!["?"; creations.len()].join(",");
            let mut keys = vec![SqlValue::Integer(id)];
            keys.extend(
                creations
                    .keys()
                    .map(|hash| SqlValue::Blob(hash.as_bytes().to_vec())),
            );
            for row in collect_rows(self.conn.query(
                format!("SELECT hash,clock FROM secure_rebootstrap_stubs WHERE scope=? AND hash IN ({placeholders})"),
                bindings(keys),
            ).await?).await? {
                let [SqlValue::Blob(hash), SqlValue::Integer(clock)] = row.as_slice() else {
                    return Err(Error::Corrupt("checkpoint creation stub".into()));
                };
                let hash = hash_of(hash)?;
                if fresh.remove(&hash) != Some(stored_clock(*clock)?) {
                    return Err(denied("checkpoint creation clocks disagree"));
                }
            }
        }
        for (hash, clock) in &fresh {
            sets.create(*hash, *clock);
        }
        let mut stored = Vec::with_capacity(page.rows.len() * 3);
        if staging.direct && !page.rows.is_empty() {
            let mut keys = vec![SqlValue::Integer(id)];
            keys.extend(
                page.rows
                    .iter()
                    .map(|(row, ..)| SqlValue::Blob(row.to_vec())),
            );
            let placeholders = vec!["?"; page.rows.len()].join(",");
            if !collect_rows(self.conn.query(
                format!("SELECT row FROM secure_rows WHERE scope=? AND row IN ({placeholders}) LIMIT 1"),
                bindings(keys),
            ).await?).await?.is_empty() {
                return Err(Error::Corrupt(
                    "a checkpoint row is already stored during its import".into(),
                ));
            }
        }
        let mut ordered: Vec<_> = page.rows.iter().collect();
        ordered.sort_unstable_by_key(|(row, ..)| *row);
        for (row, _, record) in ordered {
            let fields = self.encode_record(record).await?;
            stored.extend([
                SqlValue::Integer(id),
                SqlValue::Blob(row.to_vec()),
                SqlValue::Blob(fields),
            ]);
            let moved_out = record.deleted && record.fields.is_empty();
            if let Some(tuple) = &statement.partition
                && !moved_out
            {
                let change = RowChange {
                    scope,
                    row: record
                        .key
                        .ok_or_else(|| denied("partition checkpoint row lacks key"))?,
                    before: None,
                    after: Some(record.fields.clone()),
                };
                if self
                    .app
                    .row_partition(&change)
                    .map_err(|error| denied(&error))?
                    .as_ref()
                    != Some(tuple)
                {
                    return Err(denied("checkpoint row differs from its partition"));
                }
            }
            if let Some(identity) = record.identity
                && !statement.identities.iter().any(|mark| {
                    mark.author == identity.writer()
                        && mark.sequence >= identity.sequence()
                        && (record.moved_from.is_some() || mark.own >= identity.sequence())
                })
            {
                return Err(denied("checkpoint identity exceeds its permanent mark"));
            }
            if staging.direct {
                self.index_row_key(&blob, row, None, record).await?;
            }
        }
        for (direct, stubs) in [(false, &fresh), (true, &creations)] {
            if stubs.is_empty() || direct && !staging.direct {
                continue;
            }
            let (table, value, conflict) = match direct {
                true => (
                    "secure_stubs(scope,hash,clock,creation)",
                    "(?,?,?,1)",
                    " ON CONFLICT(hash) DO UPDATE SET creation=1",
                ),
                false => ("secure_rebootstrap_stubs(scope,hash,clock)", "(?,?,?)", ""),
            };
            let values = vec![value; stubs.len()].join(",");
            let parameters = stubs
                .iter()
                .flat_map(|(hash, clock)| {
                    [
                        SqlValue::Integer(id),
                        SqlValue::Blob(hash.as_bytes().to_vec()),
                        integer(*clock),
                    ]
                })
                .collect();
            self.conn
                .execute(
                    format!("INSERT INTO {table} VALUES {values}{conflict}"),
                    bindings(parameters),
                )
                .await?;
        }
        if !stored.is_empty() {
            let values = vec!["(?,?,?)"; page.rows.len()].join(",");
            if staging.direct {
                self.conn
                    .execute(
                        format!("INSERT INTO secure_rows VALUES {values}"),
                        bindings(stored.clone()),
                    )
                    .await?;
            }
            let table = match staging.direct {
                true => "secure_checkpoint_rows",
                false => "secure_rebootstrap_rows",
            };
            self.conn
                .execute(
                    format!("INSERT INTO {table} VALUES {values}"),
                    bindings(stored),
                )
                .await?;
        }
        if staging.direct && !page.rows.is_empty() {
            let (rows, tombstones, mut digest, applied) = self.scope_state(id).await?;
            digest.merge(&page.delta.digest);
            self.store_scope_state(
                id,
                rows + page.delta.rows,
                tombstones + page.delta.tombstones,
                &digest,
                applied,
            )
            .await?;
            self.cause(crate::observe::Cause::Sync);
            self.conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
        }
        let cursor = finish.flatten().or(staging.cursor);
        let row = |row: Option<Commitment>| {
            row.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec()))
        };
        if finished {
            self.conn
                .execute(
                    "INSERT INTO secure_rebootstrap_pages VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(after.map(|row| row.to_vec()).unwrap_or_default()),
                        row(page.next),
                        SqlValue::Blob(page.signature.to_bytes().to_vec()),
                    ]),
                )
                .await?;
        }
        self.conn
            .execute(
                "UPDATE secure_rebootstrap SET cursor=?,complete=?,sets=? WHERE scope=?",
                bindings(vec![
                    row(cursor),
                    SqlValue::Integer(i64::from(finished && page.next.is_none())),
                    SqlValue::Blob(sets.to_bytes()),
                    SqlValue::Integer(id),
                ]),
            )
            .await?;
        self.fault(Stage::Import)?;
        Ok(())
    }
    fn check_export_source(&self, source: &ExportSource) -> Result<(), Error> {
        if self.authority != source.authority
            || self.context_epoch.load(Ordering::Relaxed) != source.epoch
            || source.writes.table_count(&source.table) != source.version
        {
            return Err(denied("checkpoint source changed; restart the page"));
        }
        Ok(())
    }
    fn checkpoint_key(&self, signed: &SignedCheckpoint) -> Option<PrivateKey> {
        let signer = signed.signer();
        self.node_author
            .as_ref()
            .map(|author| &author.key)
            .into_iter()
            .chain(self.credential_signer.as_ref())
            .chain([&self.config.node])
            .chain(self.line_nodes.values().map(|author| &author.key))
            .find(|key| key.public_key() == signer)
            .cloned()
    }
    async fn applied_any(&self, id: i64) -> Result<bool, Error> {
        let mut applied = self
            .conn
            .query("SELECT 1 FROM secure_applied WHERE scope=? LIMIT 1", [id])
            .await?;
        Ok(applied.next().await?.is_some())
    }
    pub(in crate::replica) async fn holds_anything(
        &self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<bool, Error> {
        if self.replayed_scope(scope) && self.holds_replayed_rows(scope).await? {
            return Ok(true);
        }
        let held = collect_rows(
            self.conn
                .query(
                    "SELECT EXISTS(SELECT 1 FROM secure_row_records WHERE scope=?) OR EXISTS(SELECT 1 FROM secure_checkpoints WHERE scope=?2) OR EXISTS(SELECT 1 FROM secure_applied WHERE scope=?2)",
                    bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(id)]),
                )
                .await?,
        )
        .await?;
        Ok(held != vec![vec![SqlValue::Integer(0)]])
    }
    pub(super) async fn abort_direct(
        &mut self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<(), Error> {
        self.clear_seal_anchor(id).await?;
        let blob = encode(&scope)?;
        self.conn
            .execute(
                "UPDATE secure_operations SET status='pending' WHERE hash IN(SELECT hash FROM secure_applied WHERE scope=?)",
                [id],
            )
            .await?;
        self.clear_applied_cache();
        for sql in [
            "DELETE FROM secure_row_records WHERE scope=?",
            "DELETE FROM secure_move_rows WHERE scope=?",
        ] {
            self.conn.execute(sql, [blob.clone()]).await?;
        }
        for sql in [
            "DELETE FROM secure_applied WHERE scope=?",
            "DELETE FROM secure_logs WHERE scope=?",
            "DELETE FROM secure_checkpoint_base WHERE scope=?",
            "DELETE FROM secure_stubs WHERE scope=?",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        self.store_scope_state(id, 0, 0, &LtHash16::new(), 0)
            .await?;
        if self.replayed_scope(scope) {
            self.clear_replayed(scope).await?;
        }
        self.reproject(scope).await?;
        self.reseed_ledger(scope, id).await?;
        self.clear_staging(id).await
    }
    pub(in crate::replica) async fn load_direct(&mut self) -> Result<(), Error> {
        let mut direct = BTreeSet::new();
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT s.scope FROM secure_rebootstrap b JOIN secure_scope_state s ON s.id=b.scope WHERE b.direct=1",
                    (),
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(scope)] = record.as_slice() else {
                return Err(Error::Corrupt("direct import scope".into()));
            };
            direct.insert(decode(scope)?);
        }
        self.direct = direct;
        Ok(())
    }
    async fn page_record(
        &self,
        id: i64,
        after: Option<Commitment>,
    ) -> Result<Option<(Option<Commitment>, Signature)>, Error> {
        let record = collect_rows(
            self.conn
                .query(
                    "SELECT next,signature FROM secure_checkpoint_pages WHERE scope=? AND after=?",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(after.map(|row| row.to_vec()).unwrap_or_default()),
                    ]),
                )
                .await?,
        )
        .await?;
        match record.first().map(Vec::as_slice) {
            None => Ok(None),
            Some([next, SqlValue::Blob(signature)]) => {
                let next = match next {
                    SqlValue::Blob(next) => Some(row_of(next)?),
                    SqlValue::Null => None,
                    _ => return Err(Error::Corrupt("checkpoint page record".into())),
                };
                let signature = Signature::try_from(signature.as_slice())
                    .map_err(|_| Error::Corrupt("checkpoint page signature".into()))?;
                Ok(Some((next, signature)))
            }
            Some(_) => Err(Error::Corrupt("checkpoint page record".into())),
        }
    }
    pub(super) async fn clear_staging(&self, id: i64) -> Result<(), Error> {
        for table in [
            "secure_rebootstrap",
            "secure_rebootstrap_rows",
            "secure_rebootstrap_stubs",
            "secure_rebootstrap_pages",
            "secure_replayed_staged",
        ] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        Ok(())
    }
    pub(crate) async fn has_pruned_prefix(&self, scope: ResourceScope) -> Result<bool, Error> {
        match self.find_scope_id(scope).await? {
            Some(id) => Ok(!self.logs(id, None, true, 1).await?.is_empty()),
            None => Ok(false),
        }
    }
    pub(in crate::replica) async fn offered_checkpoint(
        &mut self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        self.authorize_peer(peer, credentials, scope)?;
        let partitioned_table_scope =
            self.partitioned(scope) && !self.partitions.contains_key(&scope);
        if partitioned_table_scope {
            return Ok(None);
        }
        if self.importing.contains(&scope) || self.importing(scope).await? {
            return Ok(None);
        }
        let time = now()?;
        if self.no_frames_base(scope).await?.is_some() {
            return Ok(self
                .served_checkpoint(scope)
                .await?
                .map(|(_, signed)| signed)
                .filter(|signed| signed.signer() != peer));
        }
        let keyless_holder = !self.can_read_key(scope, &self.local_grants(time)?);
        if keyless_holder {
            return Ok(self
                .archived_checkpoint(scope, true)
                .await?
                .filter(|signed| {
                    signed.signer() != peer && signed.verify_authority(self, time).is_ok()
                }));
        }
        let Ok((key_epoch, _)) = self.scope_key(scope) else {
            return Ok(None);
        };
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        let Some(signed) = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
        else {
            return Ok(None);
        };
        if signed.signer() == peer
            || self
                .manifest_of_tip(signed.statement.tip)
                .and_then(|manifest| self.admits(manifest))
                .is_err()
            || signed.statement.key_epoch() != Some(key_epoch)
            || signed.verify_authority(self, time).is_err()
        {
            return Ok(None);
        }
        Ok(self
            .served_checkpoint(scope)
            .await?
            .filter(|(_, signed)| signed.signer() != peer)
            .map(|(_, signed)| signed))
    }

    pub(in crate::replica) async fn served_checkpoint(
        &mut self,
        scope: ResourceScope,
    ) -> Result<Option<(i64, SignedCheckpoint)>, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        let Some(record) = self.checkpoint_record(id, true).await? else {
            return Ok(None);
        };
        let signed = self.verify_record(scope, &record)?;
        if self.graphs.contains_key(&scope.domain)
            && !signed.statement.seal.is_some_and(|seal| {
                self.path_index(scope.domain, seal)
                    .is_ok_and(|at| at.is_some())
            })
        {
            return Ok(None);
        }
        if self
            .manifest_of_tip(signed.statement.tip)
            .and_then(|manifest| self.admits(manifest))
            .is_err()
        {
            return Ok(None);
        }
        signed.verify_authority(self, now()?)?;
        if signed.statement.no_frames() {
            self.check_no_frames(&signed).await?;
            return Ok(Some((id, signed)));
        }
        if Some(self.scope_key(scope)?.0) != signed.statement.key_epoch() {
            return Ok(None);
        }
        Ok(Some((id, signed)))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn pruned_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        after: Option<PublicKey>,
    ) -> Result<PrunedPage, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner = flushed(inner, &self.inner).await?;
        inner.authorize_peer(peer, credentials, scope)?;
        let mut logs = match inner.find_scope_id(scope).await? {
            Some(id) => inner.logs(id, after, true, STEP_ROWS + 1).await?,
            None => vec![],
        };
        let more = logs.len() > STEP_ROWS;
        logs.truncate(STEP_ROWS);
        Ok(PrunedPage {
            scope,
            next: logs.last().filter(|_| more).map(|(log, _)| log.author),
            logs: logs
                .into_iter()
                .map(|(log, pruned_prefix)| (log.author, pruned_prefix as u64))
                .collect(),
        })
    }
    pub async fn behind_pruned(&self, pruned_prefix: &PrunedPage) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let id = inner.find_scope_id(pruned_prefix.scope).await?;
        for (author, sequence) in &pruned_prefix.logs {
            let own = match id {
                Some(id) => inner
                    .log(id, *author)
                    .await?
                    .map_or(-1, |log| log.pruned_prefix),
                None => -1,
            };
            let needed = *sequence as i64 - own;
            if needed <= 0 {
                continue;
            }
            let held = collect_rows(
                inner
                    .conn
                    .query(
                        "SELECT COUNT(*) FROM secure_operations WHERE author=? AND scope=? AND sequence>? AND sequence<=?",
                        bindings(vec![
                            SqlValue::Blob(author.as_bytes().to_vec()),
                            SqlValue::Blob(encode(&pruned_prefix.scope)?),
                            SqlValue::Integer(own),
                            SqlValue::Integer(*sequence as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if held != vec![vec![SqlValue::Integer(needed)]] {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub async fn checkpoint_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let offered = inner.offered_checkpoint(peer, credentials, scope).await?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(offered)
    }
    pub(crate) async fn checkpoint_installed(
        &self,
        signed: &SignedCheckpoint,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(id) = inner.find_scope_id(signed.statement.scope).await? else {
            return Ok(false);
        };
        let installed = inner
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed);
        Ok(match installed {
            Some(installed) => installed.id()? == signed.id()?,
            None => false,
        })
    }
    pub(crate) async fn owns_checkpoint_signer(&self, signed: &SignedCheckpoint) -> bool {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.checkpoint_key(signed).is_some()
    }
    pub async fn checkpoint_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
        byte_quota: usize,
    ) -> Result<CheckpointPage, Error> {
        let page_rows = STEP_ROWS;
        #[cfg(test)]
        let page_rows = STORE_ROWS_FOR_TEST.with(|rows| {
            if rows.get().is_some() {
                PAGE_ROWS
            } else {
                page_rows
            }
        });
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner = flushed(inner, &self.inner).await?;
        inner
            .authorize_checkpoint_peer(peer, credentials, scope, checkpoint)
            .await?;
        if !(4096..=MAX_SECURE_FRAME).contains(&byte_quota) {
            return Err(denied("sync quota"));
        }
        if inner.scope_key(scope).is_err() {
            return inner
                .archived_page(scope, checkpoint, after, byte_quota)
                .await;
        }
        let (generation, key) = inner.scope_key(scope)?;
        let (id, signed, served) = match inner.served_checkpoint(scope).await? {
            Some((id, signed)) if signed.id()? == checkpoint => (id, signed, served::Served::Base),
            offered => match inner.served_seal_checkpoint(scope, checkpoint).await? {
                Some(found) => found,
                None if offered.is_some() => {
                    return Err(Error::Application(Box::new(
                        crate::network::CheckpointSuperseded(
                            "checkpoint superseded; restart the rebootstrap",
                        ),
                    )));
                }
                None => return Err(denied("no checkpoint to transfer")),
            },
        };
        let table = match served {
            served::Served::Base => "secure_checkpoints",
            served::Served::Kept => "secure_served",
        };
        let table = inner
            .conn
            .names()
            .physical(table)
            .unwrap_or(table)
            .to_owned();
        let writes = inner
            .conn
            .connection()
            .watch_writes([(table.clone(), true)])?;
        let source = ExportSource {
            authority: inner.authority,
            epoch: inner.context_epoch.load(Ordering::Relaxed),
            version: writes.table_count(&table),
            writes,
            table,
        };
        if after.is_none() {
            for chunk in signed.statement.frontier.chunks(EXPORT_LOGS) {
                let logs: Vec<_> = chunk.iter().map(|log| (log.author, log.sequence)).collect();
                inner.offer_covered(scope, &logs).await?;
                drop(inner);
                rocky_platform::task::yield_now().await;
                inner = self.inner.lock(Class::Background).await;
                inner.check_export_source(&source)?;
            }
        }
        const PAGE_FRAMING_BYTES: usize = 2048;
        const PACKAGE_FRAMING_BYTES: usize = 1024;
        let budget = (byte_quota - PAGE_FRAMING_BYTES)
            .min(rocky_core::MAX_PAYLOAD_BYTES - PACKAGE_FRAMING_BYTES);
        let signer = inner.checkpoint_key(&signed);
        let stored = match signer {
            Some(_) => None,
            None => Some(inner.page_record(id, after).await?.ok_or_else(|| {
                Error::Application(Box::new(crate::network::CheckpointPageUnavailable))
            })?),
        };
        let through = stored.as_ref().and_then(|(next, _)| *next);
        let replayed = inner.replayed_scope(scope);
        let rows_sql = match (replayed, &served) {
            (true, served::Served::Base) => "SELECT row,image FROM secure_replayed_base",
            _ => &format!("SELECT row,fields FROM {}", served.rows()),
        };
        let mut plain = PagePlain {
            checkpoint,
            after,
            next: None,
            rows: vec![],
            legend: vec![],
            origins: vec![],
            creations: vec![],
            authors: BTreeMap::new(),
            bindings: vec![],
            headers: vec![],
            signature: Signature::from_bytes(&[0; 64]),
        };
        let mut named = BTreeSet::new();
        let mut named_headers = BTreeSet::new();
        let mut header_bytes = BTreeMap::new();
        let mut legend_names = BTreeMap::new();
        let mut legend_origins = BTreeSet::new();
        let mut legend_creations = BTreeSet::new();
        let mut used = encode(&plain)?.len();
        let mut scanned = after;
        let mut had_records = false;
        'records: loop {
            drop(inner);
            rocky_platform::task::yield_now().await;
            inner = self.inner.lock(Class::Background).await;
            inner.check_export_source(&source)?;
            let records = collect_rows(inner.conn.query(
                format!("{rows_sql} WHERE scope=? AND row>? AND (? IS NULL OR row<=?) ORDER BY row LIMIT ?"),
                bindings(vec![SqlValue::Integer(id), SqlValue::Blob(scanned.map(|row| row.to_vec()).unwrap_or_default()), through.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec())), through.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec())), SqlValue::Integer(EXPORT_ROWS.min(page_rows + 1 - plain.rows.len()) as i64)]),
            ).await?).await?;
            #[cfg(feature = "qualification")]
            inner.units("served_checkpoint_row", records.len() as u64);
            if records.is_empty() {
                break;
            }
            had_records = true;
            let ordinals = match (replayed, &served) {
                (true, served::Served::Base) => Some(inner.stored_field_ordinals().await?),
                _ => None,
            };
            for record in &records {
                let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("checkpoint row".into()));
                };
                if replayed {
                    let fields = match &ordinals {
                        Some(ordinals) => ordinals.unpack(fields)?,
                        None => fields.clone(),
                    };
                    let weight = ROW_OVERHEAD + fields.len();
                    if plain.rows.len() == page_rows || used + weight > budget {
                        if stored.is_some() {
                            return Err(Error::Corrupt(
                                "checkpoint page exceeds its signed cut".into(),
                            ));
                        }
                        plain.next = plain
                            .rows
                            .last()
                            .map(|last| row_of(&last.row))
                            .transpose()?;
                        break 'records;
                    }
                    used += weight;
                    scanned = Some(row_of(row)?);
                    plain.rows.push(PageRow {
                        row: row.clone(),
                        fields,
                        clock: 0,
                    });
                    continue;
                }
                let record = inner.decode_record(fields).await?;
                let set = match record.proof.as_ref().map(|proof| proof.bindings()) {
                    Some(digest) if !named.contains(&digest) => {
                        Some((digest, inner.stored_bindings(digest).await?))
                    }
                    _ => None,
                };
                let header = match record.proof.as_ref().map(|proof| proof.header()) {
                    Some(digest) => {
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            header_bytes.entry(digest)
                        {
                            entry.insert(inner.stored_creation_header(digest).await?);
                        }
                        if !named_headers.contains(&digest)
                            && inner.page_header_needed(id, &served, digest, after).await?
                        {
                            Some((digest, header_bytes[&digest].clone()))
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                let mut added_names = BTreeMap::new();
                for field in record.fields.keys().chain(record.stamps.keys()) {
                    if !legend_names.contains_key(field) {
                        added_names.insert(*field, inner.record_name(*field).await?);
                    }
                }
                let origin = record
                    .proof
                    .as_ref()
                    .map(|proof| {
                        proof.origin().checked_checkpoint_bytes(
                            inner
                                .crypto
                                .creation_header(&header_bytes[&proof.header()])?
                                .as_ref(),
                        )
                    })
                    .transpose()?;
                let mut new_authors = BTreeMap::new();
                for author in record
                    .stamps
                    .values()
                    .chain(&record.created)
                    .map(|stamp| stamp.author)
                    .collect::<BTreeSet<_>>()
                {
                    if !plain.authors.contains_key(&author) {
                        new_authors.insert(
                            author,
                            *record
                                .authors
                                .get(&author)
                                .ok_or_else(|| denied("row stamp signer missing"))?,
                        );
                    }
                }
                let weight = ROW_OVERHEAD
                    + if legend_creations.contains(&record.creation) {
                        0
                    } else {
                        36
                    }
                    + new_authors.len() * 48
                    + added_names
                        .values()
                        .map(|name| name.len() + 16)
                        .sum::<usize>()
                    + origin
                        .as_ref()
                        .filter(|origin| !legend_origins.contains(*origin))
                        .map_or(0, |origin| origin.len() + 16)
                    + records::Record::wire_weight(&record)?
                    + header
                        .as_ref()
                        .map_or(0, |(_, header)| ROW_OVERHEAD + header.len())
                    + set.as_ref().map_or(0, |(_, set)| ROW_OVERHEAD + set.len());
                if plain.rows.len() == page_rows || used + weight > budget {
                    if stored.is_some() {
                        return Err(Error::Corrupt(
                            "checkpoint page exceeds its signed cut".into(),
                        ));
                    }
                    plain.next = plain
                        .rows
                        .last()
                        .map(|last| row_of(&last.row))
                        .transpose()?;
                    break 'records;
                }
                used += weight;
                plain.authors.extend(new_authors);
                legend_creations.insert(record.creation);
                legend_names.extend(added_names);
                if let Some(origin) = origin {
                    legend_origins.insert(origin);
                }
                if let Some((digest, header)) = header {
                    named_headers.insert(digest);
                    plain.headers.push(header.into());
                }
                if let Some((digest, set)) = set {
                    named.insert(digest);
                    plain.bindings.push(serde_bytes::ByteBuf::from(set));
                }
                let clock = inner
                    .creation_clock(record.creation)
                    .await?
                    .ok_or_else(|| Error::Corrupt("checkpoint row creation is unknown".into()))?;
                scanned = Some(row_of(row)?);
                plain.rows.push(PageRow {
                    row: row.clone(),
                    fields: encode(&record)?,
                    clock,
                });
            }
            #[cfg(test)]
            if let Some((reached, resume)) = inner.checkpoint_export_pause.take() {
                drop(inner);
                let _ = reached.send(());
                let _ = resume.await;
                inner = self.inner.lock(Class::Background).await;
                inner.check_export_source(&source)?;
            }
        }
        drop(inner);
        if plain.rows.is_empty() && had_records {
            return Err(denied("byte quota smaller than one checkpoint row"));
        }
        plain.legend = legend_names
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        plain.origins = legend_origins.into_iter().map(Into::into).collect();
        let field_ids: BTreeMap<_, _> = legend_names
            .into_iter()
            .map(|(field, name)| {
                let id = plain
                    .legend
                    .binary_search(&name)
                    .map_err(|_| Error::Corrupt("checkpoint field legend".into()))?;
                Ok((field, id as i64))
            })
            .collect::<Result<_, Error>>()?;
        plain.creations = legend_creations.into_iter().collect();
        for (index, row) in plain.rows.iter_mut().enumerate().filter(|_| !replayed) {
            if index % EXPORT_ROWS == 0 {
                rocky_platform::task::yield_now().await;
            }
            let record: Materialized = decode(&row.fields)?;
            let origin = record
                .proof
                .as_ref()
                .map(|proof| {
                    let bytes = serde_bytes::ByteBuf::from(proof.origin().checkpoint_bytes()?);
                    plain
                        .origins
                        .binary_search(&bytes)
                        .map(|id| id as i64)
                        .map_err(|_| Error::Corrupt("checkpoint origin legend".into()))
                })
                .transpose()?;
            row.fields = encode(&records::Record::pack(
                &record,
                &field_ids,
                origin,
                0,
                u32::try_from(
                    plain
                        .creations
                        .binary_search(&record.creation)
                        .map_err(|_| Error::Corrupt("checkpoint creation legend".into()))?,
                )
                .map_err(|_| Error::Corrupt("checkpoint creation legend".into()))?,
            )?)?;
        }
        plain.signature = match (signer, stored) {
            (Some(signer), _) => signer.sign(&page_signing_bytes(&plain)?),
            (None, Some((next, signature))) => {
                let last = plain
                    .rows
                    .last()
                    .map(|last| row_of(&last.row))
                    .transpose()?;
                if next.is_some() && last != next {
                    return Err(Error::Corrupt(
                        "checkpoint rows differ from their signed cut".into(),
                    ));
                }
                plain.next = next;
                signature
            }
            (None, None) => {
                return Err(Error::Application(Box::new(
                    crate::network::CheckpointPageUnavailable,
                )));
            }
        };
        let page = CheckpointPage {
            scope,
            checkpoint,
            after,
            next: plain.next,
            rows: EncryptedPackage::seal(
                scope,
                generation,
                PackageKind::Checkpoint,
                &key,
                &encode(&plain)?,
            )?,
        };
        if encode(&page)?.len() > byte_quota {
            return Err(denied("checkpoint page exceeds the byte quota"));
        }
        inner = self.inner.lock(Class::Background).await;
        inner.check_export_source(&source)?;
        #[cfg(feature = "qualification")]
        inner.units("served_checkpoint_page", 1);
        inner = flush_guard(inner, &self.inner).await?;
        inner.check_export_source(&source)?;
        inner
            .authorize_checkpoint_peer(peer, credentials, scope, checkpoint)
            .await?;
        drop(inner);
        Ok(page)
    }
    #[cfg(test)]
    pub(crate) async fn pause_checkpoint_export_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) {
        self.inner
            .lock(Class::Background)
            .await
            .checkpoint_export_pause = Some((reached, resume));
    }
    #[cfg(test)]
    pub(crate) async fn pause_checkpoint_store_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) {
        self.inner
            .lock(Class::Background)
            .await
            .checkpoint_store_pause = Some((reached, resume));
    }
    pub async fn cancel_rebootstrap(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(());
        };
        let Some(staging) = inner.staging(id).await? else {
            return Ok(());
        };
        if staging.signed.id()? != checkpoint {
            return Ok(());
        }
        transaction!(inner, |inner| {
            if staging.direct {
                inner.abort_direct(scope, id).await?;
            } else {
                inner.clear_staging(id).await?;
            }
            Ok(())
        })?;
        inner.load_direct().await?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await?;
        drop(inner);
        self.settle().await?;
        Ok(())
    }

    pub async fn begin_rebootstrap(&self, signed: SignedCheckpoint) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.verify_import_statement(&signed).await?;
        let statement = &signed.statement;
        let scope = statement.scope;
        if matches!(
            inner.holding(scope).await?,
            Some(
                crate::replica::roles::Holding::Evicting | crate::replica::roles::Holding::Evicted
            )
        ) {
            return Err(denied("scope is not held by this replica"));
        }
        if signed.signer() == inner.config.node.public_key() {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
        let bytes = signed.to_bytes()?;
        if statement.no_frames() {
            transaction!(inner, |inner| {
                let id = inner.scope_id(scope).await?;
                inner.check_frontier(id, &signed).await?;
                if inner
                    .staging(id)
                    .await?
                    .is_some_and(|staging| staging.direct)
                {
                    inner.abort_direct(scope, id).await?;
                }
                inner.clear_staging(id).await?;
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_rebootstrap VALUES(?,?,NULL,1,?,0)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(bytes.clone()),
                            SqlValue::Blob(Sets::new().to_bytes()),
                        ]),
                    )
                    .await?;
                Ok(())
            })?;
            return inner.load_direct().await;
        }
        transaction!(inner, |inner| {
            let id = inner.scope_id(scope).await?;
            identity::check_node_chain(&signed.credentials)?;
            inner.check_frontier(id, &signed).await?;
            if inner
                .staging(id)
                .await?
                .is_some_and(|staging| staging.direct)
            {
                inner.abort_direct(scope, id).await?;
            }
            inner.clear_staging(id).await?;
            let direct = !inner.holds_anything(scope, id).await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_rebootstrap VALUES(?,?,NULL,0,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(bytes.clone()),
                        SqlValue::Blob(Sets::new().to_bytes()),
                        SqlValue::Integer(i64::from(direct)),
                    ]),
                )
                .await?;
            Ok(())
        })?;
        inner.load_direct().await
    }
    pub async fn rebootstrap_progress(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(Commitment, Option<Commitment>, bool)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(None);
        };
        match inner.staging(id).await? {
            Some(staging) => Ok(Some((
                staging.signed.id()?,
                staging.cursor,
                staging.complete,
            ))),
            None => Ok(None),
        }
    }
    pub async fn rebootstrap_signer(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<PublicKey>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(None);
        };
        Ok(inner
            .staging(id)
            .await?
            .map(|staging| staging.signed.signer()))
    }
    pub async fn stage_checkpoint_page(&self, page: CheckpointPage) -> Result<(), Error> {
        let span = tracing::debug_span!(
            "checkpoint_page",
            scope = rocky_core::scope_label(&page.scope),
            outcome = tracing::field::Empty
        );
        let result =
            tracing::Instrument::instrument(self.stage_checkpoint_page_inner(page), span.clone())
                .await;
        crate::network::trace::outcome(&span, &result);
        result
    }
    async fn stage_checkpoint_page_inner(&self, page: CheckpointPage) -> Result<(), Error> {
        if self
            .inner
            .lock(Class::Background)
            .await
            .replayed_scope(page.scope)
        {
            return self.stage_replayed_page(page).await;
        }
        let store = store_rows()?;
        #[cfg(feature = "qualification")]
        let metadata_step = crate::qualification_bound(
            "ROCKY_CHECKPOINT_STORE_METADATA",
            STORE_METADATA,
            PAGE_ROWS,
        )?;
        #[cfg(not(feature = "qualification"))]
        let metadata_step = STORE_METADATA;
        let scope = page.scope;
        let (checkpoint, after) = (page.checkpoint, page.after);
        let (key, signed, cursor, publisher, crypto) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            let (key, staging) = inner.expected_page(&page).await?;
            (
                key,
                staging.signed,
                staging.cursor,
                *inner.config.publisher.as_bytes(),
                inner.crypto.clone(),
            )
        };
        let (page, plain, supplied, origins) = rocky_platform::task::spawn_blocking(move || {
            let plain: PagePlain = decode(&page.rows.open(&key)?)?;
            let supplied: BTreeSet<_> = plain
                .headers
                .iter()
                .map(|header| moves::header_digest(header))
                .collect();
            let origins = plain
                .origins
                .iter()
                .map(|origin| moves::Origin::checkpoint_header(origin))
                .collect::<Result<Vec<_>, Error>>()?;
            Ok::<_, Error>((page, plain, supplied, origins))
        })
        .await
        .map_err(|_| Error::Closed)??;
        let mut known_headers = BTreeMap::new();
        for digest in origins {
            rocky_platform::task::yield_now().await;
            if !supplied.contains(&digest) && !known_headers.contains_key(&digest) {
                let inner = self.inner.lock(Class::Background).await;
                known_headers.insert(digest, inner.stored_creation_header(digest).await?);
            }
        }
        let opened = rocky_platform::task::spawn_blocking(move || {
            open_page(
                page,
                plain,
                &signed,
                cursor,
                publisher,
                known_headers,
                &crypto,
            )
        })
        .await
        .map_err(|_| Error::Closed)??;
        let validate = |staging: &Staging| -> Result<(), Error> {
            if staging.signed.id()? != checkpoint || staging.complete || staging.cursor != after {
                return Err(denied("unexpected checkpoint page"));
            }
            Ok(())
        };
        let (writes, mut source) = {
            let inner = self.inner.lock(Class::Background).await;
            let id = inner
                .find_scope_id(scope)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            let keys = [
                ("secure_scope_state", 0),
                ("secure_rows", 0),
                ("secure_checkpoint_base", 0),
                ("secure_checkpoint_stubs", 0),
                ("secure_rebootstrap", 0),
                ("secure_rebootstrap_rows", 0),
                ("secure_rebootstrap_stubs", 0),
                ("secure_rebootstrap_pages", 0),
                ("secure_stubs", 1),
            ]
            .into_iter()
            .map(|(table, column)| {
                (
                    inner
                        .conn
                        .names()
                        .physical(table)
                        .unwrap_or(table)
                        .to_owned(),
                    column,
                    id,
                )
            });
            let writes = inner.conn.connection().watch_integer_rows(keys)?;
            let version = (writes.count(), inner.authority, inner.workspace.tip);
            (writes, version)
        };
        for rows in opened.rows.chunks(store) {
            rocky_platform::task::yield_now().await;
            let inner = self.inner.lock(Class::Background).await;
            let version = (writes.count(), inner.authority, inner.workspace.tip);
            if source != version {
                return Err(Error::RetryExhausted);
            }
            let id = inner
                .find_scope_id(scope)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            let staging = inner
                .staging(id)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            validate(&staging)?;
            let placeholders = vec!["?"; rows.len()].join(",");
            let mut parameters = vec![SqlValue::Integer(id)];
            parameters.extend(
                rows.iter()
                    .map(|(_, _, record)| SqlValue::Blob(record.creation.as_bytes().to_vec())),
            );
            let stored = collect_rows(inner.conn.query(
                format!("SELECT hash,clock FROM secure_rebootstrap_stubs WHERE scope=? AND hash IN ({placeholders})"),
                bindings(parameters)).await?).await?;
            let mut clocks = BTreeMap::new();
            for record in stored {
                let [SqlValue::Blob(hash), SqlValue::Integer(clock)] = record.as_slice() else {
                    return Err(Error::Corrupt("checkpoint creation clock".into()));
                };
                clocks.insert(hash.clone(), stored_clock(*clock)?);
            }
            for (_, page_row, record) in rows {
                if clocks
                    .get(record.creation.as_bytes().as_slice())
                    .is_some_and(|clock| *clock != page_row.clock)
                {
                    return Err(denied("checkpoint creation clocks disagree"));
                }
            }
            if staging.direct {
                let mut parameters = vec![SqlValue::Integer(id)];
                parameters.extend(rows.iter().map(|(row, _, _)| SqlValue::Blob(row.to_vec())));
                let collisions = collect_rows(inner.conn.query(
                    format!("SELECT r.row FROM secure_rows r WHERE r.scope=? AND r.row IN ({placeholders}) AND NOT EXISTS(SELECT 1 FROM secure_checkpoint_rows c WHERE c.scope=r.scope AND c.row=r.row) LIMIT 1"),
                    bindings(parameters)).await?).await?;
                if !collisions.is_empty() {
                    return Err(Error::Corrupt(
                        "a checkpoint row is already stored during its import".into(),
                    ));
                }
            }
            #[cfg(test)]
            {
                drop(inner);
                let pause = self
                    .inner
                    .lock(Class::Background)
                    .await
                    .checkpoint_store_pause
                    .take();
                if let Some((reached, resume)) = pause {
                    let _ = reached.send(());
                    let _ = resume.await;
                }
            }
        }
        let metadata: Vec<_> = opened
            .headers
            .iter()
            .map(PageMetadata::Header)
            .chain(opened.bindings.iter().map(PageMetadata::Bindings))
            .chain(
                opened
                    .authors
                    .iter()
                    .map(|(author, signer)| PageMetadata::Author(author, signer)),
            )
            .collect();
        let split = metadata.len().saturating_sub(1) / metadata_step * metadata_step;
        let (leading_metadata, final_metadata) = metadata.split_at(split);
        for chunk in leading_metadata.chunks(metadata_step) {
            rocky_platform::task::yield_now().await;
            let mut inner = self.inner.lock(Class::Background).await;
            let id = inner
                .find_scope_id(scope)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            let staging = inner
                .staging(id)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            validate(&staging)?;
            if source != (writes.count(), inner.authority, inner.workspace.tip) {
                return Err(Error::RetryExhausted);
            }
            inner.verify_import_statement(&staging.signed).await?;
            #[cfg(any(test, feature = "qualification"))]
            inner.units("checkpoint_store_metadata", chunk.len() as u64);
            transaction!(@without_wal_checkpoint inner, |inner| inner.store_page_metadata(chunk).await)?;
            source = (writes.count(), inner.authority, inner.workspace.tip);
        }
        let finish_cursor = opened.rows.last().map(|(row, ..)| *row);
        let chunks = opened.rows.len().max(1).div_ceil(store);
        for chunk_index in 0..chunks {
            rocky_platform::task::yield_now().await;
            let mut inner = self.inner.lock(Class::Background).await;
            let id = inner
                .find_scope_id(scope)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            let staging = inner
                .staging(id)
                .await?
                .ok_or_else(|| denied("no rebootstrap in progress"))?;
            validate(&staging)?;
            if source != (writes.count(), inner.authority, inner.workspace.tip) {
                return Err(Error::RetryExhausted);
            }
            inner.verify_import_statement(&staging.signed).await?;
            let start = (chunk_index * store).min(opened.rows.len());
            let end = (start + store).min(opened.rows.len());
            let table = if staging.direct {
                "secure_checkpoint_rows"
            } else {
                "secure_rebootstrap_rows"
            };
            let partial = collect_rows(
                inner
                    .conn
                    .query(
                        format!(
                            "SELECT row FROM {table} WHERE scope=? AND row>? ORDER BY row LIMIT ?"
                        ),
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(after.map(|row| row.to_vec()).unwrap_or_default()),
                            SqlValue::Integer(PAGE_ROWS as i64 + 1),
                        ]),
                    )
                    .await?,
            )
            .await?;
            if partial.len() > PAGE_ROWS
                || staging.sets.rows < partial.len() as u64
                || after.is_none() && staging.sets.rows != partial.len() as u64
            {
                return Err(Error::Corrupt("checkpoint partial row count".into()));
            }
            for record in &partial {
                let [SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("checkpoint partial row key".into()));
                };
                let key = row_of(bytes)?;
                if opened.next.is_none_or(|through| key <= through)
                    && opened
                        .rows
                        .binary_search_by_key(&key, |(row, ..)| *row)
                        .is_err()
                {
                    return Err(Error::Corrupt(
                        "checkpoint partial rows differ from the signed page".into(),
                    ));
                }
            }
            let mut kept = BTreeMap::new();
            if start < end {
                let placeholders = vec!["?"; end - start].join(",");
                let mut parameters = vec![SqlValue::Integer(id)];
                parameters.extend(
                    opened.rows[start..end]
                        .iter()
                        .map(|(row, _, _)| SqlValue::Blob(row.to_vec())),
                );
                let stored = collect_rows(inner.conn.query(
                    format!("SELECT row,fields FROM {table} WHERE scope=? AND row IN ({placeholders})"),
                    bindings(parameters)).await?).await?;
                for record in stored {
                    let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = record.as_slice() else {
                        return Err(Error::Corrupt("checkpoint partial row".into()));
                    };
                    kept.insert(row_of(row)?, bytes.clone());
                }
            }
            let mut fresh = Vec::new();
            let mut delta = Sets::new();
            for (row, page_row, record) in &opened.rows[start..end] {
                if let Some(bytes) = kept.get(row) {
                    if encode(&inner.decode_record(bytes).await?)? != encode(record)? {
                        return Err(Error::Corrupt("checkpoint partial row changed".into()));
                    }
                } else {
                    delta.replace_materialized(row, None, record)?;
                    fresh.push((*row, page_row.clone(), record.clone()));
                }
            }
            if partial.len() + fresh.len() > PAGE_ROWS {
                return Err(Error::Quota {
                    resource: "checkpoint partial rows",
                    limit: PAGE_ROWS,
                });
            }
            let metadata = if chunk_index == 0 {
                final_metadata
            } else {
                &[]
            };
            if fresh.is_empty() && metadata.is_empty() && chunk_index + 1 != chunks {
                continue;
            }
            let chunk = PageStep {
                rows: fresh,
                next: opened.next,
                signature: opened.signature,
                delta,
            };
            #[cfg(any(test, feature = "qualification"))]
            {
                inner.row_work(store as u64);
                inner.units("checkpoint_store_metadata", metadata.len() as u64);
                inner.units("checkpoint_store_row", chunk.rows.len() as u64);
            }
            transaction!(@without_wal_checkpoint inner, |inner| {
                inner.store_page_metadata(metadata).await?;
                inner.store_page(scope, id, &staging, &chunk, if chunk_index + 1 == chunks { Some(finish_cursor) } else { None }).await
            })?;
            source = (writes.count(), inner.authority, inner.workspace.tip);
            if staging.direct {
                inner.publish_revision().await?;
            }
            if chunk_index + 1 == chunks {
                inner.imported.send_modify(|pages| *pages += 1);
            }
        }
        Ok(())
    }
    async fn stage_replayed_page(&self, page: CheckpointPage) -> Result<(), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let (key, staging) = inner.expected_page(&page).await?;
        let plain: PagePlain = decode(&page.rows.open(&key)?)?;
        let rows = plain
            .rows
            .iter()
            .map(|row| Ok((row_of(&row.row)?, row.fields.clone())))
            .collect::<Result<Vec<_>, Error>>()?;
        if plain.checkpoint != page.checkpoint
            || plain.after != page.after
            || plain.next != page.next
            || rows.len() > PAGE_ROWS
            || plain.next.is_some() && plain.next != rows.last().map(|(row, _)| *row)
            || !(plain.legend.is_empty()
                && plain.origins.is_empty()
                && plain.creations.is_empty()
                && plain.authors.is_empty()
                && plain.bindings.is_empty()
                && plain.headers.is_empty())
            || plain.rows.iter().any(|row| row.clock != 0)
        {
            return Err(denied("checkpoint page binding mismatch"));
        }
        if !staging
            .signed
            .signer()
            .verify(&page_signing_bytes(&plain)?, &plain.signature)
        {
            return Err(denied("checkpoint page signature mismatch"));
        }
        let mut last = staging.cursor;
        for (row, _) in &rows {
            if last.is_some_and(|last| *row <= last) {
                return Err(denied("checkpoint rows out of order"));
            }
            last = Some(*row);
        }
        inner.check_base_identities(
            page.scope,
            staging.signed.statement.tip,
            &rows,
            &staging.signed.statement.identities,
        )?;
        let id = inner
            .find_scope_id(page.scope)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        transaction!(inner, |inner| {
            if staging.direct {
                inner
                    .restore_images(page.scope, staging.signed.statement.tip, &rows)
                    .await?;
            }
            let mut sets = staging.sets.clone();
            let fields = replay::fields_of(rows.iter().map(|(_, image)| image.as_slice()))?;
            let ordinals = inner.field_ordinals(fields).await?;
            for (row, image) in &rows {
                replay::add_row(&mut sets, row, image);
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_replayed_staged VALUES(?,?,?)",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(row.to_vec()),
                            SqlValue::Blob(ordinals.pack(image)?),
                        ]),
                    )
                    .await?;
            }
            let row = |row: Option<Commitment>| {
                row.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec()))
            };
            inner
                .conn
                .execute(
                    "INSERT INTO secure_rebootstrap_pages VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(page.after.map(|row| row.to_vec()).unwrap_or_default()),
                        row(plain.next),
                        SqlValue::Blob(plain.signature.to_bytes().to_vec()),
                    ]),
                )
                .await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_rebootstrap SET cursor=?,complete=?,sets=? WHERE scope=?",
                    bindings(vec![
                        row(last),
                        SqlValue::Integer(i64::from(plain.next.is_none())),
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Integer(id),
                    ]),
                )
                .await?;
            inner.fault(Stage::Import)
        })?;
        inner.imported.send_modify(|pages| *pages += 1);
        Ok(())
    }
    pub async fn imported_rows(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<u64, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(0);
        };
        if let Some(staging) = inner.staging(id).await?
            && staging.signed.id()? == checkpoint
        {
            return Ok(staging.sets.rows);
        }
        Ok(match inner.checkpoint_record(id, true).await? {
            Some(Record {
                signed: Some(signed),
                ..
            }) if signed.id()? == checkpoint => signed.statement.rows.count,
            _ => 0,
        })
    }
    pub async fn finish_rebootstrap(
        &self,
        scope: ResourceScope,
    ) -> Result<RebootstrapReport, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let time = now()?;
        let generation = inner
            .scope_key(scope)
            .ok()
            .map(|(generation, _)| generation);
        let id = inner
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        let staging = inner
            .staging(id)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        if !staging.complete {
            return Err(denied("checkpoint rows are incomplete"));
        }
        let signed = staging.signed;
        let direct = staging.direct;
        signed.verify_authority(&inner, time)?;
        let statement = signed.statement.clone();
        if statement.no_frames()
            && let Err(error) = inner.check_no_frames(&signed).await
        {
            if matches!(error, Error::Rejected(_) | Error::Protocol(_)) {
                transaction!(inner, |inner| inner.clear_staging(id).await)?;
                inner.load_direct().await?;
                drop(flush_guard(inner, &self.inner).await?);
            }
            return Err(error);
        }
        if inner.graphs.contains_key(&scope.domain) && statement.seal.is_none() {
            transaction!(inner, |inner| match direct {
                true => inner.abort_direct(scope, id).await,
                false => inner.clear_staging(id).await,
            })?;
            inner.load_direct().await?;
            drop(flush_guard(inner, &self.inner).await?);
            return Err(denied(
                "with a tip graph an imported checkpoint sits at a seal",
            ));
        }
        let moves = match direct {
            true => inner.imported_moves(id).await?,
            false => inner.rebootstrap_moves(id).await?,
        };
        if moves != statement.moves
            || staging.sets.rows_summary() != statement.rows
            || staging.sets.tombstones != statement.tombstones
            || staging.sets.creations_summary() != statement.creations
            || (!statement.no_frames()
                && (generation.is_none() || generation != statement.key_epoch()))
        {
            transaction!(inner, |inner| match direct {
                true => inner.abort_direct(scope, id).await,
                false => inner.clear_staging(id).await,
            })?;
            inner.load_direct().await?;
            inner.publish_revision().await?;
            return Err(Error::Corrupt(
                "checkpoint rows differ from the signed checkpoint".into(),
            ));
        }
        let sets = staging.sets;
        inner.direct.remove(&scope);
        let report = transaction!(inner, |inner| {
            inner
                .switch_to_staged(scope, id, &signed, &sets, direct, time)
                .await
        });
        inner.load_direct().await?;
        let report = report?;
        inner.verified_at.remove(&id);
        inner.publish_revision().await?;
        drop(inner);
        self.settle().await?;
        Ok(report)
    }
}

impl<A: RowApplication> Inner<A> {
    async fn keep_uncovered_own_endpoint(
        &mut self,
        scope: ResourceScope,
        id: i64,
        frontier: &[LogPosition],
    ) -> Result<(), Error> {
        let node = self.config.node.public_key();
        if let Some(log) = self.log(id, node).await?
            && log.pruned_prefix
                > frontier
                    .iter()
                    .find(|position| position.author == node)
                    .map_or(-1, |position| position.sequence as i64)
        {
            self.merge_restoration_guard(scope, (log.sequence, Some(log.hash)))
                .await?;
        }
        Ok(())
    }
    pub(super) async fn switch_to_staged(
        &mut self,
        scope: ResourceScope,
        id: i64,
        signed: &SignedCheckpoint,
        sets: &Sets,
        direct: bool,
        time: u64,
    ) -> Result<RebootstrapReport, Error> {
        let off_path = self.base_off_path(scope, id).await?;
        self.forget_import_failures(scope).await?;
        self.clear_seal_anchor(id).await?;
        for table in ["secure_seal_bounds", "secure_seal_drops"] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        let statement = signed.statement.clone();
        if self.graphs.contains_key(&scope.domain) && statement.seal.is_none() {
            return Err(denied(
                "with a tip graph an imported checkpoint sits at a seal",
            ));
        }
        let key = match statement.no_frames() {
            true => None,
            false => Some(self.scope_key(scope)?),
        };
        if key.is_some() {
            identity::check_node_chain(&signed.credentials)?;
        }
        self.check_frontier(id, signed).await?;
        if off_path {
            self.keep_uncovered_own_endpoint(scope, id, &statement.frontier)
                .await?;
        }
        let mut report = RebootstrapReport {
            checkpoint: signed.id()?,
            superseded: 0,
            reapplied: vec![],
            rejected: vec![],
            pending: vec![],
        };
        let replayed = self.replayed_scope(scope);
        let rewrite = !replayed && (!direct || self.applied_any(id).await?);
        self.check_statement_marks(scope, &statement.identities)
            .await?;
        if !replayed {
            self.check_covered_identities(
                match direct {
                    true => "secure_checkpoint_rows",
                    false => "secure_rebootstrap_rows",
                },
                id,
                &statement.identities,
            )
            .await?;
            self.check_row_provenance(
                scope,
                match direct {
                    true => "secure_checkpoint_rows",
                    false => "secure_rebootstrap_rows",
                },
                id,
            )
            .await?;
        }
        if replayed {
            let ordinals = self.stored_field_ordinals().await?;
            let mut after = None;
            loop {
                let rows = self
                    .stored_page("secure_replayed_staged", id, after, &ordinals)
                    .await?;
                let Some((last, _)) = rows.last() else {
                    break;
                };
                after = Some(*last);
                self.check_base_identities(scope, statement.tip, &rows, &statement.identities)
                    .map_err(|_| {
                        Error::Corrupt("a staged row exceeds its permanent mark".into())
                    })?;
            }
        }
        let reapply = self.supersede(scope, &statement, &mut report).await?;
        if replayed {
            self.clear_replayed(scope).await?;
            self.restore_staged(scope, statement.tip, id).await?;
            self.mark_rederive_if_orphaned(scope, id).await?;
            self.cover_kept(scope, id, &statement).await?;
            self.store_staged_base(id).await?;
        }
        let blob = encode(&scope)?;
        if rewrite {
            self.in_rows_bulk(scope, async {
                self.conn
                    .execute("DELETE FROM secure_row_records WHERE scope=?", [blob.clone()])
                    .await?;
                self
                    .conn
                    .execute(
                        match direct {
                            true => "INSERT INTO secure_row_records SELECT ?,row,fields FROM secure_checkpoint_rows WHERE scope=?",
                            false => "INSERT INTO secure_row_records SELECT ?,row,fields FROM secure_rebootstrap_rows WHERE scope=?",
                        },
                        bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(id)]),
                    )
                    .await?;
                if direct {
                    self.conn.execute(SHARE_BASE, [id]).await?;
                }
                Ok(())
            })
            .await?;
        }
        for mark in &statement.identities {
            self.put_identity_mark(
                scope,
                &mark.mark(),
                identity::Raise::Statement { own: mark.own },
            )
            .await?;
        }
        self.conn
            .execute(
                "DELETE FROM secure_identity_ledger WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        for mark in &statement.identities {
            if let Some(own) = mark.own_mark() {
                self.fold_ledger(scope, &own).await?;
            }
        }
        if rewrite {
            self.reproject(scope).await?;
        }
        if !direct {
            for sql in [
                "DELETE FROM secure_checkpoint_base WHERE scope=?",
                "INSERT INTO secure_checkpoint_rows SELECT scope,row,fields FROM secure_rebootstrap_rows WHERE scope=?",
            ] {
                self.conn.execute(sql, [id]).await?;
            }
        }
        self.index_page_headers(id, &served::Served::Base).await?;
        if rewrite {
            self.conn
                .execute("DELETE FROM secure_stubs WHERE scope=?", [id])
                .await?;
        }
        for sql in [
            "DELETE FROM secure_checkpoint_overlay WHERE scope=?",
            "DELETE FROM secure_checkpoint_scratch WHERE scope=?",
            "INSERT INTO secure_stubs SELECT hash,scope,clock,1 FROM secure_rebootstrap_stubs WHERE scope=? ON CONFLICT(hash) DO UPDATE SET creation=1",
            "DELETE FROM secure_logs WHERE scope=?",
            "DELETE FROM secure_checkpoints WHERE scope=?",
            "DELETE FROM secure_checkpoint_stubs WHERE scope=?",
            "INSERT INTO secure_checkpoint_stubs SELECT * FROM secure_rebootstrap_stubs WHERE scope=?",
            "DELETE FROM secure_checkpoint_pages WHERE scope=?",
            "INSERT INTO secure_checkpoint_pages SELECT * FROM secure_rebootstrap_pages WHERE scope=?",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        for log in &statement.frontier {
            self.conn
                .execute(
                    "INSERT INTO secure_logs VALUES(?,?,?,?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(log.author.as_bytes().to_vec()),
                        SqlValue::Integer(log.sequence as i64),
                        SqlValue::Blob(log.hash.as_bytes().to_vec()),
                        clock_value(log.clock)?,
                        SqlValue::Integer(log.sequence as i64),
                    ]),
                )
                .await?;
        }
        if replayed {
            for log in &statement.frontier {
                self.conn
                    .execute(
                        "INSERT INTO secure_stubs VALUES(?,?,?,0) ON CONFLICT(hash) DO NOTHING",
                        bindings(vec![
                            SqlValue::Blob(log.hash.as_bytes().to_vec()),
                            SqlValue::Integer(id),
                            clock_value(log.clock)?,
                        ]),
                    )
                    .await?;
            }
        }
        match replayed {
            true => {
                self.store_scope_state(id, 0, 0, &LtHash16::new(), statement.operations)
                    .await?
            }
            false => {
                self.store_scope_state(
                    id,
                    sets.rows,
                    sets.tombstones,
                    &sets.digest,
                    statement.operations,
                )
                .await?
            }
        }
        self
            .conn
            .execute(
                "INSERT INTO secure_checkpoints SELECT ?,?,COALESCE(MAX(seq),0),?,? FROM sqlite_sequence WHERE name='secure_applied'",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Integer(statement.sequence as i64),
                    SqlValue::Blob(sets.to_bytes()),
                    SqlValue::Blob(signed.to_bytes()?),
                ]),
            )
            .await?;
        if let Some((generation, key)) = key {
            self.conn
                .execute(
                    "INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO NOTHING",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Blob(encode(&generation)?),
                        SqlValue::Blob(rocky_core::commitment("rocky.scope-key.v1", &key).to_vec()),
                    ]),
                )
                .await?;
        }
        self.keep_current(id, scope, true).await?;
        for hash in reapply {
            let frame = self
                .frame(hash)
                .await?
                .ok_or_else(|| Error::Corrupt("retained operation".into()))?;
            if self.receipt_of(hash).await?.is_none() && !self.birth_marks(&frame)?.is_empty() {
                return Err(Error::Corrupt(UNJOURNALED_BIRTH.into()));
            }
            if self.frame_reach(&frame)? == tips::FrameReach::Unfollowed {
                let operation = frame.operation_ref()?;
                self.note_applied(id, operation).await?;
                self.note_row_positions(id, &frame, &self.row_payload(&frame)?)
                    .await?;
                self.conn
                    .execute(
                        "UPDATE secure_operations SET status='applied' WHERE hash=?",
                        [hash.as_bytes().to_vec()],
                    )
                    .await?;
                let (rows, tombstones, digest, applied) = self.scope_state(id).await?;
                self.store_scope_state(id, rows, tombstones, &digest, applied + 1)
                    .await?;
                report.reapplied.push(hash);
                continue;
            }
            match self.apply_frame(&frame, time).await? {
                incoming::Outcome::Applied => match self.consumed(hash).await? {
                    Some(reason) => report.rejected.push((hash, reason)),
                    None => report.reapplied.push(hash),
                },
                incoming::Outcome::Rejected(reason) => report.rejected.push((hash, reason)),
                incoming::Outcome::Waiting(_)
                | incoming::Outcome::Blocked(_)
                | incoming::Outcome::Ahead(_) => report.pending.push(hash),
            }
        }
        self.reseed_ledger(scope, id).await?;
        self.rederive_replayed(Some(scope), time).await?;
        self.clear_staging(id).await?;
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        self.fault(Stage::Switch)?;
        Ok(report)
    }
}

impl<A: RowApplication> Inner<A> {
    async fn check_statement_marks(
        &self,
        scope: ResourceScope,
        marks: &[CoveredMark],
    ) -> Result<(), Error> {
        let (_, allocation) = self.allocation_of(scope).await?;
        for held in [
            self.identity_marks(scope).await?,
            self.identity_marks(allocation).await?,
        ] {
            if marks.iter().any(|mark| {
                held.iter()
                    .any(|held| held.author == mark.author && held.signer != mark.signer)
            }) {
                return Err(denied("checkpoint writer belongs to another signing key"));
            }
        }
        Ok(())
    }
    pub(super) async fn check_row_provenance(
        &self,
        scope: ResourceScope,
        rows: &str,
        id: i64,
    ) -> Result<(), Error> {
        let Some((allocation, _)) = self.allocation_binding(scope).await? else {
            return Ok(());
        };
        let mut after = Vec::new();
        loop {
            let records = collect_rows(
                self.conn
                    .query(
                        format!("SELECT row,fields FROM {rows} WHERE scope=? AND row>? ORDER BY row LIMIT 256"),
                        bindings(vec![SqlValue::Integer(id), SqlValue::Blob(after.clone())]),
                    )
                    .await?,
            )
            .await?;
            for record in &records {
                let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("checkpoint row".into()));
                };
                after = row.clone();
                if fields.is_empty() {
                    continue;
                }
                self.check_record_provenance(allocation, &self.decode_record(fields).await?)
                    .await?;
            }
            if records.len() < 256 {
                return Ok(());
            }
        }
    }
    pub(super) async fn check_record_provenance(
        &self,
        allocation: i64,
        record: &Materialized,
    ) -> Result<(), Error> {
        let (Some(identity), Some(proof)) = (record.identity, &record.proof) else {
            return Ok(());
        };
        let provenance = proof.provenance()?;
        if self
            .claims_on(allocation, identity)
            .await?
            .iter()
            .any(|claim| claim.provenance != provenance)
        {
            return Err(denied(
                "a checkpoint row contradicts a known original creation",
            ));
        }
        Ok(())
    }
    async fn check_frontier(&self, id: i64, signed: &SignedCheckpoint) -> Result<(), Error> {
        if self.base_off_path(signed.statement.scope, id).await? {
            return Ok(());
        }
        for (log, pruned_prefix) in self.logs(id, None, true, MAX_CHECKPOINT_LOGS + 1).await? {
            if !signed.statement.frontier.iter().any(|position| {
                position.author == log.author && position.sequence as i64 >= pruned_prefix
            }) {
                return Err(denied("checkpoint is behind this replica's pruned_prefix"));
            }
        }
        Ok(())
    }
    async fn supersede(
        &self,
        scope: ResourceScope,
        statement: &CheckpointStatement,
        report: &mut RebootstrapReport,
    ) -> Result<Vec<Hash>, Error> {
        let id = self.scope_id(scope).await?;
        let mut reapply = Vec::new();
        let mut cursor = Vec::new();
        let mut bytes = 0;
        let mut covered = BTreeMap::new();
        let mut named = BTreeSet::new();
        loop {
            let records = collect_rows(self.conn.query(
                "SELECT o.hash,o.header,o.status,LENGTH(o.header)+LENGTH(o.body),(SELECT position FROM secure_applied a WHERE a.hash=o.hash) FROM secure_operations o WHERE o.scope=? AND o.hash>? ORDER BY o.hash LIMIT 32",
                [encode(&scope)?, cursor.clone()],
            ).await?).await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let [
                    SqlValue::Blob(hash),
                    SqlValue::Blob(encoded),
                    SqlValue::Text(status),
                    SqlValue::Integer(size),
                    position,
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("retained operation".into()));
                };
                cursor = hash.clone();
                let header: p2panda_core::Header<Extensions> = decode(encoded)?;
                let covering = statement
                    .frontier
                    .iter()
                    .find(|log| log.author == header.public_key && header.seq_num <= log.sequence);
                if let Some(log) = covering {
                    if header.seq_num == log.sequence && log.hash.as_bytes().as_slice() != hash {
                        return Err(denied("checkpoint frontier forks a retained log"));
                    }
                    covered.insert(hash_of(hash)?, header.timestamp);
                    self.place_superseded(&header, encoded).await?;
                    self.keep_covered(id, hash, &statement.outcomes).await?;
                    for sql in [
                        "DELETE FROM secure_operations WHERE hash=?",
                        "DELETE FROM secure_outbox WHERE hash=?",
                        "DELETE FROM secure_unoffered WHERE hash=?",
                        "DELETE FROM secure_ack WHERE hash=?",
                        "DELETE FROM secure_applied WHERE hash=?",
                        "DELETE FROM secure_publication_frames WHERE hash=?",
                    ] {
                        self.conn.execute(sql, [hash.clone()]).await?;
                    }
                    bytes += *size;
                    report.superseded += 1;
                    continue;
                }
                named.extend(
                    header
                        .previous
                        .iter()
                        .chain(header.backlink.iter())
                        .copied(),
                );
                if status == "applied" {
                    let SqlValue::Integer(position) = position else {
                        return Err(Error::Corrupt("applied position".into()));
                    };
                    self.conn
                        .execute(
                            "UPDATE secure_operations SET status='pending' WHERE hash=?",
                            [hash.clone()],
                        )
                        .await?;
                    self.conn
                        .execute("DELETE FROM secure_applied WHERE hash=?", [hash.clone()])
                        .await?;
                    reapply.push((*position, hash_of(hash)?));
                }
            }
        }
        self.clear_applied_cache();
        self.conn
            .execute(
                "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                [report.superseded as i64, bytes],
            )
            .await?;
        for (hash, clock) in covered {
            if named.contains(&hash) {
                self.put_stub(hash, id, clock, false).await?;
            }
        }
        if report.superseded > 0 {
            self.sweep_chains().await?;
        }
        reapply.sort();
        Ok(reapply.into_iter().map(|(_, hash)| hash).collect())
    }
}

#[cfg(test)]
pub(crate) fn forge_page(
    page: &mut CheckpointPage,
    key: &[u8; 32],
    edit: impl FnOnce(&mut Vec<(Vec<u8>, Vec<u8>, u64)>),
) {
    let mut plain: PagePlain = decode(&page.rows.open(key).unwrap()).unwrap();
    let mut rows = plain
        .rows
        .into_iter()
        .map(|row| (row.row, row.fields, row.clock))
        .collect();
    edit(&mut rows);
    plain.rows = rows
        .into_iter()
        .map(|(row, fields, clock)| PageRow { row, fields, clock })
        .collect();
    page.rows = EncryptedPackage::seal(
        page.rows.scope,
        page.rows.generation,
        PackageKind::Checkpoint,
        key,
        &encode(&plain).unwrap(),
    )
    .unwrap();
}
#[cfg(test)]
pub(crate) fn forge_page_legend(
    page: &mut CheckpointPage,
    key: &[u8; 32],
    signer: &PrivateKey,
    edit: impl FnOnce(&mut Vec<String>, &mut Vec<Hash>, &mut Vec<serde_bytes::ByteBuf>),
) {
    let mut plain: PagePlain = decode(&page.rows.open(key).unwrap()).unwrap();
    edit(&mut plain.legend, &mut plain.creations, &mut plain.origins);
    plain.signature = signer.sign(&page_signing_bytes(&plain).unwrap());
    page.rows = EncryptedPackage::seal(
        page.rows.scope,
        page.rows.generation,
        PackageKind::Checkpoint,
        key,
        &encode(&plain).unwrap(),
    )
    .unwrap();
}

#[cfg(test)]
pub(crate) fn forge_page_clocks(
    page: &mut CheckpointPage,
    key: &[u8; 32],
    signer: &PrivateKey,
    all: bool,
) {
    let mut plain: PagePlain = decode(&page.rows.open(key).unwrap()).unwrap();
    for row in plain.rows.iter_mut().take(if all { usize::MAX } else { 1 }) {
        row.clock += 1;
    }
    plain.signature = signer.sign(&page_signing_bytes(&plain).unwrap());
    page.rows = EncryptedPackage::seal(
        page.rows.scope,
        page.rows.generation,
        PackageKind::Checkpoint,
        key,
        &encode(&plain).unwrap(),
    )
    .unwrap();
}
