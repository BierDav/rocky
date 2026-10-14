use super::*;

impl<A: RowApplication> Inner<A> {
    /// Admits one frame's exact writes against `grants` and merges each with
    /// the stored row. The application validates the signed transition and
    /// the actual candidate.
    pub(super) async fn materialize(
        &self,
        frame: &EncryptedFrame,
        writes: &[RowWrite],
        grants: &[rocky_core::ResourceGrant],
    ) -> Result<Vec<(RowChange, Materialized)>, Error> {
        if writes.is_empty() || writes.len() > 32 {
            return Err(denied("row change quota"));
        }
        let operation = frame.operation()?;
        let scope = operation.header.extensions.scope;
        // Authority covers the writes as signed; the current generation
        // applies what it still names, the same way on every device.
        for write in writes {
            if write.change.scope != scope
                || !write.permitted_by(grants)
                || write
                    .published_fields()
                    .iter()
                    .any(|field| self.permission_fields.contains(field))
            {
                return Err(denied(
                    "row/field transition denied; ACL changes require strong execution",
                ));
            }
        }
        let current: Vec<RowWrite> = writes
            .iter()
            .filter_map(|write| self.names.current(write, operation.header.timestamp))
            .collect();
        let writes = current.as_slice();
        let mut changed = BTreeSet::new();
        self.app
            .validate_changes(&exact(writes))
            .map_err(|message| denied(&message))?;
        let mut result = Vec::new();
        let mut identities = BTreeSet::new();
        let extension = &operation.header.extensions;
        let marks = self.identity_marks(scope).await?;
        let basis = self.finalized_basis(scope).await?;
        let finalized = basis.is_some();
        let payload = self.row_payload(frame)?;
        let ceiling = Stamp {
            clock: operation.header.timestamp,
            author: operation.header.public_key.into(),
            sequence: operation.header.seq_num,
        };
        for write in writes {
            if self
                .app
                .row_partition(&write.change)
                .map_err(|error| denied(&error))?
                != extension.partition
            {
                return Err(denied("row image differs from authenticated partition"));
            }

            let change = &write.change;
            self.app
                .validate_write(write)
                .map_err(|message| denied(&message))?;
            if change.scope != scope
                || !changed.insert(self.storage_row(change)?)
                || change
                    .before
                    .iter()
                    .chain(change.after.iter())
                    .any(|fields| {
                        fields.len() > 256
                            || fields.keys().any(|field| !self.fields.contains(field))
                    })
            {
                return Err(denied(
                    "row/field transition denied; ACL changes require strong execution",
                ));
            }
            let current = self.row(scope, self.storage_row(change)?).await?;
            // A re-added table's row created before its baseline is ignored,
            // and so is every later write to it.
            if current.is_none()
                && change.before.is_some()
                && self.names.tables.contains_key(&scope.table)
            {
                continue;
            }
            let carried = payload.carried(&change.row);
            if let Some(carried) = carried {
                if payload.membership().is_none() {
                    return Err(denied("a move or follow requires a transaction bundle"));
                }
                self.check_carried(write, carried, &ceiling)?;
                if change.before.is_some()
                    && let Some(id) = self.find_scope_id(scope).await?
                    && let Some(checkpoint) = self
                        .checkpoint_record(id, true)
                        .await?
                        .and_then(|record| record.signed)
                    && checkpoint.statement.finalized.is_some()
                    && carried
                        .stamps
                        .values()
                        .any(|stamp| stamp.clock <= checkpoint.statement.clock)
                {
                    return Err(denied("followed update is behind the destination horizon"));
                }
            }
            if let Some(identity) = self
                .app
                .row_identity(change)
                .map_err(|message| denied(&message))?
                && change.before.is_none()
            {
                let mark = marks.iter().find(|mark| mark.author == identity.author());
                if (carried.is_none() && extension.author != Some(identity.author()))
                    || mark.is_some_and(|mark| {
                        let signer = carried
                            .map_or(operation.header.public_key.into(), |carried| {
                                carried.proof.stamp().author
                            });
                        signer != mark.signer
                            || (carried.is_none()
                                && identity.sequence() <= mark.sequence
                                && !(finalized && current.is_some()))
                    })
                    || !identities.insert(identity)
                {
                    return Err(denied("forged row author or reused identity sequence"));
                }
            }
            if finalized
                && ((change.before.is_some() && current.is_none())
                    || (change.before.is_none() && current.is_some()))
            {
                if carried.is_some() {
                    return Err(denied("move or follow is behind the horizon"));
                }
                self.consume(
                    operation.hash,
                    "row is unavailable behind the finalized horizon",
                )
                .await?;
                continue;
            }
            if finalized
                && change.before.is_none()
                && self.protected_conflict(change, &payload).await?
            {
                if carried.is_some() {
                    return Err(denied("move or follow is behind the horizon"));
                }
                self.consume(
                    operation.hash,
                    "row is unavailable behind the finalized horizon",
                )
                .await?;
                continue;
            }
            if change.before.is_none()
                && let Some((sequence, hash)) = basis
            {
                if payload
                    .basis()
                    .is_none_or(|(authored, _)| authored < sequence)
                {
                    if carried.is_some() {
                        return Err(denied("move insert is behind the destination horizon"));
                    }
                    self.consume(
                        operation.hash,
                        "insert has an older or missing finalized basis",
                    )
                    .await?;
                    continue;
                }
                if payload.basis() != Some((sequence, hash)) {
                    return Err(denied("insert does not bind the current finalized horizon"));
                }
            }
            let (actual, mut record) = merge_row(write, current, &operation, carried)?;
            if change.before.is_none()
                && carried.is_none()
                && let Some(identity) = self
                    .app
                    .row_identity(change)
                    .map_err(|error| denied(&error))?
            {
                let proof = payload
                    .proof(&change.row)
                    .ok_or_else(|| denied("numeric birth requires a creation proof"))?;
                proof.verify(
                    self.config.publisher,
                    extension.manifest,
                    scope.table,
                    identity,
                    &ceiling,
                )?;
                if proof.stamp() != &ceiling {
                    return Err(denied("birth proof must name its creating frame stamp"));
                }
                record.proof = Some(proof.stored()?);
            }
            self.release_protection(&mut record, write, &operation)?;
            record.key = Some(change.row);
            record.identity = self
                .app
                .row_identity(change)
                .map_err(|message| denied(&message))?
                .map(|identity| identity.integer());
            // Validate both the signed transition and the actual candidate under concurrent merges.
            if changes(&actual) {
                self.app
                    .validate_changes(std::slice::from_ref(&actual))
                    .map_err(|message| denied(&message))?;
            }
            result.push((actual, record));
        }
        self.app
            .validate_changes(
                &result
                    .iter()
                    .map(|(change, _)| change.clone())
                    .filter(changes)
                    .collect::<Vec<_>>(),
            )
            .map_err(|message| denied(&message))?;
        Ok(result)
    }
    pub(super) async fn write_materialized(
        &self,
        frame: &EncryptedFrame,
        materialized: Vec<(RowChange, Materialized)>,
    ) -> Result<(), Error> {
        let operation = frame.operation()?;
        let payload = self.row_payload(frame)?;
        let scope = operation.header.extensions.scope;
        let id = self.scope_id(scope).await?;
        // Consumed births reserve identities even when their row is definitely dropped.
        for write in payload.clone().into_writes() {
            if write.change.before.is_none()
                && let Some(identity) = self
                    .app
                    .row_identity(&write.change)
                    .map_err(|message| denied(&message))?
            {
                self.put_identity_mark(
                    scope,
                    &checkpoint::IdentityMark {
                        author: identity.author(),
                        signer: match payload.carried(&write.change.row) {
                            Some(carried) => carried.proof.stamp().author.key()?,
                            None => operation.header.public_key,
                        },
                        sequence: identity.sequence(),
                    },
                )
                .await?;
            }
        }
        // The scope digest follows every stored row in the same transaction.
        let (rows, tombstones, digest, applied) = self.scope_state(id).await?;
        let mut sets = checkpoint::Sets::new();
        (sets.rows, sets.tombstones, sets.digest) = (rows, tombstones, digest);
        let mut projections = Vec::new();
        for (change, record) in materialized {
            let row = match record.identity {
                Some(id) => rocky_core::commitment("rocky.row-incarnation.v2", &id.to_le_bytes()),
                None => change.row,
            };
            if record.creation == operation.hash
                && payload.carried(&change.row).is_none()
                && let Some(identity) = record.identity
            {
                let identity = rocky_core::RowIdentity::from_integer(identity)
                    .map_err(|error| denied(&error.to_string()))?;
                self.put_identity_mark(
                    scope,
                    &checkpoint::IdentityMark {
                        author: identity.author(),
                        signer: operation.header.public_key,
                        sequence: identity.sequence(),
                    },
                )
                .await?;
            }
            let mut old = self
                .conn
                .query(
                    "SELECT fields FROM secure_rows WHERE scope=? AND row=?",
                    [encode(&change.scope)?, row.to_vec()],
                )
                .await?;
            let old = match old.next().await? {
                Some(row) => Some(row.get::<Vec<u8>>(0)?),
                None => None,
            };
            self.protection_stubs(id, &record).await?;
            // The record names its proof's bindings by digest.
            if let Some(proof) = payload
                .carried(&change.row)
                .map(|carried| &carried.proof)
                .or_else(|| payload.proof(&change.row))
                .filter(|_| record.proof.is_some())
            {
                self.keep_bindings(proof).await?;
            }
            let new = encode(&record)?;
            sets.replace(&row, old.as_deref(), &new)?;
            self.conn.execute("INSERT INTO secure_rows VALUES(?,?,?,?) ON CONFLICT(scope,row) DO UPDATE SET fields=excluded.fields,stamp=excluded.stamp", [encode(&change.scope)?, row.to_vec(), new, frame.id()?.as_bytes().to_vec()]).await?;
            self.index_row_key(&encode(&change.scope)?, &row, &record)
                .await?;
            let old: Option<Materialized> = old.as_deref().map(decode).transpose()?;
            projections.push((change.scope, old, record));
        }
        for (scope, old, record) in projections {
            self.project(scope, old.as_ref(), &record).await?;
        }
        // A SQL commit settles once every frame of its bundle is stored.
        if !self.strict_projection {
            Box::pin(self.resolve(None)).await?;
        }
        self.store_scope_state(id, sets.rows, sets.tombstones, &sets.digest, applied + 1)
            .await?;
        self.note_applied(id, &operation).await?;
        self.conn
            .execute(
                "UPDATE secure_operations SET status='applied' WHERE hash=?",
                [frame.id()?.as_bytes().to_vec()],
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
}

/// Whether a merged candidate changes anything. An edit merged into a
/// permanent tombstone has neither image: the delete wins and the edit
/// still applies, so its author's log continues.
pub(super) fn changes(change: &RowChange) -> bool {
    change.before.is_some() || change.after.is_some()
}

pub(super) fn exact(writes: &[RowWrite]) -> Vec<RowChange> {
    writes.iter().map(|write| write.change.clone()).collect()
}

/// Merges one write into the stored row: every published field whose
/// register this operation's stamp wins takes the after value.
pub(super) fn merge_row(
    write: &RowWrite,
    current: Option<Materialized>,
    operation: &p2panda_core::Operation<Extensions>,
    carried: Option<&moves::Carried>,
) -> Result<(RowChange, Materialized), Error> {
    let change = &write.change;
    if change.before.is_some() {
        let creation = current
            .as_ref()
            .ok_or_else(|| denied("row creation unavailable"))?
            .creation;
        if operation.header.backlink != Some(creation)
            && !operation.header.previous.contains(&creation)
        {
            return Err(denied("missing row creation dependency"));
        }
    }
    let mut record = current.clone().unwrap_or(Materialized {
        proof: None,
        moved_from: None,
        protected: None,
        fields: BTreeMap::new(),
        stamps: BTreeMap::new(),
        deleted: false,
        creation: operation.hash,
        identity: None,
        key: Some(change.row),
        created: Some(Stamp {
            clock: operation.header.timestamp,
            author: operation.header.public_key.into(),
            sequence: operation.header.seq_num,
        }),
    });
    if current.is_some() && change.before.is_none() {
        return Err(denied("row identity already exists"));
    }
    let before = if record.deleted {
        None
    } else if current.is_some() {
        Some(record.fields.clone())
    } else {
        None
    };
    if change.after.is_none() {
        record.deleted = true;
        record.fields.clear();
        record.stamps.clear();
    } else if !record.deleted {
        let stamp = Stamp {
            clock: operation.header.timestamp,
            author: operation.header.public_key.into(),
            sequence: operation.header.seq_num,
        };
        let after = change
            .after
            .as_ref()
            .ok_or_else(|| denied("missing after image"))?;
        for field in write.published_fields() {
            let stamp = carried
                .and_then(|carried| carried.stamps.get(&field))
                .unwrap_or(&stamp);
            if record.stamps.get(&field).is_none_or(|old| stamp > old) {
                record.stamps.insert(field, stamp.clone());
                if let Some(value) = after.get(&field) {
                    record.fields.insert(field, value.clone());
                } else {
                    record.fields.remove(&field);
                }
            }
        }
    }
    if let Some(carried) = carried
        && change.before.is_none()
    {
        record.proof = Some(carried.proof.stored()?);
        record.created = Some(carried.proof.stamp().clone());
        record.moved_from = carried.moved_from.clone();
    }
    let actual = RowChange {
        scope: change.scope,
        row: change.row,
        before,
        after: if record.deleted {
            None
        } else {
            Some(record.fields.clone())
        },
    };
    Ok((actual, record))
}
