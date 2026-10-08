//! Strong bases and atomic, bounded retention. See `docs/runtime/strong-history.md`.
mod retention;
use super::*;
use rocky_core::RowApplication;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct StrongState {
    pub head: Option<(u64, Commitment)>,
    pub rows: BTreeMap<Commitment, rocky_core::RowFields>,
    pub latest: Option<ConfirmedCommand>,
    pub checkpoint: Option<SignedCheckpoint>,
}
impl StrongState {
    pub(crate) fn empty() -> Self {
        Self {
            head: None,
            rows: BTreeMap::new(),
            latest: None,
            checkpoint: None,
        }
    }
    pub(crate) fn sequence(&self) -> u64 {
        self.head.map_or(0, |head| head.0)
    }
    pub(crate) fn commitment(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.checkpoint-command-rows.v1",
            &encode(&self.rows)?,
        ))
    }
    pub(crate) fn binds(&self, signed: &SignedCheckpoint) -> Result<(), Error> {
        if self.head != signed.statement.strong.base
            || self.head != signed.statement.strong.head
            || self.commitment()? != signed.statement.strong.rows
        {
            return Err(denied("strong checkpoint rows/base mismatch"));
        }
        match (&self.latest, self.head) {
            (Some(result), Some((sequence, head)))
                if result.strong_package()?.sequence == sequence
                    && result.strong_package()?.request.scope == signed.statement.scope.domain
                    && result.receipt()?.confirmation.new_head == head =>
            {
                Ok(())
            }
            (None, None) if self.rows.is_empty() => Ok(()),
            _ => Err(denied("strong checkpoint latest result mismatch")),
        }
    }
}
impl<A: RowApplication> Inner<A> {
    pub(crate) async fn index_preparation(
        &self,
        key: &[u8],
        result: &ConfirmedCommand,
    ) -> Result<(), Error> {
        let package = result.strong_package()?;
        self.conn.execute("INSERT INTO secure_preparation_requests VALUES(?,?,?,?) ON CONFLICT(preparation) DO NOTHING", bindings(vec![SqlValue::Blob(key.to_vec()),SqlValue::Blob(package.request.hash()?.to_vec()),SqlValue::Blob(encode(&package.request.scope)?),SqlValue::Integer(package.sequence as i64)])).await?;
        Ok(())
    }
    pub(crate) async fn strong_base(&self, scope: ResourceScope) -> Result<StrongState, Error> {
        let mut records = self
            .conn
            .query(
                "SELECT state FROM secure_strong_base WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        match records.next().await? {
            Some(row) => decode(&row.get::<Vec<u8>>(0)?),
            None => Ok(StrongState::empty()),
        }
    }
    pub(crate) async fn capture_strong(&self, scope: ResourceScope) -> Result<StrongState, Error> {
        let mut state = StrongState::empty();
        let mut records = self
            .conn
            .query(
                "SELECT sequence,head,rows FROM secure_command_rows WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        if let Some(row) = records.next().await? {
            state.head = Some((
                row.get::<i64>(0)? as u64,
                row.get::<Vec<u8>>(1)?
                    .as_slice()
                    .try_into()
                    .map_err(|_| denied("strong head"))?,
            ));
            state.rows = decode(&row.get::<Vec<u8>>(2)?)?;
            let mut latest = self
                .conn
                .query(
                    "SELECT result FROM secure_results WHERE domain=? AND sequence=?",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope.domain)?),
                        SqlValue::Integer(state.sequence() as i64),
                    ]),
                )
                .await?;
            state.latest = match latest.next().await? {
                Some(row) => Some(decode(&row.get::<Vec<u8>>(0)?)?),
                None => return Err(denied("strong checkpoint has no latest result")),
            };
        }
        if let Some(result) = &state.latest {
            if state.head
                != Some((
                    result.strong_package()?.sequence,
                    result.receipt()?.confirmation.new_head,
                ))
            {
                return Err(denied("strong projection head differs from latest result"));
            }
            let part = result
                .parts
                .iter()
                .find(|part| part.scope == scope && part.kind == PackageKind::Rows);
            if let Some(part) = part
                && let Some(key) = self.opening_key(scope, part.generation)
            {
                let projection: RowProjection = decode(&part.open(&key.key)?)?;
                if projection.after != state.rows {
                    return Err(denied("strong projection differs from latest result"));
                }
            } else {
                let base = self.strong_base(scope).await?;
                let signed = base
                    .checkpoint
                    .as_ref()
                    .ok_or_else(|| denied("strong projection base unavailable"))?;
                signed.verify_history_authority(self)?;
                base.binds(signed)?;
                if base.head != state.head || base.rows != state.rows {
                    return Err(denied("strong projection differs from checkpoint base"));
                }
            }
        }
        Ok(state)
    }
    pub(crate) async fn put_strong_base(
        &self,
        scope: ResourceScope,
        state: &StrongState,
    ) -> Result<(), Error> {
        self.conn.execute("INSERT INTO secure_strong_base VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET state=excluded.state", [encode(&scope)?, encode(state)?]).await?;
        Ok(())
    }
    pub(crate) async fn install_strong(
        &mut self,
        scope: ResourceScope,
        state: &StrongState,
    ) -> Result<(), Error> {
        if self.strong_base(scope).await?.sequence() > state.sequence() {
            return Err(denied("strong checkpoint behind local base"));
        }
        let current = self.capture_strong(scope).await?;
        if current.sequence() > state.sequence()
            || current.sequence() == state.sequence() && current.head != state.head
        {
            return Err(denied(
                "strong checkpoint behind or forks local application",
            ));
        }
        self.put_strong_base(scope, state).await?;
        if let Some((sequence, head)) = state.head {
            self.conn.execute("INSERT INTO secure_command_rows VALUES(?,?,?,?) ON CONFLICT(scope) DO UPDATE SET sequence=excluded.sequence,head=excluded.head,rows=excluded.rows", bindings(vec![SqlValue::Blob(encode(&scope)?),SqlValue::Integer(sequence as i64),SqlValue::Blob(head.to_vec()),SqlValue::Blob(encode(&state.rows)?)])).await?;
            if let Some(result) = &state.latest {
                // Preserve other scopes' parts when importing a domain one scope at a time.
                let mut combined = result.clone();
                let mut held = self
                    .conn
                    .query(
                        "SELECT result FROM secure_results WHERE domain=? AND sequence=?",
                        bindings(vec![
                            SqlValue::Blob(encode(&scope.domain)?),
                            SqlValue::Integer(sequence as i64),
                        ]),
                    )
                    .await?;
                if let Some(row) = held.next().await? {
                    let old: ConfirmedCommand = decode(&row.get::<Vec<u8>>(0)?)?;
                    if old.package != result.package || old.receipt != result.receipt {
                        return Err(Error::Conflict);
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
                }
                self.conn.execute("INSERT INTO secure_results VALUES(?,?,?) ON CONFLICT(domain,sequence) DO UPDATE SET result=excluded.result", bindings(vec![SqlValue::Blob(encode(&scope.domain)?),SqlValue::Integer(sequence as i64),SqlValue::Blob(combined.to_bytes()?)])).await?;
            }
        }
        self.audited.remove(&scope);
        self.reconciled = None;
        Ok(())
    }
    pub(crate) async fn check_pruned_handle(
        &self,
        handle: Commitment,
        input: Option<Commitment>,
    ) -> Result<(), Error> {
        let mut records = self
            .conn
            .query(
                "SELECT input,node,sequence FROM secure_pruned_commands WHERE handle=?",
                [handle.to_vec()],
            )
            .await?;
        if let Some(row) = records.next().await? {
            if input.is_some_and(|input| row.get::<Vec<u8>>(0).is_ok_and(|old| old != input)) {
                return Err(Error::Conflict);
            }
            let node: [u8; 16] = row
                .get::<Vec<u8>>(1)?
                .as_slice()
                .try_into()
                .map_err(|_| denied("pruned command node"))?;
            return Err(Error::Pruned {
                node: NodeId(node),
                sequence: row.get::<i64>(2)? as u64,
            });
        }
        Ok(())
    }
}
