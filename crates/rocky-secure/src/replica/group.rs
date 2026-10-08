//! Group commit for authoring and incoming frames. Callers enqueue requests;
//! whoever holds the replica lock drains the queue as leader and runs one
//! `BEGIN IMMEDIATE` transaction with one savepoint per request. Results,
//! the revision watch and everything readers see follow the commit; results
//! that may leave the device also follow one shared flush.
use super::*;
use std::collections::VecDeque;
use tokio::sync::oneshot::{self, error::TryRecvError};

/// Requests per group transaction; also the applications per pending pass.
pub(super) const GROUP_REQUESTS: usize = 64;
/// Request bytes per group; a single larger request still forms its own group.
pub(super) const GROUP_BYTES: usize = 4 * 1024 * 1024;
/// WAL size that triggers a passive checkpoint between groups.
pub(super) const WAL_CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;
/// WAL size that triggers a truncating checkpoint between groups.
pub(super) const WAL_LIMIT_BYTES: u64 = 16 * 1024 * 1024;

pub(super) enum Work {
    Author {
        handle: Commitment,
        input: Commitment,
        changes: Vec<RowChange>,
        dependencies: Vec<Hash>,
    },
    Bundle {
        handle: Commitment,
        input: Commitment,
        writes: Vec<RowWrite>,
        dependencies: Vec<Hash>,
        /// The session authority the bundle was prepared under, if any.
        authority: Option<u64>,
    },
    Receive(EncryptedFrame),
}
pub(super) enum Done {
    Author(EncryptedFrame),
    Bundle(Vec<EncryptedFrame>),
    /// The handle's operation committed earlier and has been pruned.
    Pruned(Hash),
    Receive(ReceiveStatus),
}
/// A request's state inside the group, before its final status is known.
enum Staged {
    Author(EncryptedFrame),
    Bundle(Vec<EncryptedFrame>),
    Handled(Hash),
    Covered,
    Duplicate,
    Stored(Hash),
}
type Reply = Result<(Done, u64), Error>;
pub(super) struct Request {
    work: Work,
    weight: usize,
    /// The result may leave the device, so it waits for the flush.
    exposed: bool,
    reply: oneshot::Sender<Reply>,
}
pub(super) type Queue = std::sync::Mutex<VecDeque<Request>>;

#[derive(Default)]
struct Outcome {
    results: Vec<Result<Staged, Error>>,
    /// A failed pending pass; its savepoint is rolled back and receivers report it.
    pass: Option<Error>,
    /// The pass stopped at its bound; more passes follow the commit.
    more: bool,
    revision: u64,
}

/// Fault points inside a group, for crash and visibility tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum GroupFault {
    ExitAfterRequests(usize),
    ExitBeforeCommit,
    ExitAfterCommit,
    FailBeforeCommit,
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Enqueues `work` as contiguous requests and returns their results in order.
    pub(super) async fn submit(&self, work: Vec<(Work, usize)>, exposed: bool) -> Vec<Reply> {
        let mut pending = Vec::with_capacity(work.len());
        {
            let mut queue = self.queue();
            for (work, weight) in work {
                let (reply, done) = oneshot::channel();
                queue.push_back(Request {
                    work,
                    weight,
                    exposed,
                    reply,
                });
                pending.push(done);
            }
        }
        let mut results: Vec<Option<Reply>> = pending.iter().map(|_| None).collect();
        let (Some(last), Some(slot)) = (pending.last_mut(), results.last_mut()) else {
            return Vec::new();
        };
        // Groups take queue prefixes, so the last reply arrives after the others.
        let guard = tokio::select! {
            biased;
            result = last => {
                *slot = Some(result.unwrap_or(Err(Error::RetryExhausted)));
                None
            }
            guard = self.inner.lock() => Some(guard),
        };
        let mut guard = guard;
        let mut first = true;
        while !collect(&mut pending, &mut results) {
            // The lock is released between groups and during a group's
            // flush, so readers and other leaders run in between.
            let mut inner = match guard.take() {
                Some(guard) => guard,
                None => self.inner.lock().await,
            };
            if std::mem::take(&mut first) {
                // One scheduler pass lets concurrent callers join; no timer.
                tokio::task::yield_now().await;
            }
            let group = self.drain();
            if group.is_empty() {
                break;
            }
            let (exposed, scopes) = inner.commit_group(group).await;
            if let Some(exposed) = exposed {
                let flusher = inner.flusher.clone();
                drop(inner);
                // No await before the replies are out: a cancelled leader
                // cannot hold back flushed results.
                exposed.answer(&flusher);
                inner = self.inner.lock().await;
            }
            // Replies are out; one bounded step keeps retained history in check.
            inner.maintain(&scopes).await;
            inner.checkpoint_if_due().await;
        }
        // Another leader took these requests and answers them after its
        // flush, outside the lock. A reply is lost only when its leader was
        // cancelled while waiting out a busy file; the outcome is then
        // unknown and retrying by handle is safe.
        let mut answered = Vec::with_capacity(pending.len());
        for (receiver, result) in pending.into_iter().zip(results) {
            answered.push(match result {
                Some(result) => result,
                None => receiver.await.unwrap_or(Err(Error::RetryExhausted)),
            });
        }
        answered
    }
    fn queue(&self) -> std::sync::MutexGuard<'_, VecDeque<Request>> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Takes the oldest live requests within the group bounds.
    fn drain(&self) -> Vec<Request> {
        let mut queue = self.queue();
        let mut group = Vec::new();
        let mut bytes = 0usize;
        while let Some(request) = queue.front() {
            if request.reply.is_closed() {
                queue.pop_front();
                continue;
            }
            if !group.is_empty()
                && (group.len() == GROUP_REQUESTS
                    || bytes.saturating_add(request.weight) > GROUP_BYTES)
            {
                break;
            }
            bytes = bytes.saturating_add(request.weight);
            if let Some(request) = queue.pop_front() {
                group.push(request);
            }
        }
        group
    }
}

