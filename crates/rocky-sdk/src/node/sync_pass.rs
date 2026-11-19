use super::*;
use crate::{PeerReconciliation, SkipReason};
use std::{collections::BTreeSet, ops::ControlFlow};

const PEER_BATCH: usize = 4;

pub(super) struct PassOutcome {
    pub(super) status: SyncStatus,
    pub(super) retry: bool,
}
impl PassOutcome {
    fn new(status: SyncStatus, retry: bool) -> Self {
        Self { status, retry }
    }
    pub(super) fn idle() -> Self {
        Self::new(SyncStatus::new(SyncState::Idle), false)
    }
    pub(super) fn into_status(self) -> SyncStatus {
        SyncStatus {
            retry: self.retry,
            ..self.status
        }
    }
}

struct Pass<'a> {
    transport: &'a dyn Transport,
    settings: &'a Settings,
    needed: Vec<ResourceScope>,
    report: &'a std::sync::Mutex<Vec<PeerReconciliation>>,
    first_attempt_id: u64,
}

#[derive(Clone, Copy)]
struct RetryStamp {
    generation: u64,
    work_generation: u64,
}

#[derive(Clone)]
struct Target {
    peer: PublicKey,
    contact: Contact,
    stamp: RetryStamp,
}

struct Attempted {
    target: Target,
    result: Result<Exchanged, Failure>,
}

#[derive(Default)]
struct PassProgress {
    covered: BTreeSet<ResourceScope>,
    attempts: Vec<Attempted>,
}
impl PassProgress {
    fn covers(&self, needed: &[ResourceScope]) -> bool {
        !needed.is_empty() && needed.iter().all(|scope| self.covered.contains(scope))
    }
    fn record(&mut self, attempted: Attempted) {
        if let Ok(exchanged) = &attempted.result {
            self.covered.extend(exchanged.transfer.held.iter().copied());
        }
        self.attempts.push(attempted);
    }
    fn renewal_peer(&self, settings: &Settings) -> Option<PublicKey> {
        self.attempts
            .iter()
            .filter(|attempted| {
                fails_beyond_service(&attempted.result)
                    && !attempted.result.as_ref().is_err_and(renewal_cannot_repair)
            })
            .min_by_key(|attempted| !settings.issuers.contains(&attempted.target.peer))
            .map(|attempted| attempted.target.peer)
    }
    fn take_reruns(&mut self, rerun: Rerun, admitted: &BTreeSet<PublicKey>) -> Vec<Target> {
        self.attempts
            .retain(|attempted| admitted.contains(&attempted.target.peer));
        self.covered.clear();
        let targets = self
            .attempts
            .iter()
            .filter(|attempted| rerun.includes(&attempted.result))
            .map(|attempted| attempted.target.clone())
            .collect();
        self.attempts
            .retain(|attempted| !rerun.includes(&attempted.result));
        targets
    }
}

#[derive(Clone, Copy)]
enum Rerun {
    Renewed,
    Learned,
}
impl Rerun {
    fn includes(self, result: &Result<Exchanged, Failure>) -> bool {
        match self {
            Self::Renewed => result.is_ok() || fails_beyond_service(result),
            Self::Learned => learned_statements(result),
        }
    }
}

fn fails_beyond_service(result: &Result<Exchanged, Failure>) -> bool {
    result
        .as_ref()
        .is_err_and(|error| !error.is::<ServiceError>())
}
fn learned_statements(result: &Result<Exchanged, Failure>) -> bool {
    result
        .as_ref()
        .is_err_and(|error| error.is::<sync::Learned>())
}

