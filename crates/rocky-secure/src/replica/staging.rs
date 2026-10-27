//! The private graph and durable SQL program of a local schema publication.
use super::*;
use rocky_core::Scope;
use rocky_credentials::SealFinality;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_staged_schema(publication BLOB PRIMARY KEY,stage BLOB NOT NULL,state TEXT NOT NULL CHECK(state IN('staged','committed','released'))) WITHOUT ROWID",
];

/// Exact locally signed statements and an adapter-owned migration program.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaStage {
    pub publication: Commitment,
    pub scope: Scope,
    pub bookmark: String,
    pub source: TipId,
    pub target: TipId,
    pub from_version: u32,
    pub coordinated: bool,
    pub statements: Vec<TipStatement>,
    pub program: Vec<u8>,
    pub parents: Vec<String>,
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn check_schema_stage(&self, stage: &SchemaStage) -> Result<(), Error> {
        if stage.statements.len() > 3 || stage.statements.len() < 2 {
            return Err(denied("schema statement count"));
        }
        if self.private_schema().await? {
            return Err(denied("a schema publication is already pending"));
        }
        let graph = self.graph_for(stage.scope)?;
        if self.workspace.followed.as_ref() != Some(&(stage.scope, stage.bookmark.clone()))
            || self.workspace.tip != stage.source
            || graph.bookmark(&stage.bookmark).map_err(credential)? != Some(stage.source)
            || graph.seal(stage.source).map_err(credential)?.is_some()
        {
            return Err(denied(
                "schema publication must leave its followed open tip",
            ));
        }
        let mut candidate = graph.clone();
        for statement in &stage.statements {
            if statement.scope() != stage.scope {
                return Err(denied("schema publication scope"));
            }
            statement.append(&mut candidate);
        }
        if !candidate
            .succeeds(stage.source, stage.target)
            .map_err(credential)?
            || candidate.bookmark(&stage.bookmark).map_err(credential)? != Some(stage.target)
        {
            return Err(denied("schema publication does not advance its bookmark"));
        }
        if stage.coordinated
            && graph
                .line_minimum(stage.source)
                .map_err(credential)?
                .is_none()
        {
            return Err(denied("source line requires an authenticated minimum"));
        }

        let close = stage
            .statements
            .iter()
            .find_map(|statement| match statement {
                TipStatement::Seal(seal) => Some(seal),
                _ => None,
            })
            .ok_or_else(|| denied("schema publication has no seal"))?;
        if close.statement.frontier
            != self
                .seal_frontier(stage.scope, !stage.parents.is_empty())
                .await?
        {
            return Err(denied("source frontier changed before schema staging"));
        }
        Ok(())
    }
    pub(super) async fn record_schema_stage(&self, stage: &SchemaStage) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_staged_schema VALUES(?,?,'staged')",
                [stage.publication.to_vec(), encode(stage)?],
            )
            .await?;
        if !stage.parents.is_empty() {
            self.freeze_migration_parents(stage).await?;
        }
        Ok(())
    }
    /// Removes an uncommitted stage inside the caller's transaction: its
    /// statements were never offered and its migration never ran.
    pub(super) async fn discard_schema_stage(&self, stage: &SchemaStage) -> Result<(), Error> {
        for statement in &stage.statements {
            self.conn
                .execute(
                    "DELETE FROM secure_tip_statements WHERE digest=? AND staged=1",
                    [statement.id()?.to_vec()],
                )
                .await?;
        }
        self.conn
            .execute(
                "DELETE FROM secure_staged_schema WHERE publication=? AND state='staged'",
                [stage.publication.to_vec()],
            )
            .await?;
        self.drop_migration_parents(stage).await
    }
    pub(super) async fn private_schema(&self) -> Result<bool, Error> {
        Ok(!rows(
            &self.conn,
            "SELECT 1 FROM secure_staged_schema WHERE state!='released' LIMIT 1",
        )
        .await?
        .is_empty())
    }
    /// Runs in the same transaction as the publication's SQL and frames.
    pub(super) async fn commit_schema_stage(&self, publication: Commitment) -> Result<(), Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT stage FROM secure_staged_schema WHERE publication=?",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
            return Ok(());
        };
        let stage: SchemaStage = decode(bytes)?;
        self.conn.execute("UPDATE secure_staged_schema SET state='committed' WHERE publication=? AND state='staged'", [publication.to_vec()]).await?;
        if !stage.coordinated
            && !stage
                .statements
                .iter()
                .any(|s| matches!(s, TipStatement::Opening(_)))
        {
            self.release_schema_stage(&stage, None).await?;
        }
        self.drop_migration_parents(&stage).await?;
        Ok(())
    }
    pub(super) async fn release_schema_stage(
        &self,
        stage: &SchemaStage,
        proof: Option<&SealFinality>,
    ) -> Result<(), Error> {
        if stage
            .statements
            .iter()
            .any(|s| matches!(s, TipStatement::Opening(_)))
        {
            self.require_migration_endorsement(stage.publication)
                .await?;
        }
        let mut graph = self.scope_graph(stage.scope)?;
        if let Some(proof) = proof {
            graph
                .verify_finality(
                    proof,
                    self.config.publisher,
                    Some((&self.config.revocations, now()?)),
                )
                .map_err(credential)?;
            graph.finalities.push(proof.clone());
        }
        graph
            .exclude_revoked(&self.config.revocations)
            .map_err(credential)?;
        for statement in &stage.statements {
            statement.verify(
                self.config.publisher,
                &graph,
                Some((&self.config.revocations, now()?)),
            )?;
        }
        if graph.bookmark(&stage.bookmark).map_err(credential)? != Some(stage.target)
            || !graph
                .succeeds(stage.source, stage.target)
                .map_err(credential)?
        {
            return Err(denied("staged schema no longer wins its bookmark"));
        }
        if let Some(proof) = proof {
            let statement = TipStatement::Finality(proof.clone());
            self.conn.execute("INSERT OR IGNORE INTO secure_tip_statements(digest,statement,staged) VALUES(?,?,0)", [statement.id()?.to_vec(), encode(&statement)?]).await?;
        }
        for statement in &stage.statements {
            self.conn
                .execute(
                    "UPDATE secure_tip_statements SET staged=0 WHERE digest=?",
                    [statement.id()?.to_vec()],
                )
                .await?;
        }
        self.conn
            .execute(
                "UPDATE secure_staged_schema SET state='released' WHERE publication=?",
                [stage.publication.to_vec()],
            )
            .await?;
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        Ok(())
    }
    /// Releases `stage` and loads the graph it changes in one transaction,
    /// so the replay ledgers follow the released visibility before commit.
    pub(super) async fn release_and_reload(
        &mut self,
        stage: &SchemaStage,
        proof: Option<&SealFinality>,
    ) -> Result<(), Error> {
        let (graphs, tip) = (self.graphs.clone(), self.workspace.tip);
        let result = transaction!(self, |inner| {
            inner.release_schema_stage(stage, proof).await?;
            inner.load_tip_graphs().await?;
            inner.reseed_domain_ledgers(stage.scope).await
        });
        if result.is_err() {
            self.graphs = graphs;
            self.workspace.tip = tip;
            self.invalidate_context();
        }
        result
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Stages a local candidate; all signatures are prepared before this call.
    pub async fn stage_schema(&self, stage: SchemaStage) -> Result<(), Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .check_schema_stage(&stage)
            .await?;
        self.check_statements(&stage.statements).await?;
        #[cfg(test)]
        let pause = self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .schema_stage_pause
            .take();
        #[cfg(test)]
        if let Some(pause) = pause {
            pause.reached.send(()).map_err(|_| Error::InjectedFailure)?;
            pause.resume.await.map_err(|_| Error::InjectedFailure)?;
        }
        self.admit_tips_with_stage(&stage.statements, None, &[], Some(&stage))
            .await?;
        Ok(())
    }
    pub async fn private_schema(&self) -> Result<bool, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .private_schema()
            .await
    }
    pub async fn staged_schema(&self) -> Result<Option<(SchemaStage, bool)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let records = rows(
            &inner.conn,
            "SELECT stage,state FROM secure_staged_schema WHERE state!='released'",
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(bytes), SqlValue::Text(state)]) => {
                Ok(Some((decode(bytes)?, state == "committed")))
            }
            None => Ok(None),
            _ => Err(Error::Corrupt("staged schema publication".into())),
        }
    }
    /// Releases the committed candidate and its immutable frames with verified finality.
    pub async fn release_staged_schema(
        &self,
        publication: Commitment,
        finality: SealFinality,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
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
            return Err(denied("unknown staged schema publication"));
        };
        let stage: SchemaStage = decode(bytes)?;
        if state == "released" {
            return Ok(());
        }
        if state != "committed" || !stage.coordinated {
            return Err(denied("schema publication is not committed"));
        }
        let close = stage
            .statements
            .iter()
            .find_map(|statement| match statement {
                TipStatement::Seal(seal) => Some(seal),
                _ => None,
            })
            .ok_or_else(|| denied("schema publication has no seal"))?;
        if finality.seal != close.id().map_err(credential)? {
            return Err(denied("finality names another schema publication"));
        }
        inner.release_and_reload(&stage, Some(&finality)).await?;
        inner = flush_guard(inner, &self.inner).await?;
        inner.publish_revision().await
    }
}

#[cfg(test)]
pub(super) struct StagePause {
    reached: tokio::sync::oneshot::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn pause_schema_staging(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached, ready) = tokio::sync::oneshot::channel();
        let (resume, wait) = tokio::sync::oneshot::channel();
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .schema_stage_pause = Some(StagePause {
            reached,
            resume: wait,
        });
        (ready, resume)
    }
}
