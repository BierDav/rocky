use super::*;
use std::collections::VecDeque;
use tokio::sync::oneshot::{self, error::TryRecvError};

pub(super) const GROUP_REQUESTS: usize = 64;
pub(super) const GROUP_BYTES: usize = 4 * 1024 * 1024;
const STEP_REQUESTS: usize = 2;
pub(crate) const RECEIVE_REQUESTS: usize = 4;
const STEP_BYTES: usize = 32 * 1024;
pub(super) const SETTLE_REQUESTS: usize = 1;
pub(super) const APPLICATION_ROWS: u64 = 1;
pub(super) const WAL_CHECKPOINT_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const WAL_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const TRUNCATE_WAIT: std::time::Duration = std::time::Duration::from_millis(50);
const QUIET_TRUNCATE_WAIT: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const RECLAIM_PAGES: i64 = 1024;
pub(crate) const RECLAIM_STEP: i64 = 4096;
#[cfg(target_os = "android")]
pub(super) const CACHE_KIB: i64 = -8 * 1024;
#[cfg(not(target_os = "android"))]
pub(super) const CACHE_KIB: i64 = -32 * 1024;

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
        authority: Option<u64>,
    },
    Receive(EncryptedFrame),
}
pub(super) enum Done {
    Author(EncryptedFrame),
    Bundle(Vec<EncryptedFrame>),
    Pruned(Hash),
    Receive(ReceiveStatus),
}
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
    exposed: bool,
    reply: oneshot::Sender<Reply>,
}
pub(super) type Queue = std::sync::Mutex<VecDeque<Request>>;

#[derive(Default)]
struct Outcome {
    results: Vec<Result<Staged, Error>>,
    more: bool,
    revision: u64,
    pass: Option<Error>,
}

#[cfg(test)]
#[cfg_attr(
    target_family = "wasm",
    expect(
        dead_code,
        reason = "Process-exiting faults are exercised only by native crash tests."
    )
)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum GroupFault {
    ExitAfterRequests(usize),
    ExitBeforeCommit,
    ExitAfterCommit,
    FailBeforeCommit,
    FailAfterCommits(usize),
}

