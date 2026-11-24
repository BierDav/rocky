use super::{
    cluster::Member,
    relational::{self, Authored, Position},
    schedule::{Action, Change, Fixture, LISTS, Scenario, Wall, node_name, plan},
};
use crate::{Database, Failure, TransactionToken, transport::memory::Hub};
use rocky_core::{Blocker, BlockerKind, WaitReason, replay::TransactionOutcome};

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::Arc,
};

const STEP_TIME: std::time::Duration = std::time::Duration::from_millis(1);
const MAX_STATUS_WAKES: usize = 1000;
const ALERT_WATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const MESH_WATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3600);
const RECOVERY_DRIVER_SETTLE: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) struct Report {
    pub(super) seed: u64,
    pub(super) steps: Vec<String>,
    pub(super) safety: Option<String>,
    pub(super) liveness: Option<Stuck>,
    pub(super) scope_local: usize,
    pub(super) coverage: BTreeMap<String, usize>,
    pub(super) states: Vec<String>,
}
impl Report {
    pub(super) fn digest(&self) -> String {
        let text = format!("{}{:?}{:?}", self.dump(), self.coverage, self.states);
        data_encoding::HEXLOWER.encode(&rocky_core::commitment(
            "rocky.simulation.replay",
            text.as_bytes(),
        ))
    }
    pub(super) fn dump(&self) -> String {
        let mut text = format!("seed {}\n", self.seed);
        for step in &self.steps {
            let _ = writeln!(text, "  {step}");
        }
        if let Some(safety) = &self.safety {
            let _ = writeln!(text, "safety: {safety}");
        }
        if let Some(liveness) = &self.liveness {
            let _ = writeln!(
                text,
                "liveness {:?}: {}",
                liveness.classes, liveness.described
            );
        }
        text
    }
}

struct Simulation {
    hub: Arc<Hub>,
    fixture: Fixture,
    members: Vec<Member>,
    synced: Vec<bool>,
    scope_local: usize,
    lists: Vec<String>,
    authored: Vec<Authored>,
    may_vanish: BTreeSet<TransactionToken>,
    positions: BTreeMap<TransactionToken, (Position, bool)>,
    coverage: BTreeMap<String, usize>,
    watches: BTreeMap<usize, tokio::task::JoinHandle<Result<Vec<crate::Alert>, Failure>>>,
    mesh_watches: BTreeMap<usize, tokio::task::JoinHandle<Result<MeshView, Failure>>>,
    identities: BTreeMap<String, String>,
}
type MeshView = Option<Vec<crate::MeshDevice>>;

