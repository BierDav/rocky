pub(crate) const STRONG_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rocky_strong_meta(id INTEGER PRIMARY KEY,command_sequence INTEGER NOT NULL,configuration BLOB,sequence INTEGER NOT NULL,head BLOB)",
    "INSERT INTO rocky_strong_meta SELECT 1,0,NULL,0,NULL WHERE NOT EXISTS (SELECT 1 FROM rocky_strong_meta)",
    "CREATE TABLE IF NOT EXISTS rocky_command_handles(handle BLOB PRIMARY KEY,method TEXT NOT NULL,node BLOB NOT NULL,sequence INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS rocky_requests(node BLOB NOT NULL,sequence INTEGER NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(node,sequence))",
    "CREATE TABLE IF NOT EXISTS rocky_preparations(hash BLOB PRIMARY KEY,bytes BLOB NOT NULL,state TEXT NOT NULL,receipt BLOB,verified INTEGER NOT NULL,applied INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS rocky_strong_outbox(hash BLOB PRIMARY KEY)",
    "CREATE TABLE IF NOT EXISTS rocky_rejected_receipts(hash BLOB NOT NULL,package BLOB NOT NULL,receipt BLOB NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(hash,package,receipt))",
    "CREATE TABLE IF NOT EXISTS rocky_strong_ack(peer BLOB NOT NULL,hash BLOB NOT NULL,applied INTEGER NOT NULL,PRIMARY KEY(peer,hash))",
];
use super::*;
use crate::strong::{
    CommandProgress, CommandState, ConfirmedPublication, Request, Response, StrongStatus,
};
use rocky_core::*;

/// A retained package with its state, receipt, verified and applied flags.
pub(super) type Attempt = (StrongPackage, String, Option<StrongReceipt>, bool, bool);
/// The encoded request a package begins with; see `StrongPackage::encode`.
fn package_request(package: &[u8]) -> Option<&[u8]> {
    let (length, rest) = package.split_first_chunk::<4>()?;
    rest.get(..u32::from_le_bytes(*length) as usize)
}
fn attempt(row: &[SqlValue]) -> Result<Attempt, Error> {
    let [
        SqlValue::Blob(bytes),
        SqlValue::Text(state),
        receipt,
        SqlValue::Integer(verified),
        SqlValue::Integer(applied),
    ] = row
    else {
        return Err(Error::Corrupt("preparation row".into()));
    };
    let receipt = match receipt {
        SqlValue::Null => None,
        SqlValue::Blob(bytes) => Some(StrongReceipt::decode(bytes)?),
        _ => return Err(Error::Corrupt("receipt type".into())),
    };
    Ok((
        StrongPackage::decode(bytes)?,
        state.clone(),
        receipt,
        *verified == 1,
        *applied == 1,
    ))
}

impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    /// The strong ledger is bounded by bytes, like operation history;
    /// pruning releases applied results and their requests.
    pub(super) async fn strong_budget(&self, bytes: usize) -> Result<(), Error> {
        let budget = rows(self.conn(), "SELECT (SELECT COALESCE(SUM(LENGTH(bytes)),0) FROM rocky_requests)+(SELECT COALESCE(SUM(LENGTH(bytes)+COALESCE(LENGTH(receipt),0)),0) FROM rocky_preparations)+(SELECT COALESCE(SUM(LENGTH(package)+LENGTH(receipt)),0) FROM rocky_rejected_receipts)").await?;
        let Some([SqlValue::Integer(retained)]) = budget.first().map(Vec::as_slice) else {
            return Err(Error::Corrupt("strong budget".into()));
        };
        let limit = self.retention.policy.retained_bytes;
        if (*retained as usize).saturating_add(bytes) > limit {
            return Err(Error::Retention {
                retained: *retained as usize,
                limit,
            });
        }
        Ok(())
    }
    /// The request a handle published. A handle whose request was pruned
    /// with its applied result reports `Pruned`.
    async fn lookup_command(
        &self,
        handle: crate::CommandHandle,
    ) -> Result<Option<(String, StrongRequest)>, Error> {
        let mut rows = self
            .conn()
            .query(
                "SELECT h.method,h.node,h.sequence,r.bytes FROM rocky_command_handles h LEFT JOIN rocky_requests r ON r.node=h.node AND r.sequence=h.sequence WHERE h.handle=?",
                [handle.0.to_vec()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        match row.get_value(3)? {
            crate::sqlite::Value::Blob(bytes) => {
                Ok(Some((row.get(0)?, StrongRequest::decode(&bytes)?)))
            }
            _ => Err(Error::Pruned {
                node: self.identity.node,
                sequence: row.get::<i64>(2)? as u64,
            }),
        }
    }
    fn handler(&self, method: &str) -> Result<&dyn CommandHandler<A>, Error> {
        self.app
            .command(method)
            .ok_or_else(|| Error::Incompatible("unregistered command method".into()))
    }
    pub(super) async fn strong_schema(&self) -> Result<(), Error> {
        for sql in STRONG_SCHEMA {
            self.conn().execute(*sql, ()).await?;
        }
        Ok(())
    }
    pub(super) async fn local_strong_head(&self) -> Result<crate::LocalStrongHead, Error> {
        let metadata = rows(
            self.conn(),
            "SELECT configuration,sequence,head FROM rocky_strong_meta WHERE id=1",
        )
        .await?;
        crate::read::decode_head(metadata.first().map_or(&[], Vec::as_slice))
    }
    async fn status(&self) -> Result<StrongStatus, Error> {
        let meta = rows(
            self.conn(),
            "SELECT configuration,sequence,head FROM rocky_strong_meta WHERE id=1",
        )
        .await?;
        let Some([configuration, SqlValue::Integer(sequence), head]) =
            meta.first().map(Vec::as_slice)
        else {
            return Err(Error::Corrupt("strong metadata".into()));
        };
        fn fixed<const N: usize>(v: &SqlValue) -> Result<Option<[u8; N]>, Error> {
            match v {
                SqlValue::Null => Ok(None),
                SqlValue::Blob(b) => Ok(Some(
                    b.as_slice()
                        .try_into()
                        .map_err(|_| Error::Corrupt("commitment size".into()))?,
                )),
                _ => Err(Error::Corrupt("commitment type".into())),
            }
        }
        let mut status = StrongStatus {
            scope: self.identity.scope,
            configuration: fixed(configuration)?,
            sequence: *sequence as u64,
            head: fixed(head)?,
            commands: Vec::new(),
            received: Vec::new(),
            verification: Vec::new(),
            peer_acknowledgments: Vec::new(),
        };
        let mut acks = self
            .conn()
            .query(
                "SELECT peer,hash,applied FROM rocky_strong_ack ORDER BY peer,hash",
                (),
            )
            .await?;
        while let Some(row) = acks.next().await? {
            let peer: Vec<u8> = row.get(0)?;
            let hash: Vec<u8> = row.get(1)?;
            status.peer_acknowledgments.push((
                NodeId(
                    peer.as_slice()
                        .try_into()
                        .map_err(|_| Error::Corrupt("ack peer".into()))?,
                ),
                hash.as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("ack hash".into()))?,
                row.get::<i64>(2)? == 1,
            ));
        }
        let attempts = self.attempts().await?;
        // One hash per attempt: a package names every operation of its cut.
        let mut hashes = Vec::with_capacity(attempts.len());
        for (package, _, receipt, _, _) in &attempts {
            hashes.push(match receipt {
                Some(_) => Some(package.confirmation()?.result),
                None => None,
            });
        }
        let missing = || Error::Corrupt("applied without receipt".into());
        for row in rows(
            self.conn(),
            "SELECT r.bytes,COALESCE(h.released,0) FROM rocky_requests r LEFT JOIN rocky_request_holds h ON h.node=r.node AND h.sequence=r.sequence ORDER BY r.node,r.sequence",
        )
        .await?
        {
            let [SqlValue::Blob(b), SqlValue::Integer(released)] = row.as_slice() else {
                return Err(Error::Corrupt("request row".into()));
            };
            let request = StrongRequest::decode(b)?;
            let mut state = match released {
                0 => CommandState::Queued,
                1 => CommandState::Released,
                2 => CommandState::Retired,
                3 => CommandState::Expired,
                _ => return Err(Error::Corrupt("request hold".into())),
            };
            let mut peer_received = false;
            for ((p, s, _, _, applied), hash) in attempts.iter().zip(&hashes) {
                if p.request.id != request.id {
                    continue;
                }
                state = if *applied {
                    CommandState::ConfirmedLocally
                } else if *released != 0 {
                    state
                } else {
                    if s != "unknown" { return Err(Error::Corrupt("invalid preparation state".into())); }
                    CommandState::OutcomeUnknown
                };
                if *applied {
                    let hash = hash.ok_or_else(missing)?;
                    peer_received = self.config.delivery_targets.iter().all(|peer| {
                        status
                            .peer_acknowledgments
                            .iter()
                            .any(|(p, h, _)| p == peer && *h == hash)
                    });
                    if !self.config.delivery_targets.is_empty()
                        && self.config.delivery_targets.iter().all(|peer| {
                            status
                                .peer_acknowledgments
                                .iter()
                                .any(|(p, h, applied)| p == peer && *h == hash && *applied)
                        })
                    {
                        state = CommandState::PeerApplied;
                    }
                    break;
                }
            }
            status.commands.push(CommandProgress {
                request,
                state,
                peer_received,
            });
        }
        for ((_, _, _, verified, applied), hash) in attempts.iter().zip(hashes) {
            if let Some(hash) = hash {
                status.verification.push((
                    hash,
                    if *verified {
                        crate::VerificationState::Verified
                    } else {
                        crate::VerificationState::Pending
                    },
                ));
                status.received.push((hash, *applied));
            }
        }
        for row in rows(
            self.conn(),
            "SELECT DISTINCT hash FROM rocky_rejected_receipts ORDER BY hash",
        )
        .await?
        {
            let [SqlValue::Blob(hash)] = row.as_slice() else {
                return Err(Error::Corrupt("rejection evidence".into()));
            };
            status.verification.push((
                hash.as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("rejection hash".into()))?,
                crate::VerificationState::Rejected,
            ));
        }
        Ok(status)
    }
    /// The highest own sequence through which every request is settled:
    /// retired or expired by the coordinator, or applied here and by every
    /// delivery target not declared behind, which verified its receipt. A local release
    /// still counts as open, since another replica may execute it.
    pub(super) async fn acknowledgeable(&self) -> Result<u64, Error> {
        let status = self.status().await?;
        // Peers declared behind import a checkpoint instead of verifying results.
        let peers = self.config.delivery_targets.iter().all(|peer| {
            self.retention
                .stored
                .peers
                .iter()
                .any(|(known, mark)| known == peer && mark.behind)
        });
        for progress in &status.commands {
            if progress.request.id.node != self.identity.node {
                continue;
            }
            let settled = match progress.state {
                CommandState::Retired | CommandState::Expired | CommandState::PeerApplied => true,
                CommandState::ConfirmedLocally => peers,
                _ => false,
            };
            if !settled {
                return Ok(progress.request.id.sequence.get() - 1);
            }
        }
        let mut counter = self
            .conn()
            .query(
                "SELECT command_sequence FROM rocky_strong_meta WHERE id=1",
                (),
            )
            .await?;
        Ok(counter
            .next()
            .await?
            .map(|row| row.get::<i64>(0))
            .transpose()?
            .unwrap_or(0) as u64)
    }
    /// Unreleased requests without an applied attempt, in request order, with
    /// when each was stored. An applied package is matched by the request
    /// bytes it begins with, so no package is decoded.
    pub(super) async fn open_requests(&self) -> Result<Vec<(Vec<u8>, SqlValue)>, Error> {
        let requests = rows(
            self.conn(),
            "SELECT r.bytes,h.stored FROM rocky_requests r LEFT JOIN rocky_request_holds h ON h.node=r.node AND h.sequence=r.sequence WHERE COALESCE(h.released,0)=0 ORDER BY r.node,r.sequence",
        )
        .await?;
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let mut resolved = std::collections::HashSet::new();
        self.conn().connection().scan(
            &self
                .conn()
                .names()
                .sql("SELECT bytes FROM rocky_preparations WHERE applied=1"),
            Vec::new(),
            |row| {
                if let [crate::sqlite::Value::Blob(package)] = row
                    && let Some(request) = package_request(package)
                {
                    resolved.insert(request.to_vec());
                }
                Ok(())
            },
        )?;
        let mut open = Vec::new();
        for row in requests {
            let mut row = row.into_iter();
            let (Some(SqlValue::Blob(bytes)), Some(stored)) = (row.next(), row.next()) else {
                return Err(Error::Corrupt("request row".into()));
            };
            if !resolved.contains(&bytes) {
                open.push((bytes, stored));
            }
        }
        Ok(open)
    }
    pub(super) async fn attempts(&self) -> Result<Vec<Attempt>, Error> {
        self.attempts_where("").await
    }
    /// Retained attempts in insertion order, restricted by a fixed `filter`
    /// clause so that a hot path does not decode the whole ledger.
    pub(super) async fn attempts_where(&self, filter: &'static str) -> Result<Vec<Attempt>, Error> {
        let sql = format!(
            "SELECT bytes,state,receipt,verified,applied FROM rocky_preparations {filter} ORDER BY rowid"
        );
        rows(self.conn(), &sql)
            .await?
            .iter()
            .map(|row| attempt(row))
            .collect()
    }
    async fn attempt_by_hash(&self, hash: &Commitment) -> Result<Option<Attempt>, Error> {
        let stored = self
            .conn()
            .query(
                "SELECT bytes,state,receipt,verified,applied FROM rocky_preparations WHERE hash=?",
                [hash.to_vec()],
            )
            .await?;
        storage::collect_rows(stored)
            .await?
            .first()
            .map(|row| attempt(row))
            .transpose()
    }
    /// Every retained result with a receipt, verified or not.
    async fn results(&self) -> Result<Vec<ConfirmedPublication>, Error> {
        Ok(self
            .attempts()
            .await?
            .into_iter()
            .filter_map(|(package, _, receipt, _, _)| {
                receipt.map(|receipt| ConfirmedPublication { package, receipt })
            })
            .collect())
    }
    pub(super) async fn publications(&self) -> Result<Vec<ConfirmedPublication>, Error> {
        let mut values = Vec::new();
        for (p, _, r, _, applied) in self.attempts().await? {
            if applied {
                let receipt = r.ok_or_else(|| Error::Corrupt("applied without receipt".into()))?;
                values.push(ConfirmedPublication {
                    package: p,
                    receipt,
                });
            }
        }
        values.sort_by_key(|p| p.receipt.sequence);
        Ok(values)
    }
    pub(super) async fn predecessor(&self, base: &StrongHead) -> Result<Option<Vec<u8>>, Error> {
        if base.sequence == 0 {
            if base.head != genesis(base.scope, base.configuration) {
                return Err(Error::Rejected("invalid genesis predecessor".into()));
            }
            return Ok(None);
        }
        // The predecessor is almost always the newest applied result, so
        // read applied results newest first, a few at a time.
        const PAGE: i64 = 8;
        let mut offset = 0;
        loop {
            let page = storage::collect_rows(
                self.conn()
                    .query(
                        "SELECT bytes,state,receipt,verified,applied FROM rocky_preparations WHERE verified=1 AND applied=1 ORDER BY rowid DESC LIMIT ? OFFSET ?",
                        [PAGE, offset],
                    )
                    .await?,
            )
            .await?;
            for row in &page {
                let (package, _, receipt, _, _) = attempt(row)?;
                if package.sequence == base.sequence
                    && receipt.is_some_and(|receipt| receipt.confirmation.new_head == base.head)
                {
                    return Ok(Some(package.rows));
                }
            }
            if (page.len() as i64) < PAGE {
                break;
            }
            offset += PAGE;
        }
        Err(Error::StrongPending(crate::PendingReason::MissingHead))
    }
    /// Rebuilds the complete state of a sealed cut and executes the request
    /// on it: the checkpoint copy's rows, then the retained operations up to
    /// the cut. A cut behind the checkpoint cannot be rebuilt here. The state
    /// is held in memory while this runs; see `docs/runtime/local-replica.md`.
    pub(super) async fn replay_cut(
        &self,
        request: &StrongRequest,
        previous: Option<&[u8]>,
    ) -> Result<(Vec<u8>, Vec<OperationId>), Error> {
        self.handler(&request.method)?
            .validate(request)
            .map_err(application)?;
        if request.scope != self.identity.scope {
            return Err(Error::Rejected("foreign command scope".into()));
        }
        request.encode()?;
        let missing = || Error::StrongPending(crate::PendingReason::MissingInput);
        let base = self.retention.base().ok_or_else(missing)?;
        let cut = |node: NodeId| {
            request
                .frontier
                .0
                .iter()
                .find(|(author, _)| *author == node)
                .map_or(0, |(_, sequence)| sequence.get())
        };
        if base.iter().any(|(node, sequence)| cut(*node) < *sequence) {
            return Err(missing());
        }
        let covered = |id: OperationId| {
            base.iter()
                .any(|(node, sequence)| *node == id.node && id.sequence.get() <= *sequence)
        };
        // The identities of a cut follow from its frontier: every author's
        // operations up to it. A result frame cannot name more than it holds.
        let total: u64 = request.frontier.0.iter().map(|(_, s)| s.get()).sum();
        if total.saturating_mul(24) > crate::MAX_FRAME_BYTES as u64 {
            return Err(Error::Quota {
                resource: "result frame",
                limit: crate::MAX_FRAME_BYTES,
            });
        }
        let mut required = Vec::new();
        let mut pending = std::collections::BTreeMap::new();
        for (node, sequence) in &request.frontier.0 {
            for index in 1..=sequence.get() {
                required.push(OperationId {
                    node: *node,
                    sequence: AuthorSequence::new(index)?,
                });
            }
            let after = base
                .iter()
                .find(|(author, _)| author == node)
                .map_or(0, |(_, sequence)| *sequence);
            let tail = crate::sqlite::params![node.0.to_vec(), after as i64, sequence.get() as i64];
            // Reject unknown cuts before loading any operation.
            let mut count = self.conn().query("SELECT COUNT(*) FROM rocky_operations WHERE node=? AND sequence>? AND sequence<=? AND status<>'rejected'", tail.clone()).await?;
            if count
                .next()
                .await?
                .map(|row| row.get::<i64>(0))
                .transpose()?
                != Some((sequence.get() - after) as i64)
            {
                return Err(missing());
            }
            let mut selected = self.conn().query("SELECT bytes FROM rocky_operations WHERE node=? AND sequence>? AND sequence<=? AND status<>'rejected'", tail).await?;
            while let Some(row) = selected.next().await? {
                let op = Envelope::decode(&row.get::<Vec<u8>>(0)?)?;
                pending.insert(op.id, op);
            }
        }
        required.sort();
        if pending.values().any(|op| {
            op.dependencies
                .iter()
                .any(|id| id.sequence.get() > cut(id.node))
        }) {
            return Err(Error::Rejected("input cut is not dependency closed".into()));
        }
        let mut state = if base.is_empty() {
            A::State::default()
        } else {
            self.base_state().await?
        };
        let mut clocks = std::collections::BTreeMap::new();
        while !pending.is_empty() {
            let mut advanced = false;
            for id in pending.keys().copied().collect::<Vec<_>>() {
                let op = &pending[&id];
                self.app.validate(op).map_err(application)?;
                if op
                    .dependencies
                    .iter()
                    .any(|id| !covered(*id) && !clocks.contains_key(id))
                {
                    continue;
                }
                if let Some(dependency) = op
                    .dependencies
                    .iter()
                    .find(|id| clocks.get(*id).is_some_and(|time| *time >= op.lamport))
                {
                    return Err(ProtocolError::InvalidLamport {
                        operation: op.id,
                        dependency: *dependency,
                    }
                    .into());
                }
                self.app
                    .apply_ready(&mut state, op, AdmissionBoundary::Incoming)
                    .map_err(application)?;
                clocks.insert(id, op.lamport);
                pending.remove(&id);
                advanced = true;
            }
            if !advanced {
                return Err(Error::Rejected(
                    "sealed input has unresolved dependencies".into(),
                ));
            }
        }
        let bytes = self
            .handler(&request.method)?
            .seal(&state)
            .map_err(application)?;
        if state_commitment(&bytes) != request.digest {
            return Err(Error::Rejected("sealed input digest mismatch".into()));
        }
        Ok((
            self.handler(&request.method)?
                .execute_after(&state, request, previous)
                .map_err(application)?,
            required,
        ))
    }
    /// The registered method of this version. Every version inside the
    /// application's support horizon is listed; another one is refused.
    fn method(&self, name: &str, version: u32) -> Result<&'static CommandMethod, Error> {
        let mut registered = self
            .app
            .commands()
            .iter()
            .filter(|method| method.name == name)
            .peekable();
        if registered.peek().is_none() {
            return Err(Error::Incompatible("unregistered command method".into()));
        }
        registered
            .find(|method| method.version == version)
            .ok_or_else(|| {
                Error::Incompatible(format!(
                    "unsupported method version: {name} version {version} is outside the support horizon"
                ))
            })
    }
    async fn store_request(&self, r: &StrongRequest, historical: bool) -> Result<(), Error> {
        if !historical {
            self.method(&r.method, r.version)?;
        }
        self.handler(&r.method)?.validate(r).map_err(application)?;
        if !historical {
            self.handler(&r.method)?
                .admit(r, &self.policy)
                .map_err(application)?;
        }
        if !self.config.authors.contains(&(r.id.node, r.caller)) {
            return Err(Error::Rejected("unconfigured command author".into()));
        }
        if r.scope != self.identity.scope {
            return Err(Error::Rejected("foreign request".into()));
        }
        let b = r.encode()?;
        let mut existing = self
            .conn()
            .query(
                "SELECT bytes FROM rocky_requests WHERE node=? AND sequence=?",
                crate::sqlite::params![r.id.node.0.to_vec(), r.id.sequence.get() as i64],
            )
            .await?;
        if let Some(row) = existing.next().await? {
            if row.get::<Vec<u8>>(0)? != b {
                return Err(Error::Rejected("request identity conflict".into()));
            }
            return Ok(());
        }
        self.strong_budget(b.len()).await?;
        self.conn()
            .execute(
                "INSERT INTO rocky_requests VALUES(?,?,?)",
                crate::sqlite::params![r.id.node.0.to_vec(), r.id.sequence.get() as i64, b],
            )
            .await?;
        self.conn()
            .execute(
                "INSERT OR IGNORE INTO rocky_request_holds VALUES(?,?,?,0)",
                crate::sqlite::params![
                    r.id.node.0.to_vec(),
                    r.id.sequence.get() as i64,
                    super::retention::now_millis()
                ],
            )
            .await?;
        Ok(())
    }
    async fn bind(&self, h: &StrongHead) -> Result<(), Error> {
        if h.scope != self.identity.scope {
            return Err(Error::Rejected("foreign coordinator head".into()));
        }
        // Only the head is needed; the full status reads every attempt.
        let s = self.local_strong_head().await?;
        if let Some(c) = s.configuration {
            if c != h.configuration {
                return Err(CoordinationError::CoordinationStateLost.into());
            }
        } else {
            self.conn()
                .execute(
                    "UPDATE rocky_strong_meta SET configuration=?,head=? WHERE id=1",
                    crate::sqlite::params![
                        h.configuration.to_vec(),
                        genesis(self.identity.scope, h.configuration).to_vec()
                    ],
                )
                .await?;
        }
        Ok(())
    }
    /// Every writing request is one step; `Apply` runs one step per result.
    /// Requests that write nothing use a read transaction.
    pub(super) async fn strong(&mut self, r: Request) -> Result<Response, Error> {
        if matches!(r, Request::Apply) {
            self.apply_results().await?;
            return Ok(Response::Unit);
        }
        if let Request::StoreResult(_, receipt, verdict) = &r {
            let verdict = *verdict;
            // Each part keeps its own savepoint; the group gives them one
            // commit, so a crash leaves all of them or none.
            let stored = self
                .transaction(async |worker| worker.strong_step(&r).await)
                .await??;
            if verdict == crate::strong::Verdict::Deferred {
                return Ok(stored);
            }
            let hash = StrongReceipt::decode(receipt)?.confirmation.result;
            let next = match verdict {
                crate::strong::Verdict::Verified => Request::Verify(hash),
                _ => Request::Reject(hash),
            };
            self.transaction(async |worker| worker.strong_step(&next).await)
                .await??;
            if verdict == crate::strong::Verdict::Verified {
                self.apply_results().await?;
            }
            return Ok(stored);
        }
        if r.writes() {
            self.transaction(async |worker| worker.strong_step(&r).await)
                .await?
        } else {
            self.read_transaction(async |worker| worker.strong_step(&r).await)
                .await?
        }
    }
    /// Statements for one request inside the caller's transaction. The inner
    /// error is reported after commit, so rejection evidence stays durable.
    async fn strong_step(&self, r: &Request) -> Result<Result<Response, Error>, Error> {
        Ok(Ok(match r {
            Request::LookupCommand(handle) => Response::OptionalRequest(
                self.lookup_command(*handle)
                    .await?
                    .map(|(_, request)| request),
            ),
            Request::Publish(handle, invocation) => {
                let crate::CommandInvocation {
                    method,
                    version,
                    arguments,
                    policy,
                    inputs,
                } = invocation;
                let handle = *handle;
                if let Some((old, request)) = self.lookup_command(handle).await? {
                    if old != *method
                        || request.version != *version
                        || request.arguments != *arguments
                        || request.policy != *policy
                        || inputs.as_ref().is_some_and(|inputs| {
                            inputs.frontier != request.frontier || inputs.digest != request.digest
                        })
                    {
                        return Err(Error::Conflict);
                    }
                    return Ok(Ok(Response::Request(request)));
                }
                let m = rows(
                    self.conn(),
                    "SELECT command_sequence FROM rocky_strong_meta WHERE id=1",
                )
                .await?;
                let Some([SqlValue::Integer(n)]) = m.first().map(Vec::as_slice) else {
                    return Err(Error::Corrupt("command counter".into()));
                };
                let method = self.method(method, *version)?;
                let inputs = match inputs.clone() {
                    Some(inputs) => inputs,
                    None => {
                        // The applied cut is the stored projection: seal a
                        // window over all of it.
                        let state = self
                            .app
                            .window(rows(self.conn(), &self.schema.projection_query).await?)
                            .map_err(application)?;
                        crate::SealedInputs {
                            frontier: self.applied_frontier(),
                            digest: state_commitment(
                                &self
                                    .handler(method.name)?
                                    .seal(&state)
                                    .map_err(application)?,
                            ),
                        }
                    }
                };
                let request = StrongRequest {
                    scope: self.identity.scope,
                    caller: self.identity.caller,
                    // The sequence carries its issue time for bounded coordinators.
                    id: RequestId {
                        node: self.identity.node,
                        sequence: next_request_sequence(
                            *n as u64,
                            (super::retention::now_millis() / 1000) as u64,
                        )?,
                    },
                    method: method.name.into(),
                    version: method.version,
                    schema: method.schema,
                    handler: method.handler,
                    client_request: handle.0,
                    arguments: arguments.clone(),
                    policy: *policy,
                    frontier: inputs.frontier,
                    digest: inputs.digest,
                };
                self.store_request(&request, false).await?;
                self.conn()
                    .execute(
                        "INSERT INTO rocky_command_handles VALUES(?,?,?,?)",
                        crate::sqlite::params![
                            handle.0.to_vec(),
                            request.method.clone(),
                            request.id.node.0.to_vec(),
                            request.id.sequence.get() as i64
                        ],
                    )
                    .await?;
                self.conn()
                    .execute(
                        "UPDATE rocky_strong_meta SET command_sequence=? WHERE id=1",
                        [request.id.sequence.get() as i64],
                    )
                    .await?;
                Response::Request(request)
            }
            Request::StoreCommand(b) => {
                self.store_request(&StrongRequest::decode(b)?, false)
                    .await?;
                Response::Unit
            }
            Request::Status => Response::Status(self.status().await?),
            Request::Unresolved(limit) => Response::Requests(
                self.open_requests()
                    .await?
                    .into_iter()
                    .take(*limit)
                    .map(|(bytes, _)| StrongRequest::decode(&bytes))
                    .collect::<Result<_, _>>()?,
            ),
            Request::Release(id) => {
                self.release_request(*id).await?;
                Response::Unit
            }
            Request::Settle(id, settled) => {
                self.settle_request(*id, *settled).await?;
                Response::Unit
            }
            Request::Acknowledgeable => Response::Acknowledgeable {
                node: self.identity.node,
                through: self.acknowledgeable().await?,
            },
            Request::Bind(h) => {
                self.bind(h).await?;
                Response::Unit
            }
            Request::Prepare(request, base) => {
                let request = request.as_ref().clone();
                let base = base.clone();
                self.bind(&base).await?;
                let status = self.local_strong_head().await?;
                if base.executor != self.identity.node {
                    return Err(Error::StrongPending(crate::PendingReason::Executor));
                }
                if status.sequence != base.sequence || status.head != Some(base.head) {
                    return Err(Error::StrongPending(crate::PendingReason::MissingHead));
                }
                self.handler(&request.method)?
                    .admit(&request, &self.policy)
                    .map_err(application)?;
                let previous = self.predecessor(&base).await?;
                let (rows, required) = self.replay_cut(&request, previous.as_deref()).await?;
                let package = StrongPackage {
                    request,
                    sequence: base
                        .sequence
                        .checked_add(1)
                        .ok_or(ProtocolError::InvalidCounter)?,
                    base,
                    rows,
                    required,
                };
                if package.encode()?.len().saturating_add(1024) > crate::MAX_FRAME_BYTES {
                    return Err(Error::Quota {
                        resource: "result frame",
                        limit: crate::MAX_FRAME_BYTES,
                    });
                }
                // The package is submitted right after this commit, so the
                // attempt is recorded as outcome unknown from the start.
                let hash = package.confirmation()?.result;
                let mut existing = self
                    .conn()
                    .query(
                        "SELECT 1 FROM rocky_preparations WHERE hash=?",
                        [hash.to_vec()],
                    )
                    .await?;
                if existing.next().await?.is_none() {
                    self.strong_budget(package.encode()?.len()).await?;
                }
                self.conn().execute("INSERT INTO rocky_preparations(hash,bytes,state,receipt,verified,applied) VALUES(?,?,'unknown',NULL,0,0) ON CONFLICT(hash) DO NOTHING",crate::sqlite::params![hash.to_vec(),package.encode()?]).await?;
                Response::Package(package)
            }
            // Only an unresolved request is executed, and none of its
            // attempts is applied.
            Request::Attempts(id) => Response::Packages(
                self.attempts_where("WHERE applied=0")
                    .await?
                    .into_iter()
                    .filter(|(p, _, _, _, _)| p.request.id == *id)
                    .map(|(p, _, _, _, _)| p)
                    .collect(),
            ),
            Request::StoreResult(b, r, _) => {
                let validation = (|| -> Result<(StrongPackage, StrongReceipt), Error> {
                    let p = StrongPackage::decode(b)?;
                    let receipt = StrongReceipt::decode(r)?;
                    if p.confirmation()? != receipt.confirmation || receipt.sequence != p.sequence {
                        return Err(Error::Rejected(
                            "package/receipt commitment mismatch".into(),
                        ));
                    }
                    self.handler(&p.request.method)?
                        .validate(&p.request)
                        .map_err(application)?;
                    if !self
                        .config
                        .authors
                        .contains(&(p.request.id.node, p.request.caller))
                        || p.request.scope != self.identity.scope
                    {
                        return Err(Error::Rejected("foreign result".into()));
                    }
                    Ok((p, receipt))
                })();
                let (_package, receipt) = match validation {
                    Ok(result) => result,
                    Err(error) => {
                        self.strong_budget(b.len() + r.len()).await?;
                        let hash = StrongReceipt::decode(r)
                            .map(|receipt| receipt.confirmation.result)
                            .unwrap_or_else(|_| commitment("rocky.invalid.receipt", r));
                        self.conn()
                            .execute(
                                "INSERT OR IGNORE INTO rocky_rejected_receipts VALUES(?,?,?,?)",
                                crate::sqlite::params![
                                    hash.to_vec(),
                                    b.clone(),
                                    r.clone(),
                                    error.to_string()
                                ],
                            )
                            .await?;
                        return Ok(Err(error));
                    }
                };
                // Reserve rejection evidence before accepting an unverified receipt.
                let mut existing = self
                    .conn()
                    .query(
                        "SELECT bytes,receipt FROM rocky_preparations WHERE hash=?",
                        [receipt.confirmation.result.to_vec()],
                    )
                    .await?;
                let existing = existing.next().await?;
                if let Some(row) = &existing
                    && row.get::<Vec<u8>>(0)? == *b
                    && row.get_value(1)? == crate::sqlite::Value::Blob(r.clone())
                {
                    return Ok(Ok(Response::Unit));
                }
                self.strong_budget(b.len() + r.len()).await?;
                self.conn().execute("INSERT INTO rocky_preparations(hash,bytes,state,receipt,verified,applied) VALUES(?,?,'unknown',?,0,0) ON CONFLICT(hash) DO UPDATE SET receipt=excluded.receipt WHERE rocky_preparations.verified=0",crate::sqlite::params![receipt.confirmation.result.to_vec(),b.clone(),r.clone()]).await?;
                Response::Unit
            }
            Request::Reject(hash) => {
                self.conn().execute("INSERT OR IGNORE INTO rocky_rejected_receipts SELECT hash,bytes,receipt,'invalid commitment' FROM rocky_preparations WHERE hash=? AND receipt IS NOT NULL AND verified=0", [hash.to_vec()]).await?;
                self.conn()
                    .execute(
                        "UPDATE rocky_preparations SET receipt=NULL WHERE hash=? AND verified=0",
                        [hash.to_vec()],
                    )
                    .await?;
                Response::Unit
            }
            Request::Verify(hash) => {
                // The key of a retained package is its confirmation's result.
                let all = self.attempt_by_hash(hash).await?;
                let (p, _, receipt, _, _) = all
                    .as_ref()
                    .filter(|(p, _, _, _, _)| p.confirmation().is_ok_and(|c| c.result == *hash))
                    .ok_or_else(|| Error::Corrupt("missing verified package".into()))?;
                let receipt = receipt
                    .as_ref()
                    .ok_or_else(|| Error::Corrupt("missing receipt".into()))?;
                if p.confirmation()? != receipt.confirmation {
                    return Err(Error::Rejected("receipt binding".into()));
                }
                self.store_request(&p.request, true).await?;
                self.bind(&p.base).await?;
                self.conn()
                    .execute(
                        "UPDATE rocky_preparations SET verified=1 WHERE hash=?",
                        [hash.to_vec()],
                    )
                    .await?;
                Response::Unit
            }
            // Dispatched by `strong`: each applied result is its own transaction.
            Request::Apply => Response::Unit,
            Request::RequestsPage(after) => {
                let mut requests: Vec<_> = self
                    .status()
                    .await?
                    .commands
                    .into_iter()
                    .map(|progress| progress.request)
                    .filter(|request| after.is_none_or(|cursor| request.id > cursor))
                    .collect();
                requests.sort_by_key(|request| request.id);
                let mut items = Vec::new();
                let mut size = 0;
                let mut next = None;
                for request in requests {
                    let bytes = request.encode()?.len();
                    if items.len() == crate::replication::TRANSFER_PAGE_SIZE
                        || size + bytes > crate::MAX_FRAME_BYTES
                    {
                        next = items.last().map(|request: &StrongRequest| request.id);
                        break;
                    }
                    size += bytes;
                    items.push(request);
                }
                Response::RequestsPage(crate::Page { items, next })
            }
            Request::ResultPage(after) => {
                let mut results: Vec<_> = self
                    .attempts()
                    .await?
                    .into_iter()
                    .filter_map(|(package, _, receipt, _, _)| {
                        receipt.map(|receipt| ConfirmedPublication { package, receipt })
                    })
                    .filter(|result| {
                        after.is_none_or(|cursor| result.receipt.confirmation.result > cursor)
                    })
                    .collect();
                results.sort_by_key(|result| result.receipt.confirmation.result);
                let mut items = Vec::new();
                let mut size = 0;
                let mut next = None;
                for result in results {
                    let bytes = result.package.encode()?.len() + result.receipt.encode().len();
                    if items.len() == crate::replication::TRANSFER_PAGE_SIZE
                        || size + bytes > crate::MAX_FRAME_BYTES
                    {
                        next = items.last().map(|result: &ConfirmedPublication| {
                            result.receipt.confirmation.result
                        });
                        break;
                    }
                    size += bytes;
                    items.push(result);
                }
                Response::ResultPage(crate::Page { items, next })
            }
            Request::Publications => {
                let context = AdmissionContext {
                    scope: self.identity.scope,
                    caller: self.identity.caller,
                    policy_version: self.app.policy_version(&self.policy),
                    boundary: AdmissionBoundary::Local,
                };
                self.app
                    .admit_read(context.scope, context.caller, &self.policy)
                    .map_err(application)?;
                let mut selected = Vec::new();
                for result in self.publications().await? {
                    if self
                        .handler(&result.package.request.method)?
                        .select_result(&result.package.rows, context, &self.policy)
                        .map_err(application)?
                    {
                        selected.push(result);
                    }
                }
                Response::Results(selected)
            }
            Request::Results => Response::Results(self.results().await?),
            Request::Ledger => {
                let head = self.local_strong_head().await?;
                let requests =
                    rows(self.conn(), "SELECT EXISTS(SELECT 1 FROM rocky_requests)").await?;
                let bound =
                    head.configuration.is_some() || requests != vec![vec![SqlValue::Integer(0)]];
                // Received and not verified: all that reconciliation asks
                // the coordinator about.
                let unverified = self
                    .attempts_where("WHERE receipt IS NOT NULL AND verified=0")
                    .await?
                    .into_iter()
                    .filter_map(|(package, _, receipt, _, _)| {
                        receipt.map(|receipt| ConfirmedPublication { package, receipt })
                    })
                    .collect();
                Response::Ledger { bound, unverified }
            }
            Request::Acknowledge(peer, hash, applied) => {
                let (peer, hash, applied) = (*peer, *hash, *applied);
                self.validate_peer(peer)?;
                if !self
                    .publications()
                    .await?
                    .iter()
                    .any(|p| p.receipt.confirmation.result == hash)
                {
                    return Err(Error::Rejected(
                        "acknowledgment for unconfirmed result".into(),
                    ));
                }
                self.conn().execute("INSERT INTO rocky_strong_ack VALUES(?,?,?) ON CONFLICT(peer,hash) DO UPDATE SET applied=MAX(applied,excluded.applied)",crate::sqlite::params![peer.0.to_vec(),hash.to_vec(),i64::from(applied)]).await?;
                let mut delivered = true;
                for peer in &self.config.delivery_targets {
                    let mut ack = self
                        .conn()
                        .query(
                            "SELECT applied FROM rocky_strong_ack WHERE peer=? AND hash=?",
                            crate::sqlite::params![peer.0.to_vec(), hash.to_vec()],
                        )
                        .await?;
                    if ack
                        .next()
                        .await?
                        .is_none_or(|row| row.get::<i64>(0).ok() != Some(1))
                    {
                        delivered = false;
                    }
                }
                if delivered {
                    self.conn()
                        .execute(
                            "DELETE FROM rocky_strong_outbox WHERE hash=?",
                            [hash.to_vec()],
                        )
                        .await?;
                }
                Response::Unit
            }
        }))
    }
    async fn apply_results(&mut self) -> Result<(), Error> {
        loop {
            let revision = self.revision.next()?;
            // Selection, replay and the write share one snapshot per attempt.
            let applied = self
                .transaction(async |worker| {
                    let status = worker.local_strong_head().await?;
                    let attempts = worker
                        .attempts_where("WHERE verified=1 AND applied=0")
                        .await?;
                    let next = attempts.into_iter().find(|(p, _, _, verified, applied)| {
                        *verified
                            && !applied
                            && p.base.sequence == status.sequence
                            && Some(p.base.head) == status.head
                    });
                    let Some((p, _, receipt, _, _)) = next else {
                        return Ok(None);
                    };
                    let previous = worker.predecessor(&p.base).await?;
                    let (expected, required) =
                        match worker.replay_cut(&p.request, previous.as_deref()).await {
                            Err(Error::StrongPending(_)) => return Ok(None),
                            result => result?,
                        };
                    if p.rows != expected || p.required != required {
                        return Err(Error::Rejected(
                            "result differs from exact input replay".into(),
                        ));
                    }
                    let receipt =
                        receipt.ok_or_else(|| Error::Corrupt("verified without receipt".into()))?;
                    Ok(Some(worker.write_result(&p, &receipt, revision).await?))
                })
                .await?;
            let Some(changed) = applied else {
                break;
            };
            self.revision = revision;
            self.unpublished
                .changed(changed, crate::observe::Cause::Confirmed);
            self.notify(true);
        }
        Ok(())
    }
    /// Statements for one confirmed result inside the caller's transaction.
    async fn write_result(
        &mut self,
        p: &StrongPackage,
        receipt: &StrongReceipt,
        revision: LocalRevision,
    ) -> Result<Changed, Error> {
        let hash = receipt.confirmation.result;
        let statements = self
            .handler(&p.request.method)?
            .project_result(&p.rows)
            .map_err(application)?;
        let changed = self.store(statements).await?;
        self.conn()
            .execute(
                "UPDATE rocky_preparations SET applied=1,state='confirmed' WHERE hash=?",
                [hash.to_vec()],
            )
            .await?;
        self.conn()
            .execute(
                "UPDATE rocky_strong_meta SET sequence=?,head=? WHERE id=1",
                crate::sqlite::params![p.sequence as i64, receipt.confirmation.new_head.to_vec()],
            )
            .await?;
        if !self.config.delivery_targets.is_empty() {
            self.conn()
                .execute("INSERT INTO rocky_strong_outbox VALUES(?)", [hash.to_vec()])
                .await?;
        }
        self.conn()
            .execute(
                "UPDATE rocky_replica SET revision=? WHERE id=1",
                [revision.get() as i64],
            )
            .await?;
        self.inject_fault().await?;
        Ok(changed)
    }
}
