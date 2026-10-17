//! No-frames checkpoints: a signed fact that a scope's state at a seal holds
//! no frame history. It needs no key, has no pages and places nothing but
//! its own scope. See `docs/runtime/encrypted-checkpoints.md`.
use super::*;

impl<A: RowApplication> Inner<A> {
    /// Whether this replica holds authenticated history of `scope` that the
    /// state at `seal` includes: a retained frame, void or placement header
    /// of the scope visible at the seal.
    pub(in crate::replica) async fn covered_scope_evidence(
        &self,
        scope: ResourceScope,
        seal: &rocky_credentials::SignedSeal,
    ) -> Result<bool, Error> {
        let graph = self.graph_for(scope.domain)?;
        let blob = encode(&scope)?;
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT header FROM secure_operations WHERE scope=?1 UNION ALL SELECT header FROM secure_voids WHERE scope=?1 UNION ALL SELECT header FROM secure_placements WHERE scope=?1",
                    [blob.clone()],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope history header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            let extensions = &header.extensions;
            let Some(author) = extensions.author else {
                return Ok(true);
            };
            // A frame of a tip the graph no longer resolves cannot be ruled out.
            match Self::covers_exact(
                graph,
                extensions.tip,
                author,
                extensions.author_sequence,
                seal,
            ) {
                Ok(false) => {}
                Ok(true) | Err(_) => return Ok(true),
            }
        }
        for record in collect_rows(self.conn.query(
            "SELECT entry FROM secure_admission_entries WHERE scope=? AND kind=0 ORDER BY entry",
            [blob],
        ).await?).await? {
            let [SqlValue::Integer(index)] = record.as_slice() else {
                return Err(Error::Corrupt("scope admission evidence".into()));
            };
            let identity::receipts::EntryKind::Frame(receipt) = self.verified_entry(*index as u64).await?.kind else {
                return Err(Error::Corrupt("scope admission receipt".into()));
            };
            let Some(author) = receipt.author else { return Ok(true); };
            match Self::covers_exact(graph, receipt.tip, author, receipt.author_sequence, seal) {
                Ok(false) => {},
                Ok(true) | Err(_) => return Ok(true),
            }
        }
        Ok(false)
    }
    /// Whether scope `id` has, or had, any history of its own here: applied
    /// or pruned frames, logs, rows or identities.
    async fn scope_history(&self, scope: ResourceScope, id: i64) -> Result<bool, Error> {
        let blob = encode(&scope)?;
        for (sql, binding) in [
            (
                "SELECT 1 FROM secure_admission_entries WHERE scope=? LIMIT 1",
                SqlValue::Blob(blob.clone()),
            ),
            (
                "SELECT 1 FROM secure_applied WHERE scope=? LIMIT 1",
                SqlValue::Integer(id),
            ),
            (
                "SELECT 1 FROM secure_logs WHERE scope=? LIMIT 1",
                SqlValue::Integer(id),
            ),
            (
                "SELECT 1 FROM secure_rows WHERE scope=? LIMIT 1",
                SqlValue::Blob(blob.clone()),
            ),
            (
                "SELECT 1 FROM secure_identity_marks WHERE scope=? LIMIT 1",
                SqlValue::Blob(blob.clone()),
            ),
            (
                "SELECT 1 FROM secure_checkpoint_rows WHERE scope=? LIMIT 1",
                SqlValue::Integer(id),
            ),
        ] {
            if !collect_rows(self.conn.query(sql, bindings(vec![binding])).await?)
                .await?
                .is_empty()
            {
                return Ok(true);
            }
        }
        self.has_pruned_prefix(scope).await
    }
    /// Inside the compaction transaction, signs and installs a no-frames
    /// base of keyless scope `id` at `seal`, when finalization checked the
    /// scope there (`bound`) and it holds no history of its own. Returns
    /// whether it did.
    pub(in crate::replica) async fn compact_no_frames(
        &mut self,
        scope: ResourceScope,
        id: i64,
        (depth, sealed): (i64, Commitment),
        seal: &rocky_credentials::SignedSeal,
        bound: Option<i64>,
    ) -> Result<bool, Error> {
        let Some(through) = bound else {
            return Ok(false);
        };
        let previous = self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed);
        if previous
            .as_ref()
            .is_some_and(|signed| !signed.statement.no_frames())
            || self.scope_history(scope, id).await?
            || self.covered_scope_evidence(scope, seal).await?
        {
            return Ok(false);
        }
        let tip = seal.statement.tip;
        let (handles, handle_digest) = self.handle_set().await?;
        let empty = Sets::new();
        let statement = CheckpointStatement {
            format: CHECKPOINT_FORMAT,
            seal: Some(sealed),
            partition: self.partitions.get(&scope).cloned(),
            identities: Vec::new(),
            moves: SetSummary {
                count: 0,
                digest: [0; 32],
            },
            tip,
            schema: self
                .generation_of(self.manifest_of_tip(tip)?, false)?
                .schema,
            revocations: self.revocation_digest()?,
            scope,
            state: CheckpointState::NoFrames,
            sequence: previous
                .as_ref()
                .map_or(0, |signed| signed.statement.sequence)
                + 1,
            previous: previous.as_ref().map(SignedCheckpoint::id).transpose()?,
            frontier: Vec::new(),
            clock: 0,
            operations: 0,
            rows: empty.rows_summary(),
            tombstones: 0,
            creations: empty.creations_summary(),
            handles: SetSummary {
                count: handles,
                digest: handle_digest.digest(),
            },
            custody: create::no_custody()?,
        };
        let signed = self.sign_checkpoint(statement)?;
        // Only a signer with checkpoint authority states another node's base.
        if signed.verify_authority(self, now()?).is_err() {
            return Ok(false);
        }
        self.clear_seal_anchor(id).await?;
        for table in [
            "secure_checkpoints",
            "secure_checkpoint_pages",
            "secure_checkpoint_stubs",
        ] {
            self.conn
                .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
                .await?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_checkpoints VALUES(?,?,?,?,?)",
                bindings(vec![
                    SqlValue::Integer(id),
                    SqlValue::Integer(signed.statement.sequence as i64),
                    SqlValue::Integer(through),
                    SqlValue::Blob(empty.to_bytes()),
                    SqlValue::Blob(signed.to_bytes()?),
                ]),
            )
            .await?;
        for table in ["secure_seal_bounds", "secure_seal_drops"] {
            self.conn
                .execute(
                    format!("DELETE FROM {table} WHERE scope=? AND depth<=?"),
                    [id, depth],
                )
                .await?;
        }
        self.reseed_ledger(scope, id).await?;
        Ok(true)
    }
    /// Checks a no-frames checkpoint against this replica before it is
    /// installed or trusted: its seal and tip in the graph, the tip's
    /// schema, and no authenticated covered history of the scope here.
    pub(in crate::replica) async fn check_no_frames(
        &self,
        signed: &SignedCheckpoint,
    ) -> Result<(), Error> {
        let seal = self.no_frames_seal(signed)?;
        if self
            .covered_scope_evidence(signed.statement.scope, &seal)
            .await?
        {
            return Err(denied(
                "the scope's history here contradicts a no-frames checkpoint",
            ));
        }
        Ok(())
    }
    /// The seal a no-frames checkpoint sits at, bound to its tip, scope
    /// and domain; a partitioned table's own scope has none.
    fn no_frames_seal(
        &self,
        signed: &SignedCheckpoint,
    ) -> Result<rocky_credentials::SignedSeal, Error> {
        let statement = &signed.statement;
        let scope = statement.scope;
        let sealed = statement
            .seal
            .ok_or_else(|| denied("a no-frames checkpoint sits at a seal"))?;
        let graph = self.graph_for(scope.domain)?;
        let seal = graph.seal_by_id(sealed).map_err(credential)?;
        if seal.statement.scope != scope.domain || seal.statement.tip != statement.tip {
            return Err(denied("a no-frames checkpoint names its seal's tip"));
        }
        if self.partitioned(scope) && !self.partitions.contains_key(&scope) {
            return Err(denied("a partitioned table's own scope has no checkpoint"));
        }
        Ok(seal.clone())
    }
    /// The verified no-frames base of `scope`, when it has one.
    pub(in crate::replica) async fn no_frames_base(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<SignedCheckpoint>, Error> {
        let Some(id) = self.find_scope_id(scope).await? else {
            return Ok(None);
        };
        Ok(self
            .checkpoint_record(id, true)
            .await?
            .and_then(|record| record.signed)
            .filter(|signed| signed.statement.no_frames()))
    }
    /// A frame, void or placement header that a verified no-frames base of
    /// its scope says the state at its seal does not include is refused:
    /// never stored, pruned or applied. Callers run it in the storing
    /// transaction.
    pub(in crate::replica) async fn refuse_no_frames_contradiction(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        let extensions = &header.extensions;
        let Some(signed) = self.no_frames_base(extensions.scope).await? else {
            return Ok(());
        };
        let Some(sealed) = signed.statement.seal else {
            return Ok(());
        };
        let graph = self.graph_for(extensions.scope.domain)?;
        let seal = graph.seal_by_id(sealed).map_err(credential)?;
        let author = extensions
            .author
            .ok_or_else(|| denied("node author number missing"))?;
        if Self::covers_exact(
            graph,
            extensions.tip,
            author,
            extensions.author_sequence,
            seal,
        )? {
            return Err(denied(
                "frame contradicts a verified no-frames checkpoint of its scope",
            ));
        }
        Ok(())
    }
    /// Inside the storing transaction, just before scope evidence is
    /// stored: refuses it under an installed no-frames base that covers
    /// it, and drops a pulled no-frames state it contradicts or whose
    /// coverage it cannot decide. A pulled state is a candidate; it never
    /// suppresses live evidence.
    pub(in crate::replica) async fn no_frames_intake(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        self.refuse_no_frames_contradiction(header).await?;
        let extensions = &header.extensions;
        let Some(id) = self.find_scope_id(extensions.scope).await? else {
            return Ok(());
        };
        let records = collect_rows(
            self.conn
                .query("SELECT signed FROM secure_pulled WHERE scope=?", [id])
                .await?,
        )
        .await?;
        let Some(record) = records.first() else {
            return Ok(());
        };
        let [SqlValue::Blob(bytes)] = record.as_slice() else {
            return Err(Error::Corrupt("pulled checkpoint".into()));
        };
        let signed = SignedCheckpoint::from_bytes(bytes)?;
        if !signed.statement.no_frames() {
            return Ok(());
        }
        let covered = match (extensions.author, signed.statement.seal) {
            // Coverage that cannot be decided drops the candidate too.
            (Some(author), Some(sealed)) => self
                .graph_for(extensions.scope.domain)
                .and_then(|graph| {
                    let seal = graph.seal_by_id(sealed).map_err(credential)?;
                    Self::covers_exact(
                        graph,
                        extensions.tip,
                        author,
                        extensions.author_sequence,
                        seal,
                    )
                })
                .unwrap_or(true),
            _ => true,
        };
        if covered {
            self.drop_pulled(id).await?;
        }
        Ok(())
    }
    /// Checks no-frames records against their own stores only. An
    /// installed base is this replica's state: canonical, signed, bound to
    /// its seal, with no rows, pages or stubs, and no history here that
    /// contradicts it. A staged import and every pulled state are
    /// candidates, checked for integrity only: decoded, bound to their row,
    /// signed and, for no-frames, stored as complete empty state. Whether
    /// a candidate is still valid is checked when it is used. Runs before
    /// any key-dependent check.
    pub(in crate::replica) async fn verify_no_frames_records(&self) -> Result<(), Error> {
        let empty = Sets::new().to_bytes();
        for record in rows(
            &self.conn,
            "SELECT s.scope,c.signed,c.sets,'installed',NULL,1,0 FROM secure_checkpoints c JOIN secure_scope_state s ON s.id=c.scope \
             UNION ALL SELECT s.scope,r.signed,r.sets,'staged',r.cursor,r.complete,r.direct FROM secure_rebootstrap r JOIN secure_scope_state s ON s.id=r.scope",
        )
        .await?
        {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(bytes),
                SqlValue::Blob(sets),
                SqlValue::Text(kind),
                cursor,
                SqlValue::Integer(complete),
                SqlValue::Integer(direct),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("checkpoint record".into()));
            };
            let signed = SignedCheckpoint::from_bytes(bytes)?;
            if !signed.statement.no_frames() {
                continue;
            }
            let scope: ResourceScope = decode(scope)?;
            let unverifiable =
                |reason: &str| Error::Corrupt(format!("no-frames checkpoint {reason}"));
            if signed.statement.scope != scope {
                return Err(unverifiable("names another scope"));
            }
            signed
                .verify_signature()
                .map_err(|_| unverifiable("is not canonical or signed"))?;
            if *sets != empty
                || *complete != 1
                || *direct != 0
                || !matches!(cursor, SqlValue::Null)
            {
                return Err(unverifiable("is not stored as complete empty state"));
            }
            let installed = kind == "installed";
            let tables: &[&str] = match installed {
                true => &[
                    "secure_checkpoint_rows",
                    "secure_checkpoint_pages",
                    "secure_checkpoint_stubs",
                ],
                false => &[
                    "secure_rebootstrap_rows",
                    "secure_rebootstrap_pages",
                    "secure_rebootstrap_stubs",
                ],
            };
            let id = self.scope_id_of(scope).await?;
            if self.stores_anything(tables, id).await? {
                return Err(unverifiable("has rows or pages"));
            }
            if installed {
                self.check_no_frames(&signed)
                    .await
                    .map_err(|error| unverifiable(&error.to_string()))?;
            }
        }
        for record in rows(&self.conn, "SELECT scope FROM secure_pulled").await? {
            let [SqlValue::Integer(id)] = record.as_slice() else {
                return Err(Error::Corrupt("pulled checkpoint".into()));
            };
            self.pulled_record(*id).await?;
        }
        Ok(())
    }
    /// Whether any of `tables` holds a row of scope `id`.
    pub(in crate::replica) async fn stores_anything(
        &self,
        tables: &[&str],
        id: i64,
    ) -> Result<bool, Error> {
        for table in tables {
            if !collect_rows(
                self.conn
                    .query(format!("SELECT 1 FROM {table} WHERE scope=? LIMIT 1"), [id])
                    .await?,
            )
            .await?
            .is_empty()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn scope_id_of(&self, scope: ResourceScope) -> Result<i64, Error> {
        self.find_scope_id(scope)
            .await?
            .ok_or_else(|| Error::Corrupt("checkpoint scope".into()))
    }
}
