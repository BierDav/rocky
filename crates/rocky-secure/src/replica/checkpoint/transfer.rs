//! The pruned_prefix signal, checkpoint transfer and rebootstrap.
use super::*;

/// Logs of a scope whose operations up to a sequence were pruned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrunedPage {
    pub scope: ResourceScope,
    pub logs: Vec<(PublicKey, u64)>,
    pub next: Option<PublicKey>,
}
/// Checkpoint rows in row order, encrypted with the scope key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointPage {
    pub scope: ResourceScope,
    pub checkpoint: Commitment,
    pub after: Option<Commitment>,
    #[serde(with = "sealed")]
    pub rows: EncryptedPackage,
    pub next: Option<Commitment>,
}
/// The sealed rows with their ciphertext as one CBOR byte string.
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
/// A stored row record and the clock of the row's creation operation.
#[derive(Serialize, Deserialize)]
struct PageRow {
    #[serde(with = "serde_bytes")]
    row: Vec<u8>,
    #[serde(with = "serde_bytes")]
    fields: Vec<u8>,
    clock: u64,
}
/// Encoded bytes of a page row beyond its stored record.
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
    /// Each set of author bindings the rows' proofs name, once. The rows
    /// are signed and name them by digest, so the sets need no signature.
    bindings: Vec<serde_bytes::ByteBuf>,
    headers: Vec<serde_bytes::ByteBuf>,
    /// The checkpoint signer's signature of `page_signing_bytes`: a peer
    /// relaying the checkpoint can neither alter nor move a page's rows.
    signature: Signature,
}
/// What the checkpoint signer signs of a page: its place and its rows.
fn page_signing_bytes(plain: &PagePlain) -> Result<Vec<u8>, Error> {
    let rows = rocky_core::commitment(
        "rocky.checkpoint-page-rows.v1",
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
    signing_bytes("rocky.checkpoint-page.v1", &rows)
}
/// A page opened and checked against its signature, before it is stored.
struct OpenedPage {
    rows: Vec<(Commitment, PageRow, Materialized)>,
    /// The encoded sets of author bindings the rows name, checked.
    bindings: Vec<Vec<u8>>,
    headers: Vec<Vec<u8>>,
    authors: stamp_authors::Authors,
    next: Option<Commitment>,
    signature: Signature,
    /// The page's rows as a set of their own.
    delta: Sets,
}
/// Opens a page sealed under `key` and verifies it against the staged
/// checkpoint: binding, order and the signer's signature. Pure
/// computation, run off the replica's thread.
fn open_page(
    page: CheckpointPage,
    key: [u8; 32],
    signed: &SignedCheckpoint,
    cursor: Option<Commitment>,
    publisher: [u8; 32],
    known_headers: BTreeMap<Commitment, Vec<u8>>,
) -> Result<OpenedPage, Error> {
    let plain: PagePlain = decode(&page.rows.open(&key)?)?;
    if plain.checkpoint != page.checkpoint
        || plain.after != page.after
        || plain.next != page.next
        || plain.rows.is_empty() && plain.next.is_some()
        || plain.rows.len() > STEP_ROWS
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
    // A page carries exactly the binding sets its rows name.
    let mut sets = BTreeMap::new();
    for set in &plain.bindings {
        decode::<Vec<rocky_credentials::AuthorBinding>>(set)?;
        if sets.insert(moves::bindings_digest(set), false).is_some() {
            return Err(denied("checkpoint page repeats author bindings"));
        }
    }
    let mut headers = BTreeMap::new();
    for header in &plain.headers {
        crate::wire::verified_header(header)?;
        if headers
            .insert(moves::header_digest(header), false)
            .is_some()
        {
            return Err(denied("checkpoint page repeats a creation header"));
        }
    }
    let mut delta = Sets::new();
    let mut last = cursor;
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
        delta.replace(&id, None, &encode(&record)?)?;
        if let Some(proof) = &record.proof {
            if let std::collections::btree_map::Entry::Vacant(entry) = headers.entry(proof.header())
            {
                let bytes = known_headers
                    .get(&proof.header())
                    .ok_or_else(|| denied("checkpoint page lacks a creation header"))?;
                if moves::header_digest(bytes) != proof.header() {
                    return Err(denied("checkpoint header digest mismatch"));
                }
                crate::wire::verified_header(bytes)?;
                entry.insert(true);
            }
            *headers
                .get_mut(&proof.header())
                .ok_or_else(|| denied("checkpoint page lacks a creation header"))? = true;
            *sets
                .get_mut(&proof.bindings())
                .ok_or_else(|| denied("checkpoint page lacks a row's author bindings"))? = true;
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
            .map(|header| header.into_vec())
            .collect(),
        bindings: plain
            .bindings
            .into_iter()
            .map(|set| set.into_vec())
            .collect(),
        next: plain.next,
        signature: plain.signature,
        delta,
    })
}
/// What a rebootstrap did with the operations this replica held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootstrapReport {
    pub checkpoint: Commitment,
    /// Held operations the checkpoint covers; their copies were dropped.
    pub superseded: u64,
    /// Operations beyond the checkpoint that were applied again on top of it.
    pub reapplied: Vec<Hash>,
    /// Operations beyond the checkpoint that no longer apply, with the reason.
    pub rejected: Vec<(Hash, String)>,
    /// Operations beyond the checkpoint that wait for a dependency or key.
    pub pending: Vec<Hash>,
}
pub(in crate::replica) struct Staging {
    pub(in crate::replica) signed: SignedCheckpoint,
    pub(super) cursor: Option<Commitment>,
    pub(in crate::replica) complete: bool,
    pub(super) sets: Sets,
    /// The scope held nothing when the import began: verified pages go
    /// straight into its stored rows and projection.
    pub(super) direct: bool,
}

