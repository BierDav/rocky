//! Synchronization with one authorized peer: a device pushes and pulls
//! every collection over an authenticated native session, and serves every
//! peer whose chain and the revocation set admit it. See
//! `docs/runtime/sdk-enrollment.md`.
use crate::{
    Failure,
    enrollment::dial,
    transport::{Protocol, Stream, Transport},
};
use rocky_core::{Coordinator, ResourceScope, TypedCommands};
use rocky_secure::{
    CredentialChain, EncryptedReplica, MAX_SECURE_BYTES, MAX_SECURE_FRAME, PrivateKey, PublicKey,
    network::NativeSession,
};

/// Bytes of frames per page.
const QUOTA: usize = MAX_SECURE_FRAME / 2;
/// Sessions one sync may use when each exhausts its byte budget.
const SESSIONS: usize = 16;
/// Command results one sync pulls from a peer.
const RESULTS: usize = 64;
/// How long result sync waits for the coordinator's head.
const HEAD: std::time::Duration = std::time::Duration::from_secs(2);

/// One device's view of a peer. `addresses` are hints: the session only
/// proceeds with whoever proves the `peer` key.
pub struct Upstream<'a> {
    pub transport: &'a dyn Transport,
    pub addresses: &'a [String],
    pub peer: PublicKey,
    pub node: PrivateKey,
    pub credentials: CredentialChain,
    /// The revocation digest this sync started from, when it renews: a
    /// session that changes the set ends the sync with `Learned`.
    pub contacts: Option<&'a dyn rocky_secure::network::ContactHandler>,
    pub renew_from: Option<rocky_core::Commitment>,
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
#[derive(Debug, Default)]
pub(crate) struct Transfer {
    pub scopes: Vec<(ResourceScope, Moved)>,
    /// Scopes the peer and this device did not exchange: one side's
    /// credentials, as the other sees them, grant no whole-scope access.
    pub refused: Vec<ResourceScope>,
    pub held: Vec<ResourceScope>,
    /// Why the confirmed results pass failed; rows still synced.
    pub results: Option<String>,
}
impl Transfer {
    pub fn extend(&mut self, other: Self) {
        for (scope, moved) in other.scopes {
            self.add(scope, moved);
        }
        self.refuse(&other.refused);
        for scope in other.held {
            if !self.held.contains(&scope) {
                self.held.push(scope);
            }
        }
        if other.results.is_some() {
            self.results = other.results;
        }
    }
    fn refuse(&mut self, scopes: &[ResourceScope]) {
        for scope in scopes {
            if !self.refused.contains(scope) {
                self.refused.push(*scope);
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
/// scope, until both hold the same frames. Confirmed command results the
/// peer holds follow in a session of their own.
/// Returns whether another generation requires an upgrade, whether results
/// need another pass and what moved. A peer may run a newer definition
/// generation, whose credentials this device cannot verify: it pulls, and
/// its own changes wait until it upgrades. See `docs/runtime/generations.md`.
/// `first` is a session already open with the peer, which rows use first.
pub(crate) async fn reconcile(
    replica: &EncryptedReplica<TypedCommands>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    coordinator: Option<&dyn Coordinator>,
    progress: impl Fn(bool),
    first: Option<NativeSession>,
) -> Result<(bool, bool, Transfer), Failure> {
    let mut transfer = Transfer::default();
    let newer = Box::pin(rows(
        replica,
        upstream,
        scopes,
        progress,
        &mut transfer,
        first,
    ))
    .await?;
    // Rows stay in sync while a coordinator is unreachable or a peer holds
    // no results; a ticket then stays `confirmed` until a later sync.
    let retry = match coordinator {
        Some(coordinator) => match results(replica, upstream, scopes, coordinator).await {
            Ok(retry) => retry,
            Err(error) => {
                transfer.results = Some(error.to_string());
                true
            }
        },
        None => false,
    };
    Ok((newer, retry, transfer))
}
/// Checkpoint-first enrollment, resumed from the runtime's staged pages.
/// `None` when the peer offers no checkpoint, as a keyless custody peer:
/// the remaining intent waits for a peer that does.
pub(crate) async fn bootstrap(
    replica: &EncryptedReplica<TypedCommands>,
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
    let mut next = 0;
    let mut imported = false;
    for _ in 0..SESSIONS {
        let mut session = match first.take() {
            Some(session) => session,
            None => session(replica, upstream).await?,
        };
        let scope = scopes.get(next).ok_or("missing bootstrap scope")?;
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
            if std::fs::read(marker)? != expected {
                return Err("bootstrap authority changed; retry with the new intent".into());
            }
            next += 1;
            expected = serde_json::to_vec(
                &serde_json::json!({"intent": intent, "pending": &scopes[next..]}),
            )?;
            rocky_credentials::write_private(marker, &expected)?;
            imported = false;
        }
        session.close().await?;
        if next == scopes.len() {
            return Ok(Some(expected));
        }
    }
    Err("bootstrap exceeded its session budget; it continues on the next sync".into())
}
/// A session with the upstream peer. The handshake leaves `replica`
/// holding every revocation statement the peer sent; see `Learned`.
pub(crate) async fn session(
    replica: &EncryptedReplica<TypedCommands>,
    upstream: &Upstream<'_>,
) -> Result<NativeSession, Failure> {
    let stream = dial(upstream.transport, upstream.addresses, Protocol::Sync).await?;
    let mut session = NativeSession::authenticate(
        stream,
        replica,
        upstream.node.clone(),
        upstream.credentials.clone(),
        upstream.peer,
        MAX_SECURE_BYTES,
    )
    .await?;
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
    replica: &EncryptedReplica<TypedCommands>,
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
            if pushed.behind {
                return Err("the peer is behind this device's pruning horizon".into());
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
                if !exchange.refused.contains(scope) {
                    session
                        .acknowledge_mergeable_horizon(replica, *scope)
                        .await?;
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
/// Pulls the confirmed results this device lacks, lowest sequence first,
/// each filtered by the peer to the scopes this device may hold.
async fn results(
    replica: &EncryptedReplica<TypedCommands>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    coordinator: &dyn Coordinator,
) -> Result<bool, Failure> {
    let Some(domain) = scopes.first().map(|scope| scope.domain) else {
        return Ok(false);
    };
    // A custodian the coordinator does not name, or one that cannot reach
    // it, pulls in order until the peer holds no later result; each result
    // verifies with the receipt evidence the peer sends along.
    // The head only bounds the request; an outage must not stall row sync.
    let (head, unavailable) = match tokio::time::timeout(HEAD, coordinator.head(domain)).await {
        Ok(Ok(head)) => (head.sequence, false),
        _ => (u64::MAX, true),
    };
    let missing = replica.missing_results(domain, head, RESULTS).await?;
    if missing.is_empty() {
        let mut incomplete = Vec::new();
        for scope in scopes {
            if !replica.key_epochs(*scope).await.is_empty()
                && !replica.command_snapshot(*scope).await?.complete
            {
                incomplete.push(*scope);
            }
        }
        let repairing = !incomplete.is_empty();
        if !repairing && (unavailable || head == 0) {
            return Ok(unavailable);
        }
        // A checkpoint can supply the head before results are pulled; the
        // executor still needs the reader's applied acknowledgment.

        let mut session = session(replica, upstream).await?;
        for scope in incomplete {
            session.bootstrap_scope(replica, scope, QUOTA).await?;
        }
        if !unavailable {
            session
                .pull_result(replica, domain, head, scopes.to_vec(), coordinator)
                .await?;
        }
        session.close().await?;
        return Ok(unavailable || repairing);
    }
    let mut retry = unavailable || missing.len() == RESULTS;
    let mut session = session(replica, upstream).await?;
    for sequence in missing {
        let pulled = session
            .pull_result(replica, domain, sequence, scopes.to_vec(), coordinator)
            .await?;
        // The peer holds no later result either.
        if !pulled {
            // A source may have pruned this prefix or sealed it under an older key.
            // Existing checkpoint bootstrap carries the current strong base.
            for scope in scopes {
                if session
                    .bootstrap_scope(replica, *scope, QUOTA)
                    .await?
                    .is_some()
                {
                    retry = true;
                }
            }
            if !unavailable {
                session
                    .pull_result(replica, domain, head, scopes.to_vec(), coordinator)
                    .await?;
            }
            retry |= head != u64::MAX && sequence <= head;
            break;
        }
    }
    session.close().await?;
    Ok(retry)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve_contacts(
    stream: Stream,
    replica: &EncryptedReplica<TypedCommands>,
    node: PrivateKey,
    credentials: CredentialChain,
    coordinator: Option<&dyn Coordinator>,
    deliveries: &dyn rocky_secure::network::Deliveries,
    admitted_peer: impl FnOnce(PublicKey),
    handler: &dyn rocky_secure::network::ContactHandler,
) -> Result<(), Failure> {
    let Some(mut session) =
        NativeSession::accept_or_resolve(stream, replica, node, credentials, MAX_SECURE_BYTES)
            .await?
    else {
        return Ok(());
    };
    admitted_peer(session.peer());
    Ok(session
        .serve_all(replica, coordinator, deliveries, Some(handler))
        .await?)
}