impl Node {
    pub(super) async fn run_sync_pass(
        &self,
        transport: &dyn Transport,
        automatic: bool,
        retry_only: bool,
        report: &std::sync::Mutex<Vec<PeerReconciliation>>,
        first_attempt_id: u64,
    ) -> Result<PassOutcome, Failure> {
        self.ensure_not_revoked().await?;
        if let ControlFlow::Break(outcome) = self.pass_blocked(transport).await? {
            return Ok(outcome);
        }
        if automatic && retry_only && self.no_retry_is_due().await {
            return Ok(self.unchanged_outcome());
        }
        let _activity = SyncActivity(self.sync_state.clone());
        self.sync_state.send_replace(SyncActivityState::Syncing);
        self.prepare_pass(transport).await?;
        if !automatic
            && let ControlFlow::Break(outcome) = self.saved_contacts_unroutable(transport).await?
        {
            return Ok(outcome);
        }
        let settings = self.settings.lock().await.clone();
        self.renew_when_due(transport, &settings).await?;
        self.renew_for_rotated_keys(transport, &settings).await?;
        self.reset_backoff_on_another_tip().await;
        if !automatic {
            self.pending_sync();
        }
        let dialed = self.dialed(&*self.settings.lock().await, automatic);
        self.replica.configure_peers(settings.kept()).await;
        let initial = self.initial_repair(&dialed, &settings).await?;
        let targets = self.select_targets(dialed, &initial, automatic, retry_only, report);
        if targets.is_empty() {
            return Ok(self.unchanged_outcome());
        }
        let pass = Pass {
            transport,
            settings: &settings,
            needed: self.unpartitioned_scopes(),
            report,
            first_attempt_id,
        };
        let imports = self.replica.observe_imports().await;
        let imports_at_start = *imports.borrow();
        let progress = self.reconcile_targets(&pass, targets).await?;
        let catalog_changed = self.acquire_schemas(transport).await?;
        let schema_changed = self.follow_schema().await?;
        self.adopt_named_coordinator().await;
        self.finalize_seals().await;
        self.close_migrations().await?;
        Ok(self.settle_attempts(
            progress.attempts,
            || *imports.borrow() != imports_at_start,
            catalog_changed || schema_changed,
        ))
    }
    async fn pass_blocked(
        &self,
        transport: &dyn Transport,
    ) -> Result<ControlFlow<PassOutcome>, Failure> {
        if !self.online.load(Ordering::SeqCst) {
            return Ok(ControlFlow::Break(PassOutcome::new(
                SyncStatus::new(SyncState::NetworkingDisabled),
                false,
            )));
        }
        if self.replica.private_schema().await? && !self.relay_schema_publication(transport).await?
        {
            return Ok(ControlFlow::Break(PassOutcome::new(
                SyncStatus::new(SyncState::SchemaPublicationPending),
                true,
            )));
        }
        Ok(ControlFlow::Continue(()))
    }
    async fn no_retry_is_due(&self) -> bool {
        let every_peer_waits = {
            let targets = self.dialed(&*self.settings.lock().await, true);
            let retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
            targets.iter().all(|(peer, _)| retries.contains_key(peer))
                && !retries
                    .values()
                    .any(|state| state.pending.pending() && state.retry.ready())
        };
        every_peer_waits
            && self
                .resolution_retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
            && !self
                .schema_acquisition
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retry
                .ready()
    }
    fn unchanged_outcome(&self) -> PassOutcome {
        PassOutcome::new(
            self.sync_status.borrow().clone(),
            self.retry_deadline().is_some(),
        )
    }
    async fn prepare_pass(&self, transport: &dyn Transport) -> Result<(), Failure> {
        self.adopt_named_coordinator().await;
        let routine = self.seal_routinely().await;
        self.finalize_seals().await;
        let members = self.publish_for_members().await;
        self.seal_skipped(routine.or(members));
        self.close_migrations().await?;
        Box::pin(self.resolve_saved_contacts(transport)).await;
        Ok(())
    }
    async fn saved_contacts_unroutable(
        &self,
        transport: &dyn Transport,
    ) -> Result<ControlFlow<PassOutcome>, Failure> {
        let saved = self.settings.lock().await.clone();
        if !self.dialed(&saved, false).is_empty() || saved.unresolved_contacts().is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        if saved
            .unresolved_contacts()
            .iter()
            .all(|(_, contact)| contact.addresses(transport.kind()).is_empty())
        {
            return Err(ServiceError::NoRoute {
                peer: "a saved peer",
                network: transport.kind().into(),
            }
            .into());
        }
        Ok(ControlFlow::Break(PassOutcome::new(
            SyncStatus::failed(
                SyncState::Unavailable,
                "saved contact resolution produced no route".into(),
                true,
            ),
            true,
        )))
    }
    async fn renew_when_due(
        &self,
        transport: &dyn Transport,
        settings: &Settings,
    ) -> Result<(), Failure> {
        let member = !settings.issuer();
        let renew_to_widen_grants = member
            && settings.profile_widens(&self.domain())?
            && self
                .widened
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(HEXLOWER.encode(&settings.domain()?.manifest().hash()));
        let due =
            renew_to_widen_grants || settings.expires() < now()?.saturating_add(settings.ttl / 2);
        if !due {
            return Ok(());
        }
        let renewed = self.renew(Some(transport), false, None, false).await;
        if let Err(error) = &renewed {
            self.activity.record(crate::ActivityKind::RenewalFailed {
                error: (error.to_string()).to_owned(),
            });
        }
        if !member {
            renewed?;
        } else if renewed.is_err() && settings.expires() <= now()? {
            return Err("credentials expired; an issuer must be reachable to renew".into());
        }
        Ok(())
    }
    async fn renew_for_rotated_keys(
        &self,
        transport: &dyn Transport,
        settings: &Settings,
    ) -> Result<(), Failure> {
        if settings.renews_from_issuer() && self.awaits_rotated_keys().await {
            let _ = self.renew(Some(transport), true, None, true).await;
            self.ensure_not_revoked().await?;
        }
        Ok(())
    }
    async fn reset_backoff_on_another_tip(&self) {
        let tip = self.replica.followed_tip().await;
        let follows_another_tip = self
            .backoff_tip
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(tip)
            .is_some_and(|earlier| earlier != tip);
        if follows_another_tip {
            self.reset_retry(None);
        }
    }
    async fn initial_repair(
        &self,
        dialed: &[(PublicKey, Contact)],
        settings: &Settings,
    ) -> Result<RepairWork, Failure> {
        let meets_a_new_peer = {
            let retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
            dialed.iter().any(|(peer, _)| !retries.contains_key(peer))
        };
        if meets_a_new_peer && self.holds_frames(settings).await? {
            Ok(RepairWork::All)
        } else {
            Ok(RepairWork::Empty)
        }
    }
    async fn holds_frames(&self, settings: &Settings) -> Result<bool, Failure> {
        let node = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let mut after = None;
        loop {
            let (scopes, next) = self
                .replica
                .held_scopes(node, &settings.credentials, after)
                .await?;
            let mut has_frames = false;
            for scope in scopes {
                has_frames |= !self.replica.frontier(scope).await?.logs.is_empty();
            }
            if has_frames || next.is_none() {
                return Ok(has_frames);
            }
            after = next;
        }
    }
    fn select_targets(
        &self,
        dialed: Vec<(PublicKey, Contact)>,
        initial: &RepairWork,
        automatic: bool,
        retry_only: bool,
        report: &std::sync::Mutex<Vec<PeerReconciliation>>,
    ) -> Vec<Target> {
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        retries.retain(|peer, _| dialed.iter().any(|(node, _)| node == peer));
        dialed
            .into_iter()
            .filter_map(|(peer, contact)| {
                let state = retries.entry(peer).or_insert_with(|| PeerRetry {
                    pending: initial.clone(),
                    ..self.peer_retry(peer)
                });
                if let Some(skip) = self.skip_reason(peer, state, automatic, retry_only) {
                    Self::add_peer_report(report, Self::skipped_peer(peer.to_hex(), skip));
                    return None;
                }
                Some(Target {
                    peer,
                    contact,
                    stamp: RetryStamp {
                        generation: state.generation,
                        work_generation: state.work_generation,
                    },
                })
            })
            .collect()
    }
    fn skip_reason(
        &self,
        peer: PublicKey,
        state: &PeerRetry,
        automatic: bool,
        retry_only: bool,
    ) -> Option<SkipReason> {
        if !automatic {
            None
        } else if !state.retry.ready() {
            Some(SkipReason::Backoff)
        } else if retry_only && !state.pending.pending() {
            Some(SkipReason::NoPendingWork)
        } else if self
            .current()
            .is_some_and(|service| service.live.inbound_short(peer))
        {
            Some(SkipReason::InboundReconciliation)
        } else {
            None
        }
    }
    async fn reconcile_targets(
        &self,
        pass: &Pass<'_>,
        targets: Vec<Target>,
    ) -> Result<PassProgress, Failure> {
        let mut progress = PassProgress::default();
        self.reconcile_batches(pass, targets, &mut progress).await?;
        self.ensure_not_revoked().await?;
        if pass.settings.renews_from_issuer()
            && let Some(rerun) = self.renew_after_failures(pass, &progress).await?
        {
            let admitted: BTreeSet<_> = self
                .settings
                .lock()
                .await
                .targets()
                .into_iter()
                .map(|(peer, _)| peer)
                .collect();
            let again = progress.take_reruns(rerun, &admitted);
            self.reconcile_batches(pass, again, &mut progress).await?;
            self.ensure_not_revoked().await?;
        }
        Ok(progress)
    }
    async fn renew_after_failures(
        &self,
        pass: &Pass<'_>,
        progress: &PassProgress,
    ) -> Result<Option<Rerun>, Failure> {
        let Some(peer) = progress.renewal_peer(pass.settings) else {
            return Ok(None);
        };
        let renewed = self
            .renew(Some(pass.transport), true, Some(peer), false)
            .await
            .is_ok();
        self.ensure_not_revoked().await?;
        if renewed {
            Ok(Some(Rerun::Renewed))
        } else if progress
            .attempts
            .iter()
            .any(|attempted| learned_statements(&attempted.result))
        {
            Ok(Some(Rerun::Learned))
        } else {
            Ok(None)
        }
    }
    async fn reconcile_batches(
        &self,
        pass: &Pass<'_>,
        targets: Vec<Target>,
        progress: &mut PassProgress,
    ) -> Result<(), Failure> {
        let local_first = targets.first().is_some_and(|target| {
            pass.settings
                .contact_meta
                .get(&target.peer)
                .is_some_and(|meta| meta.local)
        });
        let mut width = if local_first { 1 } else { PEER_BATCH };
        let mut remaining = targets.into_iter();
        loop {
            let mut batch: Vec<_> = remaining.by_ref().take(width).collect();
            width = PEER_BATCH;
            if batch.is_empty() {
                break;
            }
            if progress.covers(&pass.needed) {
                batch = self.skip_covered(batch.into_iter().chain(remaining.by_ref()), pass.report);
                if batch.is_empty() {
                    break;
                }
            }
            let attempts = stream::iter(batch)
                .map(|target| async move {
                    let result = self
                        .reconcile_and_deliver_epochs(pass.transport, &target)
                        .await;
                    Attempted { target, result }
                })
                .buffer_unordered(PEER_BATCH);
            tokio::pin!(attempts);
            while let Some(attempted) = attempts.next().await {
                self.record_attempt(pass, &attempted)?;
                progress.record(attempted);
            }
        }
        Ok(())
    }
    fn skip_covered(
        &self,
        targets: impl Iterator<Item = Target>,
        report: &std::sync::Mutex<Vec<PeerReconciliation>>,
    ) -> Vec<Target> {
        let inviter = self.awaited_inviter();
        let mut choices = self.choices.lock().unwrap_or_else(PoisonError::into_inner);
        let mut owed = Vec::new();
        for target in targets {
            if Some(target.peer) == inviter {
                owed.push(target);
                continue;
            }
            choices.insert(target.peer, "covered");
            Self::add_peer_report(
                report,
                Self::skipped_peer(target.peer.to_hex(), SkipReason::Covered),
            );
        }
        owed
    }
    async fn reconcile_and_deliver_epochs(
        &self,
        transport: &dyn Transport,
        target: &Target,
    ) -> Result<Exchanged, Failure> {
        let peer = target.peer;
        let result = self.reconcile(transport, peer, &target.contact).await;
        let settings = self.settings.lock().await.clone();
        let sources = settings.epoch_sources()?;
        let owes_epochs = settings.issuer()
            && sources.contains(&peer)
            && !result
                .as_ref()
                .is_err_and(|error| error.is::<ServiceError>());
        if !owes_epochs {
            return result;
        }
        match self.renew(Some(transport), true, Some(peer), true).await {
            Ok(true) => self.reconcile(transport, peer, &target.contact).await,
            Ok(false) => result,
            Err(error) => {
                self.activity
                    .record_changed(crate::ActivityKind::EpochDeliveryFailed {
                        peer: (peer).to_owned(),
                        error: (error.to_string()).to_owned(),
                    });
                result
            }
        }
    }
    fn record_attempt(&self, pass: &Pass<'_>, attempted: &Attempted) -> Result<(), Failure> {
        let peer = attempted.target.peer;
        Self::add_peer_report(
            pass.report,
            self.peer_reconciliation(peer, &attempted.result, pass.first_attempt_id),
        );
        let mut reached = self.reached()?;
        let before = reached.insert(peer, Reached::after(&attempted.result, now()?));
        drop(reached);
        self.record_reach(peer, before, &attempted.result);
        Ok(())
    }
    fn settle_attempts(
        &self,
        attempts: Vec<Attempted>,
        imported_since_start: impl Fn() -> bool,
        schemas_changed: bool,
    ) -> PassOutcome {
        let mut connected = 0;
        let mut first_error = None;
        let mut retry = schemas_changed;
        for Attempted { target, result } in attempts {
            self.settle_retry(&target, &result, imported_since_start());
            match result {
                Ok(exchanged) => {
                    connected += 1;
                    retry |= exchanged.work == PeerWork::Owed;
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) if connected == 0 => PassOutcome::new(
                SyncStatus::failed(SyncState::Unavailable, error.to_string(), true),
                true,
            ),
            first_error => PassOutcome::new(
                SyncStatus {
                    peers: connected,
                    ..SyncStatus::new(SyncState::Connected)
                },
                retry || first_error.is_some(),
            ),
        }
    }
    fn settle_retry(
        &self,
        target: &Target,
        result: &Result<Exchanged, Failure>,
        imported_since_start: bool,
    ) {
        let mut retries = self.retries.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(state) = retries
            .get_mut(&target.peer)
            .filter(|state| state.generation == target.stamp.generation)
        else {
            return;
        };
        match result {
            Ok(Exchanged {
                work: PeerWork::Done,
                transfer,
            }) => {
                state.retry.reset();
                state.link_retry.reset();
                state.finish_repair(
                    transfer
                        .repair_generation
                        .unwrap_or(target.stamp.work_generation),
                );
            }
            Ok(Exchanged {
                work: PeerWork::Owed,
                ..
            }) => {
                if !state.pending.pending() {
                    state.repair_all();
                }
                state.retry.failed();
            }
            Err(_) if imported_since_start => {
                state.repair_all();
                state.retry.soon();
            }
            Err(_) => state.retry.failed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchanged() -> Result<Exchanged, Failure> {
        Ok(Exchanged {
            work: PeerWork::Done,
            transfer: sync::Transfer::default(),
        })
    }
    fn refused_by_service() -> Result<Exchanged, Failure> {
        Err(ServiceError::NetworkRequired("sync").into())
    }
    fn failed() -> Result<Exchanged, Failure> {
        Err("link ended".into())
    }
    fn learned() -> Result<Exchanged, Failure> {
        Err(Box::new(sync::Learned))
    }

    #[test]
    fn a_renewal_reruns_every_peer_except_those_the_service_refused() {
        assert!(Rerun::Renewed.includes(&exchanged()));
        assert!(Rerun::Renewed.includes(&failed()));
        assert!(Rerun::Renewed.includes(&learned()));
        assert!(!Rerun::Renewed.includes(&refused_by_service()));
    }

    #[test]
    fn learned_statements_rerun_only_the_peers_that_sent_them() {
        assert!(Rerun::Learned.includes(&learned()));
        assert!(!Rerun::Learned.includes(&exchanged()));
        assert!(!Rerun::Learned.includes(&failed()));
        assert!(!Rerun::Learned.includes(&refused_by_service()));
    }

    #[test]
    fn no_needed_scope_means_peers_are_never_skipped_as_covered() {
        let scope = ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
            encryption: [4; 32],
            table: [5; 32],
        };
        let mut progress = PassProgress::default();
        assert!(!progress.covers(&[]));
        assert!(!progress.covers(&[scope]));
        progress.covered.insert(scope);
        assert!(!progress.covers(&[]));
        assert!(progress.covers(&[scope]));
    }
}
