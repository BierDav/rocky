//! Synchronization with one authorized peer: a device pushes and pulls
//! every collection over request channels of its live link with that peer.
//! See `docs/runtime/live-links.md`.
use crate::Failure;
use rocky_core::{ResourceScope, TypedRows};
use rocky_secure::{
    EncryptedReplica, MAX_SECURE_FRAME, PublicKey,
    network::{NativeSession, link::channels::Handle},
};

/// Bytes of frames per page.
const QUOTA: usize = MAX_SECURE_FRAME / 2;
/// Request channels one sync may open when each exhausts its byte budget.
const SESSIONS: usize = 16;

/// One device's view of a peer: the link that proved the `peer` key.
pub(crate) struct Upstream<'a> {
    pub link: Handle,
    pub observations: &'a crate::status::Observations,
    pub peer: PublicKey,
    pub contacts: Option<&'a dyn rocky_secure::network::ContactHandler>,
    /// The revocation digest this sync started from, when it renews: a
    /// session that changes the set ends the sync with `Learned`.
    pub renew_from: Option<rocky_core::Commitment>,
    /// Failed optional imports in this process, by exact scope and checkpoint.
    pub state_failures: &'a std::sync::Mutex<
        std::collections::BTreeMap<(ResourceScope, rocky_core::Commitment), u8>,
    >,
    /// Whether an active staged import should continue with its original signer.
    pub keep: Option<&'a dyn Fn(ResourceScope, PublicKey, Option<rocky_core::Commitment>) -> bool>,
}

/// A session brought revocation statements. Rotated keys still arrive by
/// renewal, so the sync renews before it moves rows.
#[derive(Debug)]
pub(crate) struct Learned;
impl std::fmt::Display for Learned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the peer sent revocation statements; renewing before sync")
    }
}
impl std::error::Error for Learned {}

/// What one scope exchanged with a peer during a sync.
#[derive(Clone, Debug, Default)]
pub(crate) struct Moved {
    pub pulled: usize,
    pub pushed: usize,
    /// Encoded bytes received.
    pub bytes: usize,
    pub sent: usize,
    /// The checkpoint a rebootstrap imported, with the held operations it superseded.
    pub checkpoint: Option<(rocky_core::Commitment, u64)>,
}
impl Moved {
    pub fn is_empty(&self) -> bool {
        self.pulled == 0 && self.pushed == 0 && self.checkpoint.is_none()
    }
    fn merge(&mut self, other: Self) {
        self.pulled += other.pulled;
        self.pushed += other.pushed;
        self.bytes += other.bytes;
        self.sent += other.sent;
        self.checkpoint = other.checkpoint.or(self.checkpoint);
    }
}
/// What a reconcile did, by scope, in first-change order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Transfer {
    pub scopes: Vec<(ResourceScope, Moved)>,
    /// Scopes the peer and this device did not exchange: one side's
    /// credentials, as the other sees them, grant no whole-scope access.
    pub refused: Vec<ResourceScope>,
    pub held: Vec<ResourceScope>,
    /// Scopes the peer restores from a checkpoint, as it is importing them
    /// or behind this device's pruned prefix: no history was pushed.
    pub waiting: Vec<ResourceScope>,
    /// Scopes whose requests failed; other scopes still finish the pass.
    pub failed: Vec<ResourceScope>,
}
impl Transfer {
    pub fn extend(&mut self, other: Self) {
        for scope in other.failed {
            self.fail(scope);
        }
        for (scope, moved) in other.scopes {
            self.add(scope, moved);
        }
        self.refuse(&other.refused);
        self.wait(&other.waiting);
        for scope in other.held {
            if !self.held.contains(&scope) {
                self.held.push(scope);
            }
        }
    }
    fn request_failed(&mut self, scope: ResourceScope, error: &Failure) {
        let refused = matches!(error.downcast_ref::<rocky_secure::Error>(),
            Some(rocky_secure::Error::Application(cause))
                if cause.downcast_ref::<rocky_secure::network::RequestRefusal>()
                    == Some(&rocky_secure::network::RequestRefusal::NotPermitted));
        if refused {
            self.refuse(&[scope]);
        } else {
            self.fail(scope);
        }
    }
    fn fail(&mut self, scope: ResourceScope) {
        if !self.failed.contains(&scope) {
            self.failed.push(scope);
        }
    }
    fn refuse(&mut self, scopes: &[ResourceScope]) {
        for scope in scopes {
            if !self.refused.contains(scope) {
                self.refused.push(*scope);
            }
        }
    }
    fn wait(&mut self, scopes: &[ResourceScope]) {
        for scope in scopes {
            if !self.waiting.contains(scope) {
                self.waiting.push(*scope);
            }
        }
    }
    fn add(&mut self, scope: ResourceScope, moved: Moved) {
        if moved.is_empty() {
            return;
        }
        match self.scopes.iter_mut().find(|(known, _)| *known == scope) {
            Some((_, held)) => held.merge(moved),
            None => self.scopes.push((scope, moved)),
        }
    }
}

