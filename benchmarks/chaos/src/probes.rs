//! Focused reproductions of individual findings.
use crate::scenarios::{
    Failure, LISTS, NOTES, Slot, author_all, finding, populate, root, short, status_data,
    writer_all,
};
use serde_json::json;
use std::time::Duration;

/// background -> foreground on a fixed bind port, on an ephemeral port,
/// and after a socket fault; then whether sync and a restart recover.
pub async fn foreground_rebind() -> Result<(), Failure> {
    for ephemeral in [false, true] {
        println!(
            "  -- bind {}",
            if ephemeral {
                "127.0.0.1:0"
            } else {
                "fixed port"
            }
        );
        rebind_once(ephemeral).await?;
    }
    Ok(())
}
async fn rebind_once(ephemeral: bool) -> Result<(), Failure> {
    let root = root(if ephemeral { "rebind-eph" } else { "rebind" });
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    populate(a.n(), "a", 20, 10, 0, 0).await;
    let mut b = Slot::new("b", &root);
    if ephemeral {
        b.port = 0;
    }
    b.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    author_all(b.n(), a.n()).await;
    crate::cluster::mesh(&[a.n(), b.n()]).await;
    let _ = b.n().sync().await;
    for (label, calls) in [
        (
            "fault_close only (no foreground)",
            vec!["network_fault_close"],
        ),
        (
            "networking toggle after fault",
            vec!["networking_control_off", "networking_control_on"],
        ),
        (
            "background/foreground",
            vec!["network_background", "network_foreground"],
        ),
        (
            "fault_close/foreground",
            vec!["network_fault_close", "network_foreground"],
        ),
        (
            "foreground twice",
            vec!["network_foreground", "network_foreground"],
        ),
    ] {
        let mut out = Vec::new();
        for call in calls {
            let request = match call {
                "networking_control_off" => json!({"type":"networking_control","enabled":false}),
                "networking_control_on" => json!({"type":"networking_control","enabled":true}),
                other => json!({"type":other}),
            };
            out.push(format!("{call}: {}", short(&b.n().call(request).await)));
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        let before = b.n().dump(NOTES).await.map(|r| r.len()).unwrap_or(0);
        let _ = a.n().add_note(&format!("after-{label}"), "x").await;
        let sync = b.n().sync().await;
        let have = b.n().dump(NOTES).await.map(|r| r.len());
        if have.as_ref().is_ok_and(|n| *n == before) {
            finding(format!(
                "{} bind, {label}: node no longer syncs (sync status still ok)",
                if ephemeral { "ephemeral" } else { "fixed" }
            ));
        }
        println!(
            "  {label}: {out:?}\n    sync -> {}\n    b notes {have:?}",
            short(&sync.map(|r| {
                r["peers"][0]["attempts"]
                    .as_array()
                    .map(|a| a.iter().map(|x| x["outcome"].clone()).collect::<Vec<_>>())
            }))
        );
    }
    // Networking toggles do not rebind; a restart does.
    let _ = b
        .n()
        .call(json!({"type":"networking_control","enabled":false}))
        .await;
    let _ = b
        .n()
        .call(json!({"type":"networking_control","enabled":true}))
        .await;
    println!(
        "  sync after networking toggle: {}",
        short(&b.n().sync().await.is_ok())
    );
    b.close().await?;
    b.resume().await?;
    let _ = b.n().sync().await;
    println!(
        "  after restart b notes {:?}",
        b.n().dump(NOTES).await.map(|r| r.len())
    );
    b.close().await?;
    a.close().await?;
    Ok(())
}

/// The enrollment window that leaves `identity.join.json` without
/// `identity.json`: which recovery paths work?
pub async fn enrollment_window() -> Result<(), Failure> {
    let root = root("enroll-window");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    populate(a.n(), "a", 50, 10, 0, 0).await;
    let mut stuck = Vec::new();
    for attempt in 0..30u64 {
        let delay = 60 + attempt * 6;
        let name = format!("w{attempt}");
        let mut j = Slot::new(&name, &root);
        let invitation = a.n().invite(writer_all()).await;
        let options = crate::node::options(
            root.join(&name).to_str().unwrap(),
            j.port,
            json!({"mode":"join","invitation":invitation}),
        );
        let mut child = tokio::process::Command::new(crate::cluster::exe())
            .arg("node")
            .arg(options.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        let dir = root.join(&name);
        let pending =
            dir.join("identity.join.json").exists() && !dir.join("identity.json").exists();
        if !pending {
            continue;
        }
        // Odd attempts: retry the same invitation directly. Even attempts:
        // the issuer creates another invitation first (another device, or
        // the user tapping "Add device" again), then both are tried.
        let resume = j.resume().await.err().unwrap_or_else(|| "ok".into());
        let mut fresh = String::from("-");
        let fresh_invitation = (attempt % 2 == 0).then_some(());
        let fresh_invitation = match fresh_invitation {
            Some(()) => Some(a.n().invite(writer_all()).await),
            None => None,
        };
        let same = match j
            .start(json!({"mode":"join","invitation": invitation}))
            .await
        {
            Ok(n) => {
                let _ = n.sync().await;
                let notes = n.dump(NOTES).await.map(|r| r.len());
                let _ = j.close().await;
                format!("ok, notes {notes:?}")
            }
            Err(e) => e,
        };
        if let Some(other) = fresh_invitation {
            fresh = match j.start(json!({"mode":"join","invitation": other})).await {
                Ok(_) => {
                    let _ = j.close().await;
                    "ok".into()
                }
                Err(e) => e,
            };
        }
        println!(
            "  killed at {delay}ms (issuer re-invited: {}): resume: {resume}\n    same invitation: {same}\n    newer invitation: {fresh}",
            attempt % 2 == 0
        );
        if !same.starts_with("ok") && !fresh.starts_with("ok") {
            stuck.push(delay);
        }
        if stuck.len() >= 2 {
            break;
        }
    }
    // The same invitation once the issuer is unreachable.
    let name = "offline-issuer";
    let mut j = Slot::new(name, &root);
    let invitation = a.n().invite(writer_all()).await;
    for delay in [90u64, 110, 130, 150, 170, 190] {
        let _ = std::fs::remove_dir_all(root.join(name));
        let options = crate::node::options(
            root.join(name).to_str().unwrap(),
            j.port,
            json!({"mode":"join","invitation":invitation}),
        );
        let mut child = tokio::process::Command::new(crate::cluster::exe())
            .arg("node")
            .arg(options.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        let dir = root.join(name);
        if dir.join("identity.join.json").exists() && !dir.join("identity.json").exists() {
            println!("  offline-issuer variant: pending join at {delay}ms");
            break;
        }
    }
    a.kill().await;
    let r = j
        .start(json!({"mode":"join","invitation": invitation}))
        .await;
    println!(
        "  same invitation, issuer down: {}",
        short(&r.as_ref().map(|_| "ok"))
    );
    let r = j.resume().await;
    println!(
        "  resume, issuer down: {}",
        short(&r.as_ref().map(|_| "ok"))
    );
    if !stuck.is_empty() {
        finding(format!(
            "join killed at {stuck:?} ms cannot be completed with any invitation"
        ));
    }
    let _ = j.close().await;
    Ok(())
}

/// A partial replica receives one shared list: does it hold it?
pub async fn partial_holds_shared() -> Result<(), Failure> {
    let root = root("partial-shared");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 5, 10, 3, 5).await;
    let mut p = Slot::new("p", &root);
    p.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
        .await?;
    println!("  role: {}", short(&p.n().role(true).await));
    let pid = p.n().node_id().await;
    println!(
        "  share: {}",
        short(
            &a.n()
                .share(&lists[0], std::slice::from_ref(&pid))
                .await
                .map(|s| s["recipients"].clone())
        )
    );
    crate::cluster::mesh(&[a.n(), p.n()]).await;
    for round in 0..4 {
        let s = p.n().sync().await;
        let l = p.n().dump(LISTS).await;
        println!(
            "  round {round}: sync ok={} lists {:?} pending={}",
            s.is_ok(),
            l,
            status_data(p.n()).await["bootstrap_pending"]
        );
    }
    let st = p.n().call(json!({"type":"partition_status","group":"list","values":[{"type":"integer","value":lists[0]}]})).await;
    println!(
        "  partition status: {}",
        short(&st.map(|s| s["tables"].as_array().map(|t| {
            t.iter()
                .map(|t| {
                    (
                        t["table"].clone(),
                        t["holding"].clone(),
                        t["epochs"].clone(),
                    )
                })
                .collect::<Vec<_>>()
        })))
    );
    let events = p
        .n()
        .call(json!({"type":"activity"}))
        .await
        .unwrap_or_default();
    let kinds: Vec<_> = events["events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| e["kind"].as_str().unwrap_or("").to_owned())
        .collect();
    println!("  activity kinds: {kinds:?}");
    p.close().await?;
    a.close().await?;
    Ok(())
}

/// A writer that was never shared a list: does readiness ever settle?
pub async fn pending_forever() -> Result<(), Failure> {
    let root = root("pending");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 20, 10, 2, 3).await;
    for (label, share) in [
        ("no list shared", 0usize),
        ("one of two shared", 1),
        ("all shared", 2),
    ] {
        let mut b = Slot::new(&format!("b{share}"), &root);
        b.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
            .await?;
        let id = b.n().node_id().await;
        for list in lists.iter().take(share) {
            let _ = a.n().share(list, std::slice::from_ref(&id)).await;
        }
        crate::cluster::mesh(&[a.n(), b.n()]).await;
        for _ in 0..4 {
            let _ = b.n().sync().await;
        }
        let data = status_data(b.n()).await;
        let imports: Vec<_> = data["imports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| {
                format!(
                    "{}:{}",
                    i["scope"]["table"].as_str().unwrap_or("?"),
                    i["state"]
                        .as_str()
                        .unwrap_or(&i.to_string()[..60.min(i.to_string().len())])
                )
            })
            .collect();
        println!(
            "  {label}: bootstrap_pending={} bootstrap={} imports={imports:?}",
            data["bootstrap_pending"], data["bootstrap"]
        );
        b.close().await?;
        b.resume().await?;
        let _ = b.n().sync().await;
        println!(
            "    after reopen+sync: pending={}",
            status_data(b.n()).await["bootstrap_pending"]
        );
        if status_data(b.n()).await["bootstrap_pending"] == true {
            finding(format!(
                "bootstrap_pending stays true forever for a full writer with {label}"
            ));
        }
        b.close().await?;
    }
    a.close().await?;
    Ok(())
}

/// Eviction on a partial replica with the full node named either by node
/// key (what `session`/`peers` expose) or by its PeerId commitment.
pub async fn evict_with_full_node() -> Result<(), Failure> {
    use rocky_sdk::credentials::{PeerId, PublicKey};
    let root = root("evict-full");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 5, 10, 2, 200).await;
    let a_node = a.n().node_id().await;
    let key: PublicKey = a_node.parse().map_err(|e| format!("{e:?}"))?;
    let peer = PeerId::of(key).to_hex();
    for (label, reference) in [("node key", a_node.clone()), ("PeerId", peer.clone())] {
        let name = if label == "PeerId" { "p2" } else { "p1" };
        let mut p = Slot::new(name, &root);
        p.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
            .await?;
        let role = p.n().call(json!({"type":"partition_role","role":{"role":"Partial","full_nodes":[reference],"custody":1}})).await;
        author_all(p.n(), a.n()).await;
        let _ = a.n().share(&lists[0], &[p.n().node_id().await]).await;
        crate::cluster::mesh(&[a.n(), p.n()]).await;
        for _ in 0..2 {
            let _ = p.n().sync().await;
        }
        let mut written = 0;
        for i in 0..20 {
            written += usize::from(
                p.n()
                    .add_item(&lists[0], &format!("{name}-item{i}"))
                    .await
                    .is_ok(),
            );
        }
        println!("  {name}: own items written {written}/20");
        for _ in 0..3 {
            let _ = p.n().sync().await;
        }
        let values = json!([{"type":"integer","value":lists[0]}]);
        let evict = p
            .n()
            .call(json!({"type":"partition_evict","group":"list","values":values}))
            .await;
        println!(
            "  full node by {label}: role {} -> evict {}",
            short(&role.is_ok()),
            short(&evict)
        );
        if label == "node key" && evict.is_err() {
            finding(
                "full_nodes accepts a node public key (what the JSON API exposes) but only matches a PeerId commitment, which no JSON call returns: eviction/custody silently never succeed",
            );
        }
        if evict.is_ok() {
            // Kill during re-subscribe bootstrap, then write before syncing.
            let kind = if label == "PeerId" {
                "partition_bootstrap"
            } else {
                "partition_subscribe"
            };
            let sub = p
                .n()
                .call(json!({"type":kind,"group":"list","values":values}))
                .await;
            println!("    via {kind}");
            println!("    resubscribe: {}", short(&sub));
            let early = p.n().add_item(&lists[0], "before-restore").await;
            println!("    write before own log restored: {}", short(&early));
            if std::env::var("CHAOS_EVICT_KILL").is_ok() {
                let n = p.n().clone();
                let sync = tokio::spawn(async move { n.sync().await });
                tokio::time::sleep(Duration::from_millis(15)).await;
                p.kill().await;
                let _ = sync.await;
                p.resume().await?;
            }
            for _ in 0..3 {
                let _ = p.n().sync().await;
            }
            let write = p.n().add_item(&lists[0], "after-restore").await;
            let _ = p.n().sync().await;
            let q = format!(
                "SELECT title FROM items WHERE list = {} ORDER BY title",
                lists[0]
            );
            let (pp, aa) = (p.n().dump(&q).await, a.n().dump(&q).await);
            println!(
                "    write after restore: {}; items partial {:?} issuer {:?}",
                short(&write.is_ok()),
                pp.as_ref().map(|r| r.len()),
                aa.as_ref().map(|r| r.len())
            );
            if pp != aa {
                finding("partial replica diverges from the issuer after evict + resubscribe");
            }
        }
        p.close().await?;
    }
    a.close().await?;
    Ok(())
}

