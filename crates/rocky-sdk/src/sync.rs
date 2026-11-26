use crate::{
    Failure,
    bootstrap_intent::{BootstrapIntent, BootstrapIntentFile},
};
use rocky_core::{ResourceScope, TypedRows};
use rocky_secure::{
    EncryptedReplica, MAX_SECURE_FRAME, PublicKey,
    network::{NativeSession, link::channels::Handle},
};

const QUOTA: usize = MAX_SECURE_FRAME / 2;
const SESSIONS: usize = 16;
const PARALLEL: usize = rocky_secure::network::link::CHANNELS as usize - 3;

pub(crate) struct Upstream<'a> {
    pub link: Handle,
    pub observations: &'a crate::status::Observations,
    pub peer: PublicKey,
    pub refresh_control: bool,
    pub control_started: std::cell::Cell<bool>,
    pub contacts: Option<&'a dyn rocky_secure::network::ContactHandler>,
    pub renew_from: Option<rocky_core::Commitment>,
    pub keep: Option<&'a dyn Fn(ResourceScope, PublicKey, Option<rocky_core::Commitment>) -> bool>,
}

#[derive(Debug)]
pub(crate) struct Learned;
impl std::fmt::Display for Learned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the peer sent revocation statements; renewing before sync")
    }
}
impl std::error::Error for Learned {}

#[derive(Debug)]
struct Unfinished;
impl std::fmt::Display for Unfinished {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bootstrap exceeded its session budget; it continues on the next sync")
    }
}
impl std::error::Error for Unfinished {}

