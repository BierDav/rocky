//! Runs one seed: setup, the schedule with safety checked after every
//! step, then healing and the liveness rounds.
use super::{
    cluster::Member,
    schedule::{Action, Scenario, node_name, plan},
};
use crate::{Failure, transport::memory::Hub};
use rocky_core::{Blocker, BlockerKind, WaitReason};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fmt::Write as _, sync::Arc};

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
    members: Vec<Member>,
    /// Members that ran a sync pass since they last opened.
    synced: Vec<bool>,
    scope_local: usize,
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
    let mut simulation = Simulation {
        hub: Arc::new(Hub::seeded(seed)),
        members: (0..scenario.nodes)
            .map(|node| Member::new(root.path(), &node_name(node)))
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
            simulation.stop().await;
            return Ok(report);
        }
    }
    match simulation.heal(scenario.rounds).await {
        Ok(stuck) => report.liveness = stuck,
        Err(violation) => report.safety = Some(format!("while healing: {violation}")),
    }
    report.scope_local = simulation.scope_local;
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
                Ok("revoked".into())
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
            GroupBeforeCommit | GroupAfterCommit => {
                member.write(&format!("w{index}"), index as u64).await
            }
            Import | Switch | Settle | Evict => self.sync(node).await.map(drop),
        };
        let fired = replica.crashed().await;
        drop(replica);
        match fired {
            Some(_) => format!("{point:?} fired"),
            None => format!("{point:?} not reached"),
        }
    }
    async fn invite(&mut self, by: usize, node: usize) -> Result<String, Failure> {
        if self.members[node].node_key.is_some() {
            return Err("skipped: already a member".into());
        }
        let uri = self
            .running(by)?
            .call(json!({"type": "pairing", "profile": {"kind": "writer", "scopes": ["items", "parts"]}}))
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
    async fn safety(&self) -> Result<(), String> {
        for (index, member) in self.members.iter().enumerate() {
            let Some(node) = member
                .running
                .as_ref()
                .and_then(|running| running.node.upgrade())
            else {
                continue;
            };
            for scope in node.scopes() {
                if let Err(error) = node.replica.audit(scope).await
                    && !tolerated(&error)
                {
                    return Err(format!(
                        "{} audit of {}: {error}",
                        member.name,
                        rocky_core::scope_label(&scope)
                    ));
                }
            }
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
            for scope in running.scopes() {
                if let Err(error) = running.replica.audit(scope).await {
                    return Err(format!(
                        "{} audit of {} after healing: {error}",
                        member.name,
                        rocky_core::scope_label(&scope)
                    ));
                }
            }
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
            match member.keys().await {
                Ok(keys) => tables.push((member.name.clone(), keys)),
                Err(error) => stuck.add(
                    "unreadable",
                    format!("{} cannot read: {}", member.name, short(&error.to_string())),
                ),
            }
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
