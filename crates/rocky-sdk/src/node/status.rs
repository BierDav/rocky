use super::*;
use crate::status::*;

impl Node {
    pub(super) fn status_time(&self) -> Result<u64, Failure> {
        #[cfg(test)]
        if let Some((time, anchor)) = *self
            .status_clock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Ok(time.saturating_add(anchor.elapsed().as_secs()));
        }
        now()
    }
    pub(super) fn observed_scope(&self, scope: ResourceScope) -> crate::ScopeStatus {
        let domain = self.domain();
        let root = domain.scopes().find(|(_, root)| root.table == scope.table);
        crate::ScopeStatus {
            table: root.map(|(name, _)| name.to_owned()),
            table_id: HEXLOWER.encode(&scope.table),
            encryption: HEXLOWER.encode(&scope.encryption),
            partition: root.is_some_and(|(_, root)| root != scope),
        }
    }
    pub(super) async fn observed_peers(&self) -> Result<Vec<crate::Peer>, Failure> {
        let observations = self.observations.snapshot();
        let mut peers = Vec::new();
        for value in self.peers().await {
            let node = value["node"]
                .as_str()
                .ok_or("missing peer node")?
                .to_owned();
            let link = &value["link"];
            fn count(value: &Value, key: &str) -> Result<u64, Failure> {
                Ok(value[key].as_str().ok_or("missing path count")?.parse()?)
            }
            let paths = link["paths"]
                .as_array()
                .ok_or("missing paths")?
                .iter()
                .map(|path| {
                    Ok(crate::LinkPath {
                        selected: path["selected"].as_bool().ok_or("missing selected path")?,
                        kind: path["kind"].as_str().ok_or("missing path kind")?.into(),
                        remote: path["remote"].as_str().ok_or("missing remote path")?.into(),
                        local: path["local"].as_str().map(str::to_owned),
                        round_trip_micros: count(path, "rtt_us")?,
                        mtu: u32::try_from(count(path, "mtu")?)?,
                        window: count(path, "window")?,
                        sent_bytes: count(path, "sent_bytes")?,
                        sent_datagrams: count(path, "sent_datagrams")?,
                        received_bytes: count(path, "received_bytes")?,
                        received_datagrams: count(path, "received_datagrams")?,
                        lost_packets: count(path, "lost_packets")?,
                        lost_bytes: count(path, "lost_bytes")?,
                        congestion_events: count(path, "congestion_events")?,
                    })
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            let key: PublicKey = node.parse()?;
            let retry = self
                .retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&key)
                .map(|retry| RetryStatus {
                    pending: retry.pending,
                    reconciliation_retry_at_epoch_millis: retry.retry.deadline_epoch_millis(),
                    link_retry_at_epoch_millis: retry.link_retry.deadline_epoch_millis(),
                });
            peers.push(crate::Peer {
                node: node.clone(),
                is_issuer: value["issuer"].as_bool().ok_or("missing issuer")?,
                is_configured: value["configured"]
                    .as_bool()
                    .ok_or("missing configured peer")?,
                source: value["source"]
                    .as_str()
                    .ok_or("missing peer source")?
                    .into(),
                choice: value["choice"]
                    .as_str()
                    .ok_or("missing peer choice")?
                    .into(),
                routes: serde_json::from_value(value["routes"].clone())?,
                is_local: value["local"].as_bool().ok_or("missing locality")?,
                link: PeerLink {
                    state: serde_json::from_value(link["state"].clone())?,
                    opened_at_epoch_seconds: link["opened"].as_str().map(str::parse).transpose()?,
                    round_trip_millis: link["round_trip_ms"]
                        .as_str()
                        .map(str::parse)
                        .transpose()?,
                    paths,
                },
                attempts: observations
                    .iter()
                    .filter(|attempt| attempt.target == node)
                    .cloned()
                    .collect(),
                last_authenticated_exchange_at_epoch_seconds: self
                    .met
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&key)
                    .copied(),
                retry,
            });
        }
        Ok(peers)
    }
    pub(super) async fn observed_network(&self) -> Result<crate::NetworkStatus, Failure> {
        let service = self.current();
        let discovery = self.discovery_status();
        let path = |name: &str| -> Result<DiscoveryPath, Failure> {
            let path = &discovery["paths"][name];
            Ok(DiscoveryPath {
                state: serde_json::from_value(path["state"].clone())?,
                reason: path["reason"].as_str().map(str::to_owned),
            })
        };
        let peers = self.observed_peers().await?;
        let connected_peer_count = peers
            .iter()
            .filter(|peer| peer.link.state == LinkState::Open)
            .count() as u64;
        Ok(crate::NetworkStatus {
            attachment: match &service {
                None => AttachmentState::Detached,
                Some(service) if *service.stage.borrow() != Stage::Open => {
                    AttachmentState::Draining
                }
                Some(_) => AttachmentState::Attached,
            },
            kind: service
                .as_ref()
                .map(|service| service.transport.kind().into()),
            ownership: service.as_ref().map(|service| {
                if service.owned {
                    NetworkOwnership::Rocky
                } else {
                    NetworkOwnership::Application
                }
            }),
            networking_enabled: self.online.load(Ordering::SeqCst),
            foreground: self.foreground.load(Ordering::SeqCst),
            worker_enabled: self.automatic.load(Ordering::SeqCst),
            worker_running: *self.worker_running.borrow(),
            reconciling: *self.sync_state.borrow() != "idle",
            inbound: service.as_ref().map(|service| InboundStatus {
                active: (INBOUND - service.active.available_permits()) as u64,
                limit: INBOUND as u64,
                served: service.served.load(Ordering::Relaxed),
                refused: service.refused.load(Ordering::Relaxed),
            }),
            peers,
            connected_peer_count,
            contact_resolutions: self
                .observations
                .snapshot()
                .into_iter()
                .filter(|attempt| attempt.operation == AttemptOperation::ResolveContact)
                .collect(),
            discovery: DiscoveryStatus {
                announcing: discovery["announcing"]
                    .as_bool()
                    .ok_or("missing announcement state")?,
                local: path("local")?,
                topic: path("topic")?,
            },
            last_reconciliation: self
                .last_reconciliation
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            relay: RelayObservation {
                initial_configuration: service
                    .as_ref()
                    .and_then(|service| service.transport.initial_relay_configuration()),
                relays: service
                    .as_ref()
                    .and_then(|service| service.transport.relays())
                    .map(|relays| {
                        relays
                            .into_iter()
                            .map(|relay| crate::RelayStatus {
                                url: relay.url,
                                connected: relay.connected,
                                last_error: relay.last_error,
                                auth_denied_reason: relay.auth_denied_reason,
                            })
                            .collect()
                    }),
            },
        })
    }
    pub(super) async fn observed_status(&self) -> Result<DatabaseStatus, Failure> {
        let settings = self.settings.lock().await.clone();
        let generation = self.replica.generations().await?;
        let mut imports = Vec::new();
        for scope in self.scopes() {
            let staging = self.replica.rebootstrap_progress(scope).await?;
            imports.push(ScopeImportStatus {
                scope: self.observed_scope(scope),
                horizon_rebootstrap_required: self
                    .replica
                    .horizon_rebootstrap_required(scope)
                    .await?,
                checkpoint: staging.map(|(id, _, _)| HEXLOWER.encode(&id)),
                cursor: staging
                    .and_then(|(_, cursor, _)| cursor.map(|cursor| HEXLOWER.encode(&cursor))),
                pages_complete: staging.map(|(_, _, complete)| complete),
            });
        }
        let bootstrap_pending = self.path.with_file_name("bootstrap.json").exists()
            || imports
                .iter()
                .any(|scope| scope.horizon_rebootstrap_required || scope.checkpoint.is_some());
        let renewal_routes = settings.renewal_routes();
        let profile = serde_json::to_value(&settings.profile)?;
        let mut status = DatabaseStatus {
            token: String::new(),
            observed_at_epoch_millis: epoch_millis(),
            authority: AuthorityStatus {
                principal: settings.identity.to_hex(),
                node: self
                    .node_identity
                    .clone()
                    .map_err(|error| -> Failure { error.into() })?,
                is_issuer: settings.issuer(),
                expires_at_epoch_seconds: settings.expires(),
                expired: settings.expires() <= self.status_time()?,
                logged_out: self.ended.lock().await.is_some() || settings.logged_out,
                verified_revocation: settings
                    .revocation(&self.path)?
                    .map(|notice| {
                        Ok::<_, Failure>(VerifiedRevocation {
                            id: HEXLOWER.encode(&notice.id()?),
                            statement: serde_json::to_vec(&notice)?,
                        })
                    })
                    .transpose()?,
                revocations: serde_json::from_value(settings.revocation_status()?)?,
                profile: AuthorityProfile {
                    kind: profile["kind"]
                        .as_str()
                        .ok_or("missing grant profile kind")?
                        .into(),
                    scopes: profile
                        .get("scopes")
                        .map(|scopes| serde_json::from_value(scopes.clone()))
                        .transpose()?
                        .unwrap_or_default(),
                    grants: profile
                        .get("grants")
                        .map(|grants| serde_json::from_value(grants.clone()))
                        .transpose()?
                        .unwrap_or_default(),
                },
                renewal_routes,
            },
            data: DataStatus {
                revision: *self.replica.observe_revision().borrow(),
                generation: crate::GenerationStatus {
                    manifest: HEXLOWER.encode(&generation.manifest),
                    current: crate::Generation {
                        schema: generation.current.schema,
                    },
                    previous: generation
                        .previous
                        .into_iter()
                        .map(|(generation, manifest)| GenerationRecord {
                            generation: crate::Generation {
                                schema: generation.schema,
                            },
                            manifest: HEXLOWER.encode(&manifest),
                        })
                        .collect(),
                    baseline: generation.baseline,
                    application: self.application(&settings)?,
                    migration: self.migration(&settings).await?,
                    waiting_for_issuer: !settings.issuer()
                        && self.application(&settings)? == crate::DeclaredSchema::Ahead,
                    rebuilding: generation.plan.map(|plan| RebuildProgress {
                        schema: HEXLOWER.encode(&plan.schema),
                        filled: plan.filled as u64,
                        tables: plan.tables as u64,
                    }),
                },
                bootstrap_pending,
                bootstrap: serde_json::from_value(self.bootstrap_progress().await?)?,
                imports,
                key_epochs: settings
                    .keys
                    .iter()
                    .map(|(collection, epochs)| KeyEpochStatus {
                        collection: collection.clone(),
                        epochs: epochs
                            .iter()
                            .map(|epoch| EncryptionEpoch {
                                generation: epoch.generation,
                                secret_hash: HEXLOWER.encode(&epoch.secret_hash),
                            })
                            .collect(),
                        current: epochs.iter().max().map(|epoch| EncryptionEpoch {
                            generation: epoch.generation,
                            secret_hash: HEXLOWER.encode(&epoch.secret_hash),
                        }),
                    })
                    .collect(),
            },
            network: self.observed_network().await?,
            coordination: CoordinationStatus {
                mode: if self.service.is_some() {
                    CoordinationMode::Service
                } else {
                    CoordinationMode::NotConfigured
                },
                attempts: self
                    .observations
                    .snapshot()
                    .into_iter()
                    .filter(|attempt| attempt.target == "coordinator")
                    .collect(),
            },
        };
        let mut token_status = status.clone();
        token_status.observed_at_epoch_millis = None;
        for peer in &mut token_status.network.peers {
            peer.link.paths.clear();
            peer.link.round_trip_millis = None;
        }
        status.token = HEXLOWER
            .encode(&commitment("rocky.sdk.status.v2", &serde_json::to_vec(&token_status)?)[..16]);
        Ok(status)
    }
}

