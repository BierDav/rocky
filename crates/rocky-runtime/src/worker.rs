mod audit;
mod incoming;
mod pruning;
mod rebootstrap;
pub(crate) mod retention;
use crate::read::{Changed, ReadPlan, ReadPool, Reads};
use crate::scoped_connection::ScopedConnection;
use crate::{
    Command, Identity, Mutation, Snapshot,
    storage::{self, Error, application, bindings, finish, retryable, rows},
    writer::{Answer, Answered, Metrics, nanos},
};
use rocky_core::{
    AdmissionBoundary, AuthorSequence, Envelope, LamportTime, LocalRevision, NodeId, OperationId,
    ProjectionStatement, SqlValue, StoredApplication, required_dependencies,
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::watch;
use web_time::Instant;

#[cfg(test)]
#[cfg_attr(
    target_family = "wasm",
    expect(
        dead_code,
        reason = "Process-exiting faults are exercised only by native crash tests."
    )
)]
pub(crate) enum Fault {
    FailBeforeCommit,
    RetryBeforeCommit(usize),
    AbortAfterWrites(usize),
    AbortBeforeCommit,
    AbortAfterCommit,
    FailCommit,
    AbortInCheckpoint,
    Pause(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    ),
    RecordGroups(GroupLog),
    SnapshotAtFlush(std::path::PathBuf),
    CheckpointFrames(i64),
    FailFollowing,
    VmSteps(tokio::sync::oneshot::Sender<u64>),
}
#[cfg(test)]
pub(crate) type GroupLog = Arc<std::sync::Mutex<Vec<Vec<LoggedStep>>>>;
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct LoggedStep {
    pub(crate) domain: usize,
    pub(crate) queue_weight: usize,
}
#[cfg(test)]
impl Fault {
    pub(crate) fn file_level(&self) -> bool {
        matches!(
            self,
            Self::AbortBeforeCommit
                | Self::AbortAfterCommit
                | Self::FailCommit
                | Self::AbortInCheckpoint
                | Self::RecordGroups(_)
                | Self::SnapshotAtFlush(_)
                | Self::CheckpointFrames(_)
                | Self::VmSteps(_)
        )
    }
}

