use super::*;
use rocky_secure::network::{NativeSession, link::channels::Reconcile};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PeerWork {
    Done,
    Owed,
}

pub(super) struct Exchanged {
    pub(super) work: PeerWork,
    pub(super) transfer: sync::Transfer,
}

enum Bootstrap {
    NotPending { first: Box<NativeSession> },
    Finished,
    Interrupted { importing: Vec<ResourceScope> },
}

impl Node {
    pub(super) async fn exchange(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
        attempt: &mut crate::status::Attempt,
    ) -> Result<Exchanged, Failure> {
        let mut node = peer.to_hex();
        node.truncate(12);
        let span = tracing::info_span!("exchange", peer = node, outcome = tracing::field::Empty);
        let result = tracing::Instrument::instrument(
            self.exchange_inner(transport, peer, contact, attempt),
            span.clone(),
        )
        .await;
        span.record("outcome", if result.is_ok() { "ok" } else { "error" });
        result
    }
    async fn exchange_inner(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        contact: &Contact,
        attempt: &mut crate::status::Attempt,
    ) -> Result<Exchanged, Failure> {
        let service = self
            .current()
            .ok_or(ServiceError::NetworkRequired("sync"))?;
        let revocations = self.replica.revocation_digest().await?;
        let link = self.request_link(transport, peer, contact).await?;
        let repair_generation = self
            .retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .map(|state| state.work_generation);
        attempt.stage(crate::AttemptStage::Reconciliation);
        let _short = service.live.short(peer);
        let renew_from = self
            .settings
            .lock()
            .await
            .renews_from_issuer()
            .then_some(revocations);
        let keep = |scope, signer, cursor| self.keeps_import_source(scope, signer, cursor);
        let control_generation = *self.sync_control.borrow();
        let (control_link, refresh_control) = service.live.begin_control(
            peer,
            control_generation,
            self.selected_repair(peer).is_some(),
        );
        let upstream = Upstream {
            observations: &self.observations,
            link,
            peer,
            contacts: Some(self),
            refresh_control,
            control_started: Default::default(),
            renew_from,
            keep: Some(&keep),
        };
        let reconcile = upstream.link.begin_reconcile().await?;
        let first = match self.deliver(&upstream).await {
            Ok(delivered) => delivered,
            Err(error) => {
                abandon(reconcile, &upstream).await;
                return Err(error);
            }
        };
        let import_slot = self.bootstrap.lock().await;
        let mut transfer = sync::Transfer {
            repair_generation,
            ..Default::default()
        };
        let (first, importing, bootstrap_work) = match self
            .resume_bootstrap(transport, peer, &upstream, first, &mut transfer)
            .await?
        {
            Bootstrap::NotPending { first } => (Some(*first), Vec::new(), PeerWork::Done),
            Bootstrap::Finished => (None, Vec::new(), PeerWork::Done),
            Bootstrap::Interrupted { importing } => (None, importing, PeerWork::Owed),
        };
        self.sync_state.send_replace(SyncActivityState::Syncing);
        let scopes = self.reconciliation_scopes(peer, &importing).await?;
        let reconciled = sync::reconcile(
            &self.replica,
            &upstream,
            &scopes,
            |bootstrap| {
                self.sync_state.send_replace(if bootstrap {
                    SyncActivityState::Bootstrapping
                } else {
                    SyncActivityState::Syncing
                });
            },
            first,
        )
        .await;
        drop(import_slot);
        let (retry, rows) = match reconciled {
            Err(error) => {
                self.observations.record_partial(peer.to_hex(), transfer);
                abandon(reconcile, &upstream).await;
                return Err(error);
            }
            Ok(reconciled) => reconciled,
        };
        transfer.extend(rows);
        let closed = match reconcile.close().await {
            Ok(()) => upstream.link.drain().await,
            Err(error) => Err(error),
        };
        if let Err(error) = closed {
            self.observations.record_partial(peer.to_hex(), transfer);
            return Err(error.into());
        }
        let work = if retry {
            PeerWork::Owed
        } else {
            bootstrap_work
        };
        if work == PeerWork::Done {
            service
                .live
                .finish_control(peer, control_link, control_generation);
        }
        Ok(Exchanged { work, transfer })
    }
    fn selected_repair(
        &self,
        peer: PublicKey,
    ) -> Option<std::collections::BTreeSet<ResourceScope>> {
        self.retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&peer)
            .and_then(|state| state.pending.selected())
            .cloned()
    }
    async fn resume_bootstrap(
        &self,
        transport: &dyn Transport,
        peer: PublicKey,
        upstream: &Upstream<'_>,
        first: NativeSession,
        transfer: &mut sync::Transfer,
    ) -> Result<Bootstrap, Failure> {
        let file = &self.bootstrap_intent;
        if !file.exists() {
            return Ok(Bootstrap::NotPending {
                first: Box::new(first),
            });
        }
        self.sync_state
            .send_replace(SyncActivityState::Bootstrapping);
        self.acquire_schemas(transport).await?;
        let expected = file.load()?.ok_or("the bootstrap intent disappeared")?;
        let pending = self.bootstrap_pending(&expected).await?;
        self.replica
            .set_importing(pending.iter().copied().collect())
            .await;
        self.activity
            .record_changed(crate::ActivityKind::BootstrapStarted {
                peer: (peer).to_owned(),
                scopes: pending.len(),
            });
        let completed = sync::bootstrap(
            &self.replica,
            upstream,
            &pending,
            file,
            expected,
            transfer,
            Some(first),
        )
        .await
        .ok()
        .flatten();
        match completed {
            Some(completed) => {
                self.finish_bootstrap(peer, &completed).await?;
                Ok(Bootstrap::Finished)
            }
            None => Ok(Bootstrap::Interrupted {
                importing: match file.load()? {
                    Some(intent) => self.bootstrap_pending(&intent).await?,
                    None => Vec::new(),
                },
            }),
        }
    }
    async fn finish_bootstrap(
        &self,
        peer: PublicKey,
        completed: &BootstrapIntent,
    ) -> Result<(), Failure> {
        self.replica.flush().await?;
        let finished = self.bootstrap_intent.finish(
            completed,
            peer,
            &self.settings.lock().await.revocations,
        )?;
        self.replica
            .set_importing(std::collections::BTreeSet::new())
            .await;
        if finished == Finished::AwaitingInviter {
            return Ok(());
        }
        self.activity
            .record(crate::ActivityKind::BootstrapFinished {
                peer: (peer).to_owned(),
            });
        Ok(())
    }
    async fn reconciliation_scopes(
        &self,
        peer: PublicKey,
        importing: &[ResourceScope],
    ) -> Result<Vec<ResourceScope>, Failure> {
        let eligible: Vec<_> = self
            .unpartitioned_scopes()
            .into_iter()
            .filter(|scope| !importing.contains(scope))
            .collect();
        match self.selected_repair(peer) {
            Some(selected) => Ok(self
                .replica
                .expand_repair_scopes(&selected.into_iter().collect::<Vec<_>>(), &eligible)
                .await?),
            None => Ok(eligible),
        }
    }
}

async fn abandon(reconcile: Reconcile, upstream: &Upstream<'_>) {
    let _ = reconcile.close().await;
    let _ = upstream.link.drain().await;
}