impl Node {
    pub(super) fn skipped_peer(node: String, skip: SkipReason) -> PeerReconciliation {
        PeerReconciliation {
            node,
            skip: Some(skip),
            attempts: Vec::new(),
            transfers: Vec::new(),
            refused: Vec::new(),
            waiting: Vec::new(),
            upgrade_required: false,
            retry_pending: false,
        }
    }
    pub(super) fn peer_reconciliation(
        &self,
        peer: PublicKey,
        result: &Result<(bool, sync::Transfer), Failure>,
        first_attempt_id: u64,
    ) -> PeerReconciliation {
        let node = peer.to_hex();
        let mut report = PeerReconciliation {
            node: node.clone(),
            skip: None,
            attempts: self
                .observations
                .snapshot()
                .into_iter()
                .filter(|attempt| attempt.target == node && attempt.id >= first_attempt_id)
                .collect(),
            transfers: Vec::new(),
            refused: Vec::new(),
            waiting: Vec::new(),
            upgrade_required: false,
            retry_pending: result.is_err(),
        };
        let partial = self.observations.partial_transfer(&node);
        let transfer = match result {
            Ok((retry, transfer)) => {
                report.retry_pending = *retry;
                Some(transfer)
            }
            Err(_) => partial.as_ref(),
        };
        if let Some(transfer) = transfer {
            report.refused = transfer
                .refused
                .iter()
                .map(|scope| self.observed_scope(*scope))
                .collect();
            report.waiting = transfer
                .waiting
                .iter()
                .map(|scope| self.observed_scope(*scope))
                .collect();
            report.transfers = transfer
                .scopes
                .iter()
                .map(|(scope, moved)| ScopeReconciliationTransfer {
                    scope: self.observed_scope(*scope),
                    pulled: moved.pulled as u64,
                    pushed: moved.pushed as u64,
                    bytes: moved.bytes as u64,
                    checkpoint: moved.checkpoint.map(|(id, _)| HEXLOWER.encode(&id)),
                    superseded: moved.checkpoint.map(|(_, rows)| rows),
                    finalized: moved.finalized,
                })
                .collect();
        }
        report
    }
    pub(super) async fn reconciliation_pass(
        &self,
        transport: Option<&dyn Transport>,
        automatic: bool,
        retry_only: bool,
    ) -> Result<(ReconciliationReport, Value, bool), Failure> {
        let _syncing = self.syncing.lock().await;
        if let Some(statement) = self.settings.lock().await.revocation(&self.path)? {
            return Err(Box::new(crate::enrollment::Revoked(statement)));
        }
        let started_at_epoch_millis = epoch_millis();
        let first_attempt_id = self.observations.next_id();
        let peers = std::sync::Mutex::new(Vec::new());
        let skip = if !self.online.load(Ordering::SeqCst) {
            Some(SkipReason::NetworkingDisabled)
        } else if !self.foreground.load(Ordering::SeqCst) {
            Some(SkipReason::Suspended)
        } else {
            None
        };
        let result = if skip.is_some() {
            Ok((json!({"state":"idle"}), false))
        } else {
            self.run_sync_pass(
                transport.ok_or(ServiceError::NetworkRequired("sync"))?,
                automatic,
                retry_only,
                &peers,
                first_attempt_id,
            )
            .await
        };
        // A received move may have switched this device to another line.
        if result.is_ok() {
            self.try_ensure_tip_node().await;
        }
        let peers = peers.into_inner().unwrap_or_else(PoisonError::into_inner);
        let report = ReconciliationReport {
            contact_resolutions: self
                .observations
                .snapshot()
                .into_iter()
                .filter(|attempt| {
                    attempt.operation == AttemptOperation::ResolveContact
                        && attempt.id >= first_attempt_id
                })
                .collect(),
            started_at_epoch_millis,
            ended_at_epoch_millis: epoch_millis(),
            skip,
            peers,
        };
        *self
            .last_reconciliation
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(report.clone());
        self.observations.touch();
        result.map(|(status, retry)| (report, status, retry))
    }
}

impl Node {
    pub(super) fn add_peer_report(
        reports: &std::sync::Mutex<Vec<PeerReconciliation>>,
        mut next: PeerReconciliation,
    ) {
        let mut reports = reports.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(prior) = reports.iter_mut().find(|prior| prior.node == next.node) {
            if next.skip.is_some() {
                return;
            }
            prior.skip = None;
            for attempt in next.attempts.drain(..) {
                if !prior.attempts.iter().any(|old| old.id == attempt.id) {
                    prior.attempts.push(attempt);
                }
            }
            prior.transfers.extend(next.transfers);
            prior.refused = next.refused;
            prior.waiting = next.waiting;
            prior.retry_pending = next.retry_pending;
        } else {
            reports.push(next);
        }
    }
}
