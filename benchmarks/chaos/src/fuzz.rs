//! Seeded random fault schedule over trusted, full and partial nodes, then a
//! heal phase that checks the system recovers. `fuzz <seed> <steps>`.
use crate::cluster::{Node, mesh};
use crate::scenarios::{
    Failure, ITEMS, LISTS, NOTES, Slot, author_all, finding, integrity, root, short, status_data,
    writer_all,
};
use rand::{Rng, SeedableRng, rngs::StdRng, seq::SliceRandom};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Trusted,
    Full,
    Partial,
}
struct Member {
    slot: Slot,
    kind: Kind,
    node: String,
    /// Lists whose key this member was sent.
    lists: BTreeSet<String>,
}
struct World {
    rng: StdRng,
    members: Vec<Member>,
    lists: Vec<String>,
    /// Note keys whose insert was acknowledged, and keys deleted since.
    acked: BTreeSet<String>,
    deleted: BTreeSet<String>,
    next: usize,
    log: Vec<String>,
}

impl World {
    fn alive(&self) -> Vec<usize> {
        (0..self.members.len())
            .filter(|i| self.members[*i].slot.node.is_some())
            .collect()
    }
    fn pick_alive(&mut self) -> Option<usize> {
        let alive = self.alive();
        alive.choose(&mut self.rng).copied()
    }
    fn issuer(&self) -> Option<Arc<Node>> {
        self.members
            .iter()
            .filter(|m| m.kind == Kind::Trusted)
            .find_map(|m| m.slot.node.clone())
    }
    fn note(&mut self, text: String) {
        self.log.push(text);
    }
}