pub(super) async fn run(seed: u64, scenario: Scenario) -> Result<Report, Failure> {
    run_steps(seed, scenario, plan(seed, scenario)).await
}
pub(super) async fn run_isolated(
    seed: u64,
    scenario: Scenario,
    steps: Vec<Action>,
) -> Result<Report, Failure> {
    let thread = std::thread::Builder::new()
        .name(format!("simulation-{seed}"))
        .spawn(move || -> Result<Report, Failure> {
            let mut builder = tokio::runtime::Builder::new_current_thread();
            builder.enable_all().start_paused(true);
            #[cfg(tokio_unstable)]
            builder.rng_seed(tokio::runtime::RngSeed::from_bytes(&seed.to_le_bytes()));
            let runtime = builder.build()?;
            tokio::task::LocalSet::new().block_on(&runtime, run_steps(seed, scenario, steps))
        })?;
    match tokio::task::spawn_blocking(move || thread.join()).await? {
        Ok(report) => report,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
pub(super) async fn run_steps(
    seed: u64,
    scenario: Scenario,
    steps: Vec<Action>,
) -> Result<Report, Failure> {
    let _replays_seed_on_this_thread = rocky_platform::entropy::simulation::seed(seed);
    if scenario.wall == Wall::Frozen {
        rocky_platform::entropy::simulation::freeze();
    }
    let root = tempfile::tempdir()?;
    let fixture = scenario.fixture;
    let mut simulation = Simulation {
        hub: Arc::new(Hub::seeded(seed)),
        fixture,
        lists: Vec::new(),
        authored: Vec::new(),
        may_vanish: BTreeSet::new(),
        positions: BTreeMap::new(),
        coverage: BTreeMap::new(),
        members: (0..scenario.nodes)
            .map(|node| Member::new(root.path(), &node_name(node), fixture.schema()))
            .collect(),
        synced: vec![false; scenario.nodes],
        scope_local: 0,
        watches: BTreeMap::new(),
        mesh_watches: BTreeMap::new(),
        identities: BTreeMap::new(),
    };
    let mut report = Report {
        seed,
        steps: Vec::new(),
        safety: None,
        liveness: None,
        scope_local: 0,
        coverage: BTreeMap::new(),
        states: Vec::new(),
    };
    simulation.setup().await?;
    for (index, action) in steps.into_iter().enumerate() {
        let started = std::time::Instant::now();
        rocky_platform::entropy::simulation::pass(STEP_TIME);
        if std::env::var_os("ROCKY_SIM_TRACE").is_some() {
            eprintln!("    > {action}");
        }
        let outcome = match simulation.step(index, &action).await {
            Ok(outcome) => outcome,
            Err(error) => format!("refused: {}", short(&error.to_string())),
        };
        report
            .steps
            .push(format!("{index:3} {action} -> {outcome}"));
        if std::env::var_os("ROCKY_SIM_TRACE").is_some() {
            eprintln!("{} [{:?}]", report.steps[index], started.elapsed());
        }
        if let Err(violation) = simulation.safety().await {
            report.safety = Some(format!("after step {index}: {violation}"));
            report.coverage = std::mem::take(&mut simulation.coverage);
            simulation.stop().await;
            return Ok(report);
        }
    }
    match simulation.heal(scenario.rounds).await {
        Ok(None) if fixture == Fixture::Relational => {
            match simulation.quiesce(scenario.rounds).await {
                Ok(stuck) => report.liveness = stuck,
                Err(violation) => report.safety = Some(format!("at quiescence: {violation}")),
            }
        }
        Ok(stuck) => report.liveness = stuck,
        Err(violation) => report.safety = Some(format!("while healing: {violation}")),
    }
    report.scope_local = simulation.scope_local;
    report.coverage = std::mem::take(&mut simulation.coverage);
    report.states = simulation.states().await;
    simulation.stop().await;
    Ok(report)
}

fn short(message: &str) -> String {
    message.chars().take(160).collect()
}

impl Simulation {
    async fn setup(&mut self) -> Result<(), Failure> {
        self.members[0].create(&self.hub).await?;
        self.invite(0, 1).await?;
        self.sync(1).await?;
        if self.fixture == Fixture::Relational {
            for list in LISTS {
                self.step(0, &Action::Partition(list.into())).await?;
            }
            self.sync(1).await?;
        }
        Ok(())
    }
    fn running(&self, node: usize) -> Result<&Member, Failure> {
        let member = &self.members[node];
        match (&member.running, member.revoked) {
            (Some(_), false) => Ok(member),
            _ => Err("skipped: not running".into()),
        }
    }
    async fn step(&mut self, index: usize, action: &Action) -> Result<String, Failure> {
        match action {
            Action::Write(node) => {
                let key = format!("w{index}");
                let elsewhere = self.waits_elsewhere(*node).await;
                self.running(*node)?.write(&key, index as u64).await?;
                self.scope_local += usize::from(elsewhere);
                Ok("written".into())
            }
            Action::WriteAhead(node, by) => {
                let member = self.running(*node)?;
                rocky_secure::skew_clock(by.as_secs());
                let written = member.write(&format!("w{index}"), index as u64).await;
                rocky_secure::skew_clock(0);
                written.map(|()| "written".into())
            }
            Action::Ahead(node) => {
                let member = self.running(*node)?;
                let running = member
                    .running
                    .as_ref()
                    .and_then(|running| running.node.upgrade())
                    .ok_or("the node is not running")?;
                let frames: u64 = running
                    .blockers()
                    .await?
                    .iter()
                    .map(|blocker| match blocker.kind {
                        BlockerKind::ClockAhead { count, .. } => count,
                        _ => 0,
                    })
                    .sum();
                Ok(format!(
                    "{frames} ahead, {} keys",
                    member.keys().await?.len()
                ))
            }
            Action::Clock(node, by) => {
                self.members[*node]
                    .ahead
                    .store(by.as_secs(), std::sync::atomic::Ordering::Relaxed);
                Ok("set".into())
            }
            Action::Alerts(node) => {
                let status = self.running(*node)?.database()?.status().await?;
                let alerts = status.alerts;
                Ok(self.alerts(&alerts))
            }
            Action::Watch(node) => {
                let mut changes = self.database(*node)?.status_changes();
                let watch = tokio::task::spawn_local(async move {
                    let before = changes.next().await?.alerts;
                    for _ in 0..MAX_STATUS_WAKES {
                        let status = changes.next().await?;
                        if status.alerts != before {
                            return Ok(status.alerts);
                        }
                    }
                    Err("the alerts did not change".into())
                });
                self.watches.insert(*node, watch);
                Ok("watching".into())
            }
            Action::Woken(node) => {
                let watch = self.watches.remove(node).ok_or("no watch")?;
                let alerts = rocky_platform::time::timeout(ALERT_WATCH_TIMEOUT, watch)
                    .await
                    .map_err(|_| "not woken")???;
                Ok(self.alerts(&alerts))
            }
            Action::Trust(node) => {
                self.invite_as(0, *node, crate::Access::Full).await?;
                let identity = self.database(*node)?.status().await?.authority.principal;
                let root = self
                    .running(0)?
                    .root
                    .clone()
                    .ok_or("member 0 holds no root")?;
                self.database(0)?
                    .make_admin(crate::PersonId::new(identity.parse()?), root)
                    .await?;
                let creator = self.database(0)?.status().await?;
                for (member, identity) in
                    [(0, creator.authority.principal), (*node, identity.clone())]
                {
                    let name = self.members[member].name.clone();
                    self.identities.insert(identity, name);
                }
                self.database(*node)?.renew().await?;
                Ok("trusted".into())
            }
            Action::StaleAfter(node, after) => {
                self.running(*node)?
                    .database()?
                    .set_trusted_stale_after(after.as_secs())
                    .await?;
                Ok("set".into())
            }
            Action::Mesh(node) => {
                let status = self.database(*node)?.status().await?;
                Ok(self.mesh(&status.mesh))
            }
            Action::WatchMesh(node, device) => {
                let mut changes = self.database(*node)?.status_changes();
                let device = self.members[*device]
                    .node_key
                    .clone()
                    .ok_or("the device has no key")?;
                let watch = tokio::task::spawn_local(async move {
                    let entry_without_times = |mesh: &Option<Vec<crate::MeshDevice>>| {
                        mesh.iter()
                            .flatten()
                            .find(|entry| entry.node == device)
                            .map(|entry| {
                                let report = crate::HealthReport {
                                    produced_at_epoch_seconds: 0,
                                    ..entry.report.clone()
                                };
                                (report, entry.silent)
                            })
                    };
                    let before = entry_without_times(&changes.next().await?.mesh);
                    for _ in 0..MAX_STATUS_WAKES {
                        let status = changes.next().await?;
                        if entry_without_times(&status.mesh) != before {
                            return Ok(status.mesh);
                        }
                    }
                    Err("the device's health did not change".into())
                });
                self.mesh_watches.insert(*node, watch);
                Ok("watching".into())
            }
            Action::WokenMesh(node) => {
                let watch = self.mesh_watches.remove(node).ok_or("no watch")?;
                let mesh = rocky_platform::time::timeout(MESH_WATCH_TIMEOUT, watch)
                    .await
                    .map_err(|_| "not woken")???;
                Ok(self.mesh(&mesh))
            }
            Action::Received(node) => {
                let running = self.members[*node]
                    .running
                    .as_ref()
                    .and_then(|running| running.node.upgrade())
                    .ok_or("the node is not running")?;
                let received = running
                    .reports_received
                    .load(std::sync::atomic::Ordering::Relaxed);
                Ok(format!(
                    "{} reports",
                    if received == 0 { "no" } else { "some" }
                ))
            }
            Action::Probe(node) => {
                let member = self.running(*node)?;
                let key = format!("w{index}");
                let (synced, written) =
                    tokio::join!(member.sync(), member.write(&key, index as u64));
                self.synced[*node] = true;
                written?;
                synced?;
                Ok("probed".into())
            }
            Action::Sync(node) => self.sync(*node).await,
            Action::SyncPair(node, peer) => {
                self.running(*node)?;
                let others: Vec<String> = (0..self.members.len())
                    .filter(|other| other != node && other != peer)
                    .map(node_name)
                    .collect();
                let name = node_name(*node);
                let isolated: Vec<&String> = others
                    .iter()
                    .filter(|other| !self.hub.is_down(&name, other))
                    .collect();
                for other in &isolated {
                    self.hub.set_down(&name, other, true);
                }
                let result = self.sync(*node).await;
                for other in &isolated {
                    self.hub.set_down(&name, other, false);
                }
                result
            }
            Action::Faults(a, b, faults) => {
                self.hub.set_faults(&node_name(*a), &node_name(*b), *faults);
                Ok("set".into())
            }
            Action::Sever(a, b) => {
                self.hub.set_down(&node_name(*a), &node_name(*b), true);
                Ok("down".into())
            }
            Action::Heal(a, b) => {
                self.hub.set_down(&node_name(*a), &node_name(*b), false);
                Ok("up".into())
            }
            Action::NetworkChange => {
                self.hub.network_changed();
                Ok("signalled".into())
            }
            Action::Advance(by) => {
                tokio::time::sleep(*by).await;
                Ok("advanced".into())
            }
            Action::Restart(node) => {
                self.running(*node)?;
                self.members[*node].close().await?;
                self.reopen(*node).await
            }
            Action::Crash(node, point) => {
                self.running(*node)?;
                let fired = match point {
                    Some(point) => self.crash_at(*node, *point, index).await,
                    None => "killed".into(),
                };
                self.members[*node].crash().await;
                self.lose_unsent(*node);
                Ok(format!("{fired}, {}", self.reopen(*node).await?))
            }
            Action::Seal(node) => {
                let sealed = self.running(*node)?.database()?.seal().await?;
                Ok(format!("sealed {}", short_hash(&sealed.tip)))
            }
            Action::Compact(node) => {
                let report = self
                    .running(*node)?
                    .database()
                    .expect("running crash fixture")
                    .compact(0)
                    .await?;
                Ok(format!("compacted, pruned {}", report.pruned))
            }
            Action::Partition(list) => {
                self.running(0)?
                    .database()?
                    .create_partition(&crate::Partition::new(
                        "list",
                        vec![crate::SqlValue::Text(list.clone())],
                    ))
                    .await?;
                self.lists.push(list.clone());
                Ok("created".into())
            }
            Action::WritePart(node, list) => {
                let elsewhere = self.waits_elsewhere(*node).await;
                self.running(*node)?
                    .write_part(list, &format!("p{index}"), index as u64)
                    .await?;
                self.scope_local += usize::from(elsewhere);
                Ok("written".into())
            }
            Action::Invite(by, node) => self.invite(*by, *node).await,
            Action::Revoke(node) => {
                let identity = self.running(*node)?.database()?.me().await?.person.key();
                self.running(0)?
                    .database()?
                    .remove(crate::PersonId::new(identity), crate::Removal::Departed)
                    .await?;
                self.members[*node].revoked = true;
                self.members[*node].close().await?;
                self.lose_unsent(*node);
                Ok("revoked".into())
            }
            Action::Change(node, change) => self.change(*node, index, change).await,
            Action::Offline(node) | Action::Online(node) => {
                let online = matches!(action, Action::Online(_));
                self.database(*node)?
                    .set_sync(if online {
                        crate::SyncMode::Manual
                    } else {
                        crate::SyncMode::Off
                    })
                    .await?;
                Ok(if online { "online" } else { "offline" }.into())
            }
        }
    }
    fn count(&mut self, what: &str) {
        *self.coverage.entry(what.into()).or_default() += 1;
    }
    fn database(&self, node: usize) -> Result<Database, Failure> {
        let member = self.running(node)?;
        Ok(member
            .running
            .as_ref()
            .ok_or("not running")?
            .database
            .clone())
    }
    fn lose_unsent(&mut self, node: usize) {
        for authored in &self.authored {
            if authored.node == node {
                self.may_vanish.insert(authored.token);
            }
        }
    }
    async fn change(
        &mut self,
        node: usize,
        index: usize,
        change: &Change,
    ) -> Result<String, Failure> {
        let database = self.database(node)?;
        let (authored, committed) = relational::author(&database, node, index, change).await?;
        let token = authored.token;
        self.authored.push(authored);
        match committed {
            Ok(()) => Ok("committed".into()),
            Err(error) => {
                self.may_vanish.insert(token);
                Err(error)
            }
        }
    }
    async fn waits_elsewhere(&self, node: usize) -> bool {
        let Some(running) = self.members[node]
            .running
            .as_ref()
            .and_then(|running| running.node.upgrade())
        else {
            return false;
        };
        running
            .blockers()
            .await
            .unwrap_or_default()
            .iter()
            .any(|blocker| {
                matches!(
                    blocker.kind,
                    BlockerKind::SealRebootstrap { .. }
                        | BlockerKind::StagedImport { sealed: false, .. }
                )
            })
    }
    fn mesh(&self, mesh: &Option<Vec<crate::MeshDevice>>) -> String {
        let Some(devices) = mesh else {
            return "mesh none".into();
        };
        let mut described: Vec<String> = devices
            .iter()
            .map(|device| {
                let mut text = format!(
                    "{}: {}",
                    self.name(&device.node),
                    self.alerts(&device.report.alerts)
                );
                for blocker in &device.report.blockers {
                    text += &format!(", {} {}", blocker.kind, blocker.count);
                }
                if let Some(via) = &device.via {
                    text += &format!(", via {}", self.name(via));
                }
                if device.silent {
                    text += ", silent";
                }
                text
            })
            .collect();
        described.sort();
        format!("mesh {described:?}")
    }
    fn name(&self, key: &str) -> String {
        self.members
            .iter()
            .find(|member| member.node_key.as_deref() == Some(key))
            .map_or("?".into(), |member| member.name.clone())
    }
    fn alerts(&self, alerts: &[crate::Alert]) -> String {
        let name = |key: &str| {
            self.members
                .iter()
                .find(|member| member.node_key.as_deref() == Some(key))
                .map_or("?".into(), |member| member.name.clone())
        };
        let mut described: Vec<String> = alerts
            .iter()
            .map(|alert| match &alert.kind {
                crate::AlertKind::ClockAheadOfPeer { node, seconds } => {
                    format!("ahead of {} {} min", name(node), seconds / 60)
                }
                crate::AlertKind::PeerClockAhead { node, seconds } => {
                    format!("{} ahead {} min", name(node), seconds / 60)
                }
                crate::AlertKind::MembershipFork { identity, sequence } => {
                    format!("membership fork {identity} at {sequence}")
                }
                crate::AlertKind::SealSkipped { reason } => format!("seal skipped {reason}"),
                crate::AlertKind::TipStatementsPastCut {
                    node, statements, ..
                } => format!("{} past cut {statements}", name(node)),
                crate::AlertKind::TipChainEquivocation { node, sequence, .. } => {
                    format!("{} forked at {sequence}", name(node))
                }
                crate::AlertKind::StoreSuperseded { node, .. } => {
                    format!("{} superseded", name(node))
                }
                crate::AlertKind::PeerStoreRegressed { node, .. } => {
                    format!("{} regressed", name(node))
                }
                crate::AlertKind::TrustedNodeLinked { node, .. } => {
                    format!("{} linked", name(node))
                }
                crate::AlertKind::TrustedNodeUnlinked { node, .. } => {
                    format!("{} unlinked", name(node))
                }
                crate::AlertKind::TrustedIdentityStale { identity, .. } => format!(
                    "{} stale",
                    self.identities.get(identity).map_or("?", String::as_str)
                ),
            })
            .collect();
        described.sort();
        format!("alerts {described:?}")
    }
    async fn sync(&mut self, node: usize) -> Result<String, Failure> {
        let report = self.running(node)?.sync().await;
        self.synced[node] = true;
        let report = report?;

        let peers = report.peers.len();
        let completed = report
            .peers
            .iter()
            .filter(|peer| {
                peer.attempts.iter().any(|attempt| {
                    attempt.operation == crate::AttemptOperation::Reconcile
                        && attempt.outcome == Some(crate::AttemptOutcome::Completed)
                })
            })
            .count();
        Ok(format!("reconciled {completed}/{peers}"))
    }
    async fn crash_at(
        &mut self,
        node: usize,
        point: rocky_secure::CrashPoint,
        index: usize,
    ) -> String {
        use rocky_secure::CrashPoint::*;
        let Some(replica) = self.members[node]
            .running
            .as_ref()
            .and_then(|running| running.node.upgrade())
            .map(|node| node.replica.clone())
        else {
            return "not running".into();
        };
        replica.arm_crash(Some(point)).await;
        let member = &self.members[node];
        let _ = match point {
            Prune | Compact | BasePrepare | BasePrepared | BasePublished | BaseCleanup
            | BaseCleaned => member
                .database()
                .expect("running crash fixture")
                .compact(0)
                .await
                .map(drop)
                .map_err(Failure::from),
            GroupBeforeCommit | GroupAfterCommit if self.fixture == Fixture::Relational => {
                let change = Change::Board {
                    list: LISTS[index % 2].into(),
                    code: (index % 4) as u8,
                    cap: 6,
                };
                self.change(node, index, &change).await.map(drop)
            }
            GroupBeforeCommit | GroupAfterCommit => {
                member.write(&format!("w{index}"), index as u64).await
            }
            Import | Switch | Settle | Evict | Apply | Rewind | Redo | UndoDeletion => {
                self.sync(node).await.map(drop)
            }
        };
        let fired = replica.crashed().await;
        drop(replica);
        match fired {
            Some(_) => {
                self.count(&format!("crash {point:?}"));
                format!("{point:?} fired")
            }
            None => format!("{point:?} not reached"),
        }
    }
    async fn invite(&mut self, by: usize, node: usize) -> Result<String, Failure> {
        let writer = crate::Access::Full;
        self.invite_as(by, node, writer).await
    }
    async fn invite_as(
        &mut self,
        by: usize,
        node: usize,
        profile: crate::Access,
    ) -> Result<String, Failure> {
        if self.members[node].node_key.is_some() {
            return Err("skipped: already a member".into());
        }
        let code = self
            .running(by)?
            .database()?
            .invite(crate::Invite::new(profile))
            .await?
            .code;
        let hub = self.hub.clone();
        self.members[node].join(&hub, &code).await?;
        self.synced[node] = false;
        self.introduce(node).await;
        Ok("joined".into())
    }
    async fn introduce(&mut self, node: usize) {
        let Ok(contact) = self.members[node].contact().await else {
            return;
        };
        for other in 0..self.members.len() {
            if other == node || self.running(other).is_err() {
                continue;
            }
            let Ok(theirs) = self.members[other].contact().await else {
                continue;
            };
            let _ = self.members[other]
                .database()
                .expect("running peer")
                .add_peer(&contact)
                .await;
            let _ = self.members[node]
                .database()
                .expect("running peer")
                .add_peer(&theirs)
                .await;
        }
    }
    async fn reopen(&mut self, node: usize) -> Result<String, Failure> {
        self.synced[node] = false;
        let hub = self.hub.clone();
        self.members[node].open(&hub).await?;
        Ok("reopened".into())
    }
    async fn states(&self) -> Vec<String> {
        let mut states = Vec::new();
        for member in &self.members {
            let Some(node) = member
                .running
                .as_ref()
                .and_then(|running| running.node.upgrade())
            else {
                states.push(format!("{}: closed", member.name));
                continue;
            };
            let mut scopes = node.scopes();
            scopes.sort();
            let mut text = format!("{:?}", member.node_key);
            for scope in scopes {
                let _ = write!(
                    text,
                    "\n{scope:?} {:?} {:?}",
                    node.replica
                        .frontier(scope)
                        .await
                        .map_err(|error| error.to_string()),
                    node.replica
                        .snapshot(scope)
                        .await
                        .map(|snapshot| snapshot.rows)
                        .map_err(|error| error.to_string()),
                );
            }
            let _ = write!(
                text,
                "\n{:?}",
                node.replica
                    .replayed_positions()
                    .await
                    .map_err(|error| error.to_string())
            );
            states.push(format!(
                "{}: {}",
                member.name,
                data_encoding::HEXLOWER.encode(&rocky_core::commitment(
                    "rocky.simulation.state",
                    text.as_bytes()
                ))
            ));
        }
        states
    }
    async fn stop(&mut self) {
        for member in &mut self.members {
            let _ = member.close().await;
        }
    }

    async fn safety(&mut self) -> Result<(), String> {
        if self.fixture == Fixture::Relational {
            self.harvest().await?;
            self.relational_safety().await?;
        }
        for (index, member) in self.members.iter().enumerate() {
            let Some(node) = member
                .running
                .as_ref()
                .and_then(|running| running.node.upgrade())
            else {
                continue;
            };
            audit_node(&node, true)
                .await
                .map_err(|error| format!("{} audit: {error}", member.name))?;
            let blockers = node.blockers().await.map_err(|error| error.to_string())?;
            if self.synced[index]
                && let Some(blocker) = blockers.iter().find(|blocker| {
                    matches!(
                        blocker.kind,
                        BlockerKind::PendingFrames {
                            reason: WaitReason::Unexamined,
                            ..
                        }
                    )
                })
            {
                return Err(format!(
                    "{} has pending frames without a wait reason: {blocker:?}",
                    member.name
                ));
            }
            drop(node);
            let rows_of_a_write_commit_together = self.fixture == Fixture::Relational;
            if rows_of_a_write_commit_together {
                continue;
            }
            if let Ok(keys) = member.keys().await {
                let keys: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
                for key in &keys {
                    let (write, half) = key.rsplit_once('-').unwrap_or((key, ""));
                    let other = format!("{write}-{}", if half == "a" { "b" } else { "a" });
                    if !keys.contains(other.as_str()) {
                        return Err(format!("{} holds {key} without {other}", member.name));
                    }
                }
            }
        }
        Ok(())
    }

    async fn heal(&mut self, rounds: usize) -> Result<Option<Stuck>, String> {
        let names: Vec<String> = (0..self.members.len()).map(node_name).collect();
        for a in &names {
            for b in &names {
                if a < b {
                    self.hub.set_down(a, b, false);
                    self.hub.set_faults(a, b, Default::default());
                }
            }
        }
        let live: Vec<usize> = (0..self.members.len())
            .filter(|node| self.members[*node].node_key.is_some() && !self.members[*node].revoked)
            .collect();
        for node in &live {
            if self.members[*node].running.is_none() {
                self.reopen(*node)
                    .await
                    .map_err(|error| error.to_string())?;
            } else if self.fixture == Fixture::Relational
                && let Ok(database) = self.database(*node)
            {
                let _ = database.set_sync(rocky_sdk::SyncMode::Manual).await;
            }
        }
        for node in &live {
            self.introduce(*node).await;
        }
        let mut last: Option<Stuck> = None;
        for round in 0..rounds {
            for node in &live {
                let _ = self.sync(*node).await;
            }
            tokio::time::sleep(RECOVERY_DRIVER_SETTLE).await;
            self.safety().await?;
            let mut stuck = self.convergence(&live).await;
            if stuck.count == 0 {
                return self.audited(&live).await.map(|()| None);
            }
            stuck.described = format!("after round {round}: {}", stuck.described);
            if last.as_ref().is_some_and(|last| stuck.count > last.count) {
                stuck.classes.insert("blockers grew".into());
                return Ok(Some(stuck));
            }
            last = Some(stuck);
        }
        Ok(last)
    }
    async fn audited(&self, live: &[usize]) -> Result<(), String> {
        for node in live {
            let member = &self.members[*node];
            let Some(running) = member
                .running
                .as_ref()
                .and_then(|running| running.node.upgrade())
            else {
                continue;
            };
            audit_node(&running, false)
                .await
                .map_err(|error| format!("{} audit after healing: {error}", member.name))?;
        }
        Ok(())
    }
    async fn convergence(&self, live: &[usize]) -> Stuck {
        let mut stuck = Stuck::default();
        let mut tables: Vec<(String, Vec<String>)> = Vec::new();
        for node in live {
            let member = &self.members[*node];
            let Some(running) = member
                .running
                .as_ref()
                .and_then(|running| running.node.upgrade())
            else {
                stuck.add("not running", format!("{} is not running", member.name));
                continue;
            };
            let blockers: Vec<Blocker> = running.blockers().await.unwrap_or_default();
            let keeps_bootstrap_intent = running.bootstrap_intent.exists().await;
            drop(running);
            for blocker in blockers {
                stuck.add(
                    &format!(
                        "{} by {}",
                        variant(&blocker.kind),
                        variant(&blocker.clears_by)
                    ),
                    format!(
                        "{} {} {:?} by {:?}",
                        member.name, blocker.scope, blocker.kind, blocker.clears_by
                    ),
                );
            }
            if keeps_bootstrap_intent {
                stuck.add(
                    "bootstrap marker",
                    format!("{} keeps its bootstrap marker", member.name),
                );
            }
            let keys = match self.fixture {
                Fixture::Items => member.keys().await,
                Fixture::Relational => self.snapshot(*node).await.map(|lists| {
                    lists
                        .iter()
                        .map(|(list, rows)| format!("{list} {rows:?}"))
                        .collect()
                }),
            };
            match keys {
                Ok(keys) => tables.push((member.name.clone(), keys)),
                Err(error) => stuck.add(
                    "unreadable",
                    format!("{} cannot read: {}", member.name, short(&error.to_string())),
                ),
            }
        }
        if self.fixture == Fixture::Relational {
            let mut lists: BTreeMap<String, Vec<(&String, &String)>> = BTreeMap::new();
            for (name, held) in &tables {
                for rows in held {
                    let list = rows.split(' ').next().unwrap_or_default().to_owned();
                    lists.entry(list).or_default().push((name, rows));
                }
            }
            for (list, holders) in lists {
                if holders.iter().any(|(_, rows)| *rows != holders[0].1) {
                    let names: Vec<&String> = holders.iter().map(|(name, _)| *name).collect();
                    stuck.add("tables differ", format!("{list} differs among {names:?}"));
                }
            }
            let mut seals = BTreeMap::new();
            for node in live {
                if let Ok(known) = self.canonical_seals(*node).await {
                    seals.insert(node_name(*node), known);
                }
            }
            if seals.values().collect::<BTreeSet<_>>().len() > 1 {
                stuck.add("seals differ", format!("last seals {seals:?}"));
            }
            return stuck;
        }
        if let Some((first, keys)) = tables.first() {
            for (name, other) in &tables[1..] {
                if other != keys {
                    stuck.add(
                        "tables differ",
                        format!("{name} holds {} rows, {first} {}", other.len(), keys.len()),
                    );
                }
            }
        }
        stuck
    }
}

impl Simulation {
    async fn holdings(&self, node: usize) -> Result<BTreeSet<String>, Failure> {
        let member = self.running(node)?;
        let mut held = BTreeSet::new();
        for list in &self.lists {
            let status = member
                .database()?
                .partition_status(&crate::Partition::new(
                    "list",
                    vec![crate::SqlValue::Text(list.clone())],
                ))
                .await?;
            let tables = status.tables;
            if !tables.is_empty()
                && tables
                    .iter()
                    .all(|table| table.holding == Some(rocky_secure::Holding::Held))
            {
                held.insert(list.clone());
            }
        }
        Ok(held)
    }
    async fn under_base(&self, node: usize, authored: &Authored) -> Result<bool, Failure> {
        let Some(((_, stamp), _)) = self.positions.get(&authored.token) else {
            return Ok(false);
        };
        let member = self.running(node)?;
        let replica = member
            .running
            .as_ref()
            .and_then(|running| running.node.upgrade())
            .ok_or("not running")?
            .replica
            .clone();
        for (table, list) in &authored.scopes {
            let status = member
                .database()?
                .partition_status(&crate::Partition::new(
                    "list",
                    vec![crate::SqlValue::Text(list.clone())],
                ))
                .await?;
            let Some(scope) = status
                .tables
                .iter()
                .find(|entry| entry.table == *table)
                .and_then(|entry| entry.scope)
            else {
                return Ok(false);
            };
            if !replica.base_covers(scope, stamp).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    async fn snapshot(
        &self,
        node: usize,
    ) -> Result<BTreeMap<String, relational::ListRows>, Failure> {
        let database = self.database(node)?;
        let mut lists = BTreeMap::new();
        for list in self.holdings(node).await? {
            let rows = relational::list_rows(&database, &list).await?;
            lists.insert(list, rows);
        }
        Ok(lists)
    }
    async fn canonical_seals(&self, node: usize) -> Result<BTreeSet<String>, Failure> {
        let inspection = self.running(node)?.database()?.inspect_tips().await?;
        Ok(inspection
            .seals
            .iter()
            .filter(|seal| seal.canonical)
            .map(|seal| short_hash(&seal.id))
            .collect())
    }
    async fn harvest(&mut self) -> Result<(), String> {
        for node in 0..self.members.len() {
            let Some(running) = self.members[node]
                .running
                .as_ref()
                .and_then(|running| running.node.upgrade())
            else {
                continue;
            };
            let blockers = running.blockers().await.unwrap_or_default();
            for blocker in &blockers {
                match blocker.kind {
                    BlockerKind::SealRebootstrap { .. } => self.count("rebootstrap waits"),
                    BlockerKind::StagedImport { .. } => self.count("import waits"),
                    _ => {}
                }
            }
            let positions = running
                .replica
                .replayed_positions()
                .await
                .map_err(|error| error.to_string())?;
            drop(running);
            for (txn, (position, sealed)) in positions {
                let token = TransactionToken(txn);
                match self.positions.get_mut(&token) {
                    Some((known, true)) if sealed && known.0 != position.0 => {
                        return Err(format!(
                            "{} places a sealed transaction in era {}, another in era {}",
                            node_name(node),
                            position.0,
                            known.0
                        ));
                    }
                    Some((known, true)) if sealed => known.1 = known.1.clone().min(position.1),
                    Some((_, true)) => {}
                    Some((known, was_sealed)) if known.0 == position.0 => {
                        known.1 = known.1.clone().min(position.1);
                        *was_sealed = sealed;
                    }
                    _ => {
                        self.positions.insert(token, (position, sealed));
                    }
                }
            }
        }
        Ok(())
    }
    async fn relational_safety(&self) -> Result<(), String> {
        for node in 0..self.members.len() {
            let Ok(database) = self.database(node) else {
                continue;
            };
            let Ok(held) = self.holdings(node).await else {
                continue;
            };
            for list in held {
                if let Ok(found) = relational::violations(&database, &list).await
                    && !found.is_empty()
                {
                    return Err(format!("{} shows {found:?}", node_name(node)));
                }
            }
        }
        Ok(())
    }
    async fn quiesce(&mut self, rounds: usize) -> Result<Option<Stuck>, String> {
        if self.members[0].running.is_some() && !self.members[0].revoked {
            let sealed = self
                .step(usize::MAX, &Action::Seal(0))
                .await
                .map_err(|error| format!("final seal: {error}"))?;
            if std::env::var_os("ROCKY_SIM_TRACE").is_some() {
                eprintln!("final seal: {sealed}");
            }
        }
        if let Some(stuck) = self.heal(rounds).await? {
            return Ok(Some(stuck));
        }
        let live = self.live();
        for node in &live {
            self.members[*node]
                .close()
                .await
                .map_err(|error| error.to_string())?;
            self.reopen(*node)
                .await
                .map_err(|error| error.to_string())?;
        }
        if let Some(stuck) = self.heal(rounds).await? {
            return Ok(Some(stuck));
        }
        self.harvest().await?;
        self.invariants(&live).await.map(|()| None)
    }
    fn live(&self) -> Vec<usize> {
        (0..self.members.len())
            .filter(|node| self.members[*node].node_key.is_some() && !self.members[*node].revoked)
            .collect()
    }
    async fn invariants(&mut self, live: &[usize]) -> Result<(), String> {
        fn text(error: impl std::fmt::Display) -> String {
            error.to_string()
        }
        let mut outcomes: BTreeMap<(usize, TransactionToken), Option<TransactionOutcome>> =
            BTreeMap::new();
        for node in live {
            let database = self.database(*node).map_err(text)?;
            for authored in &self.authored {
                let outcome = database
                    .transaction_outcome(authored.token)
                    .await
                    .map_err(text)?;
                outcomes.insert((*node, authored.token), outcome);
            }
        }
        let mut included = Vec::new();
        for authored in &self.authored {
            let known = live
                .iter()
                .any(|node| outcomes[&(*node, authored.token)].is_some());
            match known {
                true => included.push(authored),
                false if self.may_vanish.contains(&authored.token) => {}
                false => {
                    return Err(format!(
                        "step {}'s committed transaction has no outcome anywhere",
                        authored.step
                    ));
                }
            }
        }
        let positions: BTreeMap<TransactionToken, Position> = self
            .positions
            .iter()
            .map(|(token, (position, _))| (*token, position.clone()))
            .collect();
        let lists: BTreeSet<String> = self.lists.iter().cloned().collect();
        let reference = relational::replay(&included, &positions, &lists)?;
        let dropped = reference
            .outcomes
            .values()
            .filter(|outcome| !matches!(outcome, TransactionOutcome::Applied { .. }))
            .count();
        let applied = reference.outcomes.len() - dropped;
        let included: Vec<Authored> = included.into_iter().cloned().collect();
        *self.coverage.entry("applied".into()).or_default() += applied;
        *self.coverage.entry("dropped".into()).or_default() += dropped;
        for node in live {
            let name = node_name(*node);
            let database = self.database(*node).map_err(text)?;
            let held = self.holdings(*node).await.map_err(text)?;
            *self.coverage.entry("lists held".into()).or_default() += held.len();
            for list in &held {
                let rows = relational::list_rows(&database, list).await.map_err(text)?;
                if rows != reference.lists[list] {
                    return Err(format!(
                        "{name} holds {list} as {rows:?}, the reference as {:?}",
                        reference.lists[list]
                    ));
                }
                let found = relational::violations(&database, list)
                    .await
                    .map_err(text)?;
                if !found.is_empty() {
                    return Err(format!("{name} shows {found:?}"));
                }
            }
            for authored in &included {
                let outcome = &outcomes[&(*node, authored.token)];
                if authored.node == *node && outcome.is_none() {
                    return Err(format!(
                        "{name} lost the outcome of its step {} after restarting",
                        authored.step
                    ));
                }
                if outcome.is_none()
                    && authored.node != *node
                    && authored.lists.is_subset(&held)
                    && self.under_base(*node, authored).await.map_err(text)?
                {
                    self.count("outcome under a base");
                    continue;
                }
                if authored.lists.is_subset(&held)
                    && outcome.as_ref() != reference.outcomes.get(&authored.token)
                {
                    return Err(format!(
                        "{name} has {outcome:?} for step {}, the reference {:?}",
                        authored.step,
                        reference.outcomes.get(&authored.token)
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Stuck {
    count: usize,
    pub(super) classes: BTreeSet<String>,
    pub(super) described: String,
}
impl Stuck {
    fn add(&mut self, class: &str, described: String) {
        self.count += 1;
        self.classes.insert(class.to_owned());
        let _ = write!(self.described, "{described}; ");
    }
}

async fn audit_node(
    node: &crate::node::Node,
    allow_blocked: bool,
) -> Result<(), rocky_secure::Error> {
    let scopes = node.scopes();
    if scopes.is_empty() {
        return Ok(());
    }
    match node.replica.audit_scopes(&scopes).await {
        Ok(_) => Ok(()),
        Err(error) if allow_blocked && tolerated(&error) => {
            for scope in scopes {
                if let Err(error) = node.replica.audit(scope).await
                    && !tolerated(&error)
                {
                    return Err(error);
                }
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn tolerated(error: &rocky_secure::Error) -> bool {
    matches!(error, rocky_secure::Error::Blocked(_))
}

fn variant(value: &impl std::fmt::Debug) -> String {
    let debug = format!("{value:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn short_hash(value: &str) -> String {
    value.chars().take(8).collect()
}