pub(crate) struct Notifications {
    pub(crate) revisions: watch::Sender<LocalRevision>,
    pub(crate) changes: watch::Sender<u64>,
}
#[derive(Default)]
struct Unpublished {
    revision: bool,
    changes: bool,
    meta: bool,
    views: Changed,
    causes: std::collections::BTreeSet<crate::observe::Cause>,
}
impl Unpublished {
    pub(crate) fn changed(&mut self, changed: Changed, cause: crate::observe::Cause) {
        if changed.any() {
            self.causes.insert(cause);
        }
        self.views.merge(changed);
    }
}
type ChangedTablesAndViews = (Vec<String>, Vec<usize>);
const MAX_STATEMENT_TARGETS: usize = 4096;
const RECORD_OVERHEAD: usize = 128;
pub(crate) struct Worker<A: StoredApplication> {
    writer: ScopedConnection,
    reader: Option<ScopedConnection>,
    reading: bool,
    unbounded: bool,
    pub(crate) in_group: bool,
    instrument: bool,
    metrics: Arc<Metrics>,
    unpublished: Unpublished,
    dirty: bool,
    flushed_sequence: u64,
    schema: rocky_core::InstalledSchema,
    app: A,
    policy: A::Policy,
    identity: Identity,
    config: crate::RuntimeConfig,
    plan: Arc<ReadPlan>,
    reads: Arc<Reads>,
    targets: std::collections::HashMap<Box<str>, Option<ChangedTablesAndViews>>,
    cascades: std::collections::HashMap<String, Vec<String>>,
    events: Arc<crate::observe::TableEvents>,
    retention: retention::Retention,
    retained: usize,
    retained_bytes: usize,
    frontier: BTreeMap<NodeId, AuthorSequence>,
    sequence: u64,
    lamport: u64,
    revision: LocalRevision,
    revisions: watch::Sender<LocalRevision>,
    changes: watch::Sender<u64>,
    views: Vec<watch::Sender<u64>>,
    meta: watch::Sender<u64>,
    #[cfg(test)]
    pub(crate) fault: Option<Fault>,
}
pub(crate) fn identity_bytes<A: StoredApplication>(
    identity: Identity,
    app: &A,
    config: &crate::RuntimeConfig,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    for id in [
        identity.scope.mesh.0,
        identity.scope.app.0,
        identity.scope.domain.0,
        identity.node.0,
        identity.caller.0,
    ] {
        bytes.extend(id);
    }
    let descriptor = app.descriptor();
    for version in [
        4u32,
        descriptor.operations.protocol,
        descriptor.operations.schema,
        descriptor.operations.operation,
        descriptor.projection,
    ] {
        bytes.extend(version.to_le_bytes());
    }
    bytes.extend((config.authors.len() as u64).to_le_bytes());
    for (node, caller) in &config.authors {
        bytes.extend(node.0);
        bytes.extend(caller.0);
    }
    bytes.extend((config.delivery_targets.len() as u64).to_le_bytes());
    for node in &config.delivery_targets {
        bytes.extend(node.0);
    }
    bytes.extend(retention::definition_commitment(app));
    bytes
}
impl<A> Worker<A>
where
    A: StoredApplication,
    A::State: Clone,
{
    pub(crate) async fn open_connection(
        conn: ScopedConnection,
        domain: crate::DomainInstallation<A>,
        options: crate::RuntimeOptions,
        notifications: Notifications,
        pool: Arc<ReadPool>,
        metrics: Arc<Metrics>,
    ) -> Result<Self, Error> {
        let crate::DomainInstallation {
            app,
            policy,
            identity,
            config,
        } = domain;
        if let crate::checkpoint::PrunedPrefixCustody::FullNodesApplied(custody) =
            options.retention.custody
            && custody.get() > config.full_nodes.len()
        {
            return Err(Error::Incompatible(
                "custody needs that many configured full nodes".into(),
            ));
        }
        app.descriptor().operations.validate()?;
        if let Some(schema) = app.logical_schema() {
            schema
                .supported_on(rocky_core::Profile::Trusted)
                .map_err(|error| Error::Incompatible(error.to_string()))?;
        }
        let schema = app.installed_schema();
        if let Some(manifest) = app.manifest() {
            manifest.validate()?;
            if !manifest.supports(
                identity.scope,
                app.descriptor().operations,
                app.descriptor().projection,
            ) {
                return Err(Error::Incompatible(
                    "installed manifest binding differs".into(),
                ));
            }
        }
        let tables = rows(&conn, "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").await?;
        let mut tables = conn.logical_tables(tables);
        let mut fresh = tables.is_empty();
        let interrupted = conn.names().namespace().is_none()
            && tables.contains(&vec![SqlValue::Text("rocky_bootstrap".into())]);
        let expected = identity_bytes(identity, &app, &config);
        if !fresh && !interrupted {
            if !tables.contains(&vec![SqlValue::Text("rocky_replica".into())]) {
                return Err(Error::Incompatible(
                    "missing Rocky identity metadata".into(),
                ));
            }
            let stored = rows(&conn, "SELECT identity FROM rocky_replica WHERE id=1").await?;
            if stored != vec![vec![SqlValue::Blob(expected.clone())]] {
                return Err(Error::Incompatible(
                    "node, scope, caller or installed definition differs".into(),
                ));
            }
        }
        crate::writer::configure(&conn, options.durability).await?;
        app.admit_read(identity.scope, identity.caller, &policy)
            .map_err(application)?;
        if conn.names().namespace().is_none()
            && ((fresh && schema.statements.len() > crate::bootstrap::STEP) || interrupted)
        {
            crate::bootstrap::install(
                &conn,
                &expected,
                crate::bootstrap::domain_statements(&conn, identity, &app, &config)?,
            )
            .await?;
            crate::writer::configure(&conn, options.durability).await?;
            tables = rows(&conn, "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").await?;
            fresh = false;
        }
        let context = rocky_core::AdmissionContext {
            scope: identity.scope,
            caller: identity.caller,
            policy_version: app.policy_version(&policy),
            boundary: AdmissionBoundary::Local,
        };
        let plan = Arc::new(ReadPlan::compile(
            conn.names(),
            app.read_views(context, &policy).map_err(application)?,
        )?);
        let (views, view_changes): (Vec<_>, Vec<_>) =
            (0..plan.views()).map(|_| watch::channel(0u64)).unzip();
        let (meta, meta_changes) = watch::channel(0u64);
        let retention = retention::Retention::new(&conn, &app, options.retention)?;
        let marker = conn
            .names()
            .physical("rocky_checkpoint")
            .unwrap_or("rocky_checkpoint")
            .to_string();
        let reads = Arc::new(Reads::new(
            identity.scope,
            plan.clone(),
            pool,
            view_changes,
            meta_changes,
            retention.row_audit(marker),
        ));
        let observed: Vec<String> = match app.logical_schema() {
            Some(logical) => logical
                .tables
                .iter()
                .map(|table| table.sql_name.clone())
                .collect(),
            None => crate::scoped_connection::tables(&schema.statements)
                .into_iter()
                .map(|(table, _)| table)
                .filter(|table| !table.to_ascii_lowercase().starts_with("rocky_"))
                .collect(),
        };
        let events = crate::observe::TableEvents::new(identity.scope, observed);
        reads.bind_events(&events);
        let authors: std::collections::BTreeSet<_> =
            config.authors.iter().map(|(node, _)| *node).collect();
        reads.bind_projection(crate::read::ProjectionReads {
            query: conn.names().sql(&schema.projection_query),
            applied: conn.names().sql(
                "SELECT sequence FROM rocky_operations WHERE node=? AND status='applied' ORDER BY sequence DESC LIMIT 1",
            ),
            retention: conn.names().sql("SELECT state FROM rocky_checkpoint WHERE id=1"),
            authors: authors.into_iter().collect(),
        });
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let initialization = async {
            if fresh {
                for sql in storage::SCHEMA {
                    conn.execute(*sql, ()).await?;
                }
                for sql in &schema.statements {
                    conn.execute(sql, ()).await?;
                }
                conn.execute(
                    "INSERT INTO rocky_replica VALUES(1,?,0,0,0)",
                    [crate::sqlite::Value::Blob(expected)],
                )
                .await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = initialization {
            return finish(conn.connection(), Err(error)).await;
        }
        let mut worker = Self {
            writer: conn,
            reader: None,
            reading: false,
            unbounded: false,
            in_group: false,
            instrument: options.instrument,
            metrics,
            unpublished: Unpublished::default(),
            dirty: true,
            flushed_sequence: 0,
            schema,
            app,
            policy,
            identity,
            config,
            plan,
            reads,
            targets: Default::default(),
            cascades: Default::default(),
            events,
            retention,
            retained: 0,
            retained_bytes: 0,
            frontier: BTreeMap::new(),
            sequence: 0,
            lamport: 0,
            revision: LocalRevision::default(),
            revisions: notifications.revisions,
            changes: notifications.changes,
            views,
            meta,
            #[cfg(test)]
            fault: None,
        };
        let migration = async {
            crate::schema_evolution::apply(worker.conn(), &worker.app).await?;
            if let Some(schema) = worker.app.logical_schema() {
                crate::logical::bind(worker.conn(), schema).await?;
                worker.reads.bind_logical(schema.clone());
            }
            if fresh {
                worker.conn().execute(storage::PEER_SCHEMA, ()).await?;
            } else {
                for table in ["rocky_mutation_handles", "rocky_peer_ack"] {
                    if !tables.contains(&vec![SqlValue::Text(table.into())]) {
                        return Err(Error::Corrupt(format!("missing storage table {table}")));
                    }
                }
            }
            if fresh {
                worker.conn().execute(storage::UNAPPLIED_INDEX, ()).await?;
            }
            if fresh {
                for sql in retention::SCHEMA {
                    worker.conn().execute(*sql, ()).await?;
                }
                worker.install_retention().await?;
            } else {
                for table in crate::scoped_connection::tables(retention::SCHEMA)
                    .into_iter()
                    .map(|(table, _)| table)
                {
                    if !tables.contains(&vec![SqlValue::Text(table.clone())]) {
                        return Err(Error::Corrupt(format!("missing storage table {table}")));
                    }
                }
            }
            Ok(())
        }
        .await;
        finish(worker.conn().connection(), migration).await?;
        worker.cascades = cascades(worker.conn(), &worker.schema.statements).await?;
        worker
            .transaction(async |worker| worker.restore(true).await)
            .await?;
        if let Err(error) = worker.process_pending().await
            && !matches!(error, Error::Rejected(_))
        {
            return Err(error);
        }
        worker.revisions.send_replace(worker.revision);
        worker.publish();
        Ok(worker)
    }
    pub(super) fn conn(&self) -> &ScopedConnection {
        match (&self.reader, self.reading) {
            (Some(reader), true) => reader,
            _ => &self.writer,
        }
    }
    pub(crate) fn attach_reader(&mut self, reader: Arc<crate::sqlite::Connection>) {
        self.retention.attach_reader(reader.clone());
        self.reader = Some(self.writer.with_connection(reader));
    }
    pub(crate) fn reads(&self) -> Arc<Reads> {
        self.reads.clone()
    }
    pub(crate) fn publish(&mut self) {
        let pending = std::mem::take(&mut self.unpublished);
        if pending.revision {
            self.revisions.send_replace(self.revision);
        }
        if pending.changes {
            self.changes
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        pending.views.publish(&self.views);
        if pending.views.any() {
            let causes: Vec<_> = pending.causes.iter().copied().collect();
            self.events
                .publish(pending.views.tables(), self.revision.get(), &causes);
        }
        if pending.meta {
            self.meta
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }
    pub(crate) fn dirty(&self) -> bool {
        self.dirty
    }
    pub(crate) fn buffered(&mut self) {
        self.dirty = true;
    }
    pub(crate) fn flushed(&mut self) {
        self.dirty = false;
        self.flushed_sequence = self.sequence;
        self.retention.flushed();
    }
    fn class(&self, operation: &Envelope) -> crate::CommitClass {
        if operation.id.sequence.get() <= self.flushed_sequence {
            crate::CommitClass::Flushed
        } else {
            crate::CommitClass::Buffered
        }
    }
    fn notify(&mut self, revision: bool) {
        self.unpublished.revision |= revision;
        self.unpublished.changes = true;
    }
    pub(crate) async fn recover(&mut self) -> Result<(), Error> {
        self.unpublished = Unpublished::default();
        self.transaction(async |worker| worker.restore(false).await)
            .await
    }
    pub(crate) fn into_notifications(self) -> Notifications {
        Notifications {
            revisions: self.revisions,
            changes: self.changes,
        }
    }
    pub(crate) async fn step(&mut self, command: Command<A::Row>) -> Box<dyn Answer> {
        match command {
            Command::Mutate(handle, mutation, baseline, reply) => {
                let result = self.mutate(handle, mutation, baseline).await;
                let result = result.map(|operation| {
                    let class = self.class(&operation);
                    crate::CommittedMutation { operation, class }
                });
                Box::new(crate::writer::Committing { reply, result })
            }
            Command::Sync(request, reply) => {
                let result = self.sync(request).await;
                if result.is_ok() {
                    self.notify(false);
                }
                Box::new(Answered { reply, result })
            }
            Command::Retention(request, reply) => {
                let result = self.retention(*request).await;
                Box::new(Answered { reply, result })
            }
            other => {
                self.read(other).await;
                Box::new(crate::writer::Unanswered)
            }
        }
    }
    pub(crate) async fn read(&mut self, command: Command<A::Row>) {
        match command {
            Command::Query(reply) => {
                let _ = reply.send(Ok(()));
            }
            Command::Decode(projected, reply) => {
                let _ = reply.send(self.decode(*projected));
            }
            Command::LookupMutation(handle, reply) => {
                let result = self
                    .read_transaction(async |worker| worker.lookup_mutation(handle).await)
                    .await;
                let result = result.and_then(|result| match result {
                    Some((_, operation)) => operation.map(|operation| {
                        Some(crate::CommittedMutation {
                            class: self.class(&operation),
                            operation,
                        })
                    }),
                    None => Ok(None),
                });
                let _ = reply.send(result);
            }
            Command::Flush(reply) => {
                let _ = reply.send(Ok(()));
            }
            Command::Sync(request, reply) => {
                let _ = reply.send(self.sync(request).await);
            }
            Command::Retention(request, reply) => {
                let _ = reply.send(self.retention(*request).await);
            }
            other => other.refuse(Error::Closed),
        }
    }
    fn admission_context(
        &self,
        op: &Envelope,
        boundary: AdmissionBoundary,
    ) -> rocky_core::AdmissionContext {
        rocky_core::AdmissionContext {
            scope: op.scope,
            caller: op.caller,
            policy_version: self.app.policy_version(&self.policy),
            boundary,
        }
    }
    pub(super) async fn transaction<T>(
        &mut self,
        mut body: impl AsyncFnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if !self.in_group {
            return self.standalone(body).await;
        }
        for _ in 0..storage::TRANSACTION_ATTEMPTS {
            if self.group_ended()? {
                return Err(storage::group_lost());
            }
            self.conn().execute("SAVEPOINT step", ()).await?;
            self.retention.begin_step()?;
            let result = match body(self).await {
                Ok(value) => match self.settle().await {
                    Ok(digests) => match self.conn().execute("RELEASE step", ()).await {
                        Ok(_) => {
                            self.adopt_digests(digests);
                            return Ok(value);
                        }
                        Err(error) => Err(Error::from(error)),
                    },
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            };
            if self.group_ended()? {
                return Err(result.err().unwrap_or_else(storage::group_lost));
            }
            let rolled_back = match self.conn().execute("ROLLBACK TO step", ()).await {
                Ok(_) => self.conn().execute("RELEASE step", ()).await,
                Err(error) => Err(error),
            };
            if let Err(rollback) = rolled_back {
                return Err(Error::Rollback {
                    cause: result.err().map(|e| e.to_string()).unwrap_or_default(),
                    rollback: rollback.into(),
                });
            }
            match result {
                Err(error) if retryable(&error) => {}
                result => return result,
            }
        }
        Err(Error::RetryExhausted)
    }
    fn group_ended(&self) -> Result<bool, Error> {
        Ok(self.conn().is_autocommit()?)
    }
    async fn standalone<T>(
        &mut self,
        mut body: impl AsyncFnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        for attempt in 0..storage::TRANSACTION_ATTEMPTS {
            let result = match self.conn().execute("BEGIN IMMEDIATE", ()).await {
                Ok(_) => {
                    let result = match self.retention.begin_step() {
                        Ok(()) => match body(self).await {
                            Ok(value) => self.settle().await.map(|digests| (value, digests)),
                            Err(error) => Err(error),
                        },
                        Err(error) => Err(error),
                    };
                    finish(self.conn().connection(), result).await
                }
                Err(error) => Err(Error::from(error)),
            };
            match result {
                Err(error) if retryable(&error) => {
                    if attempt + 1 < storage::TRANSACTION_ATTEMPTS {
                        rocky_platform::time::sleep(storage::backoff(attempt)).await;
                    }
                }
                Ok((value, digests)) => {
                    self.adopt_digests(digests);
                    return Ok(value);
                }
                Err(error) => return Err(error),
            }
        }
        Err(Error::RetryExhausted)
    }
    pub(super) async fn read_transaction<T>(
        &mut self,
        mut body: impl AsyncFnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        if self.reader.is_none() {
            return self.standalone(body).await;
        }
        for attempt in 0..storage::TRANSACTION_ATTEMPTS {
            self.reading = true;
            let deadline = (!self.unbounded)
                .then(|| web_time::Instant::now() + storage::READ_TRANSACTION_BOUND);
            let result = match self.conn().connection().set_deadline(deadline) {
                Ok(()) => match self.conn().execute("BEGIN", ()).await {
                    Ok(_) => {
                        let result = body(self).await;
                        finish(self.conn().connection(), result).await
                    }
                    Err(error) => Err(Error::from(error)),
                },
                Err(error) => Err(Error::from(error)),
            };
            let cleared = self.conn().connection().set_deadline(None);
            self.reading = false;
            cleared?;
            match result {
                Err(error) if retryable(&error) => {
                    if attempt + 1 < storage::TRANSACTION_ATTEMPTS {
                        rocky_platform::time::sleep(storage::backoff(attempt)).await;
                    }
                }
                result => return result,
            }
        }
        Err(Error::RetryExhausted)
    }
    fn decode(&self, projected: crate::read::Projected) -> Result<Snapshot<A::Row>, Error> {
        self.app
            .admit_read(self.identity.scope, self.identity.caller, &self.policy)
            .map_err(application)?;
        let rows = self.app.decode_rows(projected.rows).map_err(application)?;
        let rows = self
            .app
            .select_rows(
                rows,
                rocky_core::AdmissionContext {
                    scope: self.identity.scope,
                    caller: self.identity.caller,
                    policy_version: self.app.policy_version(&self.policy),
                    boundary: AdmissionBoundary::Local,
                },
                &self.policy,
            )
            .map_err(application)?;
        Ok(Snapshot {
            rows,
            revision: projected.revision,
            frontier: projected.frontier,
            query: crate::QueryIdentity {
                scope: self.identity.scope,
                name: self.app.definition(),
            },
        })
    }
    async fn lookup_mutation(
        &self,
        handle: crate::MutationHandle,
    ) -> Result<Option<(rocky_core::Commitment, Result<Envelope, Error>)>, Error> {
        let mut records = self
            .conn()
            .query(
                "SELECT request_hash,node,sequence FROM rocky_mutation_handles WHERE handle=?",
                [handle.0.to_vec()],
            )
            .await?;
        let Some(row) = records.next().await? else {
            return Ok(None);
        };
        let hash: rocky_core::Commitment = row
            .get::<Vec<u8>>(0)?
            .as_slice()
            .try_into()
            .map_err(|_| Error::Corrupt("mutation handle".into()))?;
        let id = incoming::operation_id(&row.get::<Vec<u8>>(1)?, row.get(2)?)?;
        Ok(Some((
            hash,
            match self.record(id).await? {
                Some((bytes, _)) if !self.retention.below_pruned_prefix(id) => {
                    Ok(Envelope::decode(&bytes)?)
                }
                _ => Err(Error::Pruned {
                    node: id.node,
                    sequence: id.sequence.get(),
                }),
            },
        )))
    }
    pub(super) fn applied_frontier(&self) -> rocky_core::AppliedFrontier {
        rocky_core::AppliedFrontier(
            self.frontier
                .iter()
                .map(|(node, sequence)| (*node, *sequence))
                .collect(),
        )
    }
    fn operation_budget(&self, bytes: usize) -> Result<(), Error> {
        let limit = self.retention.policy.retained_bytes;
        if self
            .retained_bytes
            .saturating_add(bytes)
            .saturating_add(RECORD_OVERHEAD)
            > limit
        {
            return Err(Error::Retention {
                retained: self.retained_bytes,
                limit,
            });
        }
        Ok(())
    }
    pub(super) async fn window(&self, op: &Envelope) -> Result<A::State, Error> {
        let mut loaded = Vec::new();
        for statement in self.app.touched(op) {
            loaded.extend(
                storage::collect_rows(
                    self.conn()
                        .query(&statement.sql, bindings(statement.values))
                        .await?,
                )
                .await?,
            );
        }
        self.app.window(loaded).map_err(application)
    }
    pub(super) async fn transition(
        &self,
        op: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<Result<Vec<ProjectionStatement>, A::Error>, Error> {
        let reduce_started = Instant::now();
        let before = self.window(op).await?;
        if self.retention.below_pruned_prefix(op.entity) && self.app.definite_drop(&before, op) {
            return Ok(self
                .app
                .admit_candidate(
                    &before,
                    &before,
                    op,
                    self.admission_context(op, boundary),
                    &self.policy,
                )
                .map(|()| Vec::new()));
        }
        let mut after = before.clone();
        let reduced = self.app.apply_ready(&mut after, op, boundary);
        if self.instrument {
            self.metrics.add_reduction(nanos(reduce_started));
        }
        let admit_started = Instant::now();
        let admitted = reduced.and_then(|()| {
            self.app.admit_candidate(
                &before,
                &after,
                op,
                self.admission_context(op, boundary),
                &self.policy,
            )
        });
        if self.instrument {
            self.metrics.add_admission(nanos(admit_started));
        }
        let project_started = Instant::now();
        let projected = admitted.map(|()| self.app.project(&after, op));
        if self.instrument {
            self.metrics.add_encoding(nanos(project_started));
        }
        Ok(projected)
    }
    pub(super) async fn store(
        &mut self,
        statements: Vec<ProjectionStatement>,
    ) -> Result<Changed, Error> {
        let mut changed = Changed::default();
        for statement in statements {
            if !self.targets.contains_key(statement.sql.as_ref()) {
                if self.targets.len() >= MAX_STATEMENT_TARGETS {
                    self.targets.clear();
                }
                let target = crate::read::written_table(&statement.sql).map(|table| {
                    let mut tables = vec![table.clone()];
                    tables.extend(self.cascades.get(&table).into_iter().flatten().cloned());
                    let views = tables
                        .iter()
                        .flat_map(|table| self.plan.reading(table))
                        .collect();
                    (tables, views)
                });
                self.targets.insert(statement.sql.as_ref().into(), target);
            }
            match self.targets.get(statement.sql.as_ref()) {
                Some(Some((tables, views))) => {
                    changed.record(Some(views));
                    for table in tables {
                        changed.table(table);
                    }
                }
                _ => changed.record(None),
            }
            self.conn()
                .execute(&statement.sql, bindings(statement.values))
                .await?;
        }
        Ok(changed)
    }
    async fn prepare(
        &self,
        handle: crate::MutationHandle,
        mutation: &Mutation,
        baseline: Option<LocalRevision>,
    ) -> Result<Result<Prepared, Envelope>, Error> {
        let request_hash = crate::mutation_hash(&mutation.payload, mutation.entity);
        if let Some((old, operation)) = self.lookup_mutation(handle).await? {
            if old != request_hash {
                return Err(Error::Conflict);
            }
            return operation.map(Err);
        }
        if baseline.is_some_and(|revision| revision != self.revision) {
            return Err(Error::Conflict);
        }
        let sequence = AuthorSequence::new(
            self.sequence
                .checked_add(1)
                .ok_or(rocky_core::ProtocolError::InvalidCounter)?,
        )?;
        let id = OperationId {
            node: self.identity.node,
            sequence,
        };
        let entity = mutation.entity.unwrap_or(id);
        let op = Envelope {
            scope: self.identity.scope,
            caller: self.identity.caller,
            versions: self.app.descriptor().operations,
            id,
            lamport: LamportTime::new(
                self.lamport
                    .checked_add(1)
                    .ok_or(rocky_core::ProtocolError::InvalidCounter)?,
            )?,
            entity,
            dependencies: required_dependencies(id, entity, mutation.entity.is_none())?,
            payload: mutation.payload.clone(),
        };
        let encode_started = Instant::now();
        let bytes = op.encode()?;
        if self.instrument {
            self.metrics.add_encoding(nanos(encode_started));
        }
        self.operation_budget(bytes.len())?;
        let admit_started = Instant::now();
        self.app.validate(&op).map_err(application)?;
        self.app.admit(&op, &self.policy).map_err(application)?;
        if self.instrument {
            self.metrics.add_admission(nanos(admit_started));
        }
        let ready_started = Instant::now();
        let ready = self.readiness(&op).await?;
        if self.instrument {
            self.metrics.add_reduction(nanos(ready_started));
        }
        if !matches!(ready, incoming::Readiness::Ready) {
            return Err(Error::Protocol(rocky_core::ProtocolError::NotReady(
                op.dependencies.clone(),
            )));
        }
        let statements = self
            .transition(&op, AdmissionBoundary::Local)
            .await?
            .map_err(application)?;
        let revision = self.revision.next()?;
        Ok(Ok(Prepared {
            handle,
            request_hash,
            op,
            bytes,
            statements,
            revision,
        }))
    }
    fn adopt(&mut self, op: &Envelope, bytes: usize, revision: LocalRevision) {
        self.retained += 1;
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(bytes)
            .saturating_add(RECORD_OVERHEAD);
        self.frontier.insert(op.id.node, op.id.sequence);
        self.sequence = op.id.sequence.get();
        self.lamport = op.lamport.get();
        self.revision = revision;
    }
    async fn mutate(
        &mut self,
        handle: crate::MutationHandle,
        mutation: Mutation,
        baseline: Option<LocalRevision>,
    ) -> Result<Envelope, Error> {
        let outcome = self
            .transaction(async |worker| {
                Ok(match worker.prepare(handle, &mutation, baseline).await? {
                    Ok(prepared) => {
                        let Prepared {
                            handle,
                            request_hash,
                            op,
                            bytes,
                            statements,
                            revision,
                        } = prepared;
                        let changed = worker
                            .write_mutation(handle, request_hash, &op, &bytes, statements, revision)
                            .await?;
                        Ok((op, bytes.len(), revision, changed))
                    }
                    Err(committed) => Err(committed),
                })
            })
            .await?;
        match outcome {
            Ok((op, bytes, revision, changed)) => {
                self.adopt(&op, bytes, revision);
                self.unpublished
                    .changed(changed, crate::observe::Cause::Local);
                self.notify(true);
                match self.process_following().await {
                    Err(error @ Error::Rollback { .. }) => return Err(error),
                    Err(_) => self.metrics.deferred_follow(),
                    Ok(_) => {}
                }
                Ok(op)
            }
            Err(committed) => Ok(committed),
        }
    }
    pub(super) async fn process_following(&mut self) -> Result<bool, Error> {
        #[cfg(test)]
        if matches!(self.fault, Some(Fault::FailFollowing)) {
            self.fault = None;
            return Err(Error::InjectedFailure);
        }
        let mut loaded = Vec::new();
        for query in self.app.following_queries(self.identity.node) {
            loaded.extend(
                storage::collect_rows(
                    self.conn()
                        .query(&query.sql, bindings(query.values))
                        .await?,
                )
                .await?,
            );
        }
        let follows = self
            .app
            .following_intents(loaded, self.identity.node)
            .map_err(application)?;
        let mut changed = false;
        for follow in follows {
            if follow.original.node != self.identity.node
                || self.retention.below_pruned_prefix(follow.original)
            {
                continue;
            }
            let mutation = Mutation {
                entity: follow.intent.entity,
                payload: follow.intent.payload,
            };
            let result = self
                .transaction(async |worker| {
                    match worker
                        .prepare(crate::MutationHandle(follow.handle), &mutation, None)
                        .await?
                    {
                        Ok(prepared) => {
                            let changed = worker
                                .write_mutation(
                                    prepared.handle,
                                    prepared.request_hash,
                                    &prepared.op,
                                    &prepared.bytes,
                                    prepared.statements,
                                    prepared.revision,
                                )
                                .await?;
                            Ok(Some((
                                prepared.op,
                                prepared.bytes.len(),
                                prepared.revision,
                                changed,
                            )))
                        }
                        Err(_) => Ok(None),
                    }
                })
                .await?;
            if let Some((op, bytes, revision, views)) = result {
                self.adopt(&op, bytes, revision);
                self.unpublished
                    .changed(views, crate::observe::Cause::Local);
                self.notify(true);
                changed = true;
            }
        }
        Ok(changed)
    }
    async fn write_mutation(
        &mut self,
        handle: crate::MutationHandle,
        request_hash: rocky_core::Commitment,
        op: &Envelope,
        bytes: &[u8],
        statements: Vec<ProjectionStatement>,
        revision: LocalRevision,
    ) -> Result<Changed, Error> {
        let sql_started = Instant::now();
        let changed = self
            .write_operation(handle, request_hash, op, bytes, statements)
            .await?;
        self.write_counters(op.id.sequence.get(), op.lamport.get(), revision)
            .await?;
        if self.instrument {
            self.metrics.add_sql(nanos(sql_started));
        }
        self.inject_fault().await?;
        Ok(changed)
    }
    async fn write_operation(
        &mut self,
        handle: crate::MutationHandle,
        request_hash: rocky_core::Commitment,
        op: &Envelope,
        bytes: &[u8],
        statements: Vec<ProjectionStatement>,
    ) -> Result<Changed, Error> {
        self.conn()
            .execute(
                "INSERT INTO rocky_operations VALUES(?,?,?,?,'applied','local')",
                crate::sqlite::params![
                    op.id.node.0.to_vec(),
                    op.id.sequence.get() as i64,
                    bytes.to_vec(),
                    op.lamport.get() as i64
                ],
            )
            .await?;
        self.conn()
            .execute(
                "INSERT INTO rocky_mutation_handles VALUES(?,?,?,?)",
                crate::sqlite::params![
                    handle.0.to_vec(),
                    request_hash.to_vec(),
                    op.id.node.0.to_vec(),
                    op.id.sequence.get() as i64
                ],
            )
            .await?;
        let changed = self.store(statements).await?;
        if !self.config.delivery_targets.is_empty() {
            self.conn()
                .execute(
                    "INSERT INTO rocky_outbox VALUES(?,?)",
                    crate::sqlite::params![op.id.node.0.to_vec(), op.id.sequence.get() as i64],
                )
                .await?;
        }
        Ok(changed)
    }
    async fn write_counters(
        &self,
        sequence: u64,
        lamport: u64,
        revision: LocalRevision,
    ) -> Result<(), Error> {
        self.conn()
            .execute(
                "UPDATE rocky_replica SET sequence=?,lamport=?,revision=? WHERE id=1",
                (sequence as i64, lamport as i64, revision.get() as i64),
            )
            .await?;
        Ok(())
    }
}
async fn cascades(
    conn: &ScopedConnection,
    schema: &[impl AsRef<str>],
) -> Result<std::collections::HashMap<String, Vec<String>>, Error> {
    let tables: Vec<String> = crate::scoped_connection::tables(schema)
        .into_iter()
        .map(|(table, _)| table.to_ascii_lowercase())
        .collect();
    let logical: std::collections::HashMap<String, String> = tables
        .iter()
        .filter_map(|table| {
            let physical = conn.names().physical(table)?;
            Some((physical.to_ascii_lowercase(), table.clone()))
        })
        .collect();
    let mut children: std::collections::HashMap<String, Vec<String>> = Default::default();
    for (physical, table) in &logical {
        let sql = format!(
            "PRAGMA foreign_key_list(\"{}\")",
            physical.replace('"', "\"\"")
        );
        for row in rows(conn, &sql).await? {
            let acts = |value: Option<&SqlValue>| {
                matches!(value, Some(SqlValue::Text(action))
                    if ["CASCADE", "SET NULL", "SET DEFAULT"].contains(&action.to_ascii_uppercase().as_str()))
            };
            let parent = match row.get(2) {
                Some(SqlValue::Text(parent)) => logical.get(&parent.to_ascii_lowercase()),
                _ => None,
            };
            if let (Some(parent), true) = (parent, acts(row.get(5)) || acts(row.get(6))) {
                children
                    .entry(parent.clone())
                    .or_default()
                    .push(table.clone());
            }
        }
    }
    let mut closure = std::collections::HashMap::new();
    for parent in children.keys() {
        let mut reached = std::collections::BTreeSet::new();
        let mut next = vec![parent.clone()];
        while let Some(table) = next.pop() {
            for child in children.get(&table).into_iter().flatten() {
                if reached.insert(child.clone()) {
                    next.push(child.clone());
                }
            }
        }
        reached.remove(parent);
        closure.insert(parent.clone(), reached.into_iter().collect());
    }
    Ok(closure)
}
struct Prepared {
    handle: crate::MutationHandle,
    request_hash: rocky_core::Commitment,
    op: Envelope,
    bytes: Vec<u8>,
    statements: Vec<ProjectionStatement>,
    revision: LocalRevision,
}
