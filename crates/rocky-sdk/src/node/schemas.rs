use super::*;
use crate::tip_schemas::Record;
use rocky_core::Commitment;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Acquisition {
    cursor: usize,
    pub retry: crate::scheduling::Retry,
    pairs: BTreeMap<(Commitment, PublicKey), crate::scheduling::Retry>,
}

impl Node {
    pub(super) async fn acquire_schemas(&self, transport: &dyn Transport) -> Result<bool, Failure> {
        let Ok(_active) = self.schema_fetch.try_lock() else {
            return Ok(false);
        };
        let required = self.replica.required_schemas().await?;
        let mut missing = BTreeMap::new();
        for (tip, manifest) in required {
            if self.replica.tip_schema(manifest).await?.is_none() {
                let record = Record::new(
                    &self.declared,
                    self.declared.version,
                    rocky_core::EngineIdentity::current(),
                );
                let settings = self.settings.lock().await.clone();
                if let Ok(domain) = record.domain(&settings, manifest) {
                    self.replica
                        .install_tip_schema(tip, domain.manifest(), record.bytes()?)
                        .await?;
                } else {
                    missing.entry(manifest).or_insert(tip);
                }
            }
        }
        let settings = self.settings.lock().await.clone();
        let providers: Vec<_> = settings
            .targets()
            .into_iter()
            .filter(|(_, contact)| !contact.addresses(transport.kind()).is_empty())
            .collect();
        let work: Vec<_> = missing
            .iter()
            .flat_map(|(manifest, tip)| {
                providers
                    .iter()
                    .map(move |(peer, contact)| (*manifest, *tip, *peer, contact.clone()))
            })
            .collect();
        let deadline = rocky_platform::time::Instant::now() + Duration::from_secs(30);
        let mut authority = self.replica.observe_authority().await;
        let mut attempted = BTreeSet::new();
        let mut learned = false;
        let start = self
            .schema_acquisition
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cursor;
        for offset in 0..work.len() {
            if attempted.len() >= crate::enrollment::MAX_DIALED
                || rocky_platform::time::Instant::now() >= deadline
            {
                break;
            }
            let position = start.wrapping_add(offset) % work.len();
            let (manifest, tip, peer, contact) = &work[position];
            if self.replica.tip_schema(*manifest).await?.is_some()
                || !self
                    .replica
                    .required_schemas()
                    .await?
                    .contains(&(*tip, *manifest))
            {
                continue;
            }
            let pair = (*manifest, *peer);
            {
                let mut state = self
                    .schema_acquisition
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                state.cursor = position + 1;
                if !state.pairs.entry(pair).or_default().ready() {
                    continue;
                }
            }
            if !attempted.insert(pair) {
                continue;
            }
            if !self
                .settings
                .lock()
                .await
                .targets()
                .iter()
                .any(|(node, _)| node == peer)
            {
                continue;
            }
            let request = PeerRequest::TipSchema {
                manifest: *manifest,
            };
            let addresses = contact.addresses(transport.kind());
            let response = tokio::select! {
                biased;
                _ = authority.changed() => None,
                response = rocky_platform::time::timeout_at(deadline, exchange(transport, &addresses, self.domain().scope, &request)) => response.ok().and_then(Result::ok),
            };
            if !self
                .settings
                .lock()
                .await
                .targets()
                .iter()
                .any(|(node, _)| node == peer)
            {
                continue;
            }
            let result = async {
                let Some(PeerReply::TipSchema(Some(bytes))) = response else {
                    return Err("tip schema provider unavailable".into());
                };
                let record = Record::parse(&bytes)?;
                let settings = self.settings.lock().await.clone();
                let domain = record.domain(&settings, *manifest)?;
                if !self
                    .replica
                    .required_schemas()
                    .await?
                    .contains(&(*tip, *manifest))
                {
                    return Ok(false);
                }
                self.replica
                    .install_tip_schema(*tip, domain.manifest(), bytes)
                    .await?;
                Ok::<_, Failure>(true)
            }
            .await;
            let mut state = self
                .schema_acquisition
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match result {
                Ok(changed) => {
                    learned |= changed;
                    state.pairs.remove(&pair);
                }
                Err(error) => {
                    state.pairs.entry(pair).or_default().failed();
                    self.activity
                        .record(crate::activity::ActivityKind::TipSchemaUnavailable {
                            manifest: *manifest,
                            peer: (peer).to_owned(),
                            error: (error.to_string()).to_owned(),
                        });
                }
            }
        }
        let mut unavailable = BTreeSet::new();
        for (_, manifest) in self.replica.required_schemas().await? {
            if self.replica.tip_schema(manifest).await?.is_none() {
                unavailable.insert(manifest);
            }
        }
        let mut state = self
            .schema_acquisition
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        state.pairs.retain(|(manifest, peer), _| {
            unavailable.contains(manifest) && providers.iter().any(|(node, _)| node == peer)
        });
        if unavailable.is_empty() {
            state.retry.reset();
        } else if learned {
            state.retry.soon();
        } else {
            state.retry.failed();
        }
        Ok(learned)
    }
}

impl Node {
    pub(super) async fn follow_schema(&self) -> Result<bool, Failure> {
        let Some(pending) = self.replica.pending_schema().await? else {
            return Ok(false);
        };
        if !self.replica.missing_live_schemas().await?.is_empty() {
            return Ok(false);
        }
        let Some(bytes) = self.replica.tip_schema(pending.manifest).await? else {
            return Ok(false);
        };
        let record = Record::parse(&bytes)?;
        let awaits_engine_upgrade = record.engine != rocky_core::EngineIdentity::current();
        if awaits_engine_upgrade {
            return Ok(false);
        }
        let mut settings = self.settings.lock().await;
        let domain = record.domain(&settings, pending.manifest)?;
        let mut intent = (**settings).clone();
        intent.pending = Some(crate::tip_schemas::Pending {
            tip: pending.tip,
            manifest: pending.manifest,
        });
        settings
            .commit(Shared::new(intent.clone()), &self.replica)
            .await?;
        let mut draft = intent;
        draft.schema = record.schema;
        draft.generation = record.generation;
        draft.engine = record.engine;
        draft.prepare_schema(&self.vault, &domain)?;
        self.replica
            .switch_schema(
                pending.tip,
                domain.app.clone(),
                rocky_secure::SchemaConfig {
                    security: draft.config(&self.vault, &domain)?,
                    tables: Some((domain.logical().clone(), domain.scope, domain.readers())),
                },
            )
            .await?;
        crate::database::attach(&self.replica, &mut draft, &self.vault, &domain).await?;
        crate::database::retain(&self.replica, &draft, &domain).await?;
        draft.pending = None;
        settings.commit(Shared::new(draft), &self.replica).await?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = settings.granted(&domain);
        *self.domain.write().map_err(|_| "domain catalog poisoned")? = Arc::new(domain);
        drop(settings);
        if let Err(error) = self.refresh_tip_node().await {
            self.activity
                .record_changed(crate::activity::ActivityKind::LineNodeFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
        self.observations.touch();
        self.wake_sync();
        Ok(true)
    }
    pub(super) async fn enrollment_bootstrap(
        &self,
    ) -> Result<crate::tip_schemas::Bootstrap, Failure> {
        if self.replica.private_schema().await? {
            return Err("schema publication is private".into());
        }
        Ok(crate::tip_schemas::Bootstrap {
            graph: self.replica.tip_graph(self.domain().scope).await?,
            followed: self.replica.followed_bookmark().await.map(|(_, name)| name),
        })
    }
}
