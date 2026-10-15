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
    /// Whether a scope's staged import of another node's checkpoint waits
    /// for that signer, given its last staged row, instead of restarting
    /// from this peer's checkpoint.
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
    pub bytes: usize,
    /// The checkpoint a rebootstrap imported, with the held operations it superseded.
    pub checkpoint: Option<(rocky_core::Commitment, u64)>,
    /// The scope's horizon was finalized and pruned.
    pub finalized: bool,
}
impl Moved {
    pub fn is_empty(&self) -> bool {
        self.pulled == 0 && self.pushed == 0 && self.checkpoint.is_none() && !self.finalized
    }
    fn merge(&mut self, other: Self) {
        self.pulled += other.pulled;
        self.pushed += other.pushed;
        self.bytes += other.bytes;
        self.checkpoint = other.checkpoint.or(self.checkpoint);
        self.finalized |= other.finalized;
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
    /// or behind this device's pruning horizon: no history was pushed.
    pub waiting: Vec<ResourceScope>,
}
impl Transfer {
    pub fn extend(&mut self, other: Self) {
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
/// Returns whether another generation requires an upgrade, whether another
/// pass is needed and what moved. A peer may run a newer definition
/// generation, whose credentials this device cannot verify: it pulls, and
/// its own changes wait until it upgrades. See `docs/runtime/generations.md`.
/// `first` is a session already open with the peer, which rows use first.
pub(crate) async fn reconcile(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    progress: impl Fn(bool),
    first: Option<NativeSession>,
) -> Result<(bool, bool, Transfer), Failure> {
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
    let newer = match rows_result {
        Ok(newer) => newer,
        Err(error) => {
            upstream
                .observations
                .record_partial(upstream.peer.to_hex(), transfer);
            return Err(error);
        }
    };
    Ok((newer, false, transfer))
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
/// `None` when the peer offers no checkpoint for some scope, as a keyless
/// custody peer: the remaining intent waits for a peer that does.
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
    if scopes.is_empty() {
        return Ok(Some(expected));
    }
    let mut planning = match first.take() {
        Some(session) => session,
        None => session(replica, upstream).await?,
    };
    let mut offered = planning.offered_rows(replica, scopes).await?;
    first = Some(planning);
    for (scope, offer) in scopes.iter().zip(offered.iter_mut()) {
        if deferred(
            replica,
            upstream,
            *scope,
            offer.map(|(checkpoint, _)| checkpoint),
        )
        .await?
        {
            *offer = None;
        }
    }
    let mut totals: Vec<Planned> = match saved.get("totals") {
        Some(totals) => serde_json::from_value(totals.clone())?,
        None => Vec::new(),
    };
    // A scope this peer does not import from keeps the total it was offered.
    totals.retain(|planned| {
        scopes
            .iter()
            .zip(&offered)
            .all(|(scope, offer)| *scope != planned.scope || offer.is_none())
    });
    let mut planned: Vec<Planned> = scopes
        .iter()
        .zip(&offered)
        .filter_map(|(scope, offer)| {
            offer.map(|(checkpoint, rows)| Planned {
                scope: *scope,
                checkpoint,
                rows,
            })
        })
        .collect();
    planned.sort_by_key(|planned| (planned.rows == 0, planned.rows));
    totals.extend(planned.iter().cloned());
    let importable = planned.len();
    let pending: Vec<ResourceScope> = planned
        .iter()
        .map(|planned| planned.scope)
        .chain(
            scopes
                .iter()
                .zip(&offered)
                .filter(|(_, rows)| rows.is_none())
                .map(|(scope, _)| *scope),
        )
        .collect();
    let mut next = 0;
    let record = |next: usize, expected: &mut Vec<u8>| -> Result<(), Failure> {
        if std::fs::read(marker)? != *expected {
            return Err("bootstrap authority changed; retry with the new intent".into());
        }
        let mut next_intent =
            serde_json::json!({"intent": intent, "pending": &pending[next..], "totals": &totals});
        if let Some(inviter) = saved.get("inviter") {
            next_intent["inviter"] = inviter.clone();
        }
        *expected = serde_json::to_vec(&next_intent)?;
        rocky_credentials::write_private(marker, expected)?;
        Ok(())
    };
    record(next, &mut expected)?;
    let mut imported = false;
    for _ in 0..SESSIONS {
        if next == importable {
            break;
        }
        let mut session = match first.take() {
            Some(session) => session,
            None => session(replica, upstream).await?,
        };
        let scope = pending.get(next).ok_or("missing bootstrap scope")?;
        let (catch_up, checkpoint) = if imported {
            (
                session.sync_scope(replica, *scope, QUOTA).await?.catch_up,
                None,
            )
        } else {
            let Some(report) = session.bootstrap_scope(replica, *scope, QUOTA).await? else {
                session.close().await?;
                return Ok(None);
            };
            imported = report.rebootstrap.is_some();
            let checkpoint = report
                .rebootstrap
                .as_ref()
                .map(|imported| (imported.checkpoint, imported.superseded));
            (report.catch_up, checkpoint)
        };
        transfer.add(
            *scope,
            Moved {
                pulled: catch_up.frames,
                bytes: catch_up.bytes,
                checkpoint,
                ..Moved::default()
            },
        );
        if !catch_up.exhausted {
            replica.flush().await?;
            next += 1;
            record(next, &mut expected)?;
            replica
                .set_importing(pending[next..].iter().copied().collect())
                .await;
            imported = false;
        }
        session.close().await?;
    }
    if next < importable {
        return Err("bootstrap exceeded its session budget; it continues on the next sync".into());
    }
    if let Some(session) = first.take() {
        session.close().await?;
    }
    Ok((importable == pending.len()).then_some(expected))
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
) -> Result<bool, Failure> {
    let mut newer = false;
    for _ in 0..SESSIONS {
        let mut session = match first.take() {
            Some(session) => session,
            None => session(replica, upstream).await?,
        };
        let mut exhausted = false;
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
            let mut pulled = session.catch_up(replica, *scope, QUOTA).await?;
            let mut moved = Moved {
                pulled: pulled.frames,
                bytes: pulled.bytes,
                ..Moved::default()
            };
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
            // Refused locally before anything is sent: the session goes on.
            let pushed = match session.push(replica, *scope, QUOTA).await {
                Err(rocky_secure::Error::Incompatible(_)) => {
                    newer = true;
                    Default::default()
                }
                other => other?,
            };
            // The peer restores this scope from a checkpoint; others go on.
            if pushed.behind {
                transfer.wait(&[*scope]);
            }
            moved.pushed = pushed.frames;
            transfer.add(*scope, moved);
            if pushed.exhausted {
                exhausted = true;
                break;
            }
        }
        // Existence logs of referenced tables, held or not (`docs/runtime/existence-logs.md`).
        if !exhausted {
            for scope in replica.existence_scopes().await? {
                session.sync_existence(replica, scope).await?;
            }
            for scope in scopes {
                // A refused scope is not exchanged with this peer at all.
                // A statement of a newer generation is acknowledged after
                // this device adopts it.
                if !exchange.refused.contains(scope) {
                    match session.acknowledge_mergeable_horizon(replica, *scope).await {
                        Err(rocky_secure::Error::Incompatible(_)) => newer = true,
                        other => {
                            other?;
                        }
                    }
                }
                if replica.can_finalize_horizon(*scope).await?
                    && let Some(signed) = replica.admitted_horizon(*scope).await?
                    && replica.finalized_horizon(*scope).await?.as_ref() != Some(&signed)
                    && !replica.horizon_rebootstrap_required(*scope).await?
                    && replica.horizon_progress(*scope).await?.waiting.is_empty()
                    && !replica.horizon_dependents_pending(*scope).await?
                {
                    replica.finalize_horizon(*scope).await?;
                    replica.prune(*scope).await?;
                    transfer.add(
                        *scope,
                        Moved {
                            finalized: true,
                            ..Moved::default()
                        },
                    );
                }
            }
        }
        session.close().await?;
        if !exhausted {
            return Ok(newer);
        }
    }
    Err("sync exceeded its session budget; it continues on the next sync".into())
}
