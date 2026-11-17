use crate::{scoped_connection::ScopedConnection, storage, worker, *};

pub struct DomainInstallation<A: StoredApplication> {
    pub app: A,
    pub policy: A::Policy,
    pub identity: Identity,
    pub config: RuntimeConfig,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Durability {
    #[default]
    Buffered,
    Full,
}
impl Durability {
    pub(crate) fn synchronous(self) -> i64 {
        match self {
            Self::Buffered => 1,
            Self::Full => 2,
        }
    }
}
pub(crate) const MAX_FILES: usize = 16;
pub(crate) const MAX_PLACED_FILES: usize = 256;
pub(crate) const MAX_GROUP_COMMIT: usize = 256;
pub(crate) const MIN_CHECKPOINT_FRAMES: u32 = 256;
pub(crate) const MAX_CHECKPOINT_FRAMES: u32 = 1 << 20;
const DEFAULT_GROUP_COMMIT: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeOptions {
    pub durability: Durability,
    pub group_commit: usize,
    pub retention: RetentionPolicy,
    pub files: usize,
    pub checkpoint_frames: u32,
    pub instrument: bool,
}
impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            durability: Durability::Buffered,
            group_commit: DEFAULT_GROUP_COMMIT,
            retention: RetentionPolicy::default(),
            files: 1,
            checkpoint_frames: crate::writer::CHECKPOINT_FRAMES,
            instrument: false,
        }
    }
}
impl RuntimeOptions {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.group_commit == 0 || self.group_commit > MAX_GROUP_COMMIT {
            return Err(Error::Quota {
                resource: "group commit steps",
                limit: MAX_GROUP_COMMIT,
            });
        }
        if !(MIN_CHECKPOINT_FRAMES..=MAX_CHECKPOINT_FRAMES).contains(&self.checkpoint_frames) {
            return Err(Error::Quota {
                resource: "checkpoint frames",
                limit: MAX_CHECKPOINT_FRAMES as usize,
            });
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomainPlacement {
    pub scope: Scope,
    pub file: usize,
}
pub struct Runtime<A: StoredApplication> {
    domains: std::collections::HashMap<Scope, Replica<A>>,
    threads: Vec<rocky_platform::task::Dedicated<()>>,
}
impl<A> Runtime<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub async fn open(
        path: impl AsRef<Path>,
        installations: Vec<DomainInstallation<A>>,
    ) -> Result<Self, Error> {
        Self::open_with_options(path, installations, RuntimeOptions::default()).await
    }
    pub async fn open_with_options(
        path: impl AsRef<Path>,
        installations: Vec<DomainInstallation<A>>,
        options: RuntimeOptions,
    ) -> Result<Self, Error> {
        Self::open_layout(path, installations, options, None).await
    }
    pub async fn open_placed(
        path: impl AsRef<Path>,
        installations: Vec<DomainInstallation<A>>,
        placement: Vec<DomainPlacement>,
        options: RuntimeOptions,
    ) -> Result<Self, Error> {
        Self::open_layout(path, installations, options, Some(placement)).await
    }
    async fn open_layout(
        path: impl AsRef<Path>,
        mut installations: Vec<DomainInstallation<A>>,
        options: RuntimeOptions,
        placement: Option<Vec<DomainPlacement>>,
    ) -> Result<Self, Error> {
        options.validate()?;
        let max_files = if placement.is_some() {
            MAX_PLACED_FILES
        } else {
            MAX_FILES
        };
        if options.files == 0 || options.files > max_files {
            return Err(Error::Quota {
                resource: "runtime files",
                limit: max_files,
            });
        }
        if installations.is_empty() || installations.len() > 4096 {
            return Err(Error::Incompatible(
                "install between one and 4096 domains".into(),
            ));
        }
        installations.sort_by_key(|domain| {
            (
                domain.identity.scope.mesh,
                domain.identity.scope.app,
                domain.identity.scope.domain,
            )
        });
        let mut binding = Vec::new();
        let mut scopes = Vec::new();
        for domain in &mut installations {
            validate_config(domain.identity, &mut domain.config)?;
            domain.app.descriptor().operations.validate()?;
            let manifest = domain.app.manifest().ok_or_else(|| {
                Error::Incompatible("shared runtime requires an installed manifest".into())
            })?;
            manifest.validate()?;
            if !manifest.supports(
                domain.identity.scope,
                domain.app.descriptor().operations,
                domain.app.descriptor().projection,
            ) {
                return Err(Error::Incompatible(
                    "manifest does not bind installed domain and generations".into(),
                ));
            }
            if scopes.contains(&domain.identity.scope) {
                return Err(Error::Conflict);
            }
            domain
                .app
                .admit_read(
                    domain.identity.scope,
                    domain.identity.caller,
                    &domain.policy,
                )
                .map_err(storage::application)?;
            scopes.push(domain.identity.scope);
            let bytes = worker::identity_bytes(domain.identity, &domain.app, &domain.config);
            binding.extend((bytes.len() as u32).to_le_bytes());
            binding.extend(bytes);
        }
        let path = path.as_ref().to_path_buf();
        let used = if placement.is_some() {
            options.files
        } else {
            options.files.min(installations.len())
        };
        let routes: Vec<usize> = if let Some(placement) = &placement {
            let mut assigned = std::collections::HashMap::new();
            if placement.is_empty() || placement.len() > 4096 {
                return Err(Error::Incompatible(
                    "placement requires one to 4096 domains".into(),
                ));
            }
            for item in placement {
                if item.file >= used || assigned.insert(item.scope, item.file).is_some() {
                    return Err(Error::Incompatible(
                        "invalid or duplicate domain placement".into(),
                    ));
                }
            }
            scopes
                .iter()
                .map(|scope| {
                    assigned
                        .get(scope)
                        .copied()
                        .ok_or_else(|| Error::Incompatible("unassigned scope".into()))
                })
                .collect::<Result<_, _>>()?
        } else {
            (0..installations.len()).map(|index| index % used).collect()
        };
        let metrics: Vec<_> = (0..used)
            .map(|_| Arc::new(crate::writer::Metrics::default()))
            .collect();
        let mut domains = Vec::new();
        let mut channels = Vec::new();
        for (index, scope) in scopes.into_iter().enumerate() {
            let (sender, receiver) = mpsc::channel(32);
            let (revisions, revision_receiver) = watch::channel(LocalRevision::default());
            let (changes, change_receiver) = watch::channel(0);
            let nonce = rocky_core::commitment(
                "rocky.runtime.session",
                &[
                    binding.as_slice(),
                    &rocky_platform::entropy::random::<32>(),
                    &scope.domain.0,
                ]
                .concat(),
            );
            domains.push((
                scope,
                Replica {
                    inner: Arc::new(Inner {
                        scope,
                        commands: CommandQueue {
                            sender,
                            bytes: Arc::new(tokio::sync::Semaphore::new(MAX_QUEUE_BYTES)),
                        },
                        revisions: revision_receiver,
                        changes: change_receiver,
                        thread: Mutex::new(None),
                        nonce,
                        submissions: std::sync::atomic::AtomicU64::new(0),
                        observers: Arc::new(tokio::sync::Semaphore::new(MAX_OBSERVERS)),
                        reads: std::sync::OnceLock::new(),
                        metrics: metrics[routes[index]].clone(),
                    }),
                },
            ));
            channels.push((receiver, worker::Notifications { revisions, changes }));
        }
        let (installations, files) = if let Some(placement) = placement {
            crate::placement::install(path, options.files, placement, installations).await?
        } else {
            let (installations, files) =
                crate::layout::install(path, options.files, used, binding, installations).await?;
            (installations, files.into_iter().enumerate().collect())
        };
        let mut assigned: Vec<Vec<_>> = (0..used).map(|_| Vec::new()).collect();
        for (index, domain) in installations.into_iter().zip(channels).enumerate() {
            assigned[routes[index]].push(domain);
        }
        let mut threads = Vec::new();
        let mut readiness = Vec::new();
        let mut failure = None;
        for (index, (owner, raw)) in files {
            let domains = std::mem::take(&mut assigned[index]);
            let (ready, initialized) = oneshot::channel();
            let metrics = metrics[index].clone();
            let spawned = rocky_platform::task::dedicated(
                format!("rocky-writer-{index}"),
                move || async move {
                    let pool = crate::read::ReadPool::new();
                    let setup = async {
                        let conn = Arc::new(raw);
                        let mut workers = Vec::new();
                        let mut reads = Vec::new();
                        for (domain, (receiver, notifications)) in domains {
                            let scoped = ScopedConnection::shared(
                                conn.clone(),
                                Some(&namespace(domain.identity.scope)),
                                &domain.app.installed_schema().statements,
                            );
                            let scope = domain.identity.scope;
                            let worker = worker::Worker::open_connection(
                                scoped,
                                domain,
                                options,
                                notifications,
                                pool.clone(),
                                metrics.clone(),
                            )
                            .await?;
                            reads.push((scope, worker.reads()));
                            workers.push((worker, receiver));
                        }
                        let writer = crate::writer::Writer::new(
                            owner, conn, workers, options, pool, metrics,
                        )?;
                        Ok((writer, reads))
                    }
                    .await;
                    match setup {
                        Ok((writer, reads)) => {
                            let _ = ready.send(Ok(reads));
                            writer.run().await;
                        }
                        Err(error) => {
                            let _ = ready.send(Err(error));
                        }
                    }
                },
            );
            match spawned {
                Ok(thread) => {
                    threads.push(thread);
                    readiness.push(initialized);
                }
                Err(error) => {
                    failure = Some(Error::Io(error));
                    break;
                }
            }
        }
        for initialized in readiness {
            let outcome: Result<Vec<(Scope, Arc<crate::read::Reads>)>, Error> = initialized
                .await
                .map_err(|_| Error::Closed)
                .and_then(|result| result);
            match outcome {
                Ok(reads) => {
                    for (scope, reads) in reads {
                        if let Some((_, domain)) = domains.iter().find(|(key, _)| *key == scope) {
                            let _ = domain.inner.reads.set(reads);
                        }
                    }
                }
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            drop(domains);
            for thread in threads {
                let _ = thread.join().await;
            }
            return Err(error);
        }
        let domains: std::collections::HashMap<_, _> = domains.into_iter().collect();
        Ok(Self { domains, threads })
    }
    pub fn domain(&self, scope: Scope) -> Result<Replica<A>, Error> {
        self.domains
            .get(&scope)
            .cloned()
            .ok_or(Error::Protocol(rocky_core::ProtocolError::ForeignScope))
    }
    pub async fn close(mut self) -> Result<(), Error> {
        let mut failure = None;
        for domain in self.domains.values() {
            match domain.close().await {
                Ok(()) | Err(Error::Closed) => {}
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
            }
        }
        for thread in std::mem::take(&mut self.threads) {
            thread.join().await.map_err(|_| Error::Closed)?;
        }
        failure.map_or(Ok(()), Err)
    }
}
pub(crate) fn namespace(scope: Scope) -> String {
    let bytes = [scope.mesh.0, scope.app.0, scope.domain.0].concat();
    let hash = rocky_core::commitment("rocky.domain.tables.v1", &bytes);
    let mut namespace = String::from("d");
    for byte in hash {
        use std::fmt::Write;
        let _ = write!(namespace, "{byte:02x}");
    }
    namespace
}
pub(crate) fn validate_config(identity: Identity, config: &mut RuntimeConfig) -> Result<(), Error> {
    if config.authors.len() > 64 || config.delivery_targets.len() > 64 {
        return Err(Error::Quota {
            resource: "configured peers",
            limit: 64,
        });
    }
    config.authors.sort();
    config.authors.dedup();
    config.delivery_targets.sort();
    config.delivery_targets.dedup();
    config
        .delivery_targets
        .retain(|node| *node != identity.node);
    config.full_nodes.sort();
    config.full_nodes.dedup();
    if !config.authors.contains(&(identity.node, identity.caller))
        || config
            .delivery_targets
            .iter()
            .any(|node| !config.authors.iter().any(|(author, _)| author == node))
        || config
            .full_nodes
            .iter()
            .any(|node| !config.delivery_targets.contains(node))
    {
        return Err(Error::Incompatible(
            "invalid author or delivery configuration".into(),
        ));
    }
    Ok(())
}