/// Pushes what the peer lacks and pulls what this device lacks, for every
/// scope, until both hold the same frames.
/// Returns whether another pass is needed and what moved. Frames outside
/// the followed tip return an admission error and never trigger adoption.
/// `first` is a session already open with the peer, which rows use first.
pub(crate) async fn reconcile(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    progress: impl Fn(bool),
    first: Option<NativeSession>,
) -> Result<(bool, Transfer), Failure> {
    let mut transfer = Transfer::default();
    let rows_result = Box::pin(rows(
        replica,
        upstream,
        scopes,
        progress,
        &mut transfer,
        first,
    ))
    .await;
    if let Err(error) = rows_result {
        upstream
            .observations
            .record_partial(upstream.peer.to_hex(), transfer);
        return Err(error);
    }
    Ok((!transfer.failed.is_empty(), transfer))
}
/// One scope of a bootstrap: the checkpoint its peer offered and the
/// stored rows it holds, which progress counts.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Planned {
    pub scope: ResourceScope,
    pub checkpoint: rocky_core::Commitment,
    pub rows: u64,
}
/// Checkpoint-first enrollment, resumed from the runtime's staged pages.
/// The peer's offers come first: populated scopes import smallest first,
/// followed by empty scopes. The intent records each scope's rows for progress.
/// A scope the peer offers no checkpoint of joins by frames. `None` when an
/// offered import cannot start with this peer: the intent waits for another.
pub(crate) async fn bootstrap(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    marker: &std::path::Path,
    mut expected: Vec<u8>,
    transfer: &mut Transfer,
    mut first: Option<NativeSession>,
) -> Result<Option<Vec<u8>>, Failure> {
    let saved: serde_json::Value = serde_json::from_slice(&expected)?;
    let intent = saved.get("intent").unwrap_or(&saved).clone();
    let mut pending = scopes.to_vec();
    let mut totals: Vec<Planned> = saved
        .get("totals")
        .map(|totals| serde_json::from_value(totals.clone()))
        .transpose()?
        .unwrap_or_default();
    let mut planned = Vec::new();
    let mut tips_synced = false;
    for scope in scopes {
        let offer = async {
            let mut planning = match first.take() {
                Some(session) => session,
                None => session(replica, upstream).await?,
            };
            if !tips_synced {
                planning.sync_tip_graph(replica).await?;
                tips_synced = true;
            }
            if !planning.follows_same_tip(replica).await {
                return Ok::<_, Failure>(None);
            }
            let offer = planning
                .offered_rows(replica, &[*scope])
                .await?
                .into_iter()
                .next()
                .flatten();
            first = Some(planning);
            Ok(Some(offer))
        }
        .await;
        match offer {
            Err(error) => transfer.request_failed(*scope, &error),
            Ok(None) => {}
            Ok(Some(offer)) => {
                if deferred(
                    replica,
                    upstream,
                    *scope,
                    offer.map(|(checkpoint, _)| checkpoint),
                )
                .await?
                {
                    continue;
                }
                if let Some((checkpoint, rows)) = offer {
                    let plan = Planned {
                        scope: *scope,
                        checkpoint,
                        rows,
                    };
                    totals.retain(|old| old.scope != *scope);
                    totals.push(plan.clone());
                    planned.push(plan);
                } else if replica.rebootstrap_progress(*scope).await?.is_none() {
                    // The peer has never compacted this scope: it joins by frames.
                    pending.retain(|known| known != scope);
                }
            }
        }
    }
    planned.sort_by_key(|plan| (plan.rows == 0, plan.rows));
    let record = |pending: &[ResourceScope], expected: &mut Vec<u8>| -> Result<(), Failure> {
        if std::fs::read(marker)? != *expected {
            return Err("bootstrap authority changed; retry with the new intent".into());
        }
        let mut next = serde_json::json!({"intent": intent, "pending": pending, "totals": totals});
        if let Some(inviter) = saved.get("inviter") {
            next["inviter"] = inviter.clone();
        }
        *expected = serde_json::to_vec(&next)?;
        rocky_credentials::write_private(marker, expected)?;
        Ok(())
    };
    record(&pending, &mut expected)?;
    replica
        .set_importing(pending.iter().copied().collect())
        .await;
    use futures_util::{StreamExt, stream};
    let mut imports = stream::iter(planned.into_iter().map(|plan| {
        let mut first = first.take();
        async move {
            let scope = plan.scope;
            let mut moved = Transfer::default();
            let imported = async {
                let mut imported = false;
                for _ in 0..SESSIONS {
                    let mut session = match first.take() {
                        Some(session) => session,
                        None => session(replica, upstream).await?,
                    };
                    let (catch_up, checkpoint) = if imported {
                        (
                            session.sync_scope(replica, scope, QUOTA).await?.catch_up,
                            None,
                        )
                    } else {
                        let report = session
                            .bootstrap_scope(replica, scope, QUOTA)
                            .await?
                            .ok_or("the peer withdrew its checkpoint")?;
                        imported = report.rebootstrap.is_some();
                        (
                            report.catch_up,
                            report.rebootstrap.map(|r| (r.checkpoint, r.superseded)),
                        )
                    };
                    moved.add(
                        scope,
                        Moved {
                            pulled: catch_up.frames,
                            bytes: catch_up.bytes,
                            checkpoint,
                            ..Moved::default()
                        },
                    );
                    session.close().await?;
                    if !catch_up.exhausted {
                        replica.flush().await?;
                        return Ok::<_, Failure>(());
                    }
                }
                Err("bootstrap exceeded its session budget; it continues on the next sync".into())
            }
            .await;
            (scope, moved, imported)
        }
    }))
    .buffer_unordered(4);
    while let Some((scope, moved, imported)) = imports.next().await {
        transfer.extend(moved);
        match imported {
            Ok(()) => {
                pending.retain(|known| *known != scope);
                record(&pending, &mut expected)?;
                replica
                    .set_importing(pending.iter().copied().collect())
                    .await;
            }
            Err(error) => transfer.request_failed(scope, &error),
        }
    }
    drop(imports);
    if let Some(session) = first.take() {
        session.close().await?;
    }
    Ok(pending.is_empty().then_some(expected))
}
/// Whether `scope` holds pages of another node's checkpoint than the one
/// `offered` that should go on with that signer: two peers signing the same
/// rows still offer different checkpoints, and switching discards the pages.
async fn deferred(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scope: ResourceScope,
    offered: Option<rocky_core::Commitment>,
) -> Result<bool, Failure> {
    let Some(keep) = upstream.keep else {
        return Ok(false);
    };
    let Some((staged, cursor, _)) = replica.rebootstrap_progress(scope).await? else {
        return Ok(false);
    };
    if offered == Some(staged) || cursor.is_none() {
        return Ok(false);
    }
    Ok(match replica.rebootstrap_signer(scope).await? {
        Some(signer) => signer != upstream.peer && keep(scope, signer, cursor),
        None => false,
    })
}
/// A request channel on the link with the upstream peer. The link handshake
/// left `replica` holding every revocation statement the peer sent; see
/// `Learned`.
pub(crate) async fn session(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
) -> Result<NativeSession, Failure> {
    let mut session = upstream.link.open().await?;
    if let Some(handler) = upstream.contacts {
        crate::mesh::exchange(&mut session, handler).await?;
    }
    if let Some(digest) = upstream.renew_from
        && replica.revocation_digest().await? != digest
    {
        session.close().await?;
        return Err(Box::new(Learned));
    }
    Ok(session)
}
async fn rows(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    progress: impl Fn(bool),
    transfer: &mut Transfer,
    mut first: Option<NativeSession>,
) -> Result<(), Failure> {
    use futures_util::{StreamExt, stream};
    let mut planning = match first.take() {
        Some(session) => session,
        None => session(replica, upstream).await?,
    };
    planning.sync_tip_graph(replica).await?;
    planning.close().await?;
    let mut pending = stream::iter(scopes.iter().copied().map(|scope| {
        let progress = &progress;
        async move {
            let mut moved = Transfer::default();
            let result = rows_scope(
                replica,
                upstream,
                &[scope],
                progress,
                &mut moved,
                None,
                false,
            )
            .await;
            (scope, moved, result)
        }
    }))
    .buffer_unordered(4);
    while let Some((scope, moved, result)) = pending.next().await {
        transfer.extend(moved);
        match result {
            Ok(session) => session.close().await?,
            Err(error) => {
                if error.is::<Learned>() {
                    return Err(error);
                }
                transfer.request_failed(scope, &error);
                progress(false);
            }
        }
    }
    let mut session = session(replica, upstream).await?;
    session.pull_placements(replica).await?;
    session.close().await?;
    Ok(())
}
async fn rows_scope(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    progress: impl Fn(bool),
    transfer: &mut Transfer,
    mut first: Option<NativeSession>,
    refresh: bool,
) -> Result<NativeSession, Failure> {
    for _ in 0..SESSIONS {
        let mut session = match first.take() {
            Some(session) => session,
            None => session(replica, upstream).await?,
        };
        if refresh {
            session.sync_tip_graph(replica).await?;
        }
        // A compacted seal that lost, or a base at another cut, moves to a
        // seal of the followed path before the scopes sync.
        if !session.rebootstrap_scopes_at_seals(replica, scopes).await? {
            session.close().await?;
            continue;
        }
        // Seals the application asked to hold, from a peer that keeps them.
        if !session
            .pull_wanted_seals_for_scopes(replica, scopes)
            .await?
        {
            session.close().await?;
            continue;
        }
        let mut exhausted = false;
        if !replica.tip_statements(None, 1).await?.is_empty() {
            for scope in scopes {
                if session.sync_held_changes(replica, *scope, QUOTA).await? {
                    exhausted = true;
                    break;
                }
            }
        }
        let exchange = session.changed_scopes(replica, scopes).await?;
        transfer.refuse(&exchange.refused);
        // An importing peer takes the checkpoint, not this device's history.
        transfer.wait(&exchange.importing);
        for scope in scopes {
            if !exchange.refused.contains(scope) && !transfer.held.contains(scope) {
                transfer.held.push(*scope);
            }
        }
        for scope in &exchange.changed {
            // Restore the peer's retained base before publishing local edits.
            let mut pulled = session
                .catch_up_with_state(replica, *scope, QUOTA, true)
                .await?;
            let mut moved = Moved {
                pulled: pulled.frames,
                bytes: pulled.bytes,
                ..Moved::default()
            };
            if let Some(advice) = pulled.state.take() {
                let key = (*scope, advice.checkpoint);
                let failures = upstream
                    .state_failures
                    .lock()
                    .map_err(|_| "state retry guard poisoned")?
                    .get(&key)
                    .copied()
                    .unwrap_or(0);
                if deferred(replica, upstream, *scope, Some(advice.checkpoint)).await? {
                    transfer.add(*scope, moved);
                    continue;
                }
                if failures < 3 {
                    progress(true);
                    match session
                        .bootstrap_advised_scope(replica, *scope, QUOTA, Some(&advice))
                        .await
                    {
                        Ok(Some(report)) => {
                            pulled = report.catch_up;
                            moved.pulled += pulled.frames;
                            moved.bytes += pulled.bytes;
                            moved.checkpoint = report
                                .rebootstrap
                                .map(|imported| (imported.checkpoint, imported.superseded));
                        }
                        result => {
                            *upstream
                                .state_failures
                                .lock()
                                .map_err(|_| "state retry guard poisoned")?
                                .entry(key)
                                .or_default() += 1;
                            // The request channel is retired; the next sync retries
                            // this exact checkpoint, then falls back to frames after three failures.
                            return Err(match result {
                                Err(error) => error.into(),
                                _ => "the peer withdrew the advised checkpoint".into(),
                            });
                        }
                    }
                    if !pulled.exhausted {
                        progress(false);
                    }
                } else {
                    replica
                        .cancel_rebootstrap(*scope, advice.checkpoint)
                        .await?;
                    pulled = session.catch_up(replica, *scope, QUOTA).await?;
                    moved.pulled += pulled.frames;
                    moved.bytes += pulled.bytes;
                }
            }
            if pulled.behind
                && !pulled.exhausted
                && deferred(replica, upstream, *scope, None).await?
            {
                // Its staged import goes on with that import's signer.
                transfer.add(*scope, moved);
                continue;
            }
            if pulled.behind && !pulled.exhausted {
                progress(true);
                let report = session
                    .bootstrap_scope(replica, *scope, QUOTA)
                    .await?
                    .ok_or("the peer offers no checkpoint to restore its retained base")?;
                pulled = report.catch_up;
                moved.pulled += pulled.frames;
                moved.bytes += pulled.bytes;
                moved.checkpoint = report
                    .rebootstrap
                    .as_ref()
                    .map(|imported| (imported.checkpoint, imported.superseded));
                if !pulled.exhausted {
                    progress(false);
                }
            }
            if pulled.exhausted {
                transfer.add(*scope, moved);
                exhausted = true;
                break;
            }
            let pushed = session.push(replica, *scope, QUOTA).await?;
            // The peer restores this scope from a checkpoint; others go on.
            if pushed.behind {
                transfer.wait(&[*scope]);
            }
            moved.pushed = pushed.frames;
            moved.sent = pushed.bytes;
            transfer.add(*scope, moved);
            if pushed.exhausted {
                exhausted = true;
                break;
            }
        }
        if !exhausted {
            for scope in replica.existence_scopes().await? {
                if scopes.contains(&scope) {
                    session.sync_existence(replica, scope).await?;
                }
            }
            return Ok(session);
        }
        session.close().await?;
    }
    Err("sync exceeded its session budget; it continues on the next sync".into())
}