async fn join(world: &mut World, name: &str, kind: Kind) -> Result<(), Failure> {
    let issuer = world.issuer().ok_or("no live issuer to invite")?;
    let profile = if kind == Kind::Trusted {
        json!({"kind":"issuer"})
    } else {
        writer_all()
    };
    let mut slot = Slot::new(name, &world.members[0].slot.root);
    // Inviting can be refused transiently (issuer registry still importing).
    let invitation = issuer
        .call(json!({"type":"pairing","profile":profile}))
        .await
        .map_err(|e| format!("pairing refused: {e}"))?["uri"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let node = slot
        .start(json!({"mode":"join","invitation":invitation}))
        .await?;
    if kind == Kind::Partial {
        let a = issuer.node_id().await;
        let _ = node.call(json!({"type":"partition_role","role":{"role":"Partial","full_nodes":[a],"custody":1}})).await;
    }
    author_all(&node, &issuer).await;
    let id = node.node_id().await;
    let mut lists = BTreeSet::new();
    if kind != Kind::Partial {
        for list in world.lists.clone() {
            if issuer.share(&list, std::slice::from_ref(&id)).await.is_ok() {
                lists.insert(list);
            }
        }
    }
    let mut all: Vec<&Node> = world
        .members
        .iter()
        .filter_map(|m| m.slot.node.as_deref())
        .collect();
    all.push(&node);
    mesh(&all).await;
    world.members.push(Member {
        slot,
        kind,
        node: id,
        lists,
    });
    Ok(())
}

pub async fn run(args: &[String]) -> Result<(), Failure> {
    let seed: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
    let steps: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
    println!("=== fuzz seed {seed} steps {steps}");
    let root = root(&format!("fuzz-{seed}"));
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let a_id = a.n().node_id().await;
    let mut world = World {
        rng: StdRng::seed_from_u64(seed),
        members: vec![Member {
            slot: a,
            kind: Kind::Trusted,
            node: a_id,
            lists: BTreeSet::new(),
        }],
        lists: Vec::new(),
        acked: BTreeSet::new(),
        deleted: BTreeSet::new(),
        next: 0,
        log: Vec::new(),
    };
    // Seed data, so later joins bootstrap something.
    let issuer = world.members[0].slot.n().clone();
    issuer
        .add_notes_batch("seed", 0, 300, 400)
        .await
        .map_err(|e| e.to_string())?;
    for i in 0..300 {
        world.acked.insert(format!("seed{i}"));
    }
    for l in 0..2 {
        let id = issuer
            .add_list(&format!("seedlist{l}"))
            .await
            .map_err(|e| e.to_string())?;
        for i in 0..30 {
            let _ = issuer.add_item(&id, &format!("seeditem{l}.{i}")).await;
        }
        world.lists.push(id.clone());
        world.members[0].lists.insert(id);
    }
    join(&mut world, "t", Kind::Trusted).await?;
    join(&mut world, "f", Kind::Full).await?;
    join(&mut world, "p", Kind::Partial).await?;
    let mut joined = 0;

    for step in 0..steps {
        // Keep the schedule meaningful: two live nodes, one of them an issuer.
        while world.alive().len() < 2 || world.issuer().is_none() {
            let dead: Vec<usize> = (0..world.members.len())
                .filter(|i| world.members[*i].slot.node.is_none())
                .filter(|i| world.issuer().is_some() || world.members[*i].kind == Kind::Trusted)
                .collect();
            let Some(&i) = dead.choose(&mut world.rng) else {
                break;
            };
            let r = world.members[i].slot.resume().await;
            if let Err(e) = &r {
                finding(format!(
                    "{} cannot resume during the run: {e}",
                    world.members[i].slot.name
                ));
                break;
            }
            world.note(format!("[{step}] {} revived", world.members[i].slot.name));
        }
        let roll = world.rng.gen_range(0..100);
        let result: Result<String, String> = match roll {
            0..=24 => op_note(&mut world).await,
            25..=34 => op_item(&mut world).await,
            35..=37 => op_list(&mut world).await,
            38..=49 => op_sync(&mut world).await,
            50..=56 => op_kill(&mut world).await,
            57..=63 => op_restart(&mut world).await,
            64..=67 => op_close(&mut world).await,
            68..=74 => op_network(&mut world).await,
            75..=79 => op_kill_during_sync(&mut world).await,
            80..=83 => op_burst(&mut world).await,
            84..=85 if joined < 3 => {
                joined += 1;
                op_join_and_kill(&mut world, joined).await
            }
            86..=87 => op_prune(&mut world).await,
            88..=91 => op_hostile(&mut world).await,
            92..=95 => op_share_partial(&mut world).await,
            _ => op_sync(&mut world).await,
        };
        let line = format!("[{step}] {}", result.unwrap_or_else(|e| format!("ERR {e}")));
        if std::env::var("CHAOS_VERBOSE").is_ok() {
            println!("{line}");
        }
        world.note(line);
    }
    heal_and_check(&mut world, &root).await
}

fn key(world: &mut World) -> String {
    world.next += 1;
    format!("k{}", world.next)
}

async fn op_note(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    match world.rng.gen_range(0..4) {
        0 | 1 => {
            let k = key(world);
            let r = node.add_note(&k, "v0").await;
            if r.is_ok() {
                world.acked.insert(k.clone());
            }
            Ok(format!("{} insert {k}: {}", node.name, short(&r.is_ok())))
        }
        2 => {
            let k = world
                .acked
                .iter()
                .nth(world.rng.gen_range(0..world.acked.len().max(1)))
                .cloned()
                .unwrap_or_default();
            let v = format!("v{}", world.rng.gen_range(0..1000));
            let r = node
                .sql(
                    "UPDATE notes SET body = ? WHERE key = ?",
                    json!([{"type":"text","value":v},{"type":"text","value":k}]),
                )
                .await;
            Ok(format!("{} update {k}: {}", node.name, short(&r.is_ok())))
        }
        _ => {
            let k = world
                .acked
                .iter()
                .nth(world.rng.gen_range(0..world.acked.len().max(1)))
                .cloned()
                .unwrap_or_default();
            let r = node
                .sql(
                    "DELETE FROM notes WHERE key = ?",
                    json!([{"type":"text","value":k}]),
                )
                .await;
            // A delete may land even if the reply is lost; count it as possibly deleted.
            world.deleted.insert(k.clone());
            Ok(format!("{} delete {k}: {}", node.name, short(&r.is_ok())))
        }
    }
}

async fn op_item(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let lists: Vec<String> = world.members[i].lists.iter().cloned().collect();
    let list = lists
        .choose(&mut world.rng)
        .cloned()
        .ok_or("member holds no list")?;
    let node = world.members[i].slot.n().clone();
    let title = format!("it{}", world.rng.gen_range(0..1_000_000));
    match world.rng.gen_range(0..3) {
        0 | 1 => Ok(format!(
            "{} item+ {list}: {}",
            node.name,
            short(&node.add_item(&list, &title).await.err())
        )),
        _ => {
            let r = node
                .sql(&format!("UPDATE items SET done = 1 - done WHERE list = {list} AND id IN (SELECT id FROM items WHERE list = {list} LIMIT 3)"), json!([]))
                .await;
            Ok(format!("{} item~ {list}: {}", node.name, short(&r.err())))
        }
    }
}

async fn op_list(world: &mut World) -> Result<String, String> {
    let issuers: Vec<usize> = world
        .alive()
        .into_iter()
        .filter(|i| world.members[*i].kind == Kind::Trusted)
        .collect();
    let i = *issuers.choose(&mut world.rng).ok_or("no live issuer")?;
    let node = world.members[i].slot.n().clone();
    let id = node
        .add_list(&format!("L{}", world.rng.gen_range(0..1000)))
        .await
        .map_err(|e| e.to_string())?;
    world.lists.push(id.clone());
    world.members[i].lists.insert(id.clone());
    // Share with every non-partial member; a dead one only learns it later.
    let targets: Vec<(usize, String)> = world
        .members
        .iter()
        .enumerate()
        .filter(|(j, m)| m.kind != Kind::Partial && *j != i)
        .map(|(j, m)| (j, m.node.clone()))
        .collect();
    let ids: Vec<String> = targets.iter().map(|(_, n)| n.clone()).collect();
    let r = node.share(&id, &ids).await;
    if r.is_ok() {
        for (j, _) in targets {
            world.members[j].lists.insert(id.clone());
        }
    }
    Ok(format!(
        "{} list {id} shared: {}",
        node.name,
        short(&r.err())
    ))
}

async fn op_share_partial(world: &mut World) -> Result<String, String> {
    let issuer = world.issuer().ok_or("no live issuer")?;
    let list = world
        .lists
        .choose(&mut world.rng)
        .cloned()
        .ok_or("no list")?;
    let partials: Vec<usize> = (0..world.members.len())
        .filter(|i| world.members[*i].kind == Kind::Partial)
        .collect();
    let j = *partials.choose(&mut world.rng).ok_or("no partial")?;
    let r = issuer
        .share(&list, std::slice::from_ref(&world.members[j].node))
        .await;
    if r.is_ok() {
        world.members[j].lists.insert(list.clone());
    }
    Ok(format!(
        "{} shares {list} with partial {}: {}",
        issuer.name,
        world.members[j].slot.name,
        short(&r.err())
    ))
}

async fn op_sync(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    let r = node.sync().await;
    Ok(format!("{} sync: {}", node.name, short(&r.err())))
}

async fn op_kill(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    // Keep at least one issuer alive most of the time.
    world.members[i].slot.kill().await;
    Ok(format!("{} SIGKILL", world.members[i].slot.name))
}

async fn op_restart(world: &mut World) -> Result<String, String> {
    let dead: Vec<usize> = (0..world.members.len())
        .filter(|i| world.members[*i].slot.node.is_none())
        .collect();
    let i = *dead.choose(&mut world.rng).ok_or("none dead")?;
    let r = world.members[i].slot.resume().await;
    if let Err(e) = &r {
        finding(format!(
            "{} cannot resume during the run: {e}",
            world.members[i].slot.name
        ));
    }
    Ok(format!(
        "{} resume: {}",
        world.members[i].slot.name,
        short(&r.err())
    ))
}

async fn op_close(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    // Close while something is in flight.
    let busy = node.clone();
    let task = tokio::spawn(async move { busy.sync().await });
    tokio::time::sleep(Duration::from_millis(world.rng.gen_range(0..30))).await;
    let r = tokio::time::timeout(Duration::from_secs(40), world.members[i].slot.close()).await;
    let _ = task.await;
    if r.is_err() {
        finding(format!("{} graceful close hangs over 40 s", node.name));
    }
    Ok(format!("{} close: {}", node.name, short(&r)))
}

async fn op_network(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    let call = match world.rng.gen_range(0..6) {
        0 => json!({"type":"networking_control","enabled":false}),
        1 => json!({"type":"networking_control","enabled":true}),
        2 => json!({"type":"network_background"}),
        3 => json!({"type":"network_foreground"}),
        4 => json!({"type":"sync_start","enabled":false}),
        _ => json!({"type":"sync_start","enabled":true}),
    };
    let r = node.call_for(call.clone(), Duration::from_secs(30)).await;
    Ok(format!(
        "{} {}: {}",
        node.name,
        call["type"],
        short(&r.err())
    ))
}

async fn op_kill_during_sync(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    let delay = world.rng.gen_range(0..60);
    let busy = node.clone();
    let task = tokio::spawn(async move { busy.sync().await });
    tokio::time::sleep(Duration::from_millis(delay)).await;
    world.members[i].slot.kill().await;
    let _ = task.await;
    Ok(format!("{} SIGKILL {delay}ms into sync", node.name))
}

async fn op_burst(world: &mut World) -> Result<String, String> {
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    let mut tasks = Vec::new();
    let mut keys = Vec::new();
    for _ in 0..6 {
        let k = key(world);
        keys.push(k.clone());
        let n = node.clone();
        tasks.push(tokio::spawn(async move {
            n.add_note(&k, "burst").await.is_ok()
        }));
    }
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.sync().await.is_ok() }));
    let mut ok = 0;
    for (t, k) in tasks
        .into_iter()
        .zip(keys.iter().map(Some).chain(std::iter::once(None)))
    {
        if t.await.unwrap_or(false) {
            ok += 1;
            if let Some(k) = k {
                world.acked.insert(k.clone());
            }
        }
    }
    Ok(format!(
        "{} burst of 6 concurrent tx + sync: {ok} ok",
        node.name
    ))
}