/// Evict, resubscribe and watch the list over many syncs, with and without
/// a SIGKILL during the first resync, and without any own writes.
pub async fn evict_resubscribe() -> Result<(), Failure> {
    let root = root("evict-resub");
    let mut a = Slot::new("a", &root);
    a.start(json!({"mode":"create"})).await?;
    author_all(a.n(), a.n()).await;
    let lists = populate(a.n(), "a", 5, 10, 1, 200).await;
    let a_node = a.n().node_id().await;
    for (name, kill, own_writes, how) in [
        ("q1", false, true, "partition_subscribe"),
        ("q2", true, true, "partition_subscribe"),
        ("q3", false, false, "partition_subscribe"),
        ("q4", true, false, "partition_bootstrap"),
        ("q5", false, true, "partition_bootstrap"),
    ] {
        let mut p = Slot::new(name, &root);
        p.start(json!({"mode":"join","invitation": a.n().invite(writer_all()).await}))
            .await?;
        p.n().call(json!({"type":"partition_role","role":{"role":"Partial","full_nodes":[a_node],"custody":1}})).await.ok();
        author_all(p.n(), a.n()).await;
        let _ = a.n().share(&lists[0], &[p.n().node_id().await]).await;
        crate::cluster::mesh(&[a.n(), p.n()]).await;
        for _ in 0..2 {
            let _ = p.n().sync().await;
        }
        if own_writes {
            for i in 0..20 {
                let _ = p.n().add_item(&lists[0], &format!("{name}-{i}")).await;
            }
        }
        for _ in 0..2 {
            let _ = p.n().sync().await;
        }
        let values = json!([{"type":"integer","value":lists[0]}]);
        let evict = p
            .n()
            .call(json!({"type":"partition_evict","group":"list","values":values}))
            .await;
        let sub = p
            .n()
            .call(json!({"type":how,"group":"list","values":values}))
            .await;
        if kill {
            let n = p.n().clone();
            let sync = tokio::spawn(async move { n.sync().await });
            tokio::time::sleep(Duration::from_millis(15)).await;
            p.kill().await;
            let _ = sync.await;
            p.resume().await?;
        }
        let q = format!(
            "SELECT title FROM items WHERE list = {} ORDER BY title",
            lists[0]
        );
        let want = a.n().dump(&q).await.map(|r| r.len()).unwrap_or(0);
        let mut last = String::new();
        let mut healed = false;
        for round in 0..10 {
            let _ = p.n().sync().await;
            let got = p.n().dump(&q).await;
            let holding = p
                .n()
                .call(json!({"type":"partition_status","group":"list","values":values}))
                .await
                .map(|s| {
                    s["tables"].as_array().map(|t| {
                        t.iter()
                            .map(|t| t["holding"].as_str().unwrap_or("-").to_owned())
                            .collect::<Vec<_>>()
                    })
                });
            last = format!(
                "round {round}: rows {} holding {}",
                short(&got.as_ref().map(|r| r.len())),
                short(&holding)
            );
            if got.as_ref().is_ok_and(|r| r.len() == want) {
                healed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        println!(
            "  {name} kill={kill} own_writes={own_writes} via {how}: evict {} sub {} -> healed={healed} ({last}; issuer {want})",
            short(&evict.map(|e| e["rows"].clone())),
            short(&sub.is_ok())
        );
        if !healed {
            finding(format!(
                "{name}: evicted list never restored after {how} (kill={kill}); last {last}"
            ));
            let events = p
                .n()
                .call(json!({"type":"activity"}))
                .await
                .unwrap_or_default();
            let tail: Vec<_> = events["events"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .take(8)
                .map(|e| {
                    format!(
                        "{}{}",
                        e["kind"].as_str().unwrap_or(""),
                        e.get("error").map(|x| format!("({x})")).unwrap_or_default()
                    )
                })
                .collect();
            println!("    recent activity: {tail:?}");
        }
        p.close().await?;
        if !healed {
            let mut again = Slot::new(name, &root);
            again.port = p.port;
            let n = again.resume().await?;
            for _ in 0..3 {
                let _ = n.sync().await;
            }
            println!(
                "    after restart: {}",
                short(&n.dump(&q).await.map(|r| r.len()))
            );
            again.close().await?;
        }
    }
    a.close().await?;
    Ok(())
}