impl<A: RowApplication> Inner<A> {
    /// Custody or read authority of a peer over a whole scope.
    pub(crate) fn authorize_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.same_tip_peer(peer)?;
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &[scope])?;
        if !crate::replica::roles::holds_whole(&grants, scope) {
            return Err(denied("ciphertext custody denied"));
        }
        Ok(())
    }
    /// As `authorize_peer` for a coordinated log's entries: a peer also
    /// takes those of a sealed tip its followed tip descends from, the
    /// state it starts from.
    pub(crate) fn authorize_log_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        log: rocky_core::TipLog,
    ) -> Result<(), Error> {
        let inherited = match (self.peer_tip(peer), self.graphs.get(&log.scope)) {
            (Some(tip), Some(graph)) => Self::descends(graph, tip, log.tip)?,
            _ => false,
        };
        if !inherited {
            return self.authorize_peer(peer, credentials, scope);
        }
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &[scope])?;
        if !crate::replica::roles::holds_whole(&grants, scope) {
            return Err(denied("ciphertext custody denied"));
        }
        Ok(())
    }
    pub(crate) fn authorize_log_frame(
        &self,
        peer: PublicKey,
        log: rocky_core::TipLog,
        frame: &EncryptedFrame,
    ) -> Result<(), Error> {
        let Some(mut tip) = self.peer_tip(peer) else {
            return Ok(());
        };
        let Some(graph) = self.graphs.get(&log.scope) else {
            return Ok(());
        };
        let header = frame.operation()?.header;
        let mut visited = BTreeSet::new();
        while tip != log.tip && visited.insert(tip) {
            for opening in &graph.openings {
                if opening.tip().map_err(credential)? == tip {
                    let seal = graph
                        .seal_by_id(opening.statement.seal)
                        .map_err(credential)?;
                    let covered = seal.statement.frontier.iter().any(|mark| {
                        Some(mark.author) == header.extensions.author
                            && header.extensions.author_sequence <= mark.sequence
                    });
                    if !covered {
                        return Err(denied(
                            "ancestor frame exceeds the branch's inherited frontier",
                        ));
                    }
                }
            }
            match Self::tip_parent(graph, tip)? {
                Some((parent, _)) => tip = parent,
                None => break,
            }
        }
        Ok(())
    }
    /// Whether `tip` continues or opens on a seal of `ancestor`, directly
    /// or through further seals.
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
    /// The checks an imported statement passes now: signature and current
    /// signer authority, a seal in a graph domain, the tip's schema
    /// generation, and the installed key epoch or a no-frames shape that
    /// local history does not contradict.
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
    /// Whether `scope` shows the verified pages of a direct import: its
    /// staged statement still passes the import checks now, at a seal of
    /// the followed path. Its rows are incomplete until it finishes.
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
    /// Whether the scope is being imported from a checkpoint: a staged
    /// import, or the owner's intent while the scope holds no rows yet. A
    /// scope that holds rows keeps syncing history until its import begins.
    pub(crate) async fn importing(&self, scope: ResourceScope) -> Result<bool, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(self.importing.contains(&scope));
        };
        if self.importing.contains(&scope) {
            let mut held = self
                .conn
                .query(
                    "SELECT 1 FROM secure_row_records WHERE scope=? LIMIT 1",
                    [encode(&scope)?],
                )
                .await?;
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
    /// The key and staged import a page must match, before it is opened.
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
    /// Stores one opened page in the caller's transaction: staged for the
    /// switch, or for a direct import stored and projected.
    async fn store_page(
        &mut self,
        scope: ResourceScope,
        id: i64,
        staging: &Staging,
        page: &OpenedPage,
        after: Option<Commitment>,
    ) -> Result<(), Error> {
        let statement = &staging.signed.statement;
        self.check_statement_marks(scope, &statement.identities)
            .await?;
        if let Some((allocation, _)) = self.allocation_binding(scope).await? {
            for (_, _, record) in &page.rows {
                self.check_record_provenance(allocation, record).await?;
            }
        }
        for (author, signer) in &page.authors {
            self.keep_stamp_author(scope.domain, *author, *signer)
                .await?;
        }
        for header in &page.headers {
            self.put_creation_header(header).await?;
        }
        for set in &page.bindings {
            self.put_bindings(set).await?;
        }
        let mut sets = staging.sets.clone();
        sets.merge_rows(&page.delta);
        let blob = encode(&scope)?;
        for (row, PageRow { clock, .. }, record) in &page.rows {
            let fields = self.encode_record(record).await?;
            // A row moved out leaves a tombstone without an image.
            if let Some(tuple) = &statement.partition
                && !(record.deleted && record.fields.is_empty())
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
            if let Some(identity) = record.identity {
                let identity = rocky_core::RowIdentity::from_integer(identity)
                    .map_err(|error| denied(&error.to_string()))?;
                if !statement.identities.iter().any(|mark| {
                    mark.author == identity.author()
                        && mark.sequence >= identity.sequence()
                        && (record.moved_from.is_some() || mark.own >= identity.sequence())
                }) {
                    return Err(denied("checkpoint identity exceeds its permanent mark"));
                }
            }
            if staging.direct {
                if self.row(scope, *row).await?.is_some() {
                    return Err(Error::Corrupt(
                        "a checkpoint row is already stored during its import".into(),
                    ));
                }
                self.conn
                    .execute(
                        "INSERT INTO secure_row_records VALUES(?,?,?)",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(row.to_vec()),
                            SqlValue::Blob(fields.clone()),
                        ]),
                    )
                    .await?;
                self.index_row_key(&blob, row, record).await?;
                // Every stored row's creation stays known, as after the switch.
                self.put_stub(record.creation, id, *clock, true).await?;
            }
            self.conn
                .execute(
                    match staging.direct {
                        true => "INSERT INTO secure_checkpoint_rows VALUES(?,?,?)",
                        false => "INSERT INTO secure_rebootstrap_rows VALUES(?,?,?)",
                    },
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(row.to_vec()),
                        SqlValue::Blob(fields.clone()),
                    ]),
                )
                .await?;
            let creation = record.creation;
            let known = collect_rows(
                self.conn
                    .query(
                        "SELECT clock FROM secure_rebootstrap_stubs WHERE scope=? AND hash=?",
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(creation.as_bytes().to_vec()),
                        ]),
                    )
                    .await?,
            )
            .await?;
            match known.first().map(Vec::as_slice) {
                Some([SqlValue::Integer(known)]) if *known as u64 == *clock => {}
                Some(_) => return Err(denied("checkpoint creation clocks disagree")),
                None => {
                    sets.create(creation, *clock);
                    self.conn
                        .execute(
                            "INSERT INTO secure_rebootstrap_stubs VALUES(?,?,?)",
                            bindings(vec![
                                SqlValue::Integer(id),
                                SqlValue::Blob(creation.as_bytes().to_vec()),
                                SqlValue::Integer(*clock as i64),
                            ]),
                        )
                        .await?;
                }
            }
        }
        if staging.direct {
            // The scope's digest keeps following its stored rows.
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
            for (_, _, record) in &page.rows {
                self.project(scope, None, record).await?;
            }
            Box::pin(self.resolve(None)).await?;
            self.cause(crate::observe::Cause::Sync);
            self.conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
        }
        let cursor = page.rows.last().map(|(row, ..)| *row).or(staging.cursor);
        let row = |row: Option<Commitment>| {
            row.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec()))
        };
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
        self.conn
            .execute(
                "UPDATE secure_rebootstrap SET cursor=?,complete=?,sets=? WHERE scope=?",
                bindings(vec![
                    row(cursor),
                    SqlValue::Integer(i64::from(page.next.is_none())),
                    SqlValue::Blob(sets.to_bytes()),
                    SqlValue::Integer(id),
                ]),
            )
            .await?;
        self.fault(Stage::Import)?;
        Ok(())
    }
    /// The key that signed `signed`, when this replica holds it.
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
    /// Whether operations of the scope were applied.
    async fn applied_any(&self, id: i64) -> Result<bool, Error> {
        let mut applied = self
            .conn
            .query("SELECT 1 FROM secure_applied WHERE scope=? LIMIT 1", [id])
            .await?;
        Ok(applied.next().await?.is_some())
    }
    /// Whether the scope holds stored rows, a checkpoint or applied
    /// operations: an import then stages its pages and switches at the end.
    async fn holds_anything(&self, scope: ResourceScope, id: i64) -> Result<bool, Error> {
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
    /// Undoes a direct import's rows so the scope holds nothing again.
    /// Operations applied over them wait to apply after the next import.
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
        for sql in [
            "DELETE FROM secure_row_records WHERE scope=?",
            "DELETE FROM secure_move_rows WHERE scope=?",
        ] {
            self.conn.execute(sql, [blob.clone()]).await?;
        }
        for sql in [
            "DELETE FROM secure_applied WHERE scope=?",
            "DELETE FROM secure_logs WHERE scope=?",
            "DELETE FROM secure_checkpoint_rows WHERE scope=?",
            "DELETE FROM secure_stubs WHERE scope=?",
        ] {
            self.conn.execute(sql, [id]).await?;
        }
        self.store_scope_state(id, 0, 0, &Lt::new(), 0).await?;
        self.reproject(scope).await?;
        self.reseed_ledger(scope, id).await?;
        self.clear_staging(id).await
    }
    /// Reads which scopes a direct import is filling, after a commit that
    /// began or ended one.
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
    /// The end and signature of the imported checkpoint's page after `after`.
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
        ] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        Ok(())
    }
    /// Whether the scope has pruned history a peer may be behind.
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
        // A partitioned table's own scope holds no rows; each partition
        // scope has its checkpoint.
        if self.partitioned(scope) && !self.partitions.contains_key(&scope) {
            return Ok(None);
        }
        // A scope still being imported has nothing whole to offer, nor
        // has one its owner means to replace.
        if self.importing.contains(&scope) || self.importing(scope).await? {
            return Ok(None);
        }
        let time = now()?;
        // A no-frames base is served as signed, by key holders and relays alike.
        if self.no_frames_base(scope).await?.is_some() {
            return Ok(self
                .served_checkpoint(scope)
                .await?
                .map(|(_, signed)| signed)
                .filter(|signed| signed.signer() != peer));
        }
        // A keyless holder serves only complete, verified sealed archives.
        if !self.can_read_key(scope, &self.local_grants(time)?) {
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
        // Keep a transferable cut stable across page/session retries.
        Ok(self
            .served_checkpoint(scope)
            .await?
            .filter(|(_, signed)| signed.signer() != peer)
            .map(|(_, signed)| signed))
    }

    /// The checkpoint this replica can serve: verified and signed under the
    /// current policy.
    pub(super) async fn served_checkpoint(
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
        // With a tip graph only a base at a seal of the followed path.
        if self.graphs.contains_key(&scope.domain)
            && !signed.statement.seal.is_some_and(|seal| {
                self.path_index(scope.domain, seal)
                    .is_ok_and(|at| at.is_some())
            })
        {
            return Ok(None);
        }
        // A checkpoint of a generation this replica does not admit is
        // offered again once maintenance seals one of the current generation.
        if self
            .manifest_of_tip(signed.statement.tip)
            .and_then(|manifest| self.admits(manifest))
            .is_err()
        {
            return Ok(None);
        }
        signed.verify_authority(self, now()?)?;
        // A no-frames base has no pages and needs no key.
        if signed.statement.no_frames() {
            self.check_no_frames(&signed).await?;
            return Ok(Some((id, signed)));
        }
        // Pages are sealed under the current key; a checkpoint of an earlier
        // key epoch is served again once maintenance seals the next one.
        if Some(self.scope_key(scope)?.0) != signed.statement.key_epoch() {
            return Ok(None);
        }
        Ok(Some((id, signed)))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The pruned prefix of every log in a scope, 256 logs per page. A peer
    /// compares it with what it holds through `behind_pruned`.
    pub async fn pruned_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        after: Option<PublicKey>,
    ) -> Result<PrunedPage, Error> {
        let mut inner = self.inner.lock().await;
        inner = flush_guard(inner, &self.inner).await?;
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
    /// True when a peer pruned operations this replica neither holds nor has
    /// pruned itself: the gap can only be closed by a rebootstrap.
    pub async fn behind_pruned(&self, pruned_prefix: &PrunedPage) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
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
    /// The signed checkpoint a peer behind the pruned_prefix starts from:
    /// a base at a seal, never one the peer signed itself.
    pub async fn checkpoint_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let mut inner = self.inner.lock().await;
        let offered = inner.offered_checkpoint(peer, credentials, scope).await?;
        drop(flush_guard(inner, &self.inner).await?);
        Ok(offered)
    }
    /// Whether `signed` is the verified checkpoint this replica last switched to.
    pub(crate) async fn checkpoint_installed(
        &self,
        signed: &SignedCheckpoint,
    ) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
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
        let inner = self.inner.lock().await;
        inner.checkpoint_key(signed).is_some()
    }
    /// One page of the checkpoint's rows after `after`, sealed with the scope
    /// key, within `byte_quota` encoded bytes. A checkpoint that was replaced
    /// meanwhile is refused; the peer starts again from a new offer.
    pub async fn checkpoint_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        checkpoint: Commitment,
        after: Option<Commitment>,
        byte_quota: usize,
    ) -> Result<CheckpointPage, Error> {
        let mut inner = self.inner.lock().await;
        inner = flush_guard(inner, &self.inner).await?;
        inner.authorize_peer(peer, credentials, scope)?;
        if !(4096..=MAX_SECURE_FRAME).contains(&byte_quota) {
            return Err(denied("sync quota"));
        }
        if inner.scope_key(scope).is_err() {
            return inner
                .archived_page(scope, checkpoint, after, byte_quota)
                .await;
        }
        let (generation, key) = inner.scope_key(scope)?;
        // The base the peer bootstraps from, or a seal's state it pulls.
        let (id, signed, served) = match inner.served_checkpoint(scope).await? {
            Some((id, signed)) if signed.id()? == checkpoint => (id, signed, served::Served::Base),
            offered => match inner.served_seal_checkpoint(scope, checkpoint).await? {
                Some(found) => found,
                None if offered.is_some() => {
                    return Err(denied("checkpoint superseded; restart the rebootstrap"));
                }
                None => return Err(denied("no checkpoint to transfer")),
            },
        };
        if after.is_none() {
            // The checkpoint carries this device's frames it covers.
            let logs: Vec<_> = signed
                .statement
                .frontier
                .iter()
                .map(|log| (log.author, log.sequence))
                .collect();
            inner.offer_covered(scope, &logs).await?;
        }
        // Stored bytes are CBOR byte strings in the sealed plaintext, and the
        // ciphertext is one in the page. The plaintext must fit one package.
        let budget = (byte_quota - 2048).min(rocky_core::MAX_PAYLOAD_BYTES - 1024);
        // The signer cuts and signs its own checkpoint's pages; a holder of
        // an imported one serves the pages its signer cut and signed.
        let signer = inner.checkpoint_key(&signed);
        let stored = match signer {
            Some(_) => None,
            None => Some(
                inner
                    .page_record(id, after)
                    .await?
                    .ok_or_else(|| denied("checkpoint page unavailable"))?,
            ),
        };
        let through = stored.as_ref().and_then(|(next, _)| *next);
        let records = collect_rows(
            inner
                .conn
                .query(
                    format!("SELECT row,fields FROM {} WHERE scope=? AND row>? AND (? IS NULL OR row<=?) ORDER BY row LIMIT ?", served.rows()),
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(after.map(|row| row.to_vec()).unwrap_or_default()),
                        through.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec())),
                        through.map_or(SqlValue::Null, |row| SqlValue::Blob(row.to_vec())),
                        SqlValue::Integer(STEP_ROWS as i64 + 1),
                    ]),
                )
                .await?,
        )
        .await?;
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
        let mut used = 0;
        for record in &records {
            let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                return Err(Error::Corrupt("checkpoint row".into()));
            };
            let record = inner.decode_record(fields).await?;
            // A row's bindings weigh on the page that first names them.
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
                    proof
                        .origin()
                        .checked_checkpoint_bytes(&header_bytes[&proof.header()])
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
            if plain.rows.len() == STEP_ROWS || used + weight > budget {
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
                break;
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
            row_of(row)?;
            plain.rows.push(PageRow {
                row: row.clone(),
                fields: encode(&record)?,
                clock,
            });
        }
        if plain.rows.is_empty() && !records.is_empty() {
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
        for row in &mut plain.rows {
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
            (None, None) => return Err(denied("checkpoint page unavailable")),
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
        drop(flush_guard(inner, &self.inner).await?);
        Ok(page)
    }
    /// Discards only this staged checkpoint before falling back to frames.
    /// Direct imports undo their partial rows; retained local operations settle again.
    pub async fn cancel_rebootstrap(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
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

    /// Starts importing a peer's signed checkpoint for a scope this replica
    /// reads in whole. Nothing changes until `finish_rebootstrap`.
    pub async fn begin_rebootstrap(&self, signed: SignedCheckpoint) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.verify_import_statement(&signed).await?;
        let statement = &signed.statement;
        let scope = statement.scope;
        if signed.signer() == inner.config.node.public_key() {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
        let bytes = signed.to_bytes()?;
        // A no-frames checkpoint has no pages: it stages complete at once.
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
            inner.bind_node_author(&signed.credentials).await?;
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
    /// The checkpoint being imported for a scope, the last row staged and
    /// whether every page arrived.
    pub async fn rebootstrap_progress(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<(Commitment, Option<Commitment>, bool)>, Error> {
        let inner = self.inner.lock().await;
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
    /// The node that signed the checkpoint being imported for a scope.
    pub async fn rebootstrap_signer(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<PublicKey>, Error> {
        let inner = self.inner.lock().await;
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(None);
        };
        Ok(inner
            .staging(id)
            .await?
            .map(|staging| staging.signed.signer()))
    }
    /// Imports the next page of checkpoint rows; pages arrive in order.
    /// The page is opened and verified against its signer off the
    /// replica's thread. A direct import writes the rows into the scope and
    /// its projection, readable once the page commits; otherwise they are
    /// staged for the switch.
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
        let scope = page.scope;
        let (checkpoint, after) = (page.checkpoint, page.after);
        let (key, signed, cursor, publisher, known_headers) = {
            let inner = self.inner.lock().await;
            let (key, staging) = inner.expected_page(&page).await?;
            let plain: PagePlain = decode(&page.rows.open(&key)?)?;
            let supplied: BTreeSet<_> = plain
                .headers
                .iter()
                .map(|header| moves::header_digest(header))
                .collect();
            let mut known = BTreeMap::new();
            for origin in &plain.origins {
                let digest = moves::Origin::checkpoint_header(origin)?;
                if !supplied.contains(&digest) && !known.contains_key(&digest) {
                    known.insert(digest, inner.stored_creation_header(digest).await?);
                }
            }
            (
                key,
                staging.signed,
                staging.cursor,
                *inner.config.publisher.as_bytes(),
                known,
            )
        };
        let opened = tokio::task::spawn_blocking(move || {
            open_page(page, key, &signed, cursor, publisher, known_headers)
        })
        .await
        .map_err(|_| Error::Closed)??;
        let mut inner = self.inner.lock().await;
        let id = inner
            .find_scope_id(scope)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        let staging = inner
            .staging(id)
            .await?
            .ok_or_else(|| denied("no rebootstrap in progress"))?;
        // Another import may have taken the page meanwhile.
        if staging.signed.id()? != checkpoint || staging.complete || staging.cursor != after {
            return Err(denied("unexpected checkpoint page"));
        }
        transaction!(inner, |inner| {
            inner.store_page(scope, id, &staging, &opened, after).await
        })?;
        if staging.direct {
            inner.publish_revision().await?;
        }
        inner.imported.send_modify(|pages| *pages += 1);
        Ok(())
    }
    /// The stored rows of `checkpoint` this replica has imported: all of
    /// them once it switched to it.
    pub async fn imported_rows(
        &self,
        scope: ResourceScope,
        checkpoint: Commitment,
    ) -> Result<u64, Error> {
        let inner = self.inner.lock().await;
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
    /// Verifies the staged rows against the signed checkpoint, then switches
    /// the scope to them in one transaction: held operations the checkpoint
    /// covers are dropped, and every other operation this replica had applied
    /// is applied again on top or reported as rejected with its reason.
    pub async fn finish_rebootstrap(
        &self,
        scope: ResourceScope,
    ) -> Result<RebootstrapReport, Error> {
        let mut inner = self.inner.lock().await;
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
        // History stored since begin may contradict a staged no-frames
        // claim: it is refused and nothing stays staged.
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
            // Unverifiable rows are discarded; a direct import's are undone.
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
        // Operations applied again at the switch must not wait on the import.
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
    /// Inside the caller's transaction, switches `scope` to its verified
    /// staged import of `signed`: held operations the checkpoint covers are
    /// dropped, and every other operation this replica had applied is
    /// applied again on top or reported as rejected with its reason.
    pub(super) async fn switch_to_staged(
        &mut self,
        scope: ResourceScope,
        id: i64,
        signed: &SignedCheckpoint,
        sets: &Sets,
        direct: bool,
        time: u64,
    ) -> Result<RebootstrapReport, Error> {
        // Whether the base it replaces left the followed path, before
        // anything about it changes.
        let off_path = self.base_off_path(scope, id).await?;
        self.clear_seal_anchor(id).await?;
        let statement = signed.statement.clone();
        if self.graphs.contains_key(&scope.domain) && statement.seal.is_none() {
            return Err(denied(
                "with a tip graph an imported checkpoint sits at a seal",
            ));
        }
        // A no-frames base installs no key and binds no author.
        let key = match statement.no_frames() {
            true => None,
            false => Some(self.scope_key(scope)?),
        };
        if key.is_some() {
            self.bind_node_author(&signed.credentials).await?;
        }
        self.check_frontier(id, signed).await?;
        // A replacement of an off-path base that leaves this node's pruned
        // own positions uncovered keeps the old own endpoint as an
        // obligation: those frames may return, and authoring waits for them.
        if off_path {
            let node = self.config.node.public_key();
            if let Some(log) = self.log(id, node).await?
                && log.pruned_prefix
                    > statement
                        .frontier
                        .iter()
                        .find(|position| position.author == node)
                        .map_or(-1, |position| position.sequence as i64)
            {
                self.merge_restoration_guard(scope, (log.sequence, Some(log.hash)))
                    .await?;
            }
        }
        let mut report = RebootstrapReport {
            checkpoint: signed.id()?,
            superseded: 0,
            reapplied: vec![],
            rejected: vec![],
            pending: vec![],
        };
        // A direct import's rows and projection are in place, unless a
        // local write applied over them: then they are restored and
        // the write applies again.
        let rewrite = !direct || self.applied_any(id).await?;
        // Before anything changes: the statement's marks keep every author's
        // signer, and no incoming row contradicts a known original.
        self.check_statement_marks(scope, &statement.identities)
            .await?;
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
        let reapply = self.supersede(scope, &statement, &mut report).await?;
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
                Ok(())
            })
            .await?;
        }
        // Permanent marks and the summary only rise; the replay ledger
        // starts over from exactly the new base.
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
                "DELETE FROM secure_checkpoint_rows WHERE scope=?",
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
        // A direct import kept the release stubs of each page's rows.
        for log in &statement.frontier {
            self.conn
                .execute(
                    "INSERT INTO secure_logs VALUES(?,?,?,?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Blob(log.author.as_bytes().to_vec()),
                        SqlValue::Integer(log.sequence as i64),
                        SqlValue::Blob(log.hash.as_bytes().to_vec()),
                        SqlValue::Integer(log.clock as i64),
                        SqlValue::Integer(log.sequence as i64),
                    ]),
                )
                .await?;
        }
        self.store_scope_state(
            id,
            sets.rows,
            sets.tombstones,
            &sets.digest,
            statement.operations,
        )
        .await?;
        // Operations applied from here on get later positions.
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
        // A checkpoint sealed under an earlier generation names what
        // this one removed: its rows and base keep only the rest.
        self.keep_current(id, scope, true).await?;
        for hash in reapply {
            let frame = self
                .frame(hash)
                .await?
                .ok_or_else(|| Error::Corrupt("retained operation".into()))?;
            // An applied birth without its admission evidence is a missing
            // journal, never backfilled.
            if self.receipt_of(hash).await?.is_none() && !self.birth_marks(&frame)?.is_empty() {
                return Err(Error::Corrupt(
                    "an applied birth has no admission evidence".into(),
                ));
            }
            match self.apply_frame(&frame, time).await? {
                incoming::Outcome::Applied => match self.consumed(hash).await? {
                    Some(reason) => report.rejected.push((hash, reason)),
                    None => report.reapplied.push(hash),
                },
                incoming::Outcome::Rejected(reason) => report.rejected.push((hash, reason)),
                incoming::Outcome::Waiting(_) => report.pending.push(hash),
            }
        }
        self.reseed_ledger(scope, id).await?;
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
    /// Refuses statement marks naming another signer for an author that the
    /// concrete or the allocation scope already binds.
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
                return Err(denied(
                    "checkpoint author number belongs to another signing key",
                ));
            }
        }
        Ok(())
    }
    /// Refuses checkpoint rows of scope `id` in `rows` whose original
    /// creation contradicts a claim this file holds on that identity. The
    /// same original, carried, is accepted.
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
    /// Refuses one row record whose proof contradicts a known claim.
    pub(super) async fn check_record_provenance(
        &self,
        allocation: i64,
        record: &Materialized,
    ) -> Result<(), Error> {
        let (Some(identity), Some(proof)) = (record.identity, &record.proof) else {
            return Ok(());
        };
        let identity = rocky_core::RowIdentity::from_integer(identity)
            .map_err(|error| Error::Corrupt(error.to_string()))?;
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
    /// A checkpoint that ends before this replica's own pruned_prefix would lose
    /// the operations between them.
    async fn check_frontier(&self, id: i64, signed: &SignedCheckpoint) -> Result<(), Error> {
        // A base at a seal off the followed path is replaced by one of the
        // path; frames it pruned that the new base lacks come again as late frames.
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
    /// Drops held operations the checkpoint covers and returns, in their old
    /// application order, the applied ones beyond it, reset to pending.
    async fn supersede(
        &self,
        scope: ResourceScope,
        statement: &CheckpointStatement,
        report: &mut RebootstrapReport,
    ) -> Result<Vec<Hash>, Error> {
        let mut reapply = Vec::new();
        let mut cursor = Vec::new();
        let mut bytes = 0;
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
                    SqlValue::Blob(header),
                    SqlValue::Text(status),
                    SqlValue::Integer(size),
                    position,
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("retained operation".into()));
                };
                cursor = hash.clone();
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                let covering = statement
                    .frontier
                    .iter()
                    .find(|log| log.author == header.public_key && header.seq_num <= log.sequence);
                if let Some(log) = covering {
                    if header.seq_num == log.sequence && log.hash.as_bytes().as_slice() != hash {
                        return Err(denied("checkpoint frontier forks a retained log"));
                    }
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
                } else if status == "applied" {
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
        self.conn
            .execute(
                "UPDATE secure_retained SET records=records-?,bytes=bytes-?",
                [report.superseded as i64, bytes],
            )
            .await?;
        if report.superseded > 0 {
            self.sweep_chains().await?;
        }
        reapply.sort();
        Ok(reapply.into_iter().map(|(_, hash)| hash).collect())
    }
}

/// Re-seals a page's rows after `edit`, keeping its binding and signature:
/// what a reader holding the scope key could forge.
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
/// Re-seals a page after `edit` of the author bindings beside its rows.
#[cfg(test)]
pub(crate) fn forge_page_bindings(
    page: &mut CheckpointPage,
    key: &[u8; 32],
    edit: impl FnOnce(&mut Vec<Vec<u8>>),
) {
    let mut plain: PagePlain = decode(&page.rows.open(key).unwrap()).unwrap();
    let mut sets = plain
        .bindings
        .into_iter()
        .map(|set| set.into_vec())
        .collect();
    edit(&mut sets);
    plain.bindings = sets.into_iter().map(serde_bytes::ByteBuf::from).collect();
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
pub(crate) fn page_header_count(page: &CheckpointPage, key: &[u8; 32]) -> usize {
    let plain: PagePlain = decode(&page.rows.open(key).unwrap()).unwrap();
    plain.headers.len()
}
