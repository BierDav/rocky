//! Runs one seed: setup, the schedule with safety checked after every
//! step, then healing and the liveness rounds.
use super::{
    cluster::Member,
    relational::{self, Authored, Position},
    schedule::{Action, Change, Fixture, LISTS, Scenario, node_name, plan},
};
use crate::{Failure, TransactionToken, transport::memory::Hub};
use rocky_core::{Blocker, BlockerKind, WaitReason, replay::TransactionOutcome};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    sync::Arc,
};

/// How one seed ended.
pub(super) struct Report {
    pub(super) seed: u64,
    pub(super) steps: Vec<String>,
    /// A broken invariant: the run fails.
    pub(super) safety: Option<String>,
    /// What still blocked convergence after the liveness rounds.
    pub(super) liveness: Option<Stuck>,
    /// Writes accepted while another scope of the domain waited for a
    /// rebootstrap or an import, which a domain-wide gate refused.
    pub(super) scope_local: usize,
    /// What the run reached: fired crash points, imports, rebootstraps,
    /// evictions, drops, by name.
    pub(super) coverage: BTreeMap<String, usize>,
}
impl Report {
    /// The seed, its steps and what failed, for a failure message.
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
    /// Members that ran a sync pass since they last opened.
    synced: Vec<bool>,
    scope_local: usize,
    /// Relational: the lists created, what was authored, and canonical
    /// positions as replicas reported them.
    lists: Vec<String>,
    authored: Vec<Authored>,
    /// Authored transactions a crash or revocation of their author may
    /// have lost before any peer received them.
    may_vanish: BTreeSet<TransactionToken>,
    positions: BTreeMap<TransactionToken, (Position, bool)>,
    partial: Vec<bool>,
    /// Lists shared with each member: holding one needs its key.
    shared: BTreeSet<(usize, String)>,
    coverage: BTreeMap<String, usize>,
}