/// A new full member joins; it is SIGKILLed early in its bootstrap.
async fn op_join_and_kill(world: &mut World, n: usize) -> Result<String, String> {
    let name = format!("j{n}");
    join(world, &name, Kind::Full)
        .await
        .map_err(|e| e.to_string())?;
    let delay = world.rng.gen_range(0..200);
    tokio::time::sleep(Duration::from_millis(delay)).await;
    let i = world.members.len() - 1;
    let data = status_data(world.members[i].slot.n()).await;
    world.members[i].slot.kill().await;
    Ok(format!(
        "{name} joined, SIGKILL {delay}ms later at {}",
        short(&data["bootstrap"])
    ))
}

async fn op_prune(world: &mut World) -> Result<String, String> {
    let issuer = world.issuer().ok_or("no live issuer")?;
    let table = ["notes", "lists", "items"][world.rng.gen_range(0..3)];
    let r = issuer
        .call(json!({"type":"retention","collection":table,"policy":{"required":[],"behind":[],"copies":1,"window":0},"maintain":true}))
        .await;
    Ok(format!(
        "{} prune {table}: {}",
        issuer.name,
        short(&r.map(|s| (s["covered"].clone(), s["error"].clone())))
    ))
}

async fn op_hostile(world: &mut World) -> Result<String, String> {
    const SQL: &[&str] = &[
        "BEGIN",
        "SAVEPOINT x",
        "UPDATE notes SET key = key || '-renamed' WHERE rowid IN (SELECT rowid FROM notes LIMIT 1)",
        "DELETE FROM notes WHERE rowid IN (SELECT rowid FROM notes LIMIT 5)",
        "UPDATE notes SET body = randomblob(200000)",
        "DELETE FROM lists",
        "INSERT INTO notes(id,key,body) VALUES(9223372036854775807,'max','x')",
        "INSERT INTO notes(id,key,body) VALUES(-1,'neg','x')",
        "UPDATE items SET list = 1",
        "SELECT * FROM notes, notes, notes",
    ];
    let i = world.pick_alive().ok_or("none alive")?;
    let node = world.members[i].slot.n().clone();
    let sql = *SQL.choose(&mut world.rng).ok_or("empty")?;
    let r = node
        .call_for(
            json!({"type":"sql","sql":sql,"values":[]}),
            Duration::from_secs(20),
        )
        .await;
    // Half the time, leave the transaction open across other operations.
    if world.rng.gen_bool(0.5) {
        let _ = node.sql("ROLLBACK", json!([])).await;
    }
    if sql.contains("renamed")
        || sql.starts_with("DELETE FROM notes")
        || sql.starts_with("UPDATE notes SET key")
    {
        // Unknown which keys changed: stop counting acked keys as must-exist.
        world.deleted.extend(world.acked.iter().cloned());
    }
    Ok(format!(
        "{} hostile `{sql:.40}`: {}",
        node.name,
        short(&r.is_ok())
    ))
}

