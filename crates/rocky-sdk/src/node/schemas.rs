//! Bounded exact catalog acquisition, independent of the provider's followed tip.
use super::*;
use crate::tip_schemas::{Record, Request};
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
                let record = Record::new(&self.declared, self.declared.version);
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
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
                || tokio::time::Instant::now() >= deadline
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
            let request = [
                crate::tip_schemas::PROTOCOL,
                &serde_json::to_vec(&Request {
                    manifest: *manifest,
                })?,
            ]
            .concat();
            let addresses = contact.addresses(transport.kind());
            let response = tokio::select! {
                biased;
                _ = authority.changed() => None,
                response = tokio::time::timeout_at(deadline, exchange(transport, &addresses, Protocol::Renew, &request)) => response.ok().and_then(Result::ok),
            };
            // Authority changes cancel an in-flight request. Its bytes grant nothing.
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
                let Some(bytes) = response else {
                    return Err("tip schema provider unavailable".into());
                };
                if bytes == b"null" {
                    return Err("tip schema absent".into());
                }
                let record = Record::parse(&bytes)?;
                let settings = self.settings.lock().await.clone();
                let domain = record.domain(&settings, *manifest)?;
                // The graph can change while dialing. The replica rechecks the
                // exact required pair in the transaction that saves the record.
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
                    self.activity.record("tip_schema_unavailable", json!({"manifest": HEXLOWER.encode(manifest), "peer": peer.to_hex(), "error": error.to_string()}));
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
    pub(super) async fn relay_schema_publication(
        &self,
        transport: &dyn Transport,
    ) -> Result<bool, Failure> {
        let Some((stage, committed)) = self.replica.staged_schema().await? else {
            return Ok(true);
        };
        if !committed {
            return Ok(false);
        }
        let close = stage
            .statements
            .iter()
            .find_map(|statement| match statement {
                rocky_secure::TipStatement::Seal(seal) => Some(seal),
                _ => None,
            })
            .ok_or("staged schema has no closing seal")?;
        let bytes = close.to_bytes()?;
        let request = [b"rocky.sdk.staged-close.v1\n".as_slice(), bytes.as_slice()].concat();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut authority = self.replica.observe_authority().await;
        let providers = self.settings.lock().await.targets();
        for (peer, contact) in providers.into_iter().take(crate::enrollment::MAX_DIALED) {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let addresses = contact.addresses(transport.kind());
            if addresses.is_empty() {
                continue;
            }
            let response = tokio::select! {
                biased;
                _ = authority.changed() => None,
                reply = tokio::time::timeout_at(deadline, exchange(transport, &addresses, Protocol::Renew, &request)) => reply.ok().and_then(Result::ok),
            };
            let Some(response) = response else { continue };
            let settings = self.settings.lock().await.clone();
            if !settings.targets().iter().any(|(node, _)| *node == peer) {
                continue;
            }
            let Ok(outcome) = serde_json::from_slice::<rocky_secure::StagedCloseStatus>(&response)
            else {
                continue;
            };
            match outcome {
                rocky_secure::StagedCloseStatus::Finality(proof) => {
                    if proof.seal != close.id()? {
                        continue;
                    }
                    let graph = self.replica.tip_graph(stage.scope).await?;
                    if graph
                        .verify_finality(
                            &proof,
                            settings.publisher,
                            Some((&settings.revocations, now()?)),
                        )
                        .is_err()
                    {
                        continue;
                    }
                    self.replica
                        .release_staged_schema(stage.publication, proof)
                        .await?;
                    self.schema_acquisition
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .retry
                        .reset();
                    self.topics_changed();
                    return Ok(true);
                }
                rocky_secure::StagedCloseStatus::Conflict { seal, finality } => {
                    let mut graph = self.replica.tip_graph(stage.scope).await?;
                    if seal.statement.scope != stage.scope
                        || seal.statement.tip != stage.source
                        || seal.id()? == close.id()?
                    {
                        continue;
                    }
                    if seal
                        .verify_current(settings.publisher, &graph, &settings.revocations, now()?)
                        .is_err()
                    {
                        continue;
                    }
                    graph.seals.push(*seal);
                    if graph
                        .verify_finality(
                            &finality,
                            settings.publisher,
                            Some((&settings.revocations, now()?)),
                        )
                        .is_ok()
                    {
                        self.activity.record("schema_publication_conflict", json!({"publication": HEXLOWER.encode(&stage.publication), "terminal": HEXLOWER.encode(&finality.seal)}));
                    }
                }
                rocky_secure::StagedCloseStatus::Pending => {}
            }
        }
        self.schema_acquisition
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retry
            .failed();
        Ok(false)
    }

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
        let mut settings = self.settings.lock().await;
        let domain = record.domain(&settings, pending.manifest)?;
        let mut intent = (**settings).clone();
        intent.pending = Some(crate::tip_schemas::Pending {
            tip: pending.tip,
            manifest: pending.manifest,
        });
        intent.save(&self.path)?;
        *settings = Shared::new(intent.clone());
        let mut draft = intent;
        draft.schema = record.schema;
        draft.generation = record.generation;
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
        crate::database::attach(&self.replica, &mut draft, &self.vault, &domain, &self.path)
            .await?;
        crate::database::retain(&self.replica, &draft, &domain).await?;
        draft.pending = None;
        draft.save(&self.path)?;
        *self.granted.lock().map_err(|_| "scope catalog poisoned")? = draft.granted(&domain);
        *self.domain.write().map_err(|_| "domain catalog poisoned")? = Arc::new(domain);
        *settings = Shared::new(draft);
        drop(settings);
        if let Err(error) = self.refresh_tip_node().await {
            self.activity
                .record_changed("line_node_failed", json!({"error": error.to_string()}));
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