impl<A: RowApplication> EncryptedReplica<A> {
    #[cfg(test)]
    pub(crate) async fn stage_bundle_before_apply_for_test(
        &self,
        frames: &[EncryptedFrame],
    ) -> Result<(), Error> {
        let time = now()?;
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| {
            for frame in frames {
                assert!(matches!(
                    inner.receive_step(frame, time).await?,
                    Staged::Stored(_)
                ));
            }
            Ok(())
        })
    }
    pub(super) async fn submit(&self, work: Vec<(Work, usize)>, exposed: bool) -> Vec<Reply> {
        #[cfg(feature = "qualification")]
        let receive_limit = match crate::qualification_bound(
            "ROCKY_RECEIVE_REQUESTS",
            RECEIVE_REQUESTS,
            GROUP_REQUESTS,
        ) {
            Ok(limit) => limit,
            Err(error) => return work.iter().map(|_| Err(shared(&error))).collect(),
        };
        #[cfg(not(feature = "qualification"))]
        let receive_limit = RECEIVE_REQUESTS;
        let class = class_of(work.iter().map(|(work, _)| work));
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
        let guard = tokio::select! {
            biased;
            result = last => {
                *slot = Some(result.unwrap_or(Err(Error::RetryExhausted)));
                None
            }
            guard = self.inner.lock(class) => Some(guard),
        };
        let mut guard = guard;
        let mut first = true;
        while !collect(&mut pending, &mut results) {
            let mut inner = match guard.take() {
                Some(guard) => guard,
                None => self.inner.lock(class).await,
            };
            if std::mem::take(&mut first) {
                drop(inner);
                rocky_platform::task::yield_now().await;
                inner = self.inner.lock(class).await;
            }
            let mut cohort: VecDeque<_> = self.drain().into();
            if cohort.is_empty() {
                break;
            }
            let mut exposed = None;
            let mut scopes = BTreeSet::new();
            let mut pending_replies = VecDeque::new();
            let mut settle_failure = None;
            let flusher = inner.flusher.clone();
            while !cohort.is_empty() {
                let group = step(&mut cohort, &inner, receive_limit);
                #[cfg(any(test, feature = "qualification"))]
                let row_bound = group.len() as u64 * APPLICATION_ROWS;
                #[cfg(any(test, feature = "qualification"))]
                {
                    inner.units("encoded_byte", group.iter().map(|r| r.weight as u64).sum());
                    if group.iter().all(|r| matches!(r.work, Work::Receive(_))) {
                        inner.units("received_frame", group.len() as u64);
                    }
                }
                #[cfg(any(test, feature = "qualification"))]
                let received_work = group
                    .iter()
                    .all(|request| matches!(request.work, Work::Receive(_)))
                    .then(|| {
                        (
                            inner.frames_written.load(Ordering::Relaxed),
                            inner.rows_written.load(Ordering::Relaxed),
                            inner.applied_frames,
                        )
                    });
                let (answers, written, settling) = inner.commit_group(group).await;
                #[cfg(any(test, feature = "qualification"))]
                if let Some(before) = received_work {
                    let rows = inner.rows_written.load(Ordering::Relaxed) - before.1;
                    let frames = inner.frames_written.load(Ordering::Relaxed) - before.0;
                    let requests = inner.applied_frames - before.2;
                    inner.row_work(row_bound);
                    inner.units("applied_row", rows);
                    inner.units("applied_frame", frames);
                    inner.units("applied_request", requests);
                    if rows > row_bound {
                        inner.units("oversized_request", 1);
                    }
                }
                Exposed::append(&mut exposed, answers);
                scopes.extend(written);
                drop(inner);
                pending_replies.extend(settling);
                if !pending_replies.is_empty()
                    && cohort
                        .front()
                        .is_some_and(|r| !matches!(r.work, Work::Receive(_)))
                    && let Err(error) = self.settle_received().await
                {
                    settle_failure.get_or_insert(error);
                }
                rocky_platform::task::yield_now().await;
                let class = class.min(class_of(cohort.iter().map(|request| &request.work)));
                inner = self.inner.lock(class).await;
            }
            let mut last_commit = inner.locked_commit();
            drop(inner);
            if !pending_replies.is_empty() {
                if let Err(error) = self.settle_received().await {
                    settle_failure.get_or_insert(error);
                }
                while !pending_replies.is_empty() {
                    rocky_platform::task::yield_now().await;
                    let count = pending_replies.len().min(STEP_REQUESTS);
                    let replies = pending_replies.drain(..count).collect();
                    let inner = self.inner.lock(Class::Background).await;
                    last_commit = inner.locked_commit();
                    let settled = settle_failure
                        .as_ref()
                        .map_or(Ok(()), |error| Err(shared(error)));
                    Exposed::append(&mut exposed, inner.answer_settled(replies, settled).await);
                }
            }
            if let Some(mut exposed) = exposed {
                exposed.commit = last_commit;
                exposed.answer(flusher).await;
            }
            self.maintain_scopes(scopes).await;
            let mut inner = self.inner.lock(Class::Background).await;
            inner.checkpoint_if_due().await;
        }
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

fn class_of<'a>(mut work: impl Iterator<Item = &'a Work>) -> Class {
    if work.any(|work| matches!(work, Work::Author { .. } | Work::Bundle { .. })) {
        Class::Interactive
    } else {
        Class::Background
    }
}

fn step<A: RowApplication>(
    cohort: &mut VecDeque<Request>,
    inner: &Inner<A>,
    receive_limit: usize,
) -> Vec<Request> {
    let receiving = cohort
        .front()
        .is_some_and(|r| matches!(r.work, Work::Receive(_)));
    let limit = if receiving {
        receive_limit
    } else {
        STEP_REQUESTS
    };
    let mut group = Vec::new();
    let mut bytes = 0usize;
    while let Some(request) = cohort.front() {
        if !group.is_empty()
            && (group.len() == limit
                || bytes.saturating_add(request.weight) > STEP_BYTES
                || receiving != matches!(request.work, Work::Receive(_)))
        {
            break;
        }
        let indivisible = if let Work::Receive(frame) = &request.work {
            inner
                .open_payload(frame)
                .and_then(|opened| inner.decode_row_payload(frame, &opened.payload))
                .map_or(true, |payload| {
                    payload.membership().is_some()
                        || payload.writes().len() as u64 > APPLICATION_ROWS
                })
        } else {
            false
        };
        if !group.is_empty() && indivisible {
            break;
        }
        bytes = bytes.saturating_add(request.weight);
        if let Some(request) = cohort.pop_front() {
            group.push(request);
        }
        if indivisible {
            break;
        }
    }
    group
}

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

