use super::*;
use crate::replication::{
    Acknowledgment, PeerProgress, Request, Response, SyncState, TRANSFER_PAGE_SIZE, TransferPage,
};
use rocky_core::ProtocolError;
use std::collections::BTreeSet;

/// Whether a retained operation's dependencies allow applying it now.
pub(super) enum Readiness {
    Ready,
    Pending,
    /// A dependency is rejected or not older than the operation.
    Invalid(Error),
}
/// What one pass over pending operations changed, adopted after its commit.
struct Pass {
    applied: Vec<OperationId>,
    revision: LocalRevision,
    rejection: Option<String>,
    changed: Changed,
}
pub(super) fn operation_id(node: &[u8], sequence: i64) -> Result<OperationId, Error> {
    Ok(OperationId {
        node: NodeId(
            node.try_into()
                .map_err(|_| Error::Corrupt("operation author".into()))?,
        ),
        sequence: AuthorSequence::new(sequence as u64)?,
    })
}
fn key(id: OperationId) -> Vec<crate::sqlite::Value> {
    crate::sqlite::params![id.node.0.to_vec(), id.sequence.get() as i64]
}
impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    /// Rereads the domain's counters and checkpoint state from storage.
    /// Starts from nothing, so a retried attempt cannot double-count an
    /// earlier partial pass. With `open`, the operations after the
    /// checkpoint and the strong ledger are also validated; history behind
    /// the checkpoint is verified by the explicit audit.
    pub(super) async fn restore(&mut self, open: bool) -> Result<(), Error> {
        self.load_retention().await?;
        let budget = rows(
            self.conn(),
            "SELECT COUNT(*),COALESCE(SUM(LENGTH(bytes)),0) FROM rocky_operations",
        )
        .await?;
        let Some([SqlValue::Integer(count), SqlValue::Integer(bytes)]) =
            budget.first().map(Vec::as_slice)
        else {
            return Err(Error::Corrupt("history budget".into()));
        };
        let (count, bytes) = (*count as usize, *bytes as usize);
        let metadata = rows(
            self.conn(),
            "SELECT sequence,lamport,revision FROM rocky_replica WHERE id=1",
        )
        .await?;
        let Some(
            [
                SqlValue::Integer(sequence),
                SqlValue::Integer(clock),
                SqlValue::Integer(revision),
            ],
        ) = metadata.first().map(Vec::as_slice)
        else {
            return Err(Error::Corrupt("invalid counters".into()));
        };
        if *sequence < 0 || *clock < 0 || *revision < 0 {
            return Err(Error::Corrupt("invalid counters".into()));
        }
        let mut frontier = BTreeMap::new();
        let authors: BTreeSet<_> = self.config.authors.iter().map(|(node, _)| *node).collect();
        let mut newest = 0;
        for node in authors {
            // Applied operations of an author have no gaps; later ones are
            // pending or rejected, so this walks back over that tail only.
            let mut applied = self
                .conn()
                .query(
                    "SELECT sequence FROM rocky_operations WHERE node=? AND status='applied' ORDER BY sequence DESC LIMIT 1",
                    [node.0.to_vec()],
                )
                .await?;
            let retained = match applied.next().await? {
                Some(row) => row.get::<i64>(0)? as u64,
                None => 0,
            };
            // Everything behind the horizon was applied before it was pruned.
            let applied = retained.max(self.retention.horizon(node));
            if applied > 0 {
                frontier.insert(node, AuthorSequence::new(applied)?);
            }
            if node == self.identity.node {
                let mut last = self
                    .conn()
                    .query(
                        "SELECT sequence FROM rocky_operations WHERE node=? ORDER BY sequence DESC LIMIT 1",
                        [node.0.to_vec()],
                    )
                    .await?;
                newest = match last.next().await? {
                    Some(row) => (row.get::<i64>(0)? as u64).max(applied),
                    None => applied,
                };
            }
            if open {
                self.validate_tail(node, applied).await?;
            }
        }
        // Local operations after the applied frontier wait to be applied
        // again on an imported checkpoint; none is ever missing.
        if newest != *sequence as u64 {
            return Err(Error::Corrupt(
                "counters differ from canonical history".into(),
            ));
        }
        if open {
            self.restore_strong(false).await?;
        }
        self.retained = count;
        self.retained_bytes = bytes.saturating_add(count.saturating_mul(RECORD_OVERHEAD));
        self.frontier = frontier;
        self.sequence = *sequence as u64;
        self.lamport = *clock as u64;
        self.revision = LocalRevision::new(*revision as u64)?;
        Ok(())
    }
    /// Checks one author's operations after the checkpoint: canonical bytes
    /// under the right key, an applied prefix without gaps up to `applied`,
    /// and nothing applied after it.
    async fn validate_tail(&self, node: NodeId, applied: u64) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("foreign or inconsistent history".into());
        let base = self
            .retention
            .base()
            .map_or(0, |base| {
                base.iter()
                    .find(|(author, _)| *author == node)
                    .map_or(0, |(_, sequence)| *sequence)
            })
            .max(self.retention.horizon(node));
        let mut after = base;
        loop {
            let mut page = self
                .conn()
                .query(
                    "SELECT sequence,bytes,status,origin,lamport FROM rocky_operations WHERE node=? AND sequence>? ORDER BY sequence LIMIT 256",
                    crate::sqlite::params![node.0.to_vec(), after as i64],
                )
                .await?;
            let mut empty = true;
            while let Some(row) = page.next().await? {
                empty = false;
                let sequence = row.get::<i64>(0)? as u64;
                let op = Envelope::decode(&row.get::<Vec<u8>>(1)?)?;
                let (status, origin): (String, String) = (row.get(2)?, row.get(3)?);
                self.app.validate(&op).map_err(application)?;
                if op.scope != self.identity.scope
                    || !self.config.authors.contains(&(op.id.node, op.caller))
                    || op.id.node != node
                    || op.id.sequence.get() != sequence
                    || row.get::<i64>(4)? as u64 != op.lamport.get()
                    || !["applied", "pending", "rejected"].contains(&status.as_str())
                    || (origin == "local") != (node == self.identity.node)
                    || !["local", "remote"].contains(&origin.as_str())
                    || (status == "applied") != (sequence <= applied)
                    || (sequence <= applied && sequence != after + 1)
                {
                    return Err(corrupt());
                }
                after = sequence;
            }
            if empty {
                break;
            }
        }
        if after < applied {
            return Err(corrupt());
        }
        Ok(())
    }
    /// Encoded bytes and status of a retained operation.
    pub(super) async fn record(&self, id: OperationId) -> Result<Option<(Vec<u8>, String)>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT bytes,status FROM rocky_operations WHERE node=? AND sequence=?",
                key(id),
            )
            .await?;
        match records.next().await? {
            Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
            None => Ok(None),
        }
    }
    async fn record_status(&self, id: OperationId) -> Result<Option<(u64, String)>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT lamport,status FROM rocky_operations WHERE node=? AND sequence=?",
                key(id),
            )
            .await?;
        match records.next().await? {
            Some(row) => Ok(Some((row.get::<i64>(0)? as u64, row.get(1)?))),
            None => Ok(None),
        }
    }
    /// Checks the dependencies of `op` against retained operations.
    pub(super) async fn readiness(&self, op: &Envelope) -> Result<Readiness, Error> {
        let mut pending = false;
        for dependency in &op.dependencies {
            // Behind the horizon everything was applied; its clock is gone.
            if self.retention.below_horizon(*dependency) {
                continue;
            }
            match self.record_status(*dependency).await? {
                None => pending = true,
                Some((_, status)) if status == "rejected" => {
                    return Ok(Readiness::Invalid(Error::Rejected(
                        "dependency rejected".into(),
                    )));
                }
                Some((lamport, _)) if lamport >= op.lamport.get() => {
                    return Ok(Readiness::Invalid(Error::Protocol(
                        ProtocolError::InvalidLamport {
                            operation: op.id,
                            dependency: *dependency,
                        },
                    )));
                }
                Some((_, status)) => pending |= status != "applied",
            }
        }
        Ok(if pending {
            Readiness::Pending
        } else {
            Readiness::Ready
        })
    }
    /// Identities of retained operations after `after` in key order, read
    /// from the primary key index alone.
    async fn identities(
        &self,
        after: Option<OperationId>,
        limit: usize,
    ) -> Result<Vec<OperationId>, Error> {
        let mut records = match after {
            Some(after) => {
                let mut values = key(after);
                values.push((limit as i64).into());
                self.conn().query("SELECT node,sequence FROM rocky_operations WHERE (node,sequence)>(?,?) ORDER BY node,sequence LIMIT ?", values).await?
            }
            None => {
                self.conn()
                    .query(
                        "SELECT node,sequence FROM rocky_operations ORDER BY node,sequence LIMIT ?",
                        [limit as i64],
                    )
                    .await?
            }
        };
        let mut identities = Vec::new();
        while let Some(row) = records.next().await? {
            identities.push(operation_id(&row.get::<Vec<u8>>(0)?, row.get(1)?)?);
        }
        Ok(identities)
    }
    /// The contiguous held prefix of every author's log, and the first
    /// retained operations above it that are not rejected.
    async fn held(&self) -> Result<(rocky_core::AppliedFrontier, Vec<OperationId>), Error> {
        let applied = super::retention::marks(&self.applied_frontier());
        let mut nodes: Vec<_> = self.config.authors.iter().map(|(node, _)| *node).collect();
        nodes.dedup();
        let mut held = Vec::new();
        let mut exceptions = Vec::new();
        for node in nodes {
            let mut prefix = super::retention::at(&applied, node).max(self.retention.horizon(node));
            // Received, pending and rejected operations extend the prefix
            // while they are contiguous.
            loop {
                let mut records = self
                    .conn()
                    .query(
                        "SELECT sequence FROM rocky_operations WHERE node=? AND sequence>? ORDER BY sequence LIMIT 256",
                        crate::sqlite::params![node.0.to_vec(), prefix as i64],
                    )
                    .await?;
                let start = prefix;
                let mut read = 0;
                while let Some(row) = records.next().await? {
                    read += 1;
                    if row.get::<i64>(0)? as u64 != prefix + 1 {
                        break;
                    }
                    prefix += 1;
                }
                if read < 256 || prefix - start < read {
                    break;
                }
            }
            if prefix > 0 {
                held.push((node, AuthorSequence::new(prefix)?));
            }
            if exceptions.len() < crate::replication::MAX_SYNC_EXCEPTIONS {
                let mut records = self
                    .conn()
                    .query(
                        "SELECT sequence FROM rocky_operations WHERE node=? AND sequence>? AND status<>'rejected' ORDER BY sequence LIMIT ?",
                        crate::sqlite::params![
                            node.0.to_vec(),
                            prefix as i64,
                            (crate::replication::MAX_SYNC_EXCEPTIONS - exceptions.len()) as i64
                        ],
                    )
                    .await?;
                while let Some(row) = records.next().await? {
                    exceptions.push(OperationId {
                        node,
                        sequence: AuthorSequence::new(row.get::<i64>(0)? as u64)?,
                    });
                }
            }
        }
        Ok((rocky_core::AppliedFrontier(held), exceptions))
    }
    async fn rejected(&self) -> Result<BTreeSet<OperationId>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT node,sequence FROM rocky_operations WHERE status<>'applied' AND status='rejected'",
                (),
            )
            .await?;
        let mut rejected = BTreeSet::new();
        while let Some(row) = records.next().await? {
            rejected.insert(operation_id(&row.get::<Vec<u8>>(0)?, row.get(1)?)?);
        }
        Ok(rejected)
    }
    pub(super) async fn sync(&mut self, request: Request) -> Result<Response, Error> {
        match request {
            Request::Operation(id) => {
                self.app
                    .admit_read(self.identity.scope, self.identity.caller, &self.policy)
                    .map_err(application)?;
                let record = self
                    .read_transaction(async |worker| worker.record(id).await)
                    .await?;
                Ok(Response::Operation(
                    record
                        .filter(|(_, status)| {
                            status != "rejected" && !self.retention.below_horizon(id)
                        })
                        .map(|(bytes, _)| bytes),
                ))
            }
            Request::Receive(bytes) => self.receive(bytes).await.map(Response::Ack),
            Request::ReceivePage(records) => {
                self.receive_page(records).await.map(Response::Receipts)
            }
            Request::Process => self.process_pending().await.map(Response::More),
            Request::Page(after) => {
                self.app
                    .admit_read(self.identity.scope, self.identity.caller, &self.policy)
                    .map_err(application)?;
                self.read_transaction(async |worker| worker.operations_page(after).await)
                    .await
                    .map(Response::Operations)
            }
            Request::State => {
                let applied = self.applied_frontier();
                let (held, received) = self
                    .read_transaction(async |worker| worker.held().await)
                    .await?;
                let horizon = self.retention_horizon()?;
                Ok(Response::State(SyncState {
                    held,
                    received,
                    applied,
                    horizon,
                }))
            }
            Request::Missing(known) => {
                self.app
                    .admit_read(self.identity.scope, self.identity.caller, &self.policy)
                    .map_err(application)?;
                let known: BTreeSet<_> = known.into_iter().collect();
                let records = self
                    .read_transaction(async |worker| {
                        let rejected = worker.rejected().await?;
                        let mut records = Vec::new();
                        let mut cursor = None;
                        loop {
                            let page = worker.identities(cursor, 256).await?;
                            let Some(last) = page.last() else {
                                return Ok(records);
                            };
                            cursor = Some(*last);
                            for id in page {
                                if known.contains(&id)
                                    || rejected.contains(&id)
                                    || worker.retention.below_horizon(id)
                                {
                                    continue;
                                }
                                if let Some((bytes, _)) = worker.record(id).await? {
                                    records.push(bytes);
                                }
                                if records.len() == TRANSFER_PAGE_SIZE {
                                    return Ok(records);
                                }
                            }
                        }
                    })
                    .await?;
                Ok(Response::Page(TransferPage {
                    records,
                    next: None,
                }))
            }
            Request::Acknowledge(peer, ack) => {
                self.acknowledge(peer, ack).await.map(|_| Response::Unit)
            }
            Request::PeerAcknowledgments(peer) => self
                .read_transaction(async |worker| worker.peer_acknowledgments(peer).await)
                .await
                .map(Response::Acks),
            Request::Progress(peer) => self
                .read_transaction(async |worker| worker.progress(peer).await)
                .await
                .map(Response::Progress),
        }
    }
    async fn operations_page(
        &self,
        after: Option<OperationId>,
    ) -> Result<crate::Page<Vec<u8>, OperationId>, Error> {
        let limit = (TRANSFER_PAGE_SIZE + 1) as i64;
        let mut records = match after {
            Some(after) => {
                let mut values = key(after);
                values.push(limit.into());
                self.conn().query("SELECT node,sequence,bytes FROM rocky_operations WHERE status<>'rejected' AND (node,sequence)>(?,?) ORDER BY node,sequence LIMIT ?", values).await?
            }
            None => self.conn().query("SELECT node,sequence,bytes FROM rocky_operations WHERE status<>'rejected' ORDER BY node,sequence LIMIT ?", [limit]).await?,
        };
        let mut items = Vec::new();
        let mut bytes = 0;
        let mut last = None;
        let mut more = false;
        while let Some(row) = records.next().await? {
            let encoded: Vec<u8> = row.get(2)?;
            if items.len() == TRANSFER_PAGE_SIZE || bytes + encoded.len() > crate::MAX_FRAME_BYTES {
                more = true;
                break;
            }
            bytes += encoded.len();
            let id = operation_id(&row.get::<Vec<u8>>(0)?, row.get(1)?)?;
            last = Some(id);
            if !self.retention.below_horizon(id) {
                items.push(encoded);
            }
        }
        Ok(crate::Page {
            items,
            next: if more { last } else { None },
        })
    }
    async fn receive(&mut self, bytes: Vec<u8>) -> Result<Acknowledgment, Error> {
        if bytes.len() > rocky_core::MAX_PAYLOAD_BYTES + 256 {
            return Err(ProtocolError::InvalidLength.into());
        }
        let op = Envelope::decode(&bytes)?;
        self.app.validate(&op).map_err(application)?;
        if op.scope != self.identity.scope {
            return Err(ProtocolError::ForeignScope.into());
        }
        // Applied and pruned: a late copy is a duplicate, whatever it would
        // do to rows that changed or were deleted since.
        if self.retention.below_horizon(op.id) {
            return Ok(Acknowledgment {
                id: op.id,
                applied: true,
            });
        }
        if let Some((original, status)) = self.record(op.id).await? {
            if original != bytes {
                return Err(ProtocolError::IdentityConflict(op.id).into());
            }
            if status == "rejected" {
                return Err(Error::Rejected("previously rejected operation".into()));
            }
            return Ok(Acknowledgment {
                id: op.id,
                applied: status == "applied",
            });
        }
        self.operation_budget(bytes.len())?;
        self.app.admit(&op, &self.policy).map_err(application)?;
        if !self.config.authors.contains(&(op.id.node, op.caller)) {
            return Err(Error::Rejected("unconfigured author or caller".into()));
        }
        if op.id.node == self.identity.node {
            return Err(Error::Rejected(
                "unknown operation attributed to this node".into(),
            ));
        }
        // Validate time against all available dependency bytes, including pending ones.
        if let Readiness::Invalid(error) = self.readiness(&op).await? {
            return Err(error);
        }
        let clock = self.lamport.max(op.lamport.get());
        self.transaction(async |worker| {
            worker
                .conn()
                .execute(
                    "INSERT INTO rocky_operations VALUES(?,?,?,?,'pending','remote')",
                    crate::sqlite::params![
                        op.id.node.0.to_vec(),
                        op.id.sequence.get() as i64,
                        bytes.clone(),
                        op.lamport.get() as i64
                    ],
                )
                .await?;
            worker
                .conn()
                .execute(
                    "UPDATE rocky_replica SET lamport=? WHERE id=1",
                    [clock as i64],
                )
                .await?;
            worker.inject_fault().await
        })
        .await?;
        self.lamport = clock;
        self.retained += 1;
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(bytes.len())
            .saturating_add(RECORD_OVERHEAD);
        Ok(Acknowledgment {
            id: op.id,
            applied: false,
        })
    }
    /// One writer step for a bounded batch. Every record is received in its
    /// own savepoint, so a refused one rolls back alone; one application pass
    /// follows in another. The whole step shares the group's commit.
    async fn receive_page(
        &mut self,
        records: Vec<Vec<u8>>,
    ) -> Result<Vec<Result<Acknowledgment, Error>>, Error> {
        let mut results = Vec::with_capacity(records.len());
        for bytes in records {
            match self.receive(bytes).await {
                // The connection state is unknown: the group must not commit.
                Err(error @ Error::Rollback { .. }) => return Err(error),
                result => results.push(result),
            }
        }
        if !results
            .iter()
            .any(|result| matches!(result, Ok(ack) if !ack.applied))
        {
            return Ok(results);
        }
        // A failed pass rolls back alone; the receipts stay and apply later.
        if let Err(error @ Error::Rollback { .. }) = self.process_pending().await {
            return Err(error);
        }
        for result in &mut results {
            let Ok(ack) = result else { continue };
            if ack.applied {
                continue;
            }
            match self.record_status(ack.id).await?.map(|(_, status)| status) {
                Some(status) if status == "rejected" => {
                    *result = Err(Error::Rejected("operation rejected on application".into()));
                }
                Some(status) => ack.applied = status == "applied",
                None => {}
            }
        }
        Ok(results)
    }
    pub(super) async fn process_pending(&mut self) -> Result<bool, Error> {
        let pass = self
            .transaction(async |worker| worker.apply_pending().await)
            .await?;
        let Some(pass) = pass else {
            return self.process_following().await;
        };
        for id in &pass.applied {
            self.frontier.insert(id.node, id.sequence);
        }
        self.revision = pass.revision;
        self.unpublished
            .changed(pass.changed, crate::observe::Cause::Sync);
        if !pass.applied.is_empty() {
            self.notify(true);
        }
        match pass.rejection {
            Some(error) => Err(Error::Rejected(error)),
            None => {
                self.process_following().await?;
                Ok(true)
            }
        }
    }
    /// One bounded pass inside the caller's transaction. Each ready operation
    /// is reduced against stored rows and written before the next one is
    /// read, so a successor in the same pass sees its predecessor.
    async fn apply_pending(&mut self) -> Result<Option<Pass>, Error> {
        let mut pass = Pass {
            applied: Vec::new(),
            revision: self.revision,
            rejection: None,
            changed: Changed::default(),
        };
        let mut changes = 0;
        let mut cursor: Option<OperationId> = None;
        // Re-scan in subsequent passes for predecessors sorted after successors.
        'pages: loop {
            let mut page = match cursor {
                Some(after) => self.conn().query("SELECT bytes FROM rocky_operations WHERE status<>'applied' AND status='pending' AND (node,sequence)>(?,?) ORDER BY node,sequence LIMIT 64", key(after)).await?,
                None => self.conn().query("SELECT bytes FROM rocky_operations WHERE status<>'applied' AND status='pending' ORDER BY node,sequence LIMIT 64", ()).await?,
            };
            let mut empty = true;
            while let Some(row) = page.next().await? {
                empty = false;
                let op = Envelope::decode(&row.get::<Vec<u8>>(0)?)?;
                cursor = Some(op.id);
                let outcome = match self.readiness(&op).await? {
                    Readiness::Pending => continue,
                    Readiness::Invalid(error) => Err(error.to_string()),
                    Readiness::Ready => self
                        .transition(&op, AdmissionBoundary::Incoming)
                        .await?
                        .map_err(|error| error.to_string()),
                };
                let status = match outcome {
                    Ok(statements) => {
                        pass.changed.merge(self.store(statements).await?);
                        pass.applied.push(op.id);
                        "applied"
                    }
                    Err(error) => {
                        // A refused local operation is reported, not dropped.
                        if op.id.node == self.identity.node {
                            self.conn()
                                .execute(
                                    "INSERT OR REPLACE INTO rocky_rejected_local VALUES(?,?,?)",
                                    crate::sqlite::params![
                                        op.id.node.0.to_vec(),
                                        op.id.sequence.get() as i64,
                                        error.clone()
                                    ],
                                )
                                .await?;
                        }
                        pass.rejection = Some(error);
                        "rejected"
                    }
                };
                self.conn()
                    .execute(
                        "UPDATE rocky_operations SET status=? WHERE node=? AND sequence=?",
                        crate::sqlite::params![
                            status,
                            op.id.node.0.to_vec(),
                            op.id.sequence.get() as i64
                        ],
                    )
                    .await?;
                changes += 1;
                if changes == TRANSFER_PAGE_SIZE {
                    break 'pages;
                }
            }
            if empty {
                break;
            }
        }
        if changes == 0 {
            return Ok(None);
        }
        if !pass.applied.is_empty() {
            pass.revision = self.revision.next()?;
        }
        self.conn()
            .execute(
                "UPDATE rocky_replica SET revision=? WHERE id=1",
                [pass.revision.get() as i64],
            )
            .await?;
        self.inject_fault().await?;
        Ok(Some(pass))
    }
    pub(super) fn validate_peer(&self, peer: NodeId) -> Result<(), Error> {
        if peer == self.identity.node || !self.config.delivery_targets.contains(&peer) {
            return Err(Error::Rejected("unknown named peer".into()));
        }
        Ok(())
    }
    async fn acknowledge(&mut self, peer: NodeId, ack: Acknowledgment) -> Result<(), Error> {
        self.validate_peer(peer)?;
        // Behind the horizon the acknowledgment is no longer needed.
        if self.retention.below_horizon(ack.id) {
            return Ok(());
        }
        if self
            .record_status(ack.id)
            .await?
            .is_none_or(|(_, status)| status == "rejected")
        {
            return Err(Error::Rejected(
                "acknowledgment for unknown operation".into(),
            ));
        }
        self.transaction(async |worker| {
            worker.conn().execute("INSERT INTO rocky_peer_ack VALUES(?,?,?,?) ON CONFLICT(peer,node,sequence) DO UPDATE SET applied=MAX(applied,excluded.applied)", crate::sqlite::params![peer.0.to_vec(), ack.id.node.0.to_vec(), ack.id.sequence.get() as i64, i64::from(ack.applied)]).await?;
            let mut delivered = true;
            for target in &worker.config.delivery_targets {
                // A held frontier from catch-up counts as receipt too.
                if worker.retention.held_by(*target, ack.id) { continue; }
                let mut retained = worker.conn().query("SELECT 1 FROM rocky_peer_ack WHERE peer=? AND node=? AND sequence=?", crate::sqlite::params![target.0.to_vec(), ack.id.node.0.to_vec(), ack.id.sequence.get() as i64]).await?;
                if retained.next().await?.is_none() { delivered = false; }
            }
            if delivered { worker.conn().execute("DELETE FROM rocky_outbox WHERE node=? AND sequence=?", crate::sqlite::params![ack.id.node.0.to_vec(), ack.id.sequence.get() as i64]).await?; }
            Ok(())
        }).await?;
        self.retention.acknowledged();
        Ok(())
    }
    pub(super) async fn peer_acknowledgments(
        &self,
        peer: NodeId,
    ) -> Result<Vec<Acknowledgment>, Error> {
        self.validate_peer(peer)?;
        let mut rows = self.conn().query("SELECT node,sequence,applied FROM rocky_peer_ack WHERE peer=? ORDER BY node,sequence", [peer.0.to_vec()]).await?;
        let mut acks = Vec::new();
        while let Some(row) = rows.next().await? {
            let bytes: Vec<u8> = row.get(0)?;
            let node = NodeId(
                bytes
                    .try_into()
                    .map_err(|_| Error::Corrupt("peer node length".into()))?,
            );
            acks.push(Acknowledgment {
                id: OperationId {
                    node,
                    sequence: AuthorSequence::new(row.get::<i64>(1)? as u64)?,
                },
                applied: row.get::<i64>(2)? == 1,
            });
        }
        Ok(acks)
    }
    async fn progress(&self, peer: NodeId) -> Result<PeerProgress, Error> {
        let acks = self.peer_acknowledgments(peer).await?;
        let received: BTreeSet<_> = acks.iter().map(|ack| ack.id).collect();
        let applied: BTreeSet<_> = acks
            .iter()
            .filter(|ack| ack.applied)
            .map(|ack| ack.id)
            .collect();
        // Local operations are exactly 1..=sequence. Those behind the
        // horizon were acknowledged before they were pruned, unless the peer
        // is declared behind.
        let pruned = self.retention.acknowledged_behind(peer, self.identity.node);
        let (held, reported) = self.retention.reported(peer);
        let local = |ids: &BTreeSet<OperationId>, mark: u64| {
            let mark = mark.min(self.sequence);
            (pruned.max(mark) as usize)
                + ids
                    .iter()
                    .filter(|id| {
                        id.node == self.identity.node && id.sequence.get() > pruned.max(mark)
                    })
                    .count()
        };
        let mut frontier = BTreeMap::<NodeId, u64>::new();
        for (node, _) in &self.config.authors {
            let pruned = self
                .retention
                .acknowledged_behind(peer, *node)
                .max(super::retention::at(&reported, *node));
            if pruned > 0 {
                frontier.insert(*node, pruned);
            }
        }
        for id in &applied {
            let previous = frontier.entry(id.node).or_default();
            if id.sequence.get() == *previous + 1 {
                *previous += 1;
            }
        }
        let (held, reported) = (
            super::retention::at(&held, self.identity.node),
            super::retention::at(&reported, self.identity.node),
        );
        Ok(PeerProgress {
            pending: (self.sequence as usize).saturating_sub(local(&received, held)),
            received: local(&received, held),
            applied: local(&applied, reported),
            frontier: rocky_core::AppliedFrontier(
                frontier
                    .into_iter()
                    .filter(|(_, n)| *n > 0)
                    .map(|(node, n)| Ok((node, AuthorSequence::new(n)?)))
                    .collect::<Result<_, ProtocolError>>()?,
            ),
        })
    }
    /// Test hook at the last statement of a write step.
    pub(super) async fn inject_fault(&mut self) -> Result<(), Error> {
        #[cfg(test)]
        if let Some(fault) = self.fault.take() {
            match fault {
                Fault::AbortAfterWrites(n) if n > 1 => {
                    self.fault = Some(Fault::AbortAfterWrites(n - 1));
                }
                Fault::AbortAfterWrites(_) => std::process::abort(),
                // Applied by the file writer, never stored on a worker.
                Fault::AbortBeforeCommit
                | Fault::AbortAfterCommit
                | Fault::FailCommit
                | Fault::AbortInCheckpoint
                | Fault::RecordGroups(_)
                | Fault::SnapshotAtFlush(_)
                | Fault::CheckpointFrames(_) => {}
                Fault::FailBeforeCommit => return Err(Error::InjectedFailure),
                Fault::RetryBeforeCommit(n) if n > 0 => {
                    self.fault = Some(Fault::RetryBeforeCommit(n - 1));
                    return Err(Error::from(crate::sqlite::Error::Busy("injected".into())));
                }
                Fault::RetryBeforeCommit(_) => {}
                Fault::Pause(entered, resume) => {
                    let _ = entered.send(());
                    let _ = resume.await;
                }
            }
        }
        Ok(())
    }
}