async fn heal_and_check(world: &mut World, root: &std::path::Path) -> Result<(), Failure> {
    println!("  heal: restarting dead members");
    for m in world.members.iter_mut() {
        if m.slot.node.is_none()
            && let Err(e) = m.slot.resume().await
        {
            finding(format!("{} cannot resume after the run: {e}", m.slot.name));
        }
    }
    let nodes: Vec<Arc<Node>> = world
        .members
        .iter()
        .filter_map(|m| m.slot.node.clone())
        .collect();
    for n in &nodes {
        let _ = n.sql("ROLLBACK", json!([])).await;
        for call in [
            json!({"type":"networking_control","enabled":true}),
            json!({"type":"network_foreground"}),
            json!({"type":"sync_start","enabled":true}),
        ] {
            if let Err(e) = n.call_for(call.clone(), Duration::from_secs(30)).await {
                println!("  heal {} {}: {}", n.name, call["type"], short(&e));
            }
        }
    }
    let refs: Vec<&Node> = nodes.iter().map(|n| n.as_ref()).collect();
    mesh(&refs).await;
    let full_nodes: Vec<Arc<Node>> = world
        .members
        .iter()
        .filter(|m| m.kind != Kind::Partial)
        .filter_map(|m| m.slot.node.clone())
        .collect();
    let full: Vec<&Node> = full_nodes.iter().map(|n| n.as_ref()).collect();
    let mut converged = false;
    for round in 0..12 {
        // Every full/trusted member gets every list, from whichever trusted
        // member holds its secret by now (envelopes arrive by sync).
        for list in world.lists.clone() {
            let ids: Vec<String> = world
                .members
                .iter()
                .filter(|m| m.kind != Kind::Partial)
                .map(|m| m.node.clone())
                .collect();
            for sharer in world
                .members
                .iter()
                .filter(|m| m.kind == Kind::Trusted)
                .filter_map(|m| m.slot.node.clone())
            {
                let own = sharer.node_id().await;
                let to: Vec<String> = ids.iter().filter(|id| **id != own).cloned().collect();
                if sharer.share(&list, &to).await.is_ok() {
                    break;
                }
            }
        }
        for m in world.members.iter_mut().filter(|m| m.kind != Kind::Partial) {
            m.lists.extend(world.lists.iter().cloned());
        }
        for n in &refs {
            let _ = n.sync().await;
        }
        let mut views = Vec::new();
        for n in &full {
            views.push((
                n.dump(NOTES).await,
                n.dump(LISTS).await,
                n.dump(ITEMS).await,
            ));
        }
        let mut pending = Vec::new();
        for n in &refs {
            if status_data(n).await["bootstrap_pending"] == true {
                pending.push(n.name.clone());
            }
        }
        let agree = views.windows(2).all(|w| w[0] == w[1]) && views.iter().all(|v| v.0.is_ok());
        if agree && pending.is_empty() {
            println!("  converged after {} heal rounds", round + 1);
            converged = true;
            break;
        }
        if round == 11 {
            finding(format!(
                "seed: no convergence after 12 heal rounds; still bootstrapping: {pending:?}"
            ));
            crate::scenarios::report_divergence(&full).await;
        }
    }
    // Partial members: unpartitioned notes, and every list they hold.
    if let Some(reference) = full.first() {
        let notes = reference.dump(NOTES).await;
        for m in world.members.iter().filter(|m| m.kind == Kind::Partial) {
            let Some(p) = m.slot.node.as_deref() else {
                continue;
            };
            if p.dump(NOTES).await != notes {
                finding(format!("partial {} notes differ from full members", p.name));
            }
            for list in &m.lists {
                let q =
                    format!("SELECT id, title, done FROM items WHERE list = {list} ORDER BY id");
                let (mine, theirs) = (p.dump(&q).await, reference.dump(&q).await);
                if mine != theirs {
                    finding(format!(
                        "partial {} list {list}: {} vs full {}",
                        p.name,
                        short(&mine.map(|r| r.len())),
                        short(&theirs.map(|r| r.len()))
                    ));
                }
            }
        }
        // Acknowledged inserts that were never deleted must exist.
        let present: BTreeSet<String> = notes
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.split('|').next().map(|k| k.trim_matches('"').to_owned()))
            .collect();
        let lost: Vec<_> = world
            .acked
            .difference(&world.deleted)
            .filter(|k| !present.contains(*k))
            .take(20)
            .cloned()
            .collect();
        if !lost.is_empty() {
            finding(format!(
                "acknowledged inserts missing after heal (first 20): {lost:?}"
            ));
        }
    }
    let mut statuses = BTreeMap::new();
    for n in &refs {
        statuses.insert(
            n.name.clone(),
            status_data(n).await["bootstrap_pending"].clone(),
        );
    }
    println!("  pending after heal: {statuses:?}");
    // Close, check files, reopen once more.
    for m in world.members.iter_mut() {
        if let Err(e) = m.slot.close().await {
            println!("  close {}: {e}", m.slot.name);
        }
    }
    for m in world.members.iter_mut() {
        let check = integrity(&root.join(&m.slot.name));
        if !check.starts_with("ok") {
            finding(format!("{} integrity: {check}", m.slot.name));
        }
        match m.slot.resume().await {
            Ok(_) => {
                let _ = m.slot.close().await;
            }
            Err(e) => finding(format!("{} does not reopen after heal: {e}", m.slot.name)),
        }
    }
    if !converged || std::env::var("CHAOS_LOG").is_ok() {
        let path = root.join("schedule.log");
        std::fs::write(&path, world.log.join("\n"))?;
        println!("  schedule: {}", path.display());
    }
    let _ = Value::Null;
    Ok(())
}
