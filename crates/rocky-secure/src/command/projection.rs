use super::*;
impl<A: RowCommandApplication> EncryptedReplica<A> {
    pub async fn receive_encrypted_result(
        &self,
        result: ConfirmedCommand,
        coordinator: &dyn Coordinator,
    ) -> Result<(), Error> {
        let receipt = result.receipt()?;
        coordinator.verify(&receipt).await?;
        let mut inner = self.inner.lock().await;
        result.to_bytes()?;
        // The verified receipt anchors a result confirmed before a later
        // policy epoch; its credentials verify at the epoch it was issued under.
        let package = match result.verify(&inner) {
            Ok(package) => package,
            Err(_) => result.verify_history(&inner)?,
        };
        let mut covered = !result.parts.is_empty();
        for part in result
            .parts
            .iter()
            .filter(|part| part.kind == PackageKind::Rows)
        {
            covered &= inner.strong_base(part.scope).await?.sequence() >= package.sequence;
        }
        let has_rows = result
            .parts
            .iter()
            .any(|part| part.kind == PackageKind::Rows);
        if covered && has_rows {
            return Ok(());
        }
        let grants = inner.local_grants(now()?)?;
        if result.parts.iter().any(|part| {
            !grants.iter().any(|grant| {
                grant.scope == part.scope
                    && grant.rows == RowSelection::All
                    && (grant.rights.contains(&Right::Read)
                        || grant.rights.contains(&Right::Custody))
            })
        }) {
            return Err(denied("local result ciphertext custody denied"));
        }
        // An open or an authority change can make any retained result
        // applicable; otherwise this result unblocks only its successors.
        let authority = inner.authority;
        let full = inner.reconciled != Some(authority);
        transaction!(inner, |inner| {
            inner.apply_command_result(&result, &package).await?;
            let retained = if full {
                rows(
                    &inner.conn,
                    "SELECT result FROM secure_results ORDER BY sequence",
                )
                .await?
            } else {
                collect_rows(
                    inner
                        .conn
                        .query(
                            "SELECT result FROM secure_results WHERE domain=? AND sequence>? ORDER BY sequence",
                            bindings(vec![
                                SqlValue::Blob(encode(&package.request.scope)?),
                                SqlValue::Integer(package.sequence as i64),
                            ]),
                        )
                        .await?,
                )
                .await?
            };
            for record in retained {
                let [SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(denied("corrupt retained result"));
                };
                let retained: ConfirmedCommand = decode(bytes)?;
                if !inner.projects(&retained, &grants).await? {
                    continue;
                }
                let package = retained.verify_history(inner)?;
                if retained.verify(inner).is_err() {
                    continue;
                }
                inner.apply_command_result(&retained, &package).await?;
            }
            Ok(())
        })?;
        inner.reconciled = Some(authority);
        inner.publish_revision().await?;
        let scopes = resources(&inner, &package.request)?.into_iter().collect();
        inner.maintain(&scopes).await;
        Ok(())
    }
    /// Reads confirmed command state independently of mergeable field registers.
    pub async fn command_snapshot(&self, scope: ResourceScope) -> Result<SecureSnapshot, Error> {
        let mut inner = self.inner.lock().await;
        if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
            return Err(denied("command snapshot read denied"));
        }
        let keyed = inner.config.keys.iter().any(|key| key.scope == scope);
        // History audited earlier under this authority is not read again.
        let cached = inner
            .audited
            .remove(&scope)
            .filter(|audit| (audit.authority, audit.manifest) == (inner.authority, inner.manifest));
        let mut audit = match cached {
            Some(audit) => audit,
            None => inner.audited_base(scope).await?,
        };
        let retained = collect_rows(
            inner
                .conn
                .query(
                    "SELECT result FROM secure_results WHERE domain=? AND sequence>? ORDER BY sequence",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope.domain)?),
                        SqlValue::Integer(audit.sequence as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut complete = keyed;
        for record in retained {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(denied("corrupt command history"));
            };
            let result: ConfirmedCommand = decode(bytes)?;
            let package = result.verify_history(&inner)?;
            if !resources(&inner, &package.request)?.contains(&scope) {
                continue;
            }
            let part = result
                .parts
                .iter()
                .find(|part| part.scope == scope && part.kind == PackageKind::Rows);
            let (true, Some(part)) = (keyed, part) else {
                complete = false;
                continue;
            };
            // The key epoch that sealed this part, current or earlier.
            let Some(key) = inner.opening_key(scope, part.generation) else {
                complete = false;
                continue;
            };
            let projection: RowProjection = decode(&part.open(&key.key)?)?;
            PackageSet::from_bytes(&package.rows)?.verify_subset(std::slice::from_ref(part))?;
            if package.sequence != audit.sequence + 1 {
                complete = false;
                continue;
            }
            if package.base.head
                != audit.head.unwrap_or_else(|| {
                    rocky_core::genesis(scope.domain, package.base.configuration)
                })
            {
                return Err(denied("command history head fork"));
            }
            if projection.before != audit.rows {
                return Err(denied("command history predecessor mismatch"));
            }
            audit.rows = projection.after;
            audit.sequence = package.sequence;
            audit.head = Some(result.receipt()?.confirmation.new_head);
        }
        if complete {
            let mut stored = inner
                .conn
                .query(
                    "SELECT sequence,head FROM secure_command_rows WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            if let Some(row) = stored.next().await? {
                let applied = row.get::<i64>(0)? as u64;
                let stored_head = row.get::<Vec<u8>>(1)?;
                if applied < audit.sequence {
                    complete = false;
                } else if applied != audit.sequence
                    || audit.head.is_some_and(|head| stored_head != head)
                {
                    return Err(denied("corrupt command projection head"));
                }
            } else if audit.sequence != 0 {
                complete = false;
            }
            if complete && inner.command_rows(scope).await? != audit.rows {
                return Err(denied(
                    "command projection differs from retained encrypted history",
                ));
            }
        }
        let snapshot = SecureSnapshot {
            revision: inner.revision,
            rows: if complete {
                audit.rows.clone()
            } else {
                BTreeMap::new()
            },
            complete,
        };
        inner.audited.insert(scope, audit);
        Ok(snapshot)
    }
}
impl<A: RowCommandApplication> Inner<A> {
    async fn audited_base(&self, scope: ResourceScope) -> Result<Audited, Error> {
        let base = self.strong_base(scope).await?;
        self.verify_strong_state(scope, &base)?;
        Ok(Audited {
            authority: self.authority,
            manifest: self.manifest,
            sequence: base.sequence(),
            head: base.head.map(|head| head.1),
            rows: base.rows,
        })
    }
    pub(crate) fn verify_strong_state(
        &self,
        scope: ResourceScope,
        base: &crate::command::history::StrongState,
    ) -> Result<(), Error> {
        if let Some(signed) = &base.checkpoint {
            signed.verify_history_authority(self)?;
            base.binds(signed)?;
            if signed.statement.scope != scope {
                return Err(denied("strong base scope mismatch"));
            }
            if let Some(result) = &base.latest {
                result.verify_history(self)?;
            }
        } else if let Some(result) = &base.latest {
            let package = result.verify_history(self)?;
            if base.head != Some((package.sequence, result.receipt()?.confirmation.new_head)) {
                return Err(denied("corrupt strong base head"));
            }
            let part = result
                .parts
                .iter()
                .find(|part| part.scope == scope && part.kind == PackageKind::Rows)
                .ok_or_else(|| denied("strong base rows missing"))?;
            let key = self
                .opening_key(scope, part.generation)
                .ok_or_else(|| denied("strong base key missing"))?;
            let projection: RowProjection = decode(&part.open(&key.key)?)?;
            if projection.after != base.rows {
                return Err(denied("corrupt strong base rows"));
            }
        } else if base.head.is_some() || !base.rows.is_empty() {
            return Err(denied("strong base anchor missing"));
        }
        Ok(())
    }
    /// Whether a retained result holds a row projection this replica can
    /// open and has not applied yet.
    async fn projects(
        &self,
        result: &ConfirmedCommand,
        grants: &[rocky_core::ResourceGrant],
    ) -> Result<bool, Error> {
        let sequence = result.strong_package()?.sequence;
        for part in &result.parts {
            if part.kind != PackageKind::Rows
                || !self.can_read_key(part.scope, grants)
                || self.opening_key(part.scope, part.generation).is_none()
            {
                continue;
            }
            let mut stored = self
                .conn
                .query(
                    "SELECT sequence FROM secure_command_rows WHERE scope=?",
                    [encode(&part.scope)?],
                )
                .await?;
            let applied = match stored.next().await? {
                Some(row) => row.get::<i64>(0)? as u64,
                None => 0,
            };
            if applied < sequence {
                return Ok(true);
            }
        }
        Ok(false)
    }
    async fn apply_command_result(
        &mut self,
        result: &ConfirmedCommand,
        package: &StrongPackage,
    ) -> Result<(), Error> {
        let mut retained = self
            .conn
            .query(
                "SELECT result FROM secure_results WHERE domain=? AND sequence=?",
                bindings(vec![
                    SqlValue::Blob(encode(&package.request.scope)?),
                    SqlValue::Integer(package.sequence as i64),
                ]),
            )
            .await?;
        let mut combined = result.clone();
        let mut old_size = 0;
        let exists = if let Some(row) = retained.next().await? {
            let old_bytes = row.get::<Vec<u8>>(0)?;
            old_size = old_bytes.len();
            let old: ConfirmedCommand = decode(&old_bytes)?;
            if old.package != result.package || old.receipt != result.receipt {
                return Err(denied("conflicting confirmed result"));
            }
            for part in old.parts {
                if !combined
                    .parts
                    .iter()
                    .any(|new| new.commitment().ok() == part.commitment().ok())
                {
                    combined.parts.push(part);
                }
            }
            true
        } else {
            false
        };
        combined.verify_history(self)?;
        let grants = self.local_grants(now()?)?;
        let current = self
            .verify_chain(&result.command.credentials, result.command.node, now()?)
            .and_then(|caller| {
                Ok((
                    caller,
                    self.verify_chain(&result.credentials, result.executor, now()?)?,
                ))
            });
        // A result confirmed under an earlier policy epoch keeps the grants it had.
        let (caller, executor) = match current {
            Ok(grants) => grants,
            Err(_) => result.historical_grants(self)?,
        };
        let mut changes = Vec::new();
        let mut projections = Vec::new();
        for part in &combined.parts {
            if part.kind != PackageKind::Rows || !self.can_read_key(part.scope, &grants) {
                continue;
            }
            let Some(key) = self.opening_key(part.scope, part.generation) else {
                continue;
            };
            let mut stored = self
                .conn
                .query(
                    "SELECT sequence,head FROM secure_command_rows WHERE scope=?",
                    [encode(&part.scope)?],
                )
                .await?;
            let (sequence, head) = if let Some(row) = stored.next().await? {
                (row.get::<i64>(0)? as u64, row.get::<Vec<u8>>(1)?)
            } else {
                (
                    0,
                    rocky_core::genesis(package.request.scope, package.base.configuration).to_vec(),
                )
            };
            if sequence >= package.sequence {
                continue;
            }
            if sequence != package.base.sequence {
                continue;
            }
            if head != package.base.head {
                return Err(denied("encrypted result predecessor fork"));
            }
            let projection: RowProjection = decode(&part.open(&key.key)?)?;
            if projection.before != self.command_rows(part.scope).await? {
                return Err(Error::Conflict);
            }
            for row in projection
                .before
                .keys()
                .chain(projection.after.keys())
                .copied()
                .collect::<BTreeSet<_>>()
            {
                if projection.before.get(&row) == projection.after.get(&row) {
                    continue;
                }
                let change = rocky_core::RowChange {
                    scope: part.scope,
                    row,
                    before: projection.before.get(&row).cloned(),
                    after: projection.after.get(&row).cloned(),
                };
                if !change.permitted_by(&caller) || !change.permitted_by(&executor) {
                    return Err(denied("confirmed row/field effect denied"));
                }
                changes.push(change);
            }
            projections.push((part.scope, projection.after));
        }
        validate_effects(self, &changes)?;
        self.app
            .validate_confirmed(&changes)
            .map_err(|message| denied(&message))?;
        for (scope, projection) in &projections {
            self.conn.execute("INSERT INTO secure_command_rows VALUES(?,?,?,?) ON CONFLICT(scope) DO UPDATE SET sequence=excluded.sequence,head=excluded.head,rows=excluded.rows", bindings(vec![SqlValue::Blob(encode(scope)?), SqlValue::Integer(package.sequence as i64), SqlValue::Blob(result.receipt()?.confirmation.new_head.to_vec()), SqlValue::Blob(encode(projection)?)])).await?;
            let key = self
                .config
                .keys
                .iter()
                .find(|key| key.scope == *scope)
                .ok_or_else(|| denied("missing projection key"))?;
            self.conn
                .execute(
                    "INSERT INTO secure_keys VALUES(?,?,?) ON CONFLICT(scope) DO NOTHING",
                    bindings(vec![
                        SqlValue::Blob(encode(scope)?),
                        SqlValue::Integer(key.generation as i64),
                        SqlValue::Blob(
                            rocky_core::commitment("rocky.scope-key.v1", &key.key).to_vec(),
                        ),
                    ]),
                )
                .await?;
            // A command-controlled table shows the confirmed rows at once.
            self.project_confirmed(*scope).await?;
        }
        if !projections.is_empty() {
            self.cause(crate::observe::Cause::Confirmed);
        }
        let bytes = combined.to_bytes()?;
        if !exists {
            self.command_budget(bytes.len()).await?;
        } else if bytes.len() > old_size {
            self.command_budget(bytes.len() - old_size).await?;
        }
        self.conn.execute("INSERT INTO secure_results VALUES(?,?,?) ON CONFLICT(domain,sequence) DO UPDATE SET result=excluded.result", bindings(vec![SqlValue::Blob(encode(&package.request.scope)?), SqlValue::Integer(package.sequence as i64), SqlValue::Blob(bytes)])).await?;
        self.progress(
            &package.request.hash()?,
            super::ticket::CONFIRMED,
            Some(package.sequence),
        )
        .await?;
        if !projections.is_empty() {
            self.conn
                .execute(
                    "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                    (),
                )
                .await?;
        }
        #[cfg(test)]
        {
            if self.retry_before_commit > 0 {
                self.retry_before_commit -= 1;
                return Err(crate::sqlite::Error::Busy("injected busy file".into()).into());
            }
            if self.fail_before_commit > 0 {
                self.fail_before_commit -= 1;
                return Err(Error::InjectedFailure);
            }
        }
        Ok(())
    }
}

impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Revalidate retained receipts and apply newly available scope projections after key installation.
    pub async fn reconcile_encrypted_results(
        &self,
        coordinator: &dyn Coordinator,
    ) -> Result<(), Error> {
        let retained = {
            let inner = self.inner.lock().await;
            rows(
                &inner.conn,
                "SELECT result FROM secure_results ORDER BY sequence",
            )
            .await?
        };
        for record in retained {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(denied("corrupt retained result"));
            };
            self.receive_encrypted_result(decode(bytes)?, coordinator)
                .await?;
        }
        Ok(())
    }
    pub async fn command_companion(
        &self,
        scope: ResourceScope,
        row: Commitment,
    ) -> Result<Companion, Error> {
        let snapshot = self.command_snapshot(scope).await?;
        if !snapshot.complete {
            return Ok(Companion::Unavailable);
        }
        if let Some(fields) = snapshot.rows.get(&row) {
            return Ok(Companion::Present(fields.clone()));
        }
        let inner = self.inner.lock().await;
        if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
            return Err(denied("companion read denied"));
        }
        let Some(key) = inner.config.keys.iter().find(|key| key.scope == scope) else {
            return Ok(Companion::Unavailable);
        };
        let mut existed = false;
        for record in rows(
            &inner.conn,
            "SELECT result FROM secure_results ORDER BY sequence",
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(denied("corrupt retained result"));
            };
            let result: ConfirmedCommand = decode(bytes)?;
            for part in result
                .parts
                .iter()
                .filter(|part| part.scope == scope && part.kind == PackageKind::Rows)
            {
                let projection: RowProjection = decode(&part.open(&key.key)?)?;
                existed |=
                    projection.before.contains_key(&row) || projection.after.contains_key(&row);
            }
        }
        Ok(if existed {
            Companion::Deleted
        } else {
            Companion::Unavailable
        })
    }
}