/// Runs `seed` on a current-thread runtime with a paused clock; call it
/// inside a `LocalSet`.
pub(super) async fn run(seed: u64, scenario: Scenario) -> Result<Report, Failure> {
    run_steps(seed, scenario, plan(seed, scenario)).await
}
/// Runs a given schedule; the seed still draws the hub's faults.
pub(super) async fn run_steps(
    seed: u64,
    scenario: Scenario,
    steps: Vec<Action>,
) -> Result<Report, Failure> {
    let root = tempfile::tempdir()?;
    let fixture = scenario.fixture;
    let mut simulation = Simulation {
        hub: Arc::new(Hub::seeded(seed)),
        fixture,
        lists: Vec::new(),
        authored: Vec::new(),
        may_vanish: BTreeSet::new(),
        positions: BTreeMap::new(),
        partial: vec![false; scenario.nodes],
        shared: BTreeSet::new(),
        coverage: BTreeMap::new(),
        members: (0..scenario.nodes)
            .map(|node| Member::new(root.path(), &node_name(node), fixture.schema()))
            .collect(),
        synced: vec![false; scenario.nodes],
        scope_local: 0,
    };
    let mut report = Report {
        seed,
        steps: Vec::new(),
        safety: None,
        liveness: None,
        scope_local: 0,
        coverage: BTreeMap::new(),
    };
    simulation.setup().await?;
    for (index, action) in steps.into_iter().enumerate() {
        let started = std::time::Instant::now();
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
    simulation.stop().await;
    Ok(report)
}

fn short(message: &str) -> String {
    message.chars().take(160).collect()
}

impl Simulation {
    /// Node `a` creates the domain and follows `main`; `b` joins it.
    async fn setup(&mut self) -> Result<(), Failure> {
        self.members[0].create(&self.hub).await?;
        self.members[0]
            .call(json!({"type": "tips", "action": "create", "name": "main"}))
            .await?;
        self.follow(0, "main").await?;
        self.invite(0, 1).await?;
        self.sync(1).await?;
        self.follow(1, "main").await?;
        if self.fixture == Fixture::Relational {
            for list in LISTS {
                self.step(0, &Action::Partition(list.into())).await?;
                self.step(0, &Action::Share(list.into(), 1)).await?;
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
            Action::Probe(node) => {
                let member = self.running(*node)?;
                let key = format!("w{index}");
                let (synced, written) = tokio::join!(
                    member.call(json!({"type": "sync"})),
                    member.write(&key, index as u64)
                );
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
                let sealed = self
                    .running(*node)?
                    .call(json!({"type": "tips", "action": "seal"}))
                    .await?;
                Ok(format!("sealed {}", short_hash(&sealed["tip"])))
            }
            Action::Compact(node) => {
                let report = self
                    .running(*node)?
                    .call(json!({"type": "tips", "action": "compact"}))
                    .await?;
                Ok(format!("compacted, pruned {}", report["pruned"]))
            }
            Action::Branch(node, bookmark) => self.branch(*node, bookmark).await,
            Action::Follow(node, bookmark) => self.follow(*node, bookmark).await,
            Action::Partition(list) => {
                self.running(0)?
                    .call(json!({"type": "partition_create", "group": "list",
                        "values": [{"type": "text", "value": list}]}))
                    .await?;
                self.lists.push(list.clone());
                Ok("created".into())
            }
            Action::Share(list, node) => {
                let key = self.members[*node]
                    .node_key
                    .clone()
                    .ok_or("skipped: never joined")?;
                self.running(0)?
                    .call(json!({"type": "partition_share", "group": "list",
                        "values": [{"type": "text", "value": list}], "recipients": [key]}))
                    .await?;
                self.shared.insert((*node, list.clone()));
                Ok("shared".into())
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
                let key = self.members[*node]
                    .node_key
                    .clone()
                    .ok_or("skipped: never joined")?;
                self.running(0)?
                    .call(json!({"type": "revoke", "node": key}))
                    .await?;
                self.members[*node].revoked = true;
                self.members[*node].close().await?;
                self.lose_unsent(*node);
                Ok("revoked".into())
            }
            Action::Change(node, change) => self.change(*node, index, change).await,
            Action::Evict(node, list) => {
                if !self.shared.contains(&(*node, list.clone())) {
                    return Err("skipped: never shared".into());
                }
                // A full node must hold everything this one wrote first.
                let _ = self.sync(*node).await;
                let owner = self.members[0].node_key.clone().ok_or("no owner")?;
                let owner = data_encoding::HEXLOWER.decode(owner.as_bytes())?;
                let owner =
                    rocky_credentials::PeerId::of(rocky_credentials::PublicKey::from_bytes(
                        owner.as_slice().try_into().map_err(|_| "owner key")?,
                    )?);
                let member = self.running(*node)?;
                if !self.partial[*node] {
                    // The owner holds every list: eviction waits for it.
                    member
                        .call(json!({"type": "partition_role", "role":
                            {"role": "Partial", "full_nodes": [owner], "custody": 1}}))
                        .await?;
                }
                let partition = |kind: &str| json!({"type": kind, "group": "list", "values": [{"type": "text", "value": list}]});
                member.call(partition("partition_unsubscribe")).await?;
                self.partial[*node] = true;
                let evicted = self
                    .running(*node)?
                    .call(partition("partition_evict"))
                    .await?;
                self.count("evicted");
                Ok(format!("evicted {} rows", evicted["rows"]))
            }
            Action::Hold(node, list) => {
                if !self.shared.contains(&(*node, list.clone())) {
                    return Err("skipped: never shared".into());
                }
                self.running(*node)?
                    .call(json!({"type": "partition_subscribe", "group": "list",
                        "values": [{"type": "text", "value": list}]}))
                    .await?;
                Ok("holding".into())
            }
            Action::Offline(node) | Action::Online(node) => {
                let online = matches!(action, Action::Online(_));
                self.database(*node)?
                    .session()
                    .set_networking_enabled(online)
                    .await?;
                Ok(if online { "online" } else { "offline" }.into())
            }
        }
    }
    fn count(&mut self, what: &str) {
        *self.coverage.entry(what.into()).or_default() += 1;
    }
    fn database(&self, node: usize) -> Result<Arc<crate::Database>, Failure> {
        let member = self.running(node)?;
        Ok(member
            .running
            .as_ref()
            .ok_or("not running")?
            .database
            .clone())
    }
    /// What `node` authored may be gone with it: buffered commits that no
    /// peer received do not survive a crash or a revocation.
    fn lose_unsent(&mut self, node: usize) {
        for authored in &self.authored {
            if authored.node == node {
                self.may_vanish.insert(authored.token);
            }
        }
    }
    /// Authors `change` on `node`, logging what it bound; a commit that
    /// failed is logged as uncertain.
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
    /// Whether a scope of `node` waits for a rebootstrap or an unsealed
    /// import: a write that succeeds then is accepted by a scope-local gate.
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
    async fn sync(&mut self, node: usize) -> Result<String, Failure> {
        let report = self.running(node)?.call(json!({"type": "sync"})).await;
        self.synced[node] = true;
        let report = report?;

        let peers = report["peers"].as_array().map_or(0, Vec::len);
        let completed = report["peers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|peer| {
                peer["attempts"].as_array().is_some_and(|attempts| {
                    attempts.iter().any(|attempt| {
                        attempt["operation"] == "reconcile" && attempt["outcome"] == "completed"
                    })
                })
            })
            .count();
        Ok(format!("reconciled {completed}/{peers}"))
    }
    async fn follow(&mut self, node: usize, bookmark: &str) -> Result<String, Failure> {
        self.running(node)?
            .call(json!({"type": "tips", "action": "follow", "name": bookmark}))
            .await?;
        let member = self.running(node)?;
        if member.keys().await.is_ok() {
            return Ok("following".into());
        }
        let blockers = match member
            .running
            .as_ref()
            .and_then(|running| running.node.upgrade())
        {
            Some(node) => node.blockers().await.unwrap_or_default(),
            None => Vec::new(),
        };
        let kinds: Vec<String> = blockers
            .iter()
            .map(|blocker| variant(&blocker.kind))
            .collect();
        Ok(format!("following, unreadable behind {kinds:?}"))
    }
    async fn branch(&mut self, node: usize, bookmark: &str) -> Result<String, Failure> {
        let member = self.running(node)?;
        let inspection = member
            .call(json!({"type": "tips", "action": "inspect"}))
            .await?;
        let seal = inspection["seals"]
            .as_array()
            .into_iter()
            .flatten()
            .rfind(|seal| seal["canonical"] == true)
            .map(|seal| seal["id"].clone())
            .ok_or("skipped: no seal")?;
        let tip = member
            .call(json!({"type": "tips", "action": "open", "seal": seal, "minimum_in_sync": 1}))
            .await?["tip"]
            .clone();
        member
            .call(json!({"type": "tips", "action": "create", "name": bookmark, "tip": tip}))
            .await?;
        self.follow(node, bookmark).await?;
        Ok(format!("following {}", short_hash(&tip)))
    }
    /// Arms `point` and runs the step that reaches it.
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
            Prune | Compact => member
                .call(json!({"type": "tips", "action": "compact"}))
                .await
                .map(drop),
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
            // Replay points never fire on the simulation's row-frame tables.
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
        if self.members[node].node_key.is_some() {
            return Err("skipped: already a member".into());
        }
        let uri = self
            .running(by)?
            .call(json!({"type": "pairing", "profile": {"kind": "writer", "scopes": self.fixture.scopes()}}))
            .await?["uri"]
            .as_str()
            .ok_or("pairing uri")?
            .to_owned();
        let hub = self.hub.clone();
        self.members[node].join(&hub, &uri).await?;
        self.synced[node] = false;
        self.introduce(node).await;
        Ok("joined".into())
    }
    /// Makes `node` and every running member each other's peers.
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
                .call(json!({"type": "peer_add", "contact": contact}))
                .await;
            let _ = self.members[node]
                .call(json!({"type": "peer_add", "contact": theirs}))
                .await;
        }
    }
    async fn reopen(&mut self, node: usize) -> Result<String, Failure> {
        self.synced[node] = false;
        let hub = self.hub.clone();
        self.members[node].open(&hub).await?;
        Ok("reopened".into())
    }
    async fn stop(&mut self) {
        for member in &mut self.members {
            let _ = member.close().await;
        }
    }

    /// The invariants that hold after every step on every open node.
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
            // Both rows of a write commit together, so neither shows alone.
            if self.fixture == Fixture::Relational {
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

    /// Heals every link, reopens every member, follows `main` everywhere
    /// and runs sync rounds. `Ok(Some(_))` is what still blocks.
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
                let _ = database.session().set_networking_enabled(true).await;
            }
        }
        for node in &live {
            self.introduce(*node).await;
        }
        let mut last: Option<Stuck> = None;
        for round in 0..rounds {
            for node in &live {
                let _ = self.sync(*node).await;
                let before = self.members[*node]
                    .running
                    .as_ref()
                    .and_then(|running| running.node.upgrade());
                let tip = match &before {
                    Some(node) => Some(node.replica.followed_tip().await),
                    None => None,
                };
                let _ = self.follow(*node, "main").await;
                if let Some(running) = before
                    && tip != Some(running.replica.followed_tip().await)
                {
                    // The first exchange used another tip. Service the new
                    // one before measuring recovery on the common path.
                    let _ = self.sync(*node).await;
                }
            }
            // Let the autonomous recovery driver's spacing and timer run
            // on the paused clock, without a new frame or a restart.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
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
    /// Once converged nothing blocks, so every scope on every node audits,
    /// also those written while another scope waited.
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
    /// How far from converged: blockers, bootstrap markers and differing
    /// tables.
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
            if member.directory.join("bootstrap.json").exists() {
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
            // Holdings differ: each list's rows agree among its holders.
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
    /// The lists `node` holds whole: every table of the group held.
    async fn holdings(&self, node: usize) -> Result<BTreeSet<String>, Failure> {
        let member = self.running(node)?;
        let mut held = BTreeSet::new();
        for list in &self.lists {
            let status = member
                .call(json!({"type": "partition_status", "group": "list",
                    "values": [{"type": "text", "value": list}]}))
                .await?;
            let tables = status["tables"].as_array().cloned().unwrap_or_default();
            if !tables.is_empty() && tables.iter().all(|table| table["holding"] == "held") {
                held.insert(list.clone());
            }
        }
        Ok(held)
    }
    /// Whether `node`'s installed bases cover every member scope of
    /// `authored`, at the canonical position replicas reported for it.
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
                .call(json!({"type": "partition_status", "group": "list",
                    "values": [{"type": "text", "value": list}]}))
                .await?;
            let Some(scope) = status["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|entry| entry["table"] == *table)
                .filter(|entry| !entry["scope"].is_null())
            else {
                return Ok(false);
            };
            let scope: rocky_core::ResourceScope = serde_json::from_value(scope["scope"].clone())?;
            if !replica.base_covers(scope, stamp).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// Rows of every list `node` holds.
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
    /// The canonical seals `node` knows, by short id; inspection lists
    /// them in no defined order.
    async fn canonical_seals(&self, node: usize) -> Result<BTreeSet<String>, Failure> {
        let inspection = self
            .running(node)?
            .call(json!({"type": "tips", "action": "inspect"}))
            .await?;
        Ok(inspection["seals"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|seal| seal["canonical"] == true)
            .map(|seal| {
                seal["id"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(8)
                    .collect()
            })
            .collect())
    }
    /// Merges every open replica's canonical positions. A position a seal
    /// covers never changes; two replicas never disagree on one.
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
                // A replica holding some of a bundle's members sees a later
                // earliest stamp: the least seen is the bundle's.
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
    /// No open replica shows a row that breaks a key, check or assertion.
    async fn relational_safety(&self) -> Result<(), String> {
        for node in 0..self.members.len() {
            let Ok(database) = self.database(node) else {
                continue;
            };
            let Ok(held) = self.holdings(node).await else {
                continue;
            };
            for list in held {
                // A list waiting for its history refuses reads.
                if let Ok(found) = relational::violations(&database, &list).await
                    && !found.is_empty()
                {
                    return Err(format!("{} shows {found:?}", node_name(node)));
                }
            }
        }
        Ok(())
    }
    /// After healing: node 0 seals, everyone converges at that seal, then
    /// every member restarts and converges again. Then the invariants.
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
    /// Rows and outcomes against the reference: every holder of a list has
    /// the reference's rows of it, every holder of all of a transaction's
    /// lists its outcome, and every author an outcome for what it wrote.
    async fn invariants(&mut self, live: &[usize]) -> Result<(), String> {
        let text = |error: Failure| error.to_string();
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
                // Another author's transaction a base covers is in the rows
                // only: a replica keeps outcomes only of its own members.
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

/// What kept a seed from converging after healing.
#[derive(Default)]
pub(super) struct Stuck {
    count: usize,
    /// Kinds of what blocks, without hashes: comparable across runs.
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

// All scopes share the global checks. A blocked scope must not hide an
// invalid sibling, so retain individual checks when the batch is blocked.
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

/// An audit a blocker refuses is not a violation; any other failure is.
fn tolerated(error: &rocky_secure::Error) -> bool {
    matches!(error, rocky_secure::Error::Blocked(_))
}

/// An enum value's variant name.
fn variant(value: &impl std::fmt::Debug) -> String {
    let debug = format!("{value:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn short_hash(value: &Value) -> String {
    value.as_str().unwrap_or("?").chars().take(8).collect()
}