/// Moves delivered replies into `results`; true when every request has one.
fn collect(pending: &mut [oneshot::Receiver<Reply>], results: &mut [Option<Reply>]) -> bool {
    for (receiver, slot) in pending.iter_mut().zip(results.iter_mut()) {
        if slot.is_none() {
            match receiver.try_recv() {
                Ok(result) => *slot = Some(result),
                Err(TryRecvError::Closed) => *slot = Some(Err(Error::RetryExhausted)),
                Err(TryRecvError::Empty) => {}
            }
        }
    }
    results.iter().all(Option::is_some)
}

/// Results that may leave the device: answered after a flush covers `commit`.
struct Exposed {
    replies: Vec<(oneshot::Sender<Reply>, Done)>,
    commit: u64,
}
impl Exposed {
    /// One flush covers every exposed result in the group.
    fn answer(self, flusher: &Flusher) {
        let flushed = flusher.flush(self.commit);
        for (reply, done) in self.replies {
            let _ = reply.send(match &flushed {
                Ok(()) => Ok((done, self.commit)),
                Err(error) => Err(shared(error)),
            });
        }
    }
}

impl<A: RowApplication> Inner<A> {
    /// Commits one group and answers its requests after the commit, except
    /// the exposed ones, which it returns for the flush. Also returns the
    /// scopes the group wrote, for maintenance.
    async fn commit_group(
        &mut self,
        group: Vec<Request>,
    ) -> (Option<Exposed>, BTreeSet<ResourceScope>) {
        let mut works = Vec::with_capacity(group.len());
        let mut replies = Vec::with_capacity(group.len());
        let mut scopes = BTreeSet::new();
        for request in group {
            match &request.work {
                Work::Author { changes, .. } => scopes.extend(changes.first().map(|c| c.scope)),
                Work::Bundle { writes, .. } => {
                    scopes.extend(writes.iter().map(|write| write.change.scope));
                }
                Work::Receive(frame) => scopes.extend(frame.scope().ok()),
            }
            works.push(request.work);
            replies.push((request.reply, request.exposed));
        }
        let outcome = match now() {
            Ok(time) => transaction!(@commit self, |inner| inner.run_group(&works, time).await)
                .map(|outcome| (outcome, time)),
            Err(error) => Err(error),
        };
        let (mut outcome, time) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                for (reply, _) in replies {
                    let _ = reply.send(Err(shared(&error)));
                }
                return (None, BTreeSet::new());
            }
        };
        #[cfg(test)]
        {
            if matches!(self.group_fault, Some(GroupFault::ExitAfterCommit)) {
                std::process::exit(77);
            }
            self.group_commits += 1;
        }
        if outcome.results.iter().any(|result| {
            matches!(
                result,
                Ok(Staged::Author(_) | Staged::Bundle(_) | Staged::Stored(_))
            )
        }) {
            self.frames.send_replace(());
        }
        self.revision = outcome.revision;
        let revision = outcome.revision;
        self.notifications.send_if_modified(|current| {
            let changed = *current != revision;
            *current = revision;
            changed
        });
        if outcome.more
            && outcome.pass.is_none()
            && let Err(error) = self.process_pending(time).await
        {
            outcome.pass = Some(error);
        }
        let mut exposed = Vec::new();
        for ((reply, flush), result) in replies.into_iter().zip(outcome.results) {
            let done = match result {
                Ok(Staged::Author(frame)) => Ok(Done::Author(frame)),
                Ok(Staged::Bundle(frames)) => Ok(Done::Bundle(frames)),
                Ok(Staged::Handled(hash)) => Ok(Done::Pruned(hash)),
                Ok(Staged::Covered) => Ok(Done::Receive(ReceiveStatus::Pruned)),
                Ok(Staged::Duplicate) => Ok(Done::Receive(ReceiveStatus::Duplicate)),
                Ok(Staged::Stored(hash)) => match &outcome.pass {
                    Some(error) => Err(shared(error)),
                    None => self.status(hash).await.map(Done::Receive),
                },
                Err(error) => Err(error),
            };
            match done {
                Ok(done) if flush => exposed.push((reply, done)),
                // A caller that went away still has its request committed.
                done => {
                    let _ = reply.send(done.map(|done| (done, self.commit)));
                }
            }
        }
        let exposed = (!exposed.is_empty()).then_some(Exposed {
            replies: exposed,
            commit: self.commit,
        });
        (exposed, scopes)
    }
    async fn run_group(&mut self, works: &[Work], time: u64) -> Result<Outcome, Error> {
        let mut outcome = Outcome::default();
        let mut unapplied = false;
        for work in works {
            self.cause(match work {
                Work::Receive(_) => crate::observe::Cause::Sync,
                Work::Author { .. } | Work::Bundle { .. } => crate::observe::Cause::Local,
            });
            // Authors see incoming frames of earlier requests applied, as in sequence.
            if unapplied && matches!(work, Work::Author { .. } | Work::Bundle { .. }) {
                self.group_pass(time, &mut outcome).await?;
                unapplied = false;
            }
            self.conn.execute("SAVEPOINT request", ()).await?;
            let result = match work {
                Work::Author {
                    handle,
                    input,
                    changes,
                    dependencies,
                } => match self.lookup_handle(*handle, *input).await {
                    Ok(Some(Ok(frame))) => Ok(Staged::Author(frame)),
                    Ok(Some(Err(hash))) => Ok(Staged::Handled(hash)),
                    Ok(None) => self
                        .author_transaction(*handle, *input, changes, dependencies, time)
                        .await
                        .map(Staged::Author),
                    Err(error) => Err(error),
                },
                Work::Bundle {
                    handle,
                    input,
                    writes,
                    dependencies,
                    authority,
                } => match self.lookup_bundle(*handle, *input).await {
                    Ok(Some(Ok(frames))) => Ok(Staged::Bundle(frames)),
                    Ok(Some(Err(hash))) => Ok(Staged::Handled(hash)),
                    // The commit's linearization point: authority is checked
                    // under the replica lock, inside the write transaction.
                    Ok(None) => match self.check_authority(*authority) {
                        Ok(()) => self
                            .author_bundle(*handle, *input, writes, dependencies, time)
                            .await
                            .map(Staged::Bundle),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                },
                Work::Receive(frame) => match frame.scope() {
                    Ok(scope) => match self.ensure_horizon_current(scope).await {
                        Ok(()) => self.receive_step(frame, time).await,
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                },
            };
            match result {
                Err(error) if retryable(&error) => return Err(error),
                Err(error) => {
                    self.conn.execute("ROLLBACK TO request", ()).await?;
                    self.conn.execute("RELEASE request", ()).await?;
                    outcome.results.push(Err(error));
                }
                Ok(staged) => {
                    // A covered late frame may be what a pending frame waited for.
                    unapplied |= matches!(staged, Staged::Stored(_) | Staged::Covered);
                    self.conn.execute("RELEASE request", ()).await?;
                    outcome.results.push(Ok(staged));
                }
            }
            #[cfg(test)]
            if let Some(GroupFault::ExitAfterRequests(count)) = self.group_fault
                && outcome.results.len() == count
            {
                std::process::exit(77);
            }
        }
        if unapplied {
            self.group_pass(time, &mut outcome).await?;
        }
        #[cfg(test)]
        match self.group_fault {
            Some(GroupFault::ExitBeforeCommit) => std::process::exit(77),
            Some(GroupFault::FailBeforeCommit) => return Err(Error::InjectedFailure),
            _ => {}
        }
        outcome.revision = self.stored_revision().await?;
        Ok(outcome)
    }
    async fn group_pass(&mut self, time: u64, outcome: &mut Outcome) -> Result<(), Error> {
        if outcome.pass.is_some() {
            return Ok(());
        }
        match self.apply_pending(time, GROUP_REQUESTS).await {
            Ok(more) => outcome.more |= more,
            Err(error) if retryable(&error) => return Err(error),
            Err(error) => outcome.pass = Some(error),
        }
        Ok(())
    }
    /// Admits one incoming frame into custody; application follows in the pass.
    async fn receive_step(&mut self, frame: &EncryptedFrame, time: u64) -> Result<Staged, Error> {
        let bytes = frame.to_bytes()?.len();
        let operation = frame.operation()?;
        let mut store = TransactionStore(self.conn.clone());
        let held = store.has_operation(operation.hash).await?;
        // A held frame of a generation now behind the horizon was verified
        // as it arrived; any other is verified again.
        if held && self.admits(operation.header.extensions.manifest).is_err() {
            return Ok(Staged::Duplicate);
        }
        self.verify_frame(frame, time)?;
        let local = self.local_grants(time)?;
        if !local.iter().any(|grant| {
            grant.scope == operation.header.extensions.scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&Right::Custody) || grant.rights.contains(&Right::Read))
        }) {
            return Err(denied("incoming custody denied"));
        }
        if held {
            return Ok(Staged::Duplicate);
        }
        // A late frame from before the horizon is part of the checkpoint: it
        // is not stored and cannot be applied a second time.
        let scope = operation.header.extensions.scope;
        if let Some(id) = self.find_scope_id(scope).await?
            && let Some(log) = self.log(id, operation.header.public_key).await?
            && operation.header.seq_num as i64 <= log.horizon
        {
            // A pending frame may name it as a dependency.
            if self.has_pending(scope).await? {
                self.put_stub(operation.hash, id, operation.header.timestamp, false)
                    .await?;
            }
            return Ok(Staged::Covered);
        }
        self.admit_scope(&operation.header).await?;
        self.check_budget(bytes).await?;
        store
            .insert_operation(
                operation.hash,
                &operation.header,
                operation.body.as_ref(),
                &frame.header,
                &operation.header.extensions.scope,
            )
            .await?;
        self.conn
            .execute(
                "INSERT INTO secure_outbox VALUES(?)",
                [operation.hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(Staged::Stored(operation.hash))
    }
    pub(super) async fn stored_revision(&self) -> Result<u64, Error> {
        let revision = rows(
            &self.conn,
            "SELECT revision FROM secure_metadata WHERE id=1",
        )
        .await?;
        match revision.first().map(Vec::as_slice) {
            Some([SqlValue::Integer(revision)]) if *revision >= 0 => Ok(*revision as u64),
            _ => Err(denied("invalid secure revision")),
        }
    }
    /// Keeps the WAL bounded: passive past the checkpoint size whenever the
    /// file has grown since the last one, truncating past the hard limit.
    /// Best effort: the commit already succeeded, so a failure waits for the
    /// next transaction.
    pub(crate) async fn checkpoint_if_due(&mut self) {
        let size = std::fs::metadata(&self.wal).map_or(0, |metadata| metadata.len());
        let truncate = size >= self.wal_limits.1;
        if !truncate && (size < self.wal_limits.0 || size <= self.wal_mark) {
            return;
        }
        let sql = if truncate {
            "PRAGMA wal_checkpoint(TRUNCATE)"
        } else {
            "PRAGMA wal_checkpoint(PASSIVE)"
        };
        if let Ok(result) = rows(&self.conn, sql).await {
            let complete = matches!(
                result.first().map(Vec::as_slice),
                Some([SqlValue::Integer(0), SqlValue::Integer(log), SqlValue::Integer(done)])
                    if log == done
            );
            self.wal_mark = if truncate && complete { 0 } else { size };
            #[cfg(test)]
            {
                self.checkpoints += 1;
            }
        }
    }
}

/// The group's failure as reported to each of its callers.
fn shared(error: &Error) -> Error {
    match error {
        Error::RetryExhausted => Error::RetryExhausted,
        Error::Closed => Error::Closed,
        Error::Conflict => Error::Conflict,
        Error::Backpressure => Error::Backpressure,
        Error::Quota { resource, limit } => Error::Quota {
            resource,
            limit: *limit,
        },
        Error::Rejected(message) => Error::Rejected(message.clone()),
        Error::Incompatible(message) => Error::Incompatible(message.clone()),
        Error::Durability(message) => Error::Durability(message.clone()),
        Error::Corrupt(_) | Error::Rollback { .. } => Error::Corrupt(error.to_string()),
        Error::Io(source) => Error::Io(std::io::Error::new(source.kind(), source.to_string())),
        other => Error::Io(std::io::Error::other(other.to_string())),
    }
}