fn counts_against_import(error: &Failure) -> bool {
    if error.is::<Unfinished>() || error.is::<Learned>() {
        return false;
    }
    error
        .downcast_ref::<rocky_secure::Error>()
        .is_none_or(counts_against_checkpoint)
}
fn counts_against_checkpoint(error: &rocky_secure::Error) -> bool {
    match error {
        rocky_secure::Error::Io(_) | rocky_secure::Error::Closed => false,
        rocky_secure::Error::Rejected(reason) => !reason.starts_with("link "),
        _ => true,
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Moved {
    pub pulled: usize,
    pub pushed: usize,
    pub bytes: usize,
    pub sent: usize,
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
#[derive(Clone, Debug, Default)]
pub(crate) struct Transfer {
    pub repair_generation: Option<u64>,
    pub scopes: Vec<(ResourceScope, Moved)>,
    pub refused: Vec<ResourceScope>,
    pub held: Vec<ResourceScope>,
    pub waiting: Vec<ResourceScope>,
    pub failed: Vec<ResourceScope>,
    pub denied: Vec<ResourceScope>,
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
        for scope in other.denied {
            self.deny(scope);
        }
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
            self.deny(scope);
        } else {
            self.fail(scope);
        }
    }
    fn deny(&mut self, scope: ResourceScope) {
        self.refuse(&[scope]);
        if !self.denied.contains(&scope) {
            self.denied.push(scope);
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Planned {
    pub scope: ResourceScope,
    #[serde(with = "rocky_core::byte_arrays::one")]
    pub checkpoint: rocky_core::Commitment,
    pub rows: u64,
}
pub(crate) async fn bootstrap(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    file: &BootstrapIntentFile,
    mut expected: BootstrapIntent,
    transfer: &mut Transfer,
    mut first: Option<NativeSession>,
) -> Result<Option<BootstrapIntent>, Failure> {
    let mut pending = scopes.to_vec();
    let mut totals = expected.totals().to_vec();
    let mut planned = Vec::new();
    let mut tips_synced = false;
    for scope in scopes {
        let offer = async {
            let mut planning = match first.take() {
                Some(session) => session,
                None => session(replica, upstream).await?,
            };
            if !tips_synced {
                if upstream.refresh_control {
                    planning.sync_tip_graph(replica).await?;
                }
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
                if let Some((checkpoint, _)) = offer
                    && replica.import_failures(*scope, checkpoint).await?
                        >= rocky_secure::IMPORT_ATTEMPTS
                {
                    pending.retain(|known| known != scope);
                } else if let Some((checkpoint, rows)) = offer {
                    let plan = Planned {
                        scope: *scope,
                        checkpoint,
                        rows,
                    };
                    totals.retain(|old| old.scope != *scope);
                    totals.push(plan.clone());
                    planned.push(plan);
                } else if replica.rebootstrap_progress(*scope).await?.is_none()
                    && !replica.holds_revoked_frames(*scope).await?
                {
                    pending.retain(|known| known != scope);
                }
            }
        }
    }
    planned.sort_by_key(|plan| (plan.rows == 0, plan.rows));
    expected = file.record_progress(&expected, &pending, &totals).await?;
    replica
        .set_importing(pending.iter().copied().collect())
        .await;
    use futures_util::{StreamExt, stream::FuturesUnordered};
    let settling = tokio::sync::Mutex::new((pending, expected));
    let (progress, totals) = (&settling, &totals);
    let nodes = replica.dependency_graph(planned, |plan| plan.scope).await?;
    let import = |plan: Planned, mut first: Option<NativeSession>| async move {
        let (scope, checkpoint) = (plan.scope, plan.checkpoint);
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
            Err(Box::new(Unfinished) as Failure)
        }
        .await;
        let settled = match &imported {
            Ok(()) => {
                async {
                    let mut progress = progress.lock().await;
                    let (pending, expected) = &mut *progress;
                    pending.retain(|known| *known != scope);
                    *expected = file.record_progress(expected, pending, totals).await?;
                    replica
                        .set_importing(pending.iter().copied().collect())
                        .await;
                    Ok::<_, Failure>(())
                }
                .await
            }
            Err(error) if counts_against_import(error) => replica
                .import_failed(scope, checkpoint)
                .await
                .map(drop)
                .map_err(Failure::from),
            Err(_) => Ok(()),
        };
        (scope, moved, imported, settled)
    };
    let mut schedule = DependencySchedule::new(nodes);
    let mut imports = FuturesUnordered::new();
    let run_node = |index: usize, plans: Vec<Planned>, mut reused: Option<NativeSession>| async move {
        let mut done = Vec::with_capacity(plans.len());
        let mut ok = true;
        for plan in plans {
            let outcome = import(plan, reused.take()).await;
            let unsettled = outcome.2.is_err() || outcome.3.is_err();
            done.push(outcome);
            if unsettled {
                ok = false;
                break;
            }
        }
        (index, ok, done)
    };
    while imports.len() < PARALLEL {
        let Some((index, plans)) = schedule.next_ready() else {
            break;
        };
        imports.push(run_node(index, plans, first.take()));
    }
    while let Some((index, ok, done)) = imports.next().await {
        for (scope, moved, imported, settled) in done {
            transfer.extend(moved);
            settled?;
            if let Err(error) = imported {
                transfer.request_failed(scope, &error);
            }
        }
        schedule.complete(index, ok);
        while imports.len() < PARALLEL {
            let Some((index, plans)) = schedule.next_ready() else {
                break;
            };
            imports.push(run_node(index, plans, first.take()));
        }
    }
    drop(imports);
    if let Some(session) = first.take() {
        session.close().await?;
    }
    let (pending, expected) = settling.into_inner();
    Ok(pending.is_empty().then_some(expected))
}
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
pub(crate) async fn session(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
) -> Result<NativeSession, Failure> {
    let mut session = upstream.link.open().await?;
    if upstream.refresh_control && !upstream.control_started.replace(true) {
        session.refresh_revocations(replica).await?;
        if let Some(handler) = upstream.contacts {
            crate::mesh::exchange(&mut session, handler).await?;
        }
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
    use futures_util::{StreamExt, stream::FuturesUnordered};
    let mut planning = match first.take() {
        Some(session) => session,
        None => session(replica, upstream).await?,
    };
    if upstream.refresh_control {
        planning.sync_tip_graph(replica).await?;
    }
    if !planning
        .rebootstrap_scopes_at_seals(replica, scopes)
        .await?
        || !planning
            .pull_wanted_seals_for_scopes(replica, scopes)
            .await?
    {
        planning.close().await?;
        return Err(Box::new(Unfinished));
    }
    if !replica.tip_statements(None, 1).await?.is_empty() {
        let ordered: Vec<_> = replica
            .dependency_graph(scopes.to_vec(), |scope| *scope)
            .await?
            .into_iter()
            .flat_map(|node| node.items)
            .collect();
        for scope in ordered {
            if planning.sync_held_changes(replica, scope, QUOTA).await? {
                planning.close().await?;
                return Err(Box::new(Unfinished));
            }
        }
    }
    let mut exchange = planning.changed_scopes(replica, scopes).await?;
    for scope in scopes {
        if !exchange.refused.contains(scope)
            && !exchange.importing.contains(scope)
            && !exchange.changed.contains(scope)
            && replica.rebootstrap_progress(*scope).await?.is_some()
        {
            exchange.changed.push(*scope);
        }
    }
    transfer.refuse(&exchange.refused);
    transfer.wait(&exchange.importing);
    for scope in scopes {
        if !exchange.refused.contains(scope) && !transfer.held.contains(scope) {
            transfer.held.push(*scope);
        }
    }
    let mut planning = if exchange.changed.is_empty() {
        Some(planning)
    } else {
        planning.close().await?;
        None
    };
    let nodes = replica
        .dependency_graph(exchange.changed, |scope| *scope)
        .await?;
    let mut schedule = DependencySchedule::new(nodes);
    let mut pending = FuturesUnordered::new();
    let run_node = |index: usize, node_scopes: Vec<ResourceScope>| {
        let progress = &progress;
        async move {
            let mut done = Vec::with_capacity(node_scopes.len());
            let mut kept = None;
            let mut ok = true;
            for scope in node_scopes {
                let mut moved = Transfer::default();
                match rows_scope(
                    replica,
                    upstream,
                    &[scope],
                    progress,
                    &mut moved,
                    kept.take(),
                )
                .await
                {
                    Ok(session) => {
                        kept = Some(session);
                        done.push((scope, moved, Ok(())));
                    }
                    Err(error) => {
                        ok = false;
                        done.push((scope, moved, Err(error)));
                        break;
                    }
                }
            }
            if let Some(session) = kept
                && let Err(error) = session.close().await
            {
                ok = false;
                if let Some((_, _, result)) = done.last_mut()
                    && result.is_ok()
                {
                    *result = Err(error.into());
                }
            }
            (index, ok, done)
        }
    };
    while pending.len() < PARALLEL {
        let Some((index, node_scopes)) = schedule.next_ready() else {
            break;
        };
        pending.push(run_node(index, node_scopes));
    }
    while let Some((index, ok, done)) = pending.next().await {
        for (scope, moved, result) in done {
            transfer.extend(moved);
            if let Err(error) = result {
                if error.is::<Learned>() {
                    return Err(error);
                }
                transfer.request_failed(scope, &error);
                progress(false);
            }
        }
        schedule.complete(index, ok);
        while pending.len() < PARALLEL {
            let Some((index, node_scopes)) = schedule.next_ready() else {
                break;
            };
            pending.push(run_node(index, node_scopes));
        }
    }
    if upstream.refresh_control {
        let mut placements = match planning.take() {
            Some(session) => session,
            None => session(replica, upstream).await?,
        };
        placements.pull_placements(replica).await?;
        placements.close().await?;
    } else if let Some(planning) = planning {
        planning.close().await?;
    }
    Ok(())
}
async fn rows_scope(
    replica: &EncryptedReplica<TypedRows>,
    upstream: &Upstream<'_>,
    scopes: &[ResourceScope],
    progress: impl Fn(bool),
    transfer: &mut Transfer,
    mut first: Option<NativeSession>,
) -> Result<NativeSession, Failure> {
    for _ in 0..SESSIONS {
        let mut session = match first.take() {
            Some(session) => session,
            None => {
                let session = upstream.link.open().await?;
                if let Some(digest) = upstream.renew_from
                    && replica.revocation_digest().await? != digest
                {
                    session.close().await?;
                    return Err(Box::new(Learned));
                }
                session
            }
        };
        let mut exhausted = false;
        let exchange = rocky_secure::network::FrontierExchange {
            changed: scopes.to_vec(),
            ..Default::default()
        };
        for scope in &exchange.changed {
            let mut pulled = session
                .catch_up_with_state(replica, *scope, QUOTA, true)
                .await?;
            let staging_left_by_crash_at_switch =
                replica.rebootstrap_progress(*scope).await?.is_some();
            pulled.behind |= staging_left_by_crash_at_switch;
            let mut moved = Moved {
                pulled: pulled.frames,
                bytes: pulled.bytes,
                ..Moved::default()
            };
            if let Some(advice) = pulled.state.take() {
                let failures = replica.import_failures(*scope, advice.checkpoint).await?;
                if deferred(replica, upstream, *scope, Some(advice.checkpoint)).await? {
                    transfer.add(*scope, moved);
                    continue;
                }
                if failures < rocky_secure::IMPORT_ATTEMPTS {
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
                            if result.as_ref().err().is_none_or(counts_against_checkpoint) {
                                replica.import_failed(*scope, advice.checkpoint).await?;
                            }
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
            let peer_restores_from_checkpoint = pushed.behind;
            if peer_restores_from_checkpoint {
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
            for scope in scopes {
                if !exchange.refused.contains(scope)
                    && !deferred(replica, upstream, *scope, None).await?
                    && !session.replace_with_listed(replica, *scope).await?
                {
                    exhausted = true;
                    break;
                }
            }
        }
        if !exhausted {
            return Ok(session);
        }
        session.close().await?;
    }
    Err("sync exceeded its session budget; it continues on the next sync".into())
}

struct DependencySchedule<T> {
    items: Vec<Option<Vec<T>>>,
    dependents: Vec<Vec<usize>>,
    waiting: Vec<usize>,
    blocked: Vec<bool>,
    ready: std::collections::VecDeque<usize>,
}

impl<T> DependencySchedule<T> {
    fn new(nodes: Vec<rocky_secure::DependencyNode<T>>) -> Self {
        let count = nodes.len();
        let mut items = Vec::with_capacity(count);
        let mut dependents = vec![Vec::new(); count];
        let mut waiting = Vec::with_capacity(count);
        let mut ready = std::collections::VecDeque::new();
        for (index, node) in nodes.into_iter().enumerate() {
            items.push(Some(node.items));
            waiting.push(node.parents.len());
            if node.parents.is_empty() {
                ready.push_back(index);
            }
            for parent in node.parents {
                dependents[parent].push(index);
            }
        }
        Self {
            items,
            dependents,
            waiting,
            blocked: vec![false; count],
            ready,
        }
    }

    fn next_ready(&mut self) -> Option<(usize, Vec<T>)> {
        while let Some(index) = self.ready.pop_front() {
            if self.blocked[index] {
                continue;
            }
            if let Some(items) = self.items[index].take() {
                return Some((index, items));
            }
        }
        None
    }

    fn complete(&mut self, index: usize, ok: bool) {
        if ok {
            for &child in &self.dependents[index] {
                self.waiting[child] = self.waiting[child].saturating_sub(1);
                if self.waiting[child] == 0 && !self.blocked[child] {
                    self.ready.push_back(child);
                }
            }
        } else {
            let mut stack = vec![index];
            while let Some(node) = stack.pop() {
                for &child in &self.dependents[node] {
                    if !self.blocked[child] {
                        self.blocked[child] = true;
                        stack.push(child);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DependencySchedule;
    use rocky_secure::DependencyNode;

    #[test]
    fn star_releases_all_children_after_parent_succeeds() {
        let mut schedule = DependencySchedule::new(vec![
            DependencyNode {
                items: vec!["hub"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["c1"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["c2"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["c3"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["c4"],
                parents: vec![0],
            },
        ]);
        let (hub, items) = schedule.next_ready().unwrap();
        assert_eq!((hub, items), (0, vec!["hub"]));
        assert!(schedule.next_ready().is_none());
        schedule.complete(hub, true);
        let mut children = Vec::new();
        while let Some((_, items)) = schedule.next_ready() {
            children.extend(items);
        }
        assert_eq!(children, vec!["c1", "c2", "c3", "c4"]);
    }

    #[test]
    fn failed_parent_blocks_descendants_without_blocking_unrelated_nodes() {
        let mut schedule = DependencySchedule::new(vec![
            DependencyNode {
                items: vec!["parent"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["other"],
                parents: vec![],
            },
            DependencyNode {
                items: vec!["child"],
                parents: vec![0],
            },
            DependencyNode {
                items: vec!["grandchild"],
                parents: vec![0, 2],
            },
        ]);
        let (parent, _) = schedule.next_ready().unwrap();
        let (other, _) = schedule.next_ready().unwrap();
        assert!(schedule.next_ready().is_none());
        schedule.complete(parent, false);
        schedule.complete(other, true);
        assert!(schedule.next_ready().is_none());
    }
}
