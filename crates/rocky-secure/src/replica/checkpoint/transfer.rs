//! The horizon signal, checkpoint transfer and rebootstrap.
use super::*;

/// Logs of a scope whose operations up to a sequence were pruned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HorizonPage {
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
    /// Each set of author bindings the rows' proofs name, once. The rows
    /// are signed and name them by digest, so the sets need no signature.
    bindings: Vec<serde_bytes::ByteBuf>,
    /// The checkpoint signer's signature of `page_signing_bytes`: a peer
    /// relaying the checkpoint can neither alter nor move a page's rows.
    signature: Signature,
}
/// What the checkpoint signer signs of a page: its place and its rows.
fn page_signing_bytes(
    checkpoint: Commitment,
    after: Option<Commitment>,
    next: Option<Commitment>,
    rows: &[PageRow],
) -> Result<Vec<u8>, Error> {
    let rows = rocky_core::commitment(
        "rocky.checkpoint-page-rows.v1",
        &encode(&(checkpoint, after, next, rows))?,
    );
    signing_bytes("rocky.checkpoint-page.v1", &rows)
}
/// A page opened and checked against its signature, before it is stored.
struct OpenedPage {
    rows: Vec<(Commitment, PageRow, Materialized)>,
    /// The encoded sets of author bindings the rows name, checked.
    bindings: Vec<Vec<u8>>,
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
    let signing = page_signing_bytes(plain.checkpoint, plain.after, plain.next, &plain.rows)?;
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
    let mut delta = Sets::new();
    let mut last = cursor;
    let mut rows = Vec::with_capacity(plain.rows.len());
    for row in plain.rows {
        let id = row_of(&row.row)?;
        if last.is_some_and(|last| id <= last) {
            return Err(denied("checkpoint rows out of order"));
        }
        last = Some(id);
        delta.replace(&id, None, &row.fields)?;
        let record = decode::<Materialized>(&row.fields)?;
        if let Some(proof) = &record.proof {
            *sets
                .get_mut(&proof.bindings())
                .ok_or_else(|| denied("checkpoint page lacks a row's author bindings"))? = true;
        }
        rows.push((id, row, record));
    }
    if plain.next.is_some() && plain.next != last {
        return Err(denied("checkpoint page binding mismatch"));
    }
    if sets.values().any(|named| !named) {
        return Err(denied("checkpoint page carries unnamed author bindings"));
    }
    Ok(OpenedPage {
        rows,
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
pub(super) struct Staging {
    pub(super) signed: SignedCheckpoint,
    cursor: Option<Commitment>,
    complete: bool,
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
        let grants = self.verify_chain_scopes(credentials, peer, now()?, &[scope])?;
        if !crate::replica::roles::holds_whole(&grants, scope) {
            return Err(denied("ciphertext custody denied"));
        }
        Ok(())
    }
    pub(super) async fn staging(&self, id: i64) -> Result<Option<Staging>, Error> {
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
                    "SELECT 1 FROM secure_rows WHERE scope=? LIMIT 1",
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
            || page.rows.generation != staging.signed.statement.key_epoch
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
        for set in &page.bindings {
            self.put_bindings(set).await?;
        }
        let mut sets = staging.sets.clone();
        sets.merge_rows(&page.delta);
        let blob = encode(&scope)?;
        for (row, PageRow { fields, clock, .. }, record) in &page.rows {
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
                    mark.author == identity.author() && mark.sequence >= identity.sequence()
                }) {
                    return Err(denied("checkpoint identity exceeds its permanent mark"));
                }
            }
            if staging.direct {
                let stored = self
                    .conn
                    .execute(
                        "INSERT INTO secure_rows VALUES(?,?,?,X'') ON CONFLICT(scope,row) DO NOTHING",
                        bindings(vec![
                            SqlValue::Blob(blob.clone()),
                            SqlValue::Blob(row.to_vec()),
                            SqlValue::Blob(fields.clone()),
                        ]),
                    )
                    .await?;
                // A local write cannot name a row it never saw.
                if stored == 0 {
                    return Err(Error::Corrupt(
                        "a checkpoint row is already stored during its import".into(),
                    ));
                }
                self.index_row_key(&blob, row, record).await?;
                self.protection_stubs(id, record).await?;
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
        self.row_authors
            .get(&signed.statement.scope.table)
            .map(|author| &author.key)
            .into_iter()
            .chain(self.credential_signer.as_ref())
            .chain([&self.config.node])
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
                    "SELECT EXISTS(SELECT 1 FROM secure_rows WHERE scope=?) OR EXISTS(SELECT 1 FROM secure_checkpoints WHERE scope=?2) OR EXISTS(SELECT 1 FROM secure_applied WHERE scope=?2)",
                    bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(id)]),
                )
                .await?,
        )
        .await?;
        Ok(held != vec![vec![SqlValue::Integer(0)]])
    }
    /// Undoes a direct import's rows so the scope holds nothing again.
    /// Operations applied over them wait to apply after the next import.
    async fn abort_direct(&mut self, scope: ResourceScope, id: i64) -> Result<(), Error> {
        let blob = encode(&scope)?;
        self.conn
            .execute(
                "UPDATE secure_operations SET status='pending' WHERE hash IN(SELECT hash FROM secure_applied WHERE scope=?)",
                [id],
            )
            .await?;
        for sql in [
            "DELETE FROM secure_rows WHERE scope=?",
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
    async fn clear_staging(&self, id: i64) -> Result<(), Error> {
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
    pub(crate) async fn has_horizon(&self, scope: ResourceScope) -> Result<bool, Error> {
        match self.find_scope_id(scope).await? {
            Some(id) => Ok(!self.logs(id, None, true, 1).await?.is_empty()),
            None => Ok(false),
        }
    }
    /// The checkpoint this replica can serve: verified and signed under the
    /// current policy.
    pub(super) async fn served_checkpoint(
        &mut self,
        scope: ResourceScope,
    ) -> Result<Option<(i64, SignedCheckpoint)>, Error> {
        self.settle(scope).await?;
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        let Some(record) = self.checkpoint_record(id, true).await? else {
            return Ok(None);
        };
        let signed = self.verify_record(scope, &record)?;
        if signed.statement.horizon != self.mergeable_horizon(scope).await? {
            return Ok(None);
        }
        // A checkpoint of a generation this replica does not admit is
        // offered again once maintenance seals one of the current generation.
        if self.admits(signed.statement.definition).is_err() {
            return Ok(None);
        }
        signed.verify_authority(self, now()?)?;
        // Pages are sealed under the current key; a checkpoint of an earlier
        // key epoch is served again once maintenance seals the next one.
        if self.scope_key(scope)?.0 != signed.statement.key_epoch {
            return Ok(None);
        }
        Ok(Some((id, signed)))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The pruned prefix of every log in a scope, 256 logs per page. A peer
    /// compares it with what it holds through `behind_horizon`.
    pub async fn horizon_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        after: Option<PublicKey>,
    ) -> Result<HorizonPage, Error> {
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        inner.authorize_peer(peer, credentials, scope)?;
        let mut logs = match inner.find_scope_id(scope).await? {
            Some(id) => inner.logs(id, after, true, STEP_ROWS + 1).await?,
            None => vec![],
        };
        let more = logs.len() > STEP_ROWS;
        logs.truncate(STEP_ROWS);
        Ok(HorizonPage {
            scope,
            next: logs.last().filter(|_| more).map(|(log, _)| log.author),
            logs: logs
                .into_iter()
                .map(|(log, horizon)| (log.author, horizon as u64))
                .collect(),
        })
    }
    /// True when a peer pruned operations this replica neither holds nor has
    /// pruned itself: the gap can only be closed by a rebootstrap.
    pub async fn behind_horizon(&self, horizon: &HorizonPage) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        let id = inner.find_scope_id(horizon.scope).await?;
        for (author, sequence) in &horizon.logs {
            let own = match id {
                Some(id) => inner.log(id, *author).await?.map_or(-1, |log| log.horizon),
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
                            SqlValue::Blob(encode(&horizon.scope)?),
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
    /// The signed checkpoint a peer behind the horizon starts from.
    pub async fn checkpoint_offer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let may_sign = {
            let mut inner = self.inner.lock().await;
            inner.authorize_peer(peer, credentials, scope)?;
            // A partitioned table's own scope holds no rows; each partition
            // scope has its checkpoint.
            if inner.partitioned(scope) && !inner.partitions.contains_key(&scope) {
                return Ok(None);
            }
            // A scope still being imported has nothing whole to offer, nor
            // has one its owner means to replace.
            if inner.importing.contains(&scope) || inner.importing(scope).await? {
                return Ok(None);
            }
            let time = now()?;
            // A keyless holder serves only complete, verified sealed archives.
            if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
                return Ok(inner
                    .archived_checkpoint(scope, true)
                    .await?
                    .filter(|signed| {
                        signed.signer() != peer && signed.verify_authority(&inner, time).is_ok()
                    }));
            }
            let (key_epoch, _) = inner.scope_key(scope)?;
            inner.settle(scope).await?;
            // A bootstrap requester refuses its own checkpoint signature.
            if let Some(id) = inner.find_scope_id(scope).await?
                && let Some(signed) = inner
                    .checkpoint_record(id, true)
                    .await?
                    .and_then(|record| record.signed)
                && signed.signer() != peer
                && inner.admits(signed.statement.definition).is_ok()
                && signed.statement.key_epoch == key_epoch
                && signed.statement.horizon == inner.mergeable_horizon(scope).await?
                && signed.verify_authority(&inner, now()?).is_ok()
            {
                // Keep a transferable cut stable across page/session retries.
                inner.flush()?;
                return Ok(inner
                    .served_checkpoint(scope)
                    .await?
                    .filter(|(_, signed)| signed.signer() != peer)
                    .map(|(_, signed)| signed));
            }
            inner.local_grants(now()?)?.iter().any(|grant| {
                grant.scope == scope
                    && grant.rows == RowSelection::All
                    && [Right::Insert, Right::Update, Right::Delete]
                        .iter()
                        .any(|right| grant.rights.contains(right))
            })
        };
        if may_sign {
            self.checkpoint(scope).await?;
        }
        let mut inner = self.inner.lock().await;
        inner.authorize_peer(peer, credentials, scope)?;
        inner.flush()?;
        Ok(inner
            .served_checkpoint(scope)
            .await?
            .filter(|(_, signed)| signed.signer() != peer)
            .map(|(_, signed)| signed))
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
        let signer = signed.signer();
        signer == inner.config.node.public_key()
            || inner
                .credential_signer
                .as_ref()
                .is_some_and(|key| key.public_key() == signer)
            || inner
                .row_authors
                .get(&signed.statement.scope.table)
                .is_some_and(|author| author.key.public_key() == signer)
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
        inner.flush()?;
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
        let Some((id, signed)) = inner.served_checkpoint(scope).await? else {
            return Err(denied("no checkpoint to transfer"));
        };
        if signed.id()? != checkpoint {
            return Err(denied("checkpoint superseded; restart the rebootstrap"));
        }
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
                    "SELECT row,fields FROM secure_checkpoint_rows WHERE scope=? AND row>? AND (? IS NULL OR row<=?) ORDER BY row LIMIT ?",
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
            bindings: vec![],
            signature: Signature::from_bytes(&[0; 64]),
        };
        let mut named = BTreeSet::new();
        let mut used = 0;
        for record in &records {
            let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                return Err(Error::Corrupt("checkpoint row".into()));
            };
            let record = decode::<Materialized>(fields)?;
            // A row's bindings weigh on the page that first names them.
            let set = match record.proof.as_ref().map(|proof| proof.bindings()) {
                Some(digest) if !named.contains(&digest) => {
                    Some((digest, inner.stored_bindings(digest).await?))
                }
                _ => None,
            };
            let weight = ROW_OVERHEAD
                + fields.len()
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
            if let Some((digest, set)) = set {
                named.insert(digest);
                plain.bindings.push(serde_bytes::ByteBuf::from(set));
            }
            let (_, clock) = inner
                .stub(record.creation)
                .await?
                .ok_or_else(|| Error::Corrupt("checkpoint row creation is unknown".into()))?;
            row_of(row)?;
            plain.rows.push(PageRow {
                row: row.clone(),
                fields: fields.clone(),
                clock,
            });
        }
        if plain.rows.is_empty() && !records.is_empty() {
            return Err(denied("byte quota smaller than one checkpoint row"));
        }
        plain.signature = match (signer, stored) {
            (Some(signer), _) => signer.sign(&page_signing_bytes(
                checkpoint,
                after,
                plain.next,
                &plain.rows,
            )?),
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
        Ok(page)
    }
    /// Starts importing a peer's signed checkpoint for a scope this replica
    /// reads in whole. Nothing changes until `finish_rebootstrap`.
    pub async fn begin_rebootstrap(&self, signed: SignedCheckpoint) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        signed.verify_authority(&inner, now()?)?;
        let statement = &signed.statement;
        let scope = statement.scope;
        let generation = inner.generation_of(statement.definition, true)?;
        if statement.schema != generation.schema
            || signed.signer() == inner.config.node.public_key()
            || inner.scope_key(scope)?.0 != statement.key_epoch
        {
            return Err(denied(
                "checkpoint definition/generation/key epoch mismatch",
            ));
        }
        let bytes = signed.to_bytes()?;
        transaction!(inner, |inner| {
            let id = inner.scope_id(scope).await?;
            inner.check_frontier(id, &signed).await?;
            if let Some(finalized) = &signed.statement.finalized {
                for descriptor in &finalized.statement.bases {
                    let expected = super::super::existence::ExistenceBaseAttachment {
                        horizon: finalized.clone(),
                        scope: descriptor.scope,
                    };
                    if !inner
                        .existence_base(descriptor.scope)
                        .await?
                        .map(|base| base.covers(&expected))
                        .transpose()?
                        .unwrap_or(false)
                    {
                        return Err(denied(
                            "import the authenticated existence attachment before switching checkpoint rows",
                        ));
                    }
                }
            }
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
        let scope = page.scope;
        let (checkpoint, after) = (page.checkpoint, page.after);
        let (key, signed, cursor) = {
            let inner = self.inner.lock().await;
            let (key, staging) = inner.expected_page(&page).await?;
            (key, staging.signed, staging.cursor)
        };
        let opened = tokio::task::spawn_blocking(move || open_page(page, key, &signed, cursor))
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
        let (generation, key) = inner.scope_key(scope)?;
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
        let moves = match direct {
            true => inner.imported_moves(id).await?,
            false => inner.rebootstrap_moves(id).await?,
        };
        if moves != statement.moves
            || staging.sets.rows_summary() != statement.rows
            || staging.sets.tombstones != statement.tombstones
            || staging.sets.creations_summary() != statement.creations
            || generation != statement.key_epoch
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
            inner.check_frontier(id, &signed).await?;
            if let Some(horizon) = &signed.statement.horizon {
                inner.adopt_checkpoint_horizon(horizon).await?;
                inner
                    .index_pending_horizon(
                        horizon,
                        signed.statement.finalized.as_ref() != Some(horizon),
                    )
                    .await?;
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
            let rewrite = !direct || inner.applied_any(id).await?;
            let reapply = inner.supersede(scope, &statement, &mut report).await?;
            let blob = encode(&scope)?;
            if rewrite {
                inner
                    .conn
                    .execute("DELETE FROM secure_rows WHERE scope=?", [blob.clone()])
                    .await?;
                inner
                    .conn
                    .execute(
                        match direct {
                            true => "INSERT INTO secure_rows SELECT ?,row,fields,X'' FROM secure_checkpoint_rows WHERE scope=?",
                            false => "INSERT INTO secure_rows SELECT ?,row,fields,X'' FROM secure_rebootstrap_rows WHERE scope=?",
                        },
                        bindings(vec![SqlValue::Blob(blob), SqlValue::Integer(id)]),
                    )
                    .await?;
            }
            let permanent = inner.identity_marks(scope).await?;
            inner
                .conn
                .execute(
                    "DELETE FROM secure_identity_marks WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            for mark in &statement.identities {
                inner.put_identity_mark(scope, mark).await?;
            }
            if rewrite {
                inner.reproject(scope).await?;
            }
            if !direct {
                for sql in [
                    "DELETE FROM secure_checkpoint_rows WHERE scope=?",
                    "INSERT INTO secure_checkpoint_rows SELECT scope,row,fields FROM secure_rebootstrap_rows WHERE scope=?",
                ] {
                    inner.conn.execute(sql, [id]).await?;
                }
            }
            if rewrite {
                inner
                    .conn
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
                inner.conn.execute(sql, [id]).await?;
            }
            // A direct import kept the release stubs of each page's rows.
            if rewrite {
                inner.restore_protection_stubs(scope, id).await?;
            }
            for log in &statement.frontier {
                inner
                    .conn
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
            inner
                .store_scope_state(
                    id,
                    sets.rows,
                    sets.tombstones,
                    &sets.digest,
                    statement.operations,
                )
                .await?;
            // Operations applied from here on get later positions.
            inner
                .conn
                .execute(
                    "INSERT INTO secure_checkpoints SELECT ?,?,'verified',COALESCE(MAX(seq),0),COALESCE(MAX(seq),0),?,NULL,? FROM sqlite_sequence WHERE name='secure_applied'",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Integer(statement.sequence as i64),
                        SqlValue::Blob(sets.to_bytes()),
                        SqlValue::Blob(signed.to_bytes()?),
                    ]),
                )
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO NOTHING",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Blob(encode(&generation)?),
                        SqlValue::Blob(rocky_core::commitment("rocky.scope-key.v1", &key).to_vec()),
                    ]),
                )
                .await?;
            // A checkpoint sealed under an earlier generation names what
            // this one removed: its rows and base keep only the rest.
            inner.keep_current(id, scope, true).await?;
            for hash in reapply {
                let frame = inner
                    .frame(hash)
                    .await?
                    .ok_or_else(|| Error::Corrupt("retained operation".into()))?;
                match inner.apply_frame(&frame, time).await? {
                    incoming::Outcome::Applied => match inner.consumed(hash).await? {
                        Some(reason) => report.rejected.push((hash, reason)),
                        None => report.reapplied.push(hash),
                    },
                    incoming::Outcome::Rejected(reason) => report.rejected.push((hash, reason)),
                    incoming::Outcome::Waiting => report.pending.push(hash),
                }
            }
            for mark in &permanent {
                inner.put_identity_mark(scope, mark).await?;
            }
            inner
                .conn
                .execute(
                    "DELETE FROM secure_horizon_bootstrap WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            inner.clear_staging(id).await?;
            inner
                .conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
            inner.fault(Stage::Switch)?;
            Ok(report)
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
    /// A checkpoint that ends before this replica's own horizon would lose
    /// the operations between them.
    async fn check_frontier(&self, id: i64, signed: &SignedCheckpoint) -> Result<(), Error> {
        self.check_checkpoint_horizon(signed).await?;
        if let Some(needed) = self.horizon_bootstrap(signed.statement.scope).await?
            && signed.statement.finalized.as_deref() == Some(&needed)
            && let Some(held) = self
                .checkpoint_record(id, true)
                .await?
                .and_then(|record| record.signed)
                .and_then(|checkpoint| checkpoint.statement.finalized)
            && held.statement.sequence == needed.statement.sequence
            && held.statement.previous == needed.statement.previous
            && needed.id().map_err(credential)? < held.id().map_err(credential)?
        {
            // The publisher's lower sibling replaces the invalidated replay base.
            return Ok(());
        }
        for (log, horizon) in self.logs(id, None, true, MAX_CHECKPOINT_LOGS + 1).await? {
            if !signed.statement.frontier.iter().any(|position| {
                position.author == log.author && position.sequence as i64 >= horizon
            }) {
                return Err(denied("checkpoint is behind this replica's horizon"));
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
