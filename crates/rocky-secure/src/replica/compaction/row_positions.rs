//! Source row identities and global birth order at applied positions.
use super::*;

impl<A: RowApplication> Inner<A> {
    /// Global birth order matters even when a replay visits only a few rows.
    pub(in crate::replica) async fn row_replay_ledger(
        &self,
        scope: ResourceScope,
        id: i64,
        through: i64,
        target: TipId,
    ) -> Result<identity::IdentityLedger, Error> {
        let base = self.checkpoint_record(id, true).await?;
        let (after, mut ledger) = match base {
            Some(base) => (
                base.position,
                identity::IdentityLedger::based(
                    &self.verify_record(scope, &base)?.statement.identities,
                )?,
            ),
            None => (0, identity::IdentityLedger::default()),
        };
        if !self.tables.contains(&scope.table) {
            return Ok(ledger);
        }
        let records = collect_rows(self.conn.query(
            "SELECT b.author,b.signer,MAX(b.sequence),p.tip FROM secure_birth_positions b JOIN secure_applied a ON a.position=b.position JOIN secure_operations o ON o.hash=a.hash JOIN secure_position_sources p ON p.hash=a.hash AND p.kind=0 WHERE b.scope=? AND b.position>? AND b.position<=? AND o.status='applied' GROUP BY b.author,b.signer,p.tip",
            [id, after, through],
        ).await?).await?;
        for record in records {
            let [
                SqlValue::Integer(author),
                SqlValue::Blob(signer),
                SqlValue::Integer(sequence),
                SqlValue::Blob(tip),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("birth position prefix".into()));
            };
            if self.graphs.contains_key(&scope.domain)
                && !self.on_path(scope.domain, TipId(checkpoint::row_of(tip)?), target)?
            {
                continue;
            }
            ledger.merge(checkpoint::IdentityMark {
                author: u32::try_from(*author)
                    .map_err(|_| Error::Corrupt("birth position author".into()))?,
                signer: checkpoint::key_of(signer)?,
                sequence: u32::try_from(*sequence)
                    .map_err(|_| Error::Corrupt("birth position sequence".into()))?,
            })?;
        }
        for record in collect_rows(self.conn.query(
            "SELECT b.author,b.signer,b.sequence,o.header,o.body FROM secure_birth_positions b JOIN secure_applied a ON a.position=b.position JOIN secure_operations o ON o.hash=a.hash WHERE b.scope=? AND b.position>? AND b.position<=? AND o.status='held'", [id,after,through],
        ).await?).await? {
            let [SqlValue::Integer(author), SqlValue::Blob(signer), SqlValue::Integer(sequence), SqlValue::Blob(header), SqlValue::Blob(body)] = record.as_slice() else { return Err(Error::Corrupt("held birth position".into())) };
            let frame = self.chained_frame(header.clone(), body.clone()).await?;
            let operation = frame.operation_ref()?;
            if !self.replay_eligible("held", operation, false)? || (self.graphs.contains_key(&scope.domain) && !self.on_path(scope.domain,operation.header.extensions.tip,target)?) { continue; }
            ledger.merge(checkpoint::IdentityMark {
                author: u32::try_from(*author).map_err(|_| Error::Corrupt("birth position author".into()))?,
                signer: checkpoint::key_of(signer)?,
                sequence: u32::try_from(*sequence).map_err(|_| Error::Corrupt("birth position sequence".into()))?,
            })?;
        }
        Ok(ledger)
    }
}
impl<A: RowApplication> Inner<A> {
    /// Storage identities are read in the authenticated source schema, even
    /// while this workspace filters its table or fields out.
    pub(in crate::replica) fn frame_positions(
        &self,
        frame: &EncryptedFrame,
        payload: &RowPayload,
    ) -> Result<(BTreeSet<Commitment>, Vec<checkpoint::IdentityMark>), Error> {
        let manifest = self.manifest_of_tip(frame.operation_ref()?.header.extensions.tip)?;
        let owned;
        let application: &dyn RowApplication = if manifest == self.manifest {
            self.app.as_ref()
        } else {
            let definition = self
                .catalog
                .get(&manifest)
                .ok_or_else(|| denied("source tip schema is not installed"))?;
            owned = self
                .app
                .source_application(definition)
                .map_err(Error::Incompatible)?;
            if owned.manifest().hash() != manifest {
                return Err(Error::Incompatible(
                    "source validator has a different manifest".into(),
                ));
            }
            owned.as_ref()
        };
        let mut rows = BTreeSet::new();
        let mut births = Vec::new();
        for write in payload.writes() {
            let identity = application
                .row_identity(&write.change)
                .map_err(|message| denied(&message))?;
            rows.insert(match identity {
                Some(id) => {
                    rocky_core::commitment("rocky.row-incarnation.v2", &id.integer().to_le_bytes())
                }
                None => write.change.row,
            });
            if write.change.before.is_none()
                && payload.carried(&write.change.row).is_none()
                && let Some(identity) = identity
            {
                births.push(checkpoint::IdentityMark {
                    author: identity.author(),
                    signer: frame.operation_ref()?.header.public_key,
                    sequence: identity.sequence(),
                });
            }
        }
        Ok((rows, births))
    }
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn note_row_positions(
        &self,
        id: i64,
        frame: &EncryptedFrame,
        payload: &RowPayload,
    ) -> Result<(), Error> {
        let operation = frame.operation_ref()?;
        let (positions, births) = self.frame_positions(frame, payload)?;
        for birth in births {
            self.conn.execute("INSERT INTO secure_birth_positions SELECT ?,position,?,?,? FROM secure_applied WHERE hash=? ON CONFLICT(scope,position,author) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)", bindings(vec![SqlValue::Integer(id), SqlValue::Integer(i64::from(birth.author)), SqlValue::Blob(birth.signer.as_bytes().to_vec()), SqlValue::Integer(i64::from(birth.sequence)), SqlValue::Blob(operation.hash.as_bytes().to_vec())])).await?;
        }

        for row in positions {
            self.conn.execute(
                "INSERT OR IGNORE INTO secure_row_positions SELECT ?,?,position FROM secure_applied WHERE hash=?",
                bindings(vec![SqlValue::Integer(id), SqlValue::Blob(row.to_vec()), SqlValue::Blob(operation.hash.as_bytes().to_vec())]),
            ).await?;
        }
        Ok(())
    }
}