enum PendingReply {
    Stored(Hash),
    Duplicate,
    Covered,
}
type Settling = Vec<(oneshot::Sender<Reply>, bool, PendingReply)>;

struct Exposed {
    replies: Vec<(oneshot::Sender<Reply>, Done)>,
    commit: LockedCommit,
}
impl Exposed {
    fn append(all: &mut Option<Self>, next: Option<Self>) {
        if let Some(mut next) = next {
            if let Some(all) = all {
                all.commit = all.commit.max(next.commit);
                all.replies.append(&mut next.replies);
            } else {
                *all = Some(next);
            }
        }
    }
    async fn answer(self, flusher: Arc<Flusher>) {
        let _ = rocky_platform::task::spawn_blocking(move || {
            let flushed = flusher.flush(self.commit);
            for (reply, done) in self.replies {
                let _ = reply.send(match &flushed {
                    Ok(()) => Ok((done, self.commit.count())),
                    Err(error) => Err(shared(error)),
                });
            }
        })
        .await;
    }
}

impl<A: RowApplication> Inner<A> {
    async fn commit_group(
        &mut self,
        group: Vec<Request>,
    ) -> (Option<Exposed>, BTreeSet<ResourceScope>, Settling) {
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
        let kept = self.pending_pass_current(self.commit);
        let receiving = works.iter().any(|work| matches!(work, Work::Receive(_)));
        let epoch = self.context_epoch.load(Ordering::Relaxed);
        let outcome = match now() {
            Ok(time) => transaction!(@without_wal_checkpoint self, |inner| inner.run_group(&works, time).await)
                .map(|outcome| (outcome, time)),
            Err(error) => Err(error),
        };
        let (outcome, _) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                for (reply, _) in replies {
                    let _ = reply.send(Err(shared(&error)));
                }
                return (None, BTreeSet::new(), Vec::new());
            }
        };
        if kept && !receiving {
            self.keep_pending_pass();
        } else if kept
            && outcome.pass.is_none()
            && epoch == self.context_epoch.load(Ordering::Relaxed)
        {
            self.keep_received_pass();
        }
        #[cfg(any(test, feature = "qualification"))]
        if let Err(error) = self.crash_at(|point| point == super::CrashPoint::GroupAfterCommit) {
            for (reply, _) in replies {
                let _ = reply.send(Err(shared(&error)));
            }
            return (None, BTreeSet::new(), Vec::new());
        }
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
        let more = outcome.more && outcome.pass.is_none();
        let mut exposed = Vec::new();
        let mut settling = Vec::new();
        for ((reply, flush), result) in replies.into_iter().zip(outcome.results) {
            let done = match result {
                Ok(Staged::Stored(hash)) if more => {
                    settling.push((reply, flush, PendingReply::Stored(hash)));
                    continue;
                }
                Ok(Staged::Duplicate) if more => {
                    settling.push((reply, flush, PendingReply::Duplicate));
                    continue;
                }
                Ok(Staged::Covered) if more => {
                    settling.push((reply, flush, PendingReply::Covered));
                    continue;
                }
                Ok(Staged::Author(frame)) => Ok(Done::Author(frame)),
                Ok(Staged::Bundle(frames)) => Ok(Done::Bundle(frames)),
                Ok(Staged::Handled(hash)) => Ok(Done::Pruned(hash)),
                Ok(Staged::Covered) => outcome
                    .pass
                    .as_ref()
                    .map_or(Ok(Done::Receive(ReceiveStatus::Pruned)), |error| {
                        Err(shared(error))
                    }),
                Ok(Staged::Duplicate) => outcome
                    .pass
                    .as_ref()
                    .map_or(Ok(Done::Receive(ReceiveStatus::Duplicate)), |error| {
                        Err(shared(error))
                    }),
                Ok(Staged::Stored(hash)) => match &outcome.pass {
                    Some(error) => Err(shared(error)),
                    None => self.status(hash).await.map(Done::Receive),
                },
                Err(error) => Err(error),
            };
            match done {
                Ok(done) if flush => exposed.push((reply, done)),
                done => {
                    let _ = reply.send(done.map(|done| (done, self.commit)));
                }
            }
        }
        let exposed = (!exposed.is_empty()).then_some(Exposed {
            replies: exposed,
            commit: self.locked_commit(),
        });
        (exposed, scopes, settling)
    }
    async fn answer_settled(
        &self,
        settling: Settling,
        settled: Result<(), Error>,
    ) -> Option<Exposed> {
        let mut exposed = Vec::new();
        for (reply, flush, pending) in settling {
            let done = match &settled {
                Ok(()) => match pending {
                    PendingReply::Stored(hash) => self.status(hash).await.map(Done::Receive),
                    PendingReply::Duplicate => Ok(Done::Receive(ReceiveStatus::Duplicate)),
                    PendingReply::Covered => Ok(Done::Receive(ReceiveStatus::Pruned)),
                },
                Err(error) => Err(shared(error)),
            };
            match done {
                Ok(done) if flush => exposed.push((reply, done)),
                done => {
                    let _ = reply.send(done.map(|done| (done, self.commit)));
                }
            }
        }
        (!exposed.is_empty()).then_some(Exposed {
            replies: exposed,
            commit: self.locked_commit(),
        })
    }
    async fn run_group(&mut self, works: &[Work], time: u64) -> Result<Outcome, Error> {
        let mut outcome = Outcome::default();
        let mut unapplied = false;
        for work in works {
            self.cause(match work {
                Work::Receive(_) => crate::observe::Cause::Sync,
                Work::Author { .. } | Work::Bundle { .. } => crate::observe::Cause::Local,
            });
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
                    Ok(None) => match self.check_authority(*authority) {
                        Ok(()) => self
                            .author_bundle(*handle, *input, writes, dependencies, time)
                            .await
                            .map(Staged::Bundle),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                },
                Work::Receive(frame) => self.receive_step(frame, time).await,
            };
            match result {
                Err(error) if retryable(&error) => return Err(error),
                Err(error) => {
                    self.conn.execute("ROLLBACK TO request", ()).await?;
                    self.invalidate_context();
                    self.conn.execute("RELEASE request", ()).await?;
                    outcome.results.push(Err(error));
                }
                Ok(staged) => {
                    unapplied |= matches!(
                        staged,
                        Staged::Stored(_) | Staged::Covered | Staged::Duplicate
                    );
                    self.conn.execute("RELEASE request", ()).await?;
                    if outcome.pass.is_none()
                        && matches!(staged, Staged::Stored(_) | Staged::Duplicate)
                        && let Work::Receive(frame) = work
                    {
                        self.conn
                            .execute("SAVEPOINT received_application", ())
                            .await?;
                        #[cfg(test)]
                        let before = self.applied_frames;
                        let applied = self.apply_received(frame, time).await;
                        #[cfg(test)]
                        let applied = if self.applied_frames != before && !crate::apply_allowed() {
                            Err(Error::InjectedFailure)
                        } else {
                            applied
                        };
                        if let Err(error) = applied {
                            self.conn
                                .execute("ROLLBACK TO received_application", ())
                                .await?;
                            self.invalidate_context();
                            self.invalidate_held_refresh();
                            self.untouch();
                            if retryable(&error) {
                                return Err(error);
                            }
                            outcome.pass = Some(error);
                        }
                        self.conn
                            .execute("RELEASE received_application", ())
                            .await?;
                    }
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
        outcome.more = unapplied;
        #[cfg(any(test, feature = "qualification"))]
        self.crash_at(|point| point == super::CrashPoint::GroupBeforeCommit)?;
        #[cfg(test)]
        match self.group_fault {
            Some(GroupFault::ExitBeforeCommit) => std::process::exit(77),
            Some(GroupFault::FailBeforeCommit) => return Err(Error::InjectedFailure),
            Some(GroupFault::FailAfterCommits(count)) if self.group_commits >= count => {
                return Err(Error::InjectedFailure);
            }
            _ => {}
        }
        outcome.revision = self.stored_revision().await?;
        Ok(outcome)
    }
    async fn receive_step(&mut self, frame: &EncryptedFrame, time: u64) -> Result<Staged, Error> {
        let bytes =
            usize::try_from(frame.stored_bytes()?).map_err(|_| denied("stored frame size"))?;
        let migration = self.check_migration_frame(frame, time).await?;
        let operation = frame.operation_ref()?;
        let mut store = TransactionStore(self.conn.clone());
        let held = store.has_operation(operation.hash).await?;
        let verified_on_arrival = held
            && self
                .manifest_of_tip(operation.header.extensions.tip)
                .and_then(|manifest| self.admits(manifest))
                .is_err();
        if verified_on_arrival {
            return Ok(Staged::Duplicate);
        }
        self.verify_frame(frame, time)?;
        let operation = frame.operation_ref()?;
        self.refuse_no_frames_contradiction_in_storing_transaction(&operation.header)
            .await?;
        let extension = &operation.header.extensions;
        if let Some(key) = self.opening_key(extension.scope, extension.generation)
            && let Ok(opened) = self.crypto.open(frame, key)
        {
            if opened.partition.as_ref() != self.partitions.get(&extension.scope) {
                return Err(denied(
                    "sealed partition tuple differs from its keyed scope",
                ));
            }
            RowPayload::check_formats(&opened.payload)?;
        }
        let local = self.local_grants(time)?;
        if !roles::holds_whole(&local, operation.header.extensions.scope) {
            return Err(denied("incoming custody denied"));
        }
        if held {
            return Ok(Staged::Duplicate);
        }
        let scope = operation.header.extensions.scope;
        if self.frame_reach(frame)? != tips::FrameReach::Held
            && let Some(id) = self.find_scope_id(scope).await?
            && let Some(log) = self.log(id, operation.header.public_key).await?
            && operation.header.seq_num as i64 <= log.pruned_prefix
        {
            if self.has_pending(scope).await? {
                self.put_stub(operation.hash, id, operation.header.timestamp, false)
                    .await?;
            }
            return Ok(Staged::Covered);
        }
        if self
            .voided_position(scope, operation.header.public_key, operation.header.seq_num)
            .await?
        {
            return Ok(Staged::Covered);
        }
        self.admit_scope(&operation.header).await?;
        if migration.is_none() {
            self.check_budget(bytes).await?;
        }
        self.keep_chain(frame.chain()?).await?;
        self.no_frames_intake(&operation.header).await?;
        store
            .insert_operation(
                operation.hash,
                &operation.header,
                operation.body.as_ref(),
                frame.header_bytes(),
                &operation.header.extensions.scope,
            )
            .await?;
        self.settle_placement(&operation.header).await?;
        if let Some(statement) = migration {
            let proof = &frame
                .migration
                .as_ref()
                .ok_or_else(|| denied("migration proof missing"))?
                .proof;
            self.classify_migration(&statement, frame, proof).await?;
        }
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
    pub(crate) async fn checkpoint_if_due(&mut self) {
        self.checkpoint_over(self.wal_limits.1).await;
    }
    pub(super) async fn cut_wal(&mut self) {
        self.file.passive.clone().wait().await;
        self.checkpoint_over(WAL_CHECKPOINT_BYTES).await;
    }
    pub(super) async fn checkpoint_over(&mut self, limit: u64) {
        self.reclaim_free_pages().await;
        let size = rocky_platform::fs::len(&self.wal).unwrap_or(0);
        if size < limit {
            if size >= self.wal_limits.0 {
                self.file.passive.schedule(size, self.flusher.clone());
            }
            return;
        }
        self.file.passive.clone().wait().await;
        if self.file.quiet_truncation_failed_below(size) {
            self.file.passive.schedule(size, self.flusher.clone());
            return;
        }
        if self.truncate_wal(TRUNCATE_WAIT).await {
            return;
        }
        if size >= self.wal_limits.1 {
            if self.truncate_quietly().await {
                return;
            }
            self.file
                .quiet_truncation_failed_at(size, WAL_CHECKPOINT_BYTES);
        }
        self.file.passive.schedule(size, self.flusher.clone());
    }
    async fn truncate_quietly(&mut self) -> bool {
        let quiet = self.workspace.queries.cursors.quiesce().await;
        #[cfg(test)]
        {
            self.quiesces += 1;
            if let Some((reached, resume)) = self.quiet_pause.take() {
                let _ = reached.send(());
                let _ = resume.await;
            }
        }
        quiet.preempted() > 0 && self.truncate_wal(QUIET_TRUNCATE_WAIT).await
    }
    async fn truncate_wal(&mut self, busy: std::time::Duration) -> bool {
        let waits = self.conn.connection().set_busy_timeout(busy).is_ok();
        #[cfg(feature = "qualification")]
        let started = web_time::Instant::now();
        let checkpoint = rows(&self.conn, "PRAGMA wal_checkpoint(TRUNCATE)").await;
        #[cfg(feature = "qualification")]
        if let Ok(result) = &checkpoint {
            let pages = match result.first().map(Vec::as_slice) {
                Some([_, _, SqlValue::Integer(done)]) => (*done).max(0) as u64,
                _ => 0,
            };
            self.flusher
                .measurements
                .checkpoint("truncate", pages, started.elapsed());
        }
        if waits {
            let _ = self
                .conn
                .connection()
                .set_busy_timeout(std::time::Duration::ZERO);
        }
        let Ok(result) = checkpoint else {
            return false;
        };
        #[cfg(test)]
        {
            self.checkpoints += 1;
        }
        let complete = matches!(
            result.first().map(Vec::as_slice),
            Some([SqlValue::Integer(0), SqlValue::Integer(log), SqlValue::Integer(done)])
                if log == done
        );
        if complete {
            self.file.truncated();
        }
        complete
    }
    async fn reclaim_free_pages(&self) {
        let free = rows(&self.conn, "PRAGMA freelist_count").await;
        if let Ok(free) = free
            && let Some([SqlValue::Integer(free)]) = free.first().map(Vec::as_slice)
            && *free >= RECLAIM_PAGES
        {
            let _ = rows(
                &self.conn,
                &format!("PRAGMA incremental_vacuum({RECLAIM_STEP})"),
            )
            .await;
            #[cfg(any(test, feature = "qualification"))]
            if let Ok(after) = rows(&self.conn, "PRAGMA freelist_count").await
                && let Some([SqlValue::Integer(after)]) = after.first().map(Vec::as_slice)
            {
                self.flusher.measurements.work(
                    lock::WorkUnit::ReclaimedPage,
                    free.saturating_sub(*after).max(0) as u64,
                );
            }
        }
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn reclaim_free_pages_for_test(&self) {
        self.inner
            .lock(Class::Background)
            .await
            .reclaim_free_pages()
            .await;
    }
}

pub(super) fn shared(error: &Error) -> Error {
    match error {
        #[cfg(test)]
        Error::InjectedFailure => Error::InjectedFailure,
        Error::RetryExhausted => Error::RetryExhausted,
        Error::Closed => Error::Closed,
        Error::Conflict => Error::Conflict,
        Error::Blocked(blockers) => Error::Blocked(blockers.clone()),
        Error::Sql(error) => Error::Sql(error.clone()),
        Error::Backpressure => Error::Backpressure,
        Error::Quota { resource, limit } => Error::Quota {
            resource,
            limit: *limit,
        },
        Error::Rejected(message) => Error::Rejected(message.clone()),
        Error::Incompatible(message) => Error::Incompatible(message.clone()),
        Error::Durability(message) => Error::Durability(message.clone()),
        Error::Corrupt(message) => Error::Corrupt(message.clone()),
        Error::Rollback { .. } => Error::Corrupt(error.to_string()),
        Error::Io(source) => Error::Io(std::io::Error::new(source.kind(), source.to_string())),
        other => Error::Io(std::io::Error::other(other.to_string())),
    }
}

#[cfg(test)]
mod sharing {
    use super::*;
    #[test]
    fn a_corrupt_error_shared_again_keeps_one_consistency_prefix() {
        let original = Error::Corrupt("torn page".into());
        assert_eq!(shared(&shared(&original)).to_string(), original.to_string());
    }
}
