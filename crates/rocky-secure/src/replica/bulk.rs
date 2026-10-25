//! Exact migration provenance and its independently charged durable history.
use super::*;
use rocky_core::{MigrationProof, MigrationPublicationStatement};
use rocky_credentials::SignedMigrationPublication;
mod audit;
pub(super) mod syntax;
mod tree;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_migration_publications(publication BLOB PRIMARY KEY,tip BLOB NOT NULL UNIQUE,statement BLOB NOT NULL,signed BLOB,writer BLOB NOT NULL,conflict BLOB,retained_frames INTEGER NOT NULL DEFAULT 0 CHECK(typeof(retained_frames)='integer' AND retained_frames>=0),retained_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(retained_bytes)='integer' AND retained_bytes>=0)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_migration_frames(hash BLOB PRIMARY KEY REFERENCES secure_operations(hash) ON DELETE CASCADE,publication BLOB NOT NULL REFERENCES secure_migration_publications(publication),idx INTEGER NOT NULL,bytes INTEGER NOT NULL CHECK(bytes>0),proof BLOB NOT NULL,UNIQUE(publication,idx)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_migration_nodes(publication BLOB NOT NULL,start INTEGER NOT NULL,frames INTEGER NOT NULL,bytes INTEGER NOT NULL,hash BLOB NOT NULL,PRIMARY KEY(publication,start,frames)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_migration_source(publication BLOB NOT NULL,idx INTEGER NOT NULL,hash BLOB NOT NULL,bytes INTEGER NOT NULL,PRIMARY KEY(publication,idx)) WITHOUT ROWID",
    "CREATE TRIGGER IF NOT EXISTS secure_migration_operation_delete AFTER DELETE ON secure_operations BEGIN DELETE FROM secure_migration_frames WHERE hash=OLD.hash; END",
    "CREATE TRIGGER IF NOT EXISTS secure_migration_credit AFTER INSERT ON secure_migration_frames BEGIN UPDATE secure_migration_publications SET retained_frames=retained_frames+1,retained_bytes=retained_bytes+NEW.bytes WHERE publication=NEW.publication; END",
    "CREATE TRIGGER IF NOT EXISTS secure_migration_debit AFTER DELETE ON secure_migration_frames BEGIN UPDATE secure_migration_publications SET retained_frames=retained_frames-1,retained_bytes=retained_bytes-OLD.bytes WHERE publication=OLD.publication; DELETE FROM secure_migration_nodes WHERE publication=OLD.publication AND NOT EXISTS(SELECT 1 FROM secure_migration_frames WHERE publication=OLD.publication); END",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MigrationMembership {
    pub publication: Commitment,
    pub proof: MigrationProof,
    /// A bounded exact record, never an inventory or a bundle member list.
    pub descriptor: Option<Box<SignedMigrationPublication>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationStorage {
    pub frames: u64,
    pub bytes: u64,
    pub support_bytes: u64,
}
pub(super) fn counter(value: u64) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| denied("migration storage counter overflow"))
}
fn number(value: &SqlValue) -> Result<u64, Error> {
    match value {
        SqlValue::Integer(n) if *n >= 0 => Ok(*n as u64),
        _ => Err(Error::Corrupt("migration counter".into())),
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) async fn migration_record(
        &self,
        publication: Commitment,
    ) -> Result<
        Option<(
            MigrationPublicationStatement,
            Option<SignedMigrationPublication>,
            bool,
        )>,
        Error,
    > {
        let records=collect_rows(self.conn.query("SELECT statement,signed,conflict FROM secure_migration_publications WHERE publication=?",[publication.to_vec()]).await?).await?;
        let Some(record) = records.first() else {
            return Ok(None);
        };
        let [SqlValue::Blob(statement), signed, conflict] = record.as_slice() else {
            return Err(Error::Corrupt("migration descriptor".into()));
        };
        let signed = match signed {
            SqlValue::Null => None,
            SqlValue::Blob(bytes) => {
                Some(SignedMigrationPublication::from_bytes(bytes).map_err(credential)?)
            }
            _ => return Err(Error::Corrupt("migration endorsement".into())),
        };
        Ok(Some((
            decode(statement)?,
            signed,
            !matches!(conflict, SqlValue::Null),
        )))
    }
    async fn migration_required(&self, s: &MigrationPublicationStatement) -> Result<(), Error> {
        if !self.required_schemas().await?.contains(&(s.tip, s.schema)) {
            return Err(denied("migration descriptor is not locally required"));
        }
        Ok(())
    }
    pub(super) async fn migration_membership(
        &self,
        hash: Hash,
    ) -> Result<Option<MigrationMembership>, Error> {
        let records=collect_rows(self.conn.query("SELECT f.publication,f.proof,p.signed,p.conflict FROM secure_migration_frames f JOIN secure_migration_publications p ON p.publication=f.publication WHERE f.hash=?",[hash.as_bytes().to_vec()]).await?).await?;
        let Some(record) = records.first() else {
            return Ok(None);
        };
        let [
            SqlValue::Blob(publication),
            SqlValue::Blob(proof),
            signed,
            conflict,
        ] = record.as_slice()
        else {
            return Err(Error::Corrupt("migration proof record".into()));
        };
        // Private, unsigned and equivocated publications never leave through frames.
        if !matches!(conflict, SqlValue::Null) {
            return Err(denied("migration publication equivocated"));
        }
        let descriptor = match signed {
            SqlValue::Blob(bytes) => Some(Box::new(self.stored_migration_descriptor(bytes)?)),
            SqlValue::Null => None,
            _ => return Err(Error::Corrupt("migration descriptor record".into())),
        };
        Ok(Some(MigrationMembership {
            publication: publication
                .as_slice()
                .try_into()
                .map_err(|_| denied("migration publication id"))?,
            proof: decode(proof)?,
            descriptor,
        }))
    }
    pub(super) async fn check_migration_frame(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Option<MigrationPublicationStatement>, Error> {
        let Some(membership) = &frame.migration else {
            let op = frame.operation_ref()?;
            let tip = op.header.extensions.tip;
            let publication = rocky_core::commitment("rocky.sdk.migration.v3", &tip.0);
            if let Some((s, _, _)) = self.migration_record(publication).await? {
                if op.header.public_key.as_bytes() == &s.writer
                    && op.header.extensions.author_sequence < s.frames
                {
                    return Err(denied("migration inclusion proof missing"));
                }
            } else {
                let graph = self.graph_for(op.header.extensions.scope.domain)?;
                if let Some(opening) = graph.openings.iter().find(|o| o.tip().ok() == Some(tip)) {
                    let parent = graph
                        .seal_by_id(opening.statement.seal)
                        .map_err(credential)?;
                    if matches!(
                        parent.statement.action,
                        rocky_credentials::SealAction::Close
                    ) && graph.schema_any(parent.statement.tip).map_err(credential)?
                        != opening.statement.schema
                    {
                        return Err(denied("migration descriptor missing; acquisition pending"));
                    }
                }
            }
            return Ok(None);
        };
        if membership.proof.siblings.len() > rocky_core::MAX_MIGRATION_PROOF_DEPTH {
            return Err(denied("migration proof quota"));
        }
        let Some((statement, Some(signed), conflict)) =
            self.migration_record(membership.publication).await?
        else {
            return Err(denied("migration descriptor missing; acquisition pending"));
        };
        if conflict {
            return Err(denied("migration publication equivocated"));
        }
        signed
            .verify_active(
                self.config.publisher,
                self.graph_for(statement.scope)?,
                &self.config.revocations,
                time,
            )
            .map_err(credential)?;
        // This uses the same canonical frame path and schema requirements as row admission.
        if !matches!(
            self.frame_reach(frame)?,
            tips::FrameReach::Live | tips::FrameReach::Sealed
        ) {
            return Err(denied("migration payload is outside the followed path"));
        }
        self.check_migration_leaf(&statement, frame, &membership.proof)?;
        Ok(Some(statement))
    }
    fn check_migration_leaf(
        &self,
        s: &MigrationPublicationStatement,
        frame: &EncryptedFrame,
        proof: &MigrationProof,
    ) -> Result<(), Error> {
        let op = frame.operation_ref()?;
        let e = &op.header.extensions;
        if e.tip != s.tip
            || e.scope.domain != s.scope
            || op.header.public_key.as_bytes() != &s.writer
            || e.author != Some(s.author)
            || e.author_sequence != proof.index
        {
            return Err(denied(
                "migration frame is outside the endorsed initial run",
            ));
        }
        s.verify(proof, *op.hash.as_bytes(), frame.stored_bytes()?)
            .map_err(|e| denied(&e.to_string()))
    }
    pub(super) async fn classify_migration(
        &self,
        s: &MigrationPublicationStatement,
        frame: &EncryptedFrame,
        proof: &MigrationProof,
    ) -> Result<(), Error> {
        self.check_migration_leaf(s, frame, proof)?;
        let hash = frame.id()?;
        let existing = collect_rows(
            self.conn
                .query(
                    "SELECT publication,idx,bytes,proof FROM secure_migration_frames WHERE hash=?",
                    [hash.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        let encoded = encode(proof)?;
        let bytes = frame.stored_bytes()?;
        if let Some(row) = existing.first() {
            if row
                != &vec![
                    SqlValue::Blob(s.publication.to_vec()),
                    SqlValue::Integer(counter(proof.index)?),
                    SqlValue::Integer(counter(bytes)?),
                    SqlValue::Blob(encoded),
                ]
            {
                return Err(denied("migration classification differs"));
            }
            return Ok(());
        }
        let totals=collect_rows(self.conn.query("SELECT retained_frames,retained_bytes FROM secure_migration_publications WHERE publication=?",[s.publication.to_vec()]).await?).await?;
        let Some(row) = totals.first() else {
            return Err(denied("migration account missing"));
        };
        if number(&row[0])?.checked_add(1).is_none_or(|n| n > s.frames)
            || number(&row[1])?
                .checked_add(bytes)
                .is_none_or(|n| n > s.bytes)
        {
            return Err(denied("migration extent exceeded"));
        }
        self.conn
            .execute(
                "INSERT INTO secure_migration_frames VALUES(?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                    SqlValue::Blob(s.publication.to_vec()),
                    SqlValue::Integer(counter(proof.index)?),
                    SqlValue::Integer(counter(bytes)?),
                    SqlValue::Blob(encoded),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn migration_bytes(&self) -> Result<u64, Error> {
        let rows = rows(
            &self.conn,
            "SELECT coalesce(sum(retained_bytes),0) FROM secure_migration_publications",
        )
        .await?;
        number(&rows[0][0])
    }
    pub(super) async fn store_migration_record(
        &self,
        signed: &SignedMigrationPublication,
    ) -> Result<bool, Error> {
        let s = &signed.statement;
        if let Some((known, _, conflict)) = self.migration_record(s.publication).await? {
            if conflict {
                return Err(denied("migration publication equivocated"));
            }
            if known != *s {
                self.conn.execute("UPDATE secure_migration_publications SET conflict=? WHERE publication=? AND conflict IS NULL",[signed.to_bytes().map_err(credential)?,s.publication.to_vec()]).await?;
                return Ok(false);
            }
            self.conn
                .execute(
                    "UPDATE secure_migration_publications SET signed=?,writer=? WHERE publication=?",
                    [
                        signed.to_bytes().map_err(credential)?,
                        encode(&signed.writer)?,
                        s.publication.to_vec(),
                    ],
                )
                .await?;
        } else {
            self.conn.execute("INSERT INTO secure_migration_publications(publication,tip,statement,signed,writer) VALUES(?,?,?,?,?)",[s.publication.to_vec(),encode(&s.tip)?,encode(s)?,signed.to_bytes().map_err(credential)?,encode(&signed.writer)?]).await?;
        }
        Ok(true)
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn admit_migration_publication(
        &self,
        signed: SignedMigrationPublication,
    ) -> Result<(), Error> {
        signed.to_bytes().map_err(credential)?;
        let mut inner = self.inner.lock().await;
        signed
            .verify(
                inner.config.publisher,
                inner.graph_for(signed.statement.scope)?,
                Some((&inner.config.revocations, now()?)),
            )
            .map_err(credential)?;
        if let Some((known, Some(existing), false)) =
            inner.migration_record(signed.statement.publication).await?
            && known == signed.statement
            && existing == signed
        {
            drop(flush_guard(inner, &self.inner).await?);
            return Ok(());
        }
        inner.migration_required(&signed.statement).await?;
        let accepted = transaction!(inner, |inner| inner.store_migration_record(&signed).await)?;
        drop(flush_guard(inner, &self.inner).await?);
        if !accepted {
            return Err(denied("migration publication equivocated"));
        }
        Ok(())
    }
    /// Exact lookup; no enumeration or fetching. Private candidates stay private.
    pub async fn migration_publication(
        &self,
        publication: Commitment,
    ) -> Result<Option<SignedMigrationPublication>, Error> {
        let mut inner = self.inner.lock().await;
        if inner.private_schema().await? {
            return Ok(None);
        }
        inner = flush_guard(inner, &self.inner).await?;
        match inner.migration_record(publication).await? {
            Some((_, signed, false)) => Ok(signed),
            _ => Ok(None),
        }
    }
    pub async fn migration_intent(
        &self,
        publication: Commitment,
    ) -> Result<Option<(MigrationPublicationStatement, CredentialChain)>, Error> {
        let inner = self.inner.lock().await;
        let records=collect_rows(inner.conn.query("SELECT statement,writer FROM secure_migration_publications WHERE publication=? AND conflict IS NULL AND signed IS NULL",[publication.to_vec()]).await?).await?;
        match records.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(s), SqlValue::Blob(writer)]) => {
                Ok(Some((decode(s)?, decode(writer)?)))
            }
            None => Ok(None),
            _ => Err(Error::Corrupt("migration intent".into())),
        }
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn require_migration_endorsement(
        &self,
        publication: Commitment,
    ) -> Result<(), Error> {
        let Some((_, Some(signed), false)) = self.migration_record(publication).await? else {
            return Err(denied("migration endorsement pending"));
        };
        signed
            .verify_active(
                self.config.publisher,
                self.graph_for(signed.statement.scope)?,
                &self.config.revocations,
                now()?,
            )
            .map_err(credential)
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    /// Release has the same durable boundary as the private graph and frames.
    pub async fn release_uncoordinated_schema(&self, publication: Commitment) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let records = collect_rows(
            inner
                .conn
                .query(
                    "SELECT stage,state FROM secure_staged_schema WHERE publication=?",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes), SqlValue::Text(state)]) =
            records.first().map(Vec::as_slice)
        else {
            return Err(denied("unknown schema publication"));
        };
        if state == "released" {
            return Ok(());
        }
        let stage: SchemaStage = decode(bytes)?;
        if state != "committed" || stage.coordinated {
            return Err(denied("schema publication requires terminal finality"));
        }
        inner.release_and_reload(&stage, None).await?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await
    }
    /// Storage charged to authenticated migration history, separate from ordinary history.
    pub async fn migration_storage(&self) -> Result<MigrationStorage, Error> {
        let inner = self.inner.lock().await;
        let totals=rows(&inner.conn,"SELECT coalesce(sum(retained_frames),0),coalesce(sum(retained_bytes),0) FROM secure_migration_publications").await?;
        let support=rows(&inner.conn,"SELECT (SELECT coalesce(sum(length(statement)+coalesce(length(signed),0)+length(writer)+coalesce(length(conflict),0)+length(publication)+length(tip)),0) FROM secure_migration_publications)+(SELECT coalesce(sum(length(proof)+length(hash)+length(publication)+16),0) FROM secure_migration_frames)+(SELECT coalesce(sum(length(hash)+length(publication)+24),0) FROM secure_migration_nodes)").await?;
        let mut support_bytes = number(&support[0][0])?;
        let mut after = Vec::<u8>::new();
        loop {
            let page=collect_rows(inner.conn.query("SELECT publication,statement FROM secure_migration_publications WHERE publication>? ORDER BY publication LIMIT 64",[after.clone()]).await?).await?;
            if page.is_empty() {
                break;
            }
            for record in page {
                let [SqlValue::Blob(publication), SqlValue::Blob(statement)] = record.as_slice()
                else {
                    return Err(Error::Corrupt("migration support account".into()));
                };
                let s: MigrationPublicationStatement = decode(statement)?;
                let support=collect_rows(inner.conn.query("SELECT coalesce(sum(length(record)+length(scope)+length(signer)+length(hash)+32),0) FROM secure_existence_log WHERE signer=? AND author=? AND position>=0 AND position<=?",bindings(vec![SqlValue::Blob(s.writer.to_vec()),SqlValue::Integer(i64::from(s.author)),SqlValue::Integer(s.frames.checked_sub(1).map_or(-1, |last| i64::try_from(last).unwrap_or(i64::MAX)))] )).await?).await?;
                support_bytes = support_bytes
                    .checked_add(number(&support[0][0])?)
                    .ok_or_else(|| denied("migration support overflow"))?;
                after = publication.clone();
            }
        }
        Ok(MigrationStorage {
            frames: number(&totals[0][0])?,
            bytes: number(&totals[0][1])?,
            support_bytes,
        })
    }
    /// Proof-bearing transport admission; the ordinary receive API grants no exemption.
    pub async fn receive_migration_all(
        &self,
        frames: Vec<EncryptedFrame>,
    ) -> Vec<Result<ReceiveStatus, Error>> {
        self.receive_with(frames, true, true)
            .await
            .into_iter()
            .map(|r| r.map(|s| s.value))
            .collect()
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn migration_tip_required(&self, tip: TipId) -> Result<bool, Error> {
        let inner = self.inner.lock().await;
        Ok(inner
            .required_schemas()
            .await?
            .iter()
            .any(|(required, _)| *required == tip))
    }
    pub(crate) async fn missing_migration_publications(&self) -> Result<Vec<TipId>, Error> {
        let inner = self.inner.lock().await;
        let mut missing = Vec::new();
        let required: BTreeSet<_> = inner
            .required_schemas()
            .await?
            .into_iter()
            .map(|(tip, _)| tip)
            .collect();
        for tip in required {
            let scope = inner
                .workspace
                .followed
                .as_ref()
                .map(|(scope, _)| *scope)
                .ok_or_else(|| denied("follow required"))?;
            let graph = inner.graph_for(scope)?;
            let Some(opening) = graph.openings.iter().find(|o| o.tip().ok() == Some(tip)) else {
                continue;
            };
            let close = graph
                .seal_by_id(opening.statement.seal)
                .map_err(credential)?;
            if matches!(close.statement.action, rocky_credentials::SealAction::Close)
                && opening.statement.schema
                    != graph.schema_any(close.statement.tip).map_err(credential)?
                && inner
                    .migration_record(rocky_core::commitment("rocky.sdk.migration.v3", &tip.0))
                    .await?
                    .is_none()
            {
                missing.push(tip);
            }
        }
        Ok(missing)
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn fail_flushes(&self, n: usize) {
        self.inner
            .lock()
            .await
            .flusher
            .failures
            .store(n, Ordering::Relaxed);
    }
}
