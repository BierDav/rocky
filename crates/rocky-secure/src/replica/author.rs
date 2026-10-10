use super::*;

impl<A: RowApplication> Inner<A> {
    pub(super) async fn author_transaction(
        &mut self,
        handle: Commitment,
        input: Commitment,
        changes: &[RowChange],
        dependencies: &[Hash],
        time: u64,
    ) -> Result<EncryptedFrame, Error> {
        let writes: Vec<RowWrite> = changes.iter().cloned().map(RowWrite::changed).collect();
        let frame = self
            .author_frame(&writes, dependencies, time, |_| RowPayload::rows(changes))
            .await?;
        self.put_handle(&handle, &input, frame.id()?.as_bytes())
            .await?;
        self.injected_commit_fault()?;
        Ok(frame)
    }
    #[cfg(test)]
    pub(super) fn injected_commit_fault(&mut self) -> Result<(), Error> {
        if self.retry_before_commit > 0 {
            self.retry_before_commit -= 1;
            return Err(crate::sqlite::Error::Busy("injected busy file".into()).into());
        }
        if self.fail_before_commit > 0 {
            self.fail_before_commit -= 1;
            return Err(Error::InjectedFailure);
        }
        Ok(())
    }
    #[cfg(not(test))]
    pub(super) fn injected_commit_fault(&mut self) -> Result<(), Error> {
        Ok(())
    }
    /// Signs, admits and stores one frame of `writes`, all in one scope,
    /// with its projection, outbox entry and key record, inside the caller's
    /// transaction. `payload` encodes the plaintext body for the frame's
    /// log sequence.
    pub(super) async fn author_frame(
        &self,
        writes: &[RowWrite],
        dependencies: &[Hash],
        time: u64,
        payload: impl FnOnce(u64) -> Result<Vec<u8>, Error>,
    ) -> Result<EncryptedFrame, Error> {
        Box::pin(self.author_frame_carried(
            writes,
            dependencies,
            time,
            &BTreeMap::new(),
            false,
            payload,
        ))
        .await
    }
    pub(super) async fn author_frame_carried(
        &self,
        writes: &[RowWrite],
        dependencies: &[Hash],
        time: u64,
        carried: &BTreeMap<Commitment, moves::Carried>,
        allocated: bool,
        payload: impl FnOnce(u64) -> Result<Vec<u8>, Error>,
    ) -> Result<EncryptedFrame, Error> {
        let scope = writes
            .first()
            .ok_or_else(|| denied("empty mutation"))?
            .change
            .scope;
        self.ensure_horizon_writable(scope).await?;
        let partition = self
            .app
            .row_partition(&writes[0].change)
            .map_err(|error| denied(&error))?;
        if writes
            .iter()
            .any(|write| self.app.row_partition(&write.change).ok() != Some(partition.clone()))
        {
            return Err(denied("mixed partition tuples"));
        }
        let identity_aware = writes
            .iter()
            .map(|write| {
                self.app
                    .row_identity(&write.change)
                    .map_err(|message| denied(&message))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let signer = if identity_aware.iter().any(Option::is_some) {
            Some(self.row_authors.get(&scope.table).ok_or_else(|| {
                denied("register a table pseudonym before issuing row identities")
            })?)
        } else {
            None
        };
        let signing_key = signer.map_or(&self.config.node, |author| &author.key);
        let credentials = signer.map_or(&self.config.credentials, |author| &author.credentials);
        let grants = match signer {
            Some(author) if partition.is_some() => credentials
                .verify_partition_author(
                    self.config.publisher,
                    self.manifest,
                    signing_key.public_key(),
                    author.number,
                    scope,
                    partition
                        .as_ref()
                        .ok_or_else(|| denied("missing partition tuple"))?,
                    &self.config.revocations,
                    time,
                )
                .map_err(credential)?,
            Some(author) => credentials
                .verify_author(
                    self.config.publisher,
                    self.manifest,
                    signing_key.public_key(),
                    author.number,
                    scope.table,
                    &self.config.revocations,
                    time,
                )
                .map_err(credential)?,
            None => self.local_grants(time)?.to_vec(),
        };
        if let Some(author) = signer {
            if identity_aware.iter().any(Option::is_none) {
                return Err(denied("mixed row identity formats"));
            }
            let mut inserted: Vec<_> = writes
                .iter()
                .zip(&identity_aware)
                .filter(|(write, _)| {
                    write.change.before.is_none() && !carried.contains_key(&write.change.row)
                })
                .filter_map(|(_, identity)| *identity)
                .collect();
            inserted.sort();
            let mut highest = self
                .highest_identity(self.identity_allocation_scope(scope), author.number)
                .await?;
            for identity in inserted.into_iter().filter(|_| !allocated) {
                let expected = rocky_core::RowIdentity::next(author.number, highest)
                    .map_err(|error| denied(&error.to_string()))?;
                if identity != expected {
                    return Err(denied(
                        "insert id must continue the device's own creation log",
                    ));
                }
                highest = identity.sequence();
            }
        }
        if writes.len() > 32 || dependencies.len() > 62 {
            return Err(denied("mutation quota"));
        }
        let key = self
            .config
            .keys
            .iter()
            .find(|key| key.scope == scope)
            .ok_or_else(|| denied("author needs an authorized row key"))?;
        // The log tip outlives its pruned operation, so counters continue.
        let id = self.scope_id(scope).await?;
        let tip = self.log(id, signing_key.public_key()).await?;
        self.admit_author(scope, tip.as_ref().map(|log| log.sequence))
            .await?;
        let (sequence, backlink, mut clock) = match tip {
            Some(log) => (
                (log.sequence as u64)
                    .checked_add(1)
                    .filter(|seq| *seq <= i64::MAX as u64)
                    .ok_or_else(|| denied("author counter exhausted"))?,
                Some(log.hash),
                log.clock,
            ),
            None => (0, None, 0),
        };
        clock = carried
            .values()
            .flat_map(|carried| carried.stamps.values())
            .map(|stamp| stamp.clock)
            .fold(clock, u64::max);
        let basis = self.finalized_basis(scope).await?;
        let finalized = basis.is_some();
        let mut witnesses = BTreeMap::new();
        let mut releases = BTreeSet::new();
        let mut previous: BTreeSet<Hash> = dependencies.iter().copied().collect();
        for change in writes.iter().map(|write| &write.change) {
            let held = self
                .row(scope, self.storage_row(change)?)
                .await?
                .and_then(|record| (!record.deleted).then_some(record.fields));
            let equal = if held == change.before {
                true
            } else if let (Some(typed), Some(held), Some(before)) =
                (&self.typed, &held, &change.before)
            {
                let table = typed
                    .rows
                    .table(&change.scope.table)
                    .ok_or_else(|| denied("unknown typed table"))?;
                typed
                    .rows
                    .values(table, held)
                    .map_err(|error| denied(&error))?
                    == typed
                        .rows
                        .values(table, before)
                        .map_err(|error| denied(&error))?
            } else {
                false
            };
            if !equal && !(finalized && held.is_none() && change.before.is_some()) {
                return Err(Error::Conflict);
            }
            if change.before.is_none() && finalized {
                for (hash, known) in self.protection_dependencies(change).await? {
                    previous.insert(hash);
                    releases.insert(*hash.as_bytes());
                    clock = clock.max(known);
                }
            }
            // A local write passes every merged row its constraints compare with.
            clock = clock.max(self.constraint_clock(change).await?);
            if let Some(row) = self.row(scope, self.storage_row(change)?).await? {
                previous.insert(row.creation);
                witnesses.insert(self.storage_row(change)?, *row.creation.as_bytes());
                // A write wins over every value it saw.
                clock = row
                    .stamps
                    .values()
                    .map(|stamp| stamp.clock)
                    .fold(clock, u64::max);
            }
        }
        if let Some(backlink) = backlink {
            previous.remove(&backlink);
        }
        for dependency in &previous {
            let Some(frame) = self.frame(*dependency).await? else {
                // A pruned dependency is usable while its stub is kept.
                match self.stub(*dependency).await? {
                    Some((stub, known)) if stub == id => clock = clock.max(known),
                    _ => return Err(denied("missing encrypted dependency")),
                }
                continue;
            };
            if frame.scope()? != scope || self.status(*dependency).await? != ReceiveStatus::Applied
            {
                return Err(denied(
                    "dependency must be applied in the same encryption scope",
                ));
            }
            clock = clock.max(frame.operation()?.header.timestamp);
        }
        if previous.len() > 64 {
            return Err(denied("dependency quota"));
        }
        let stamp = Stamp {
            clock: clock
                .checked_add(1)
                .ok_or_else(|| denied("logical clock exhausted"))?,
            author: signing_key.public_key(),
            sequence,
        };
        let mut proofs = BTreeMap::new();
        for write in writes.iter().filter(|write| {
            write.change.before.is_none() && !carried.contains_key(&write.change.row)
        }) {
            if let Some(id) = self
                .app
                .row_identity(&write.change)
                .map_err(|message| denied(&message))?
            {
                proofs.insert(
                    write.change.row,
                    moves::CreationProof::sign(
                        self.manifest,
                        scope.table,
                        id,
                        stamp.clone(),
                        signer.ok_or_else(|| denied("creation proof needs a table author"))?,
                    )?,
                );
            }
        }
        let frame = author(
            signing_key,
            Extensions {
                author: signer.map(|author| author.number),
                row_format: 6,
                partition,
                manifest: self.manifest,
                scope,
                generation: key.generation,
                credentials: credentials.clone(),
            },
            &key.key,
            (
                sequence,
                backlink,
                clock
                    .checked_add(1)
                    .ok_or_else(|| denied("logical clock exhausted"))?,
            ),
            previous.into_iter().collect(),
            &RowPayload::witness(
                RowPayload::stamp_body(&payload(sequence)?, proofs, carried.clone())?,
                witnesses.into_iter().collect(),
                basis,
                releases.into_iter().collect(),
            )?,
        )?;
        self.verify_frame(&frame, time)?;
        let operation = frame.operation()?;
        let materialized = self.materialize(&frame, writes, &grants).await?;
        self.check_budget(frame.to_bytes()?.len()).await?;
        TransactionStore(self.conn.clone())
            .insert_operation(
                operation.hash,
                &operation.header,
                operation.body.as_ref(),
                &frame.header,
                &scope,
            )
            .await?;
        // Liveness evidence commits with the rows, before they project.
        let effective: Vec<_> = writes
            .iter()
            .filter(|write| {
                materialized
                    .iter()
                    .any(|(change, _)| change.row == write.change.row)
            })
            .cloned()
            .collect();
        Box::pin(self.author_existence(scope, &effective)).await?;
        self.write_materialized(&frame, materialized).await?;
        self.conn
            .execute(
                "INSERT INTO secure_outbox VALUES(?)",
                [operation.hash.as_bytes().to_vec()],
            )
            .await?;
        self.record_key(key).await?;
        Ok(frame)
    }
}
