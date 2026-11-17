use super::*;

fn possibly_covers(coverage: Result<bool, Error>) -> bool {
    coverage.unwrap_or(true)
}

impl<A: RowApplication> Inner<A> {
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
            let Some(author) = extensions.author(&header.public_key) else {
                return Ok(true);
            };
            if possibly_covers(Self::covers_exact(
                graph,
                extensions.tip,
                author,
                extensions.author_sequence,
                seal,
            )) {
                return Ok(true);
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
            if possibly_covers(Self::covers_exact(graph, receipt.tip, author, receipt.author_sequence, seal)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
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
                "SELECT 1 FROM secure_row_records WHERE scope=? LIMIT 1",
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
            outcomes: SealedOutcomes::default(),
        };
        let signed = self.sign_checkpoint(statement)?;
        let states_others_bases = signed.verify_authority(self, now()?).is_ok();
        if !states_others_bases {
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
    pub(in crate::replica) async fn refuse_no_frames_contradiction_in_storing_transaction(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        self.no_frames_contradiction(header).await
    }
    pub(in crate::replica) async fn refuse_no_frames_contradiction_before_queueing(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        self.no_frames_contradiction(header).await
    }
    async fn no_frames_contradiction(
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
            .author(&header.public_key)
            .ok_or_else(|| denied("node author missing"))?;
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
    pub(in crate::replica) async fn no_frames_intake(
        &self,
        header: &p2panda_core::Header<Extensions>,
    ) -> Result<(), Error> {
        self.refuse_no_frames_contradiction_in_storing_transaction(header)
            .await?;
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
        let covered = match (extensions.author(&header.public_key), signed.statement.seal) {
            (Some(author), Some(sealed)) => {
                possibly_covers(self.graph_for(extensions.scope.domain).and_then(|graph| {
                    let seal = graph.seal_by_id(sealed).map_err(credential)?;
                    Self::covers_exact(
                        graph,
                        extensions.tip,
                        author,
                        extensions.author_sequence,
                        seal,
                    )
                }))
            }
            _ => true,
        };
        if covered {
            self.drop_pulled(id).await?;
        }
        Ok(())
    }
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
                    "secure_checkpoint_base",
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

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn coverage_the_graph_cannot_decide_counts_as_covered() {
        assert!(possibly_covers(Err(Error::Corrupt(
            "unresolved tip".into()
        ))));
        assert!(possibly_covers(Ok(true)));
        assert!(!possibly_covers(Ok(false)));
    }
}
