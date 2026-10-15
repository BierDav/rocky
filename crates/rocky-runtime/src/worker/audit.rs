use super::*;

/// Records read per query, so the audit's memory does not grow with history.
const PAGE: i64 = 256;

impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    /// The explicit audit of everything but stored rows: retained history,
    /// counters, acknowledgments, handles, outgoing work and the strong
    /// ledger must agree. It reads retained records in pages. Rows are
    /// compared with their digests off the writer thread, and the projection
    /// is recomputed by the checkpoint that follows; see `Replica::audit`.
    /// Returns the number of retained operations.
    pub(super) async fn audit(&self) -> Result<usize, Error> {
        let corrupt = |what: &str| Error::Corrupt(what.into());
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
            return Err(corrupt("invalid counters"));
        };
        let (sequence, clock) = (*sequence as u64, *clock as u64);
        let local = self.identity.node;
        let mut retained = 0usize;
        let mut maximum_clock = 0;
        let mut applied_count = 0u64;
        let mut pruned = false;
        for node in self
            .config
            .authors
            .iter()
            .map(|(node, _)| *node)
            .collect::<std::collections::BTreeSet<_>>()
        {
            let horizon = self.retention.horizon(node);
            pruned |= horizon > 0;
            let applied = self.frontier.get(&node).map_or(0, |applied| applied.get());
            applied_count += applied;
            let mut previous: Option<u64> = None;
            let mut after = 0;
            loop {
                let mut page = self
                    .conn()
                    .query(
                        "SELECT sequence,bytes,status,origin,lamport FROM rocky_operations WHERE node=? AND sequence>? ORDER BY sequence LIMIT ?",
                        crate::sqlite::params![node.0.to_vec(), after as i64, PAGE],
                    )
                    .await?;
                let mut empty = true;
                while let Some(row) = page.next().await? {
                    empty = false;
                    retained += 1;
                    after = row.get::<i64>(0)? as u64;
                    let op = Envelope::decode(&row.get::<Vec<u8>>(1)?)?;
                    let (status, origin): (String, String) = (row.get(2)?, row.get(3)?);
                    self.app.validate(&op).map_err(application)?;
                    if op.scope != self.identity.scope
                        || !self.config.authors.contains(&(op.id.node, op.caller))
                        || op.id.node != node
                        || op.id.sequence.get() != after
                        || row.get::<i64>(4)? as u64 != op.lamport.get()
                        || !["applied", "pending", "rejected"].contains(&status.as_str())
                        || !["local", "remote"].contains(&origin.as_str())
                        || (origin == "local") != (node == local)
                        || (status == "applied") != (after <= applied)
                        // Applied history has no gap; pruning removes a prefix.
                        || match previous {
                            Some(previous) => after <= applied && after != previous + 1,
                            None => after <= applied && after > horizon + 1,
                        }
                    {
                        return Err(corrupt("foreign or inconsistent history"));
                    }
                    previous = Some(after);
                    maximum_clock = maximum_clock.max(op.lamport.get());
                    if status != "applied" {
                        continue;
                    }
                    for dependency in &op.dependencies {
                        if self.retention.below_horizon(*dependency) {
                            continue;
                        }
                        let mut records = self
                            .conn()
                            .query(
                                "SELECT lamport,status FROM rocky_operations WHERE node=? AND sequence=?",
                                crate::sqlite::params![
                                    dependency.node.0.to_vec(),
                                    dependency.sequence.get() as i64
                                ],
                            )
                            .await?;
                        let ready = match records.next().await? {
                            Some(row) => {
                                row.get::<String>(1)? == "applied"
                                    && (row.get::<i64>(0)? as u64) < op.lamport.get()
                            }
                            None => false,
                        };
                        if !ready {
                            return Err(corrupt("applied history has missing dependencies"));
                        }
                    }
                }
                if empty {
                    break;
                }
            }
            if previous.unwrap_or(horizon) < applied
                || (node == local && previous.unwrap_or(horizon).max(applied) != sequence)
            {
                return Err(corrupt("counters differ from canonical history"));
            }
        }
        // An imported checkpoint and pruned history carry clocks and
        // revisions this file no longer shows.
        let exact = !pruned
            && self
                .retention
                .status_record()
                .is_none_or(|record| record.node == local);
        if clock < maximum_clock
            || (exact && clock != maximum_clock)
            || *revision < 0
            || (exact && *revision as u64 > applied_count)
            || (applied_count > 0 && *revision == 0)
        {
            return Err(corrupt("counters differ from canonical history"));
        }
        self.audit_acknowledgments().await?;
        self.audit_handles(sequence).await?;
        self.audit_outbox().await?;
        Ok(retained)
    }
    async fn status_of(&self, id: OperationId) -> Result<Option<String>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT status FROM rocky_operations WHERE node=? AND sequence=?",
                crate::sqlite::params![id.node.0.to_vec(), id.sequence.get() as i64],
            )
            .await?;
        Ok(match records.next().await? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }
    async fn audit_acknowledgments(&self) -> Result<(), Error> {
        let corrupt = |what: &str| Error::Corrupt(what.into());
        let mut cursor = (Vec::new(), Vec::new(), 0i64);
        loop {
            let mut page = self
                .conn()
                .query(
                    "SELECT peer,node,sequence,applied FROM rocky_peer_ack WHERE (peer,node,sequence)>(?,?,?) ORDER BY peer,node,sequence LIMIT ?",
                    crate::sqlite::params![cursor.0.clone(), cursor.1.clone(), cursor.2, PAGE],
                )
                .await?;
            let mut empty = true;
            while let Some(row) = page.next().await? {
                empty = false;
                let (peer, node): (Vec<u8>, Vec<u8>) = (row.get(0)?, row.get(1)?);
                let sequence: i64 = row.get(2)?;
                let named = NodeId(
                    peer.as_slice()
                        .try_into()
                        .map_err(|_| corrupt("peer acknowledgment identity"))?,
                );
                self.validate_peer(named)
                    .map_err(|_| corrupt("unconfigured acknowledgment peer"))?;
                let id =
                    incoming::operation_id(&node, sequence).map_err(|_| corrupt("ack author"))?;
                if ![0, 1].contains(&row.get::<i64>(3)?)
                    || (!self.retention.below_horizon(id)
                        && self
                            .status_of(id)
                            .await?
                            .is_none_or(|status| status == "rejected"))
                {
                    return Err(corrupt("unknown acknowledged operation"));
                }
                cursor = (peer, node, sequence);
            }
            if empty {
                return Ok(());
            }
        }
    }
    /// Every local operation still inside the retry window has one handle
    /// bound to its input; a handle without an operation is behind the horizon.
    async fn audit_handles(&self, sequence: u64) -> Result<(), Error> {
        let corrupt = || Error::Corrupt("mutation handle binding".into());
        let local = self.identity.node;
        let window = self.retention.handle_window() as u64;
        let kept_after = self
            .retention
            .horizon(local)
            .min(sequence.saturating_sub(window));
        let mut kept = 0;
        let mut after = 0i64;
        loop {
            let mut page = self
                .conn()
                .query(
                    "SELECT handle,request_hash,node,sequence FROM rocky_mutation_handles WHERE node=? AND sequence>? ORDER BY sequence LIMIT ?",
                    crate::sqlite::params![local.0.to_vec(), after, PAGE],
                )
                .await?;
            let mut empty = true;
            while let Some(row) = page.next().await? {
                empty = false;
                let (handle, hash): (Vec<u8>, Vec<u8>) = (row.get(0)?, row.get(1)?);
                let id = incoming::operation_id(&row.get::<Vec<u8>>(2)?, row.get(3)?)
                    .map_err(|_| corrupt())?;
                let sequence = id.sequence.get();
                // Sequences are unique: the page order would repeat one.
                if handle.len() != 32 || id.node != local || sequence as i64 == after {
                    return Err(corrupt());
                }
                after = sequence as i64;
                kept += u64::from(sequence > kept_after);
                match self.record(id).await? {
                    Some((bytes, _)) => {
                        let op = Envelope::decode(&bytes)?;
                        let expected = crate::mutation_hash(
                            &op.payload,
                            (op.entity != id).then_some(op.entity),
                        );
                        if hash != expected {
                            return Err(corrupt());
                        }
                    }
                    None if self.retention.below_horizon(id) => {}
                    None => return Err(Error::Corrupt("handle operation".into())),
                }
            }
            if empty {
                break;
            }
        }
        let mut foreign = self
            .conn()
            .query(
                "SELECT 1 FROM rocky_mutation_handles WHERE node<>? LIMIT 1",
                [local.0.to_vec()],
            )
            .await?;
        if kept != sequence - kept_after || foreign.next().await?.is_some() {
            return Err(Error::Corrupt("local mutation handles".into()));
        }
        Ok(())
    }
    /// The outbox holds exactly the local operations after the horizon that
    /// some delivery target has not acknowledged.
    async fn audit_outbox(&self) -> Result<(), Error> {
        let local = self.identity.node;
        let horizon = self.retention.horizon(local);
        let mut after = horizon as i64;
        loop {
            let mut page = self
                .conn()
                .query(
                    "SELECT sequence FROM rocky_operations WHERE node=? AND sequence>? ORDER BY sequence LIMIT ?",
                    crate::sqlite::params![local.0.to_vec(), after, PAGE],
                )
                .await?;
            let first = after;
            let mut expected = Vec::new();
            let mut empty = true;
            while let Some(row) = page.next().await? {
                empty = false;
                after = row.get(0)?;
                let mut delivered = true;
                for peer in &self.config.delivery_targets {
                    let id = OperationId {
                        node: local,
                        sequence: AuthorSequence::new(after as u64)?,
                    };
                    if self.retention.held_by(*peer, id) {
                        continue;
                    }
                    let mut ack = self
                        .conn()
                        .query(
                            "SELECT 1 FROM rocky_peer_ack WHERE peer=? AND node=? AND sequence=?",
                            crate::sqlite::params![peer.0.to_vec(), local.0.to_vec(), after],
                        )
                        .await?;
                    delivered &= ack.next().await?.is_some();
                }
                if !delivered {
                    expected.push(vec![SqlValue::Integer(after)]);
                }
            }
            // The last page also covers anything after the newest operation.
            let stored = storage::collect_rows(
                self.conn()
                    .query(
                        "SELECT sequence FROM rocky_outbox WHERE node=? AND sequence>? AND sequence<=? ORDER BY sequence",
                        crate::sqlite::params![
                            local.0.to_vec(),
                            first,
                            if empty { i64::MAX } else { after }
                        ],
                    )
                    .await?,
            )
            .await?;
            if stored != expected {
                return Err(Error::Corrupt(
                    "outgoing work differs from peer receipts".into(),
                ));
            }
            if empty {
                break;
            }
        }
        let mut others = self
            .conn()
            .query(
                "SELECT 1 FROM rocky_outbox WHERE node<>? LIMIT 1",
                [local.0.to_vec()],
            )
            .await?;
        if others.next().await?.is_some() {
            return Err(Error::Corrupt(
                "outgoing work differs from peer receipts".into(),
            ));
        }
        Ok(())
    }
}
