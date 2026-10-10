use super::*;
impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Filters payloads while preserving the receipt's commitment to the complete result.
    pub async fn encrypted_result_for(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        domain: Scope,
        sequence: u64,
        scopes: &[ResourceScope],
    ) -> Result<Option<ConfirmedCommand>, Error> {
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        authorize_transfer(&inner, peer, credentials, scopes)?;
        if scopes.iter().any(|scope| scope.domain != domain) {
            return Err(denied("result subscription domain mismatch"));
        }
        let mut retained = inner
            .conn
            .query(
                "SELECT result FROM secure_results WHERE domain=? AND sequence=?",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(sequence as i64),
                ]),
            )
            .await?;
        let Some(row) = retained.next().await? else {
            return Ok(None);
        };
        let mut result: ConfirmedCommand = decode(&row.get::<Vec<u8>>(0)?)?;
        result.parts.retain(|part| scopes.contains(&part.scope));
        Ok(Some(result))
    }
    /// The confirmed sequences up to `head` this replica retains no result
    /// for, lowest first and at most `limit`.
    pub async fn missing_results(
        &self,
        domain: Scope,
        head: u64,
        limit: usize,
    ) -> Result<Vec<u64>, Error> {
        let inner = self.inner.lock().await;
        let retained: BTreeSet<u64> = collect_rows(
            inner
                .conn
                .query(
                    "SELECT sequence FROM secure_results WHERE domain=?",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?
        .iter()
        .filter_map(|row| match row.as_slice() {
            [SqlValue::Integer(sequence)] => Some(*sequence as u64),
            _ => None,
        })
        .collect();
        let mut needed_scopes = BTreeSet::new();
        for record in collect_rows(
            inner
                .conn
                .query(
                    "SELECT result FROM secure_results WHERE domain=? ORDER BY sequence DESC",
                    [encode(&domain)?],
                )
                .await?,
        )
        .await?
        {
            if let [SqlValue::Blob(bytes)] = record.as_slice() {
                let request = decode::<ConfirmedCommand>(bytes)?.strong_package()?.request;
                needed_scopes.extend(resources(&inner, &request)?);
            }
        }
        let grants = inner.local_grants(now()?)?;
        let mut start = head;
        let mut found = false;
        for record in rows(&inner.conn, "SELECT scope FROM secure_command_rows").await? {
            if let [SqlValue::Blob(bytes)] = record.as_slice() {
                let scope: ResourceScope = decode(bytes)?;
                if scope.domain == domain {
                    found = true;
                    start = start.min(inner.strong_base(scope).await?.sequence());
                }
            }
        }
        for scope in needed_scopes {
            if inner.can_read_key(scope, &grants)
                && inner.config.keys.iter().any(|key| key.scope == scope)
            {
                found = true;
                start = start.min(inner.strong_base(scope).await?.sequence());
            }
        }
        if !found {
            start = 0;
        }
        Ok((start.saturating_add(1)..=head)
            .filter(|sequence| !retained.contains(sequence))
            .take(limit)
            .collect())
    }
    pub(crate) async fn authorize_command_transfer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
    ) -> Result<(), Error> {
        let inner = self.inner.lock().await;
        authorize_transfer(&inner, peer, credentials, scopes)
    }
}
fn authorize_transfer<A: RowCommandApplication>(
    inner: &Inner<A>,
    peer: PublicKey,
    credentials: &CredentialChain,
    scopes: &[ResourceScope],
) -> Result<(), Error> {
    let grants = inner.verify_chain(credentials, peer, now()?)?;
    if scopes.is_empty() || scopes.len() > 32 {
        return Err(denied("result subscription quota"));
    }
    for scope in scopes {
        if !grants.iter().any(|grant| {
            grant.scope == *scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&Right::Read) || grant.rights.contains(&Right::Custody))
        }) {
            return Err(denied("result ciphertext custody denied"));
        }
    }
    Ok(())
}

impl<A: RowCommandApplication> EncryptedReplica<A> {
    pub(crate) async fn result_ack(
        &self,
        result: &ConfirmedCommand,
    ) -> Result<Vec<(ResourceScope, bool)>, Error> {
        let mut inner = self.inner.lock().await;
        // The states are sent to the result's sender.
        inner.flush()?;
        let grants = inner.local_grants(now()?)?;
        let package = result.strong_package()?;
        let scopes: BTreeSet<_> = result.parts.iter().map(|part| part.scope).collect();
        let mut states = Vec::new();
        for scope in scopes {
            let mut rows = inner
                .conn
                .query(
                    "SELECT sequence FROM secure_command_rows WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?;
            let applied = inner.can_read_key(scope, &grants)
                && inner.config.keys.iter().any(|key| key.scope == scope)
                && rows.next().await?.is_some_and(|row| {
                    row.get::<i64>(0)
                        .is_ok_and(|sequence| sequence as u64 >= package.sequence)
                });
            states.push((scope, applied));
        }
        Ok(states)
    }
    pub(crate) async fn acknowledge_result(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        result: &ConfirmedCommand,
        states: &[(ResourceScope, bool)],
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let scopes: BTreeSet<_> = result.parts.iter().map(|part| part.scope).collect();
        if states.len() != scopes.len()
            || states
                .iter()
                .map(|(scope, _)| *scope)
                .collect::<BTreeSet<_>>()
                != scopes
        {
            return Err(denied("result acknowledgment scope mismatch"));
        }
        let grants = inner.verify_chain(credentials, peer, now()?)?;
        let package = result.strong_package()?;
        transaction!(inner, |inner| {
            for (scope, applied) in states {
                if !grants.iter().any(|grant| {
                    grant.scope == *scope
                        && grant.rows == RowSelection::All
                        && (grant.rights.contains(&Right::Read)
                            || (!applied && grant.rights.contains(&Right::Custody)))
                }) {
                    return Err(denied("result acknowledgment authority denied"));
                }
                inner.conn.execute("INSERT INTO secure_result_ack VALUES(?,?,?,?,?) ON CONFLICT(peer,domain,sequence,scope) DO UPDATE SET applied=MAX(applied,excluded.applied)", bindings(vec![SqlValue::Blob(rocky_credentials::PeerId::of(peer).as_bytes().to_vec()), SqlValue::Blob(encode(&package.request.scope)?), SqlValue::Integer(package.sequence as i64), SqlValue::Blob(encode(scope)?), SqlValue::Integer(i64::from(*applied))])).await?;
            }
            Ok(())
        })?;
        inner.flush()?;
        let scopes = states.iter().map(|(scope, _)| *scope).collect();
        inner.maintain(&scopes).await;
        inner.flush()
    }
}
