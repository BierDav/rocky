//! Existence logs folded at a seal by a replica that holds every record
//! through its frontier. See `docs/runtime/lazy-compaction.md#existence-logs`.
use super::base::ExistenceBasis;
use super::*;
use rocky_credentials::{
    EXISTENCE_BASE_FORMAT, ExistenceBase, ExistenceBaseStatement, ExistenceCut, ExistenceIdentity,
    ExistenceLog, MAX_EXISTENCE_BASE_LOGS, SignedSeal,
};

const LIVE: &str = "rocky.existence-base-live.v1";

impl<A: RowApplication> Inner<A> {
    /// Whether `scope`'s base is a fold at a seal off the followed path: a
    /// base of a seal of the path replaces it, whatever it covers.
    pub(in crate::replica) async fn fold_off_path(
        &self,
        scope: ResourceScope,
    ) -> Result<bool, Error> {
        match self
            .existence_base(scope)
            .await?
            .and_then(|base| base.seal())
        {
            Some(seal) => Ok(self.path_index(scope.domain, seal)?.is_none()),
            None => Ok(false),
        }
    }
    /// Per author and author position, the records of `scope`'s logs held.
    #[cfg(test)]
    async fn scan_existence_positions(
        &self,
        scope: ResourceScope,
    ) -> Result<BTreeMap<u32, BTreeMap<u64, u64>>, Error> {
        let mut positions = BTreeMap::<u32, BTreeMap<u64, u64>>::new();
        let mut after = None;
        loop {
            let page = self
                .existence_scan(scope, after, MAX_EXISTENCE_PAGE)
                .await?;
            for record in &page.records {
                let statement = &record.statement;
                *positions
                    .entry(statement.author)
                    .or_default()
                    .entry(statement.author_sequence)
                    .or_default() += 1;
            }
            after = page.next;
            if after.is_none() {
                return Ok(positions);
            }
        }
    }
    pub(in crate::replica) async fn existence_count(
        &self,
        scope: ResourceScope,
        author: u32,
        from: u64,
        to: u64,
    ) -> Result<u64, Error> {
        if from > i64::MAX as u64 {
            return Ok(0);
        }
        let result=collect_rows(self.conn.query("SELECT count(*) FROM secure_existence_log WHERE scope=? AND author=? AND position>=? AND position<=?",bindings(vec![SqlValue::Blob(encode(&scope)?),SqlValue::Integer(i64::from(author)),SqlValue::Integer(i64::try_from(from).unwrap_or(i64::MAX)),SqlValue::Integer(i64::try_from(to).unwrap_or(i64::MAX))])).await?).await?;
        let Some([SqlValue::Integer(count)]) = result.first().map(Vec::as_slice) else {
            return Err(corrupt());
        };
        #[cfg(test)]
        {
            let _oracle = self.position_oracles.start();
            let scan = self.scan_existence_positions(scope).await?;
            let expected = scan.get(&author).map_or(0, |counts| {
                counts
                    .range(from..=to)
                    .map(|(_, count)| *count)
                    .sum::<u64>()
            });
            if *count as u64 != expected {
                return Err(Error::Corrupt(
                    "existence record count differs from scan".into(),
                ));
            }
        }
        Ok(*count as u64)
    }
    /// Inside the caller's transaction, folds every record of `scope`'s logs
    /// that `seal`'s frontier includes into a local base at the seal: rows
    /// dead there and their records are released, identity marks keep their
    /// ids retired. Records beyond the frontier stay.
    pub(in crate::replica) async fn fold_existence(
        &self,
        scope: ResourceScope,
        seal: &SignedSeal,
    ) -> Result<(), Error> {
        let id = seal.id().map_err(credential)?;
        let previous = self.existence_base(scope).await?;
        if let Some(previous) = &previous {
            // A base at this seal or past it already folded more.
            let at = self.path_index(scope.domain, id)?;
            if let Some(folded) = previous.seal()
                && self.path_index(scope.domain, folded)? >= at
            {
                return Ok(());
            }
        }
        let frontier: BTreeMap<u32, u64> = seal
            .statement
            .frontier
            .iter()
            .map(|mark| (mark.author, mark.sequence))
            .collect();
        let (mut logs, mut marks) = match &previous {
            Some(previous) => (
                previous
                    .cut()?
                    .logs
                    .iter()
                    .map(|log| (log.signer, log.clone()))
                    .collect::<BTreeMap<_, _>>(),
                previous
                    .descriptor()?
                    .identities
                    .iter()
                    .map(|mark| (mark.author, mark.clone()))
                    .collect::<BTreeMap<_, _>>(),
            ),
            None => Default::default(),
        };
        // A signer's log folds a prefix: its author positions only grow.
        let mut unfolded = BTreeSet::new();
        let mut beyond = BTreeSet::new();
        let mut after = None;
        loop {
            let page = self
                .existence_scan(scope, after, MAX_EXISTENCE_PAGE)
                .await?;
            for record in &page.records {
                let statement = &record.statement;
                let hash = record.hash()?;
                if !frontier
                    .get(&statement.author)
                    .is_some_and(|mark| statement.author_sequence <= *mark)
                {
                    beyond.insert(record.signer);
                    unfolded.insert(hash);
                    continue;
                }
                if beyond.contains(&record.signer) {
                    return Err(denied("an existence log's author positions go back"));
                }
                logs.insert(
                    record.signer,
                    ExistenceLog {
                        signer: record.signer,
                        sequence: statement.sequence,
                        head: hash,
                    },
                );
                if statement.event == ExistenceEvent::Created {
                    let identity = statement.id;
                    let mark = marks.entry(identity.author()).or_insert(ExistenceIdentity {
                        author: identity.author(),
                        signer: record.signer,
                        sequence: identity.sequence(),
                    });
                    if mark.signer != record.signer {
                        return Err(denied("existence author changed signer"));
                    }
                    mark.sequence = mark.sequence.max(identity.sequence());
                }
            }
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        if logs.len() > MAX_EXISTENCE_BASE_LOGS || marks.len() > MAX_EXISTENCE_BASE_LOGS {
            return Err(denied("existence base author quota"));
        }
        // What the seal evidences of each row: a creation or a deletion
        // alone; rows with both are dead there and released.
        let blob = encode(&scope)?;
        self.conn
            .execute(
                "DELETE FROM secure_existence_base_rows WHERE scope=?",
                [blob.clone()],
            )
            .await?;
        let (mut count, mut digest, mut cursor) = (0u64, checkpoint::Lt::new(), 0i64);
        loop {
            let found = rows_with(
                &self.conn,
                "SELECT id,created,deleted FROM secure_existence WHERE scope=? AND id>? ORDER BY id LIMIT ?",
                vec![
                    SqlValue::Blob(blob.clone()),
                    SqlValue::Integer(cursor),
                    SqlValue::Integer(MAX_EXISTENCE_PAGE as i64),
                ],
            )
            .await?;
            if found.is_empty() {
                break;
            }
            for row in found {
                let [SqlValue::Integer(id), created, deleted] = row.as_slice() else {
                    return Err(corrupt());
                };
                cursor = *id;
                let folded = |value: &SqlValue| -> Result<Option<Commitment>, Error> {
                    Ok(match value {
                        SqlValue::Null => None,
                        SqlValue::Blob(bytes) => {
                            Some(checkpoint::row_of(bytes)?).filter(|hash| !unfolded.contains(hash))
                        }
                        _ => return Err(corrupt()),
                    })
                };
                let (created, deleted) = (folded(created)?, folded(deleted)?);
                match (created, deleted) {
                    (Some(_), Some(_)) => {
                        self.conn
                            .execute(
                                "DELETE FROM secure_existence WHERE scope=? AND id=?",
                                bindings(vec![
                                    SqlValue::Blob(blob.clone()),
                                    SqlValue::Integer(*id),
                                ]),
                            )
                            .await?;
                    }
                    (None, None) => {}
                    (created, deleted) => {
                        let hash = |value: Option<Commitment>| {
                            value.map_or(SqlValue::Null, |hash| SqlValue::Blob(hash.to_vec()))
                        };
                        digest.add(LIVE, &encode(&(*id, created, deleted))?);
                        count += 1;
                        self.conn
                            .execute(
                                "INSERT INTO secure_existence_base_rows VALUES(?,?,?,?)",
                                bindings(vec![
                                    SqlValue::Blob(blob.clone()),
                                    SqlValue::Integer(*id),
                                    hash(created),
                                    hash(deleted),
                                ]),
                            )
                            .await?;
                    }
                }
            }
        }
        for log in logs.values() {
            self.conn
                .execute(
                    "DELETE FROM secure_existence_log WHERE scope=? AND signer=? AND sequence<=?",
                    bindings(vec![
                        SqlValue::Blob(blob.clone()),
                        SqlValue::Blob(log.signer.as_bytes().to_vec()),
                        SqlValue::Integer(log.sequence as i64),
                    ]),
                )
                .await?;
        }
        let base = ExistenceBase {
            scope,
            rows: count,
            digest: digest.digest(),
            identities: marks.into_values().collect(),
        };
        let statement = ExistenceBaseStatement {
            format: EXISTENCE_BASE_FORMAT,
            scope: scope.domain,
            seal: id,
            cut: ExistenceCut {
                scope,
                logs: logs.into_values().collect(),
                state: base.state().map_err(credential)?,
            },
            base,
        };
        statement.validate().map_err(credential)?;
        let attachment = ExistenceBaseAttachment {
            basis: ExistenceBasis::Local(Box::new(statement)),
            scope,
        };
        self.conn
            .execute(
                "INSERT INTO secure_existence_base VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET attachment=excluded.attachment",
                [blob, encode(&attachment)?],
            )
            .await?;
        self.archive_existence_bases().await?;
        self.collect_existence_archives().await
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Whether `scope`'s existence base is a fold at a seal off the
    /// followed path, which a base of the path replaces.
    pub async fn existence_fold_off_path(&self, scope: ResourceScope) -> Result<bool, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .fold_off_path(scope)
            .await
    }
    /// This replica's own folds at seals in `domain`, which the owner, a
    /// trusted identity or an issuer may sign for devices that fetch a base.
    pub async fn unsigned_existence_bases(
        &self,
        domain: rocky_core::Scope,
    ) -> Result<Vec<ExistenceBaseStatement>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut statements = Vec::new();
        for row in rows(&inner.conn, "SELECT attachment FROM secure_existence_base").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            let attachment: ExistenceBaseAttachment = decode(bytes)?;
            if let ExistenceBasis::Local(statement) = attachment.basis
                && statement.scope == domain
            {
                statements.push(*statement);
            }
        }
        Ok(statements)
    }
    /// Replaces this replica's own fold with the same fold signed, which
    /// peers may then fetch.
    pub async fn install_signed_existence_base(
        &self,
        signed: rocky_credentials::SignedExistenceBase,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let scope = signed.statement.cut.scope;
        let attachment = ExistenceBaseAttachment {
            basis: ExistenceBasis::Signed(Box::new(signed)),
            scope,
        };
        inner.verify_existence_base(&attachment)?;
        let held = inner.existence_base(scope).await?;
        if held.as_ref().and_then(|held| held.statement()) != attachment.statement() {
            return Err(denied("only this replica's own fold at a seal is signed"));
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "UPDATE secure_existence_base SET attachment=? WHERE scope=?",
                    [encode(&attachment)?, encode(&scope)?],
                )
                .await?;
            Ok(())
        })?;
        flush_guard(inner, &self.inner).await.map(|_| ())
    }
}
