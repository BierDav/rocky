use super::*;
use std::future::Future;

// SQLite runs on another thread. Keep Tokio runnable so its paused clock
// advances only where the test explicitly advances it.
fn frozen<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let mut future = Box::pin(future);
    async move {
        let watchdog = std::time::Instant::now();
        loop {
            tokio::select! {
                biased;
                result = &mut future => return result,
                () = tokio::task::yield_now() => {
                    assert!(watchdog.elapsed() < Duration::from_secs(120), "test made no progress");
                }
            }
        }
    }
}
async fn until(mut ready: impl FnMut() -> bool) {
    while !ready() {
        tokio::task::yield_now().await;
    }
}
async fn contact(node: &Node) -> Result<String, Failure> {
    let link = node.network.read().await;
    Ok(node
        .settings
        .lock()
        .await
        .contact(&node.vault, &*link.as_ref().ok_or("link")?.transport)
        .await?
        .0)
}
async fn targets(a: &Node, peers: &[Arc<Node>]) -> Result<(), Failure> {
    for peer in peers {
        let contact = contact(peer).await?;
        a.configure(|settings| settings.add_peer(&a.vault, &contact))
            .await?;
    }
    Ok(())
}
async fn start(node: &Node, enabled: bool) -> Result<Value, Failure> {
    node.dispatch(
        &*node.client(1)?,
        &json!({"type":"sync_start", "enabled":enabled}),
    )
    .await
}
async fn transport(node: &Node) -> Result<Arc<dyn Transport>, Failure> {
    Ok(node
        .network
        .read()
        .await
        .as_ref()
        .ok_or("link")?
        .transport
        .clone())
}
async fn settled(node: &Node) {
    until(|| !*node.worker_running.borrow()).await;
}
async fn rows(node: &Node) -> Result<Vec<String>, Failure> {
    let page = node.dispatch(&*node.client(1)?, &json!({"type":"read", "query":{"sql":"SELECT key,title FROM items ORDER BY key", "max_rows":64}})).await?;
    page["rows"]
        .as_array()
        .ok_or("rows")?
        .iter()
        .map(|row| {
            if row[1]["value"] != "Milk" {
                return Err("partial row".into());
            }
            Ok(row[0]["value"].as_str().ok_or("key")?.to_string())
        })
        .collect()
}

#[tokio::test]
async fn enrollment_reaches_the_caller_before_its_return_route_wakes_sync() -> Result<(), Failure> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let issuer = enrolled(&root.path().join("issuer"), &hub, "issuer", None).await?;
            issuer
                .dispatch(
                    &*issuer.client(1)?,
                    &json!({"type":"pairing", "profile":{"kind":"reader", "scopes":["items"]}}),
                )
                .await?;
            let token = issuer.vault.get("invitation")?;
            let request = rocky_credentials::PairRequest::new(
                &token,
                "00".repeat(32),
                PrivateKey::new().public_key(),
                PrivateKey::new().public_key(),
                commitment(
                    "rocky.sdk.schema.v1",
                    &serde_json::to_vec(&Schema::parse(SCHEMA)?)?,
                ),
            )
            .with_routes(&token, vec![("memory".into(), "member".into())])
            .to_bytes()?;
            let before = *issuer.sync_events.borrow();
            let (mut caller, server) = tokio::io::duplex(64 * 1024);
            let serving = issuer.clone();
            let served = tokio::task::spawn_local(async move {
                serving.serve(Protocol::Pair, Box::new(server)).await
            });
            caller.write_u32(u32::try_from(request.len())?).await?;
            caller.write_all(&request).await?;
            let length = caller.read_u32().await? as usize;
            assert_eq!(*issuer.sync_events.borrow(), before);
            let mut reply = vec![0; length];
            caller.read_exact(&mut reply).await?;
            let reply: Value = serde_json::from_slice(&reply)?;
            assert!(reply["reply"].is_string(), "{reply}");
            assert_eq!(*issuer.sync_events.borrow(), before);
            caller.shutdown().await?;
            served.await??;
            assert_eq!(*issuer.sync_events.borrow(), before.wrapping_add(1));
            Ok(())
        })
        .await
}

#[tokio::test(start_paused = true)]
async fn a_joined_member_receives_later_writes_without_a_poll_or_contact_copy()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let issuer = enrolled(&root.path().join("issuer"), &hub, "issuer", None).await?;
            start(&issuer, true).await?;
            settled(&issuer).await;
            let member = enrolled(
                &root.path().join("member"),
                &hub,
                "member",
                Some((
                    &issuer,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            let member_key = PrivateKey::from_bytes(&member.vault.get("node")?).public_key();
            {
                let settings = issuer.settings.lock().await;
                assert!(settings.peers.contains_key(&member_key));
                assert!(!settings.issuers.contains(&member_key));
                let saved: Settings = serde_json::from_slice(&std::fs::read(&issuer.path)?)?;
                assert!(saved.peers.contains_key(&member_key));
            }
            until(|| hub.dials_to("member") > 0).await;
            settled(&issuer).await;
            // The member's first handshake signals that its listener is ready.
            member.sync(&*transport(&member).await?).await?;
            until(|| {
                issuer
                    .reached()
                    .expect("reached")
                    .get(&member_key)
                    .is_some_and(|peer| peer.error.is_none())
            })
            .await;
            settled(&issuer).await;
            let before = tokio::time::Instant::now();
            add(&issuer, 1).await?;
            until(|| *member.replica.observe_revision().borrow() > 0).await;
            settled(&issuer).await;
            assert_eq!(tokio::time::Instant::now(), before);
            assert_eq!(rows(&member).await?, ["1"]);
            issuer.detach(None).await;
            member.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn peers_progress_before_stalled_dial_and_signals_respect_peer_backoff() -> Result<(), Failure>
{
    peer_signals(false).await
}

#[tokio::test(start_paused = true)]
async fn a_stalled_bootstrap_peer_does_not_hold_the_checkpoint_import_slot() -> Result<(), Failure>
{
    peer_signals(true).await
}

async fn peer_signals(bootstrap: bool) -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let mut peers = Vec::new();
            for name in ["b", "c", "d"] {
                peers.push(
                    enrolled(
                        &root.path().join(name),
                        &hub,
                        name,
                        Some((&a, Profile::Issuer)),
                    )
                    .await?,
                );
            }
            targets(&a, &peers).await?;
            add(&a, 1).await?;
            if bootstrap {
                std::fs::write(a.path.with_file_name("bootstrap.json"), b"true")?;
            }
            hub.stall_peer("d", true);
            let before = tokio::time::Instant::now();
            start(&a, true).await?;
            let keys: Vec<_> = peers
                .iter()
                .map(|peer| {
                    PrivateKey::from_bytes(&peer.vault.get("node").expect("node key")).public_key()
                })
                .collect();
            until(|| {
                a.reached().expect("reached").contains_key(&keys[0])
                    && a.reached().expect("reached").contains_key(&keys[1])
            })
            .await;
            assert_eq!(tokio::time::Instant::now(), before);
            assert!(*a.worker_running.borrow());
            assert_eq!(rows(&peers[0]).await?, ["1"]);
            assert_eq!(rows(&peers[1]).await?, ["1"]);
            tokio::time::advance(Duration::from_secs(15)).await;
            until(|| {
                a.retries
                    .lock()
                    .expect("retries")
                    .get(&keys[2])
                    .is_some_and(|state| state.retry.deadline().is_some())
            })
            .await;
            settled(&a).await;
            let stalled = hub.dials_to("d");
            add(&a, 2).await?;
            while rows(&peers[0]).await? != ["1", "2"] || rows(&peers[1]).await? != ["1", "2"] {
                tokio::task::yield_now().await;
            }
            settled(&a).await;
            assert_eq!(
                hub.dials_to("d"),
                stalled,
                "a local write must preserve peer backoff"
            );
            assert_eq!(rows(&peers[0]).await?, ["1", "2"]);
            tokio::time::advance(Duration::from_millis(999)).await;
            tokio::task::yield_now().await;
            assert_eq!(hub.dials_to("d"), stalled);
            tokio::time::advance(Duration::from_millis(1)).await;
            hub.wait_dials("d", stalled + 1).await;
            tokio::time::advance(Duration::from_secs(15)).await;
            settled(&a).await;
            hub.network_changed();
            hub.wait_dials("d", stalled + 2).await;
            tokio::time::advance(Duration::from_secs(15)).await;
            settled(&a).await;
            let changed_contact = contact(&peers[2]).await?;
            a.dispatch(
                &*a.client(1)?,
                &json!({"type":"peer_add", "contact":changed_contact}),
            )
            .await?;
            hub.wait_dials("d", stalled + 3).await;
            tokio::time::advance(Duration::from_secs(15)).await;
            settled(&a).await;
            // An authenticated inbound session resets just its peer, even if empty.
            hub.stall_peer("d", false);
            let d = &peers[2];
            let dials = hub.dials_to("d");
            d.sync(&*transport(d).await?).await?;
            hub.wait_dials("d", dials + 1).await;
            settled(&a).await;
            assert_eq!(rows(d).await?, ["1", "2"]);
            // After inbound success resets its deadline, an explicit pass reaches every ready peer.
            hub.stall_peer("d", true);
            hub.network_changed();
            hub.wait_dials("d", dials + 2).await;
            tokio::time::advance(Duration::from_secs(15)).await;
            settled(&a).await;
            start(&a, false).await?;
            hub.stall_peer("d", false);
            let counts = ["b", "c", "d"].map(|name| hub.dials_to(name));
            a.dispatch(&*a.client(1)?, &json!({"type":"sync"})).await?;
            assert!(
                ["b", "c", "d"]
                    .iter()
                    .enumerate()
                    .all(|(i, name)| hub.dials_to(name) > counts[i])
            );
            for peer in peers {
                peer.detach(None).await;
            }
            a.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn pause_cancels_stalled_dial_and_foreground_starts_without_a_timer() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, Profile::Issuer)),
            )
            .await?;
            targets(&a, std::slice::from_ref(&b)).await?;
            hub.stall_peer("b", true);
            let before = hub.dials_to("b");
            start(&a, true).await?;
            hub.wait_dials("b", before + 1).await;
            let paused = tokio::time::Instant::now();
            start(&a, false).await?;
            assert!(tokio::time::Instant::now() - paused < Duration::from_secs(1));
            let counts = hub.counts();
            tokio::time::advance(Duration::from_secs(3600)).await;
            hub.network_changed();
            add(&a, 1).await?;
            assert_eq!(hub.counts(), counts);
            start(&a, true).await?;
            hub.wait_dials("b", before + 2).await;
            start(&a, false).await?;
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn unreachable_idle_peers_have_no_retry_timer_for_an_hour() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            // An unreachable contact creates no enrollment or registry work to retry.
            let peer = PrivateKey::new().public_key();
            a.configure(|settings| {
                settings.peers.insert(
                    peer,
                    Contact {
                        routes: vec![crate::enrollment::Route {
                            transport: "memory".into(),
                            address: "b".into(),
                        }],
                    },
                );
                Ok(())
            })
            .await?;
            for scope in a.scopes() {
                assert!(a.replica.frontier(scope).await?.logs.is_empty());
            }
            let mut statuses = a.sync_status.subscribe();
            start(&a, true).await?;
            statuses.changed().await?;
            settled(&a).await;
            assert!(a.retry_deadline().is_none());
            let dials = hub.counts()[1];
            tokio::time::advance(Duration::from_secs(3600)).await;
            tokio::task::yield_now().await;
            assert_eq!(hub.counts()[1], dials);
            start(&a, false).await?;
            a.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn two_automatic_workers_converge_and_then_stop_dialing() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, Profile::Issuer)),
            )
            .await?;
            targets(&a, std::slice::from_ref(&b)).await?;
            add(&a, 1).await?;
            add(&b, 2).await?;
            start(&a, true).await?;
            start(&b, true).await?;
            while rows(&a).await? != ["1", "2"] || rows(&b).await? != ["1", "2"] {
                tokio::task::yield_now().await;
            }
            for _ in 0..1000 {
                tokio::task::yield_now().await;
            }
            until(|| !*a.worker_running.borrow() && !*b.worker_running.borrow()).await;
            let dials = hub.counts()[1];
            tokio::time::advance(Duration::from_secs(3600)).await;
            for _ in 0..1000 {
                tokio::task::yield_now().await;
            }
            assert_eq!(hub.counts()[1], dials);
            assert_eq!(rows(&a).await?, ["1", "2"]);
            assert_eq!(rows(&b).await?, ["1", "2"]);
            start(&a, false).await?;
            start(&b, false).await?;
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn metadata_work_arms_an_idle_peer_retry_without_resetting_backoff() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            // An unreachable contact creates no enrollment or registry work to retry.
            let peer = PrivateKey::new().public_key();
            a.configure(|settings| {
                settings.peers.insert(
                    peer,
                    Contact {
                        routes: vec![crate::enrollment::Route {
                            transport: "memory".into(),
                            address: "b".into(),
                        }],
                    },
                );
                Ok(())
            })
            .await?;
            for scope in a.scopes() {
                assert!(a.replica.frontier(scope).await?.logs.is_empty());
            }
            let mut statuses = a.sync_status.subscribe();
            start(&a, true).await?;
            statuses.changed().await?;
            settled(&a).await;
            assert!(a.retry_deadline().is_none());
            let dials = hub.dials_to("b");
            // Key installation and holding changes request work without a row frame.
            a.wake_sync();
            until(|| a.retry_deadline().is_some()).await;
            assert_eq!(hub.dials_to("b"), dials);
            tokio::time::advance(Duration::from_secs(1)).await;
            hub.wait_dials("b", dials + 1).await;
            start(&a, false).await?;
            a.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn representative_session_cancellations_are_atomic_and_repeatable() -> Result<(), Failure> {
    cancellation_offsets(false).await
}

#[tokio::test(start_paused = true)]
#[ignore = "exhaustive phase-2 gate: run this test with --release -- --ignored"]
async fn cancellation_at_every_session_byte_is_atomic_and_the_next_pass_converges()
-> Result<(), Failure> {
    cancellation_offsets(true).await
}

async fn cancellation_offsets(exhaustive: bool) -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = frozen(async {
                let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
                let b = enrolled(
                    &root.path().join("b"),
                    &hub,
                    "b",
                    Some((&a, Profile::Issuer)),
                )
                .await?;
                targets(&a, std::slice::from_ref(&b)).await?;
                // Establish enrollment metadata before the bidirectional row session.
                a.sync(&*transport(&a).await?).await?;
                add(&a, 1).await?;
                add(&b, 2).await?;
                Ok::<_, Failure>((a, b))
            })
            .await?;
            let (a_settings, b_settings) = (
                a.settings.lock().await.clone(),
                b.settings.lock().await.clone(),
            );
            let (a_vault, b_vault) = (a.vault.clone(), b.vault.clone());
            let (a_copy, b_copy) = (
                root.path().join("a-copy.sqlite"),
                root.path().join("b-copy.sqlite"),
            );
            frozen(a.replica.backup_into(&a_copy)).await?;
            frozen(b.replica.backup_into(&b_copy)).await?;
            let before = *hub.written().borrow();
            frozen(a.sync(&*transport(&a).await?)).await?;
            let bytes = *hub.written().borrow() - before;
            assert!(bytes > 0);
            frozen(a.detach(None)).await;
            frozen(b.detach(None)).await;
            let started = std::time::Instant::now();
            let offsets: Vec<_> = if exhaustive {
                (0..=bytes).collect()
            } else {
                [0, 1, 300, 3_000, 9_000, bytes / 2, bytes - 1, bytes]
                    .into_iter()
                    .collect()
            };
            let count = offsets.len();
            eprintln!("exercising {count} session byte offsets");
            let trials = stream::iter(offsets)
                .map(|offset| {
                    let root = &root;
                    let (a_settings, b_settings) = (&a_settings, &b_settings);
                    let (a_vault, b_vault) = (&a_vault, &b_vault);
                    let (a_copy, b_copy) = (&a_copy, &b_copy);
                    frozen(async move {
                        let hub = Arc::new(Hub::default());
                        let trial = root.path().join("trial").join(offset.to_string());
                        let (a_dir, b_dir) = (trial.join("a"), trial.join("b"));
                        for (directory, copy, settings) in
                            [(&a_dir, a_copy, a_settings), (&b_dir, b_copy, b_settings)]
                        {
                            std::fs::create_dir_all(directory)?;
                            std::fs::copy(copy, directory.join("rocky.sqlite"))?;
                            settings.save(&directory.join("identity.json"))?;
                        }
                        let a = node(
                            &a_dir,
                            a_settings.clone(),
                            a_vault.clone(),
                            hub.connector("a").start(true, [0; 32]).await?,
                        )
                        .await?;
                        let b = node(
                            &b_dir,
                            b_settings.clone(),
                            b_vault.clone(),
                            hub.connector("b").start(true, [0; 32]).await?,
                        )
                        .await?;
                        let mut written = hub.written();
                        let before = *written.borrow_and_update();
                        let dials = hub.dials_to("b");
                        hub.hold_cut.store(true, Ordering::SeqCst);
                        hub.cut_after(Some(offset));
                        start(&a, true).await?;
                        hub.wait_dials("b", dials + 1).await;
                        while *written.borrow_and_update() < before + offset {
                            written.changed().await?;
                        }
                        start(&a, false).await?;
                        for scope in a.scopes() {
                            a.replica.audit(scope).await?;
                        }
                        for scope in b.scopes() {
                            b.replica.audit(scope).await?;
                        }
                        hub.cut_after(None);
                        assert!(
                            rows(&a).await?.contains(&"1".to_string()),
                            "lost local row at {offset}"
                        );
                        assert!(
                            rows(&b).await?.contains(&"2".to_string()),
                            "lost local row at {offset}"
                        );
                        a.sync(&*transport(&a).await?).await?;
                        assert_eq!(rows(&a).await?, ["1", "2"], "cut at {offset}");
                        assert_eq!(rows(&b).await?, ["1", "2"], "cut at {offset}");
                        a.detach(None).await;
                        b.detach(None).await;
                        drop(a);
                        drop(b);
                        std::fs::remove_dir_all(trial)?;
                        Ok::<_, Failure>(())
                    })
                })
                .buffer_unordered(8);
            tokio::pin!(trials);
            let mut finished = 0;
            while let Some(result) = trials.next().await {
                result?;
                finished += 1;
                if finished % 1000 == 0 {
                    eprintln!("verified {finished} offsets in {:?}", started.elapsed());
                }
            }
            eprintln!(
                "cancelled and converged at {} byte offsets in {:?}",
                count,
                started.elapsed()
            );
            Ok(())
        })
        .await
}

#[tokio::test(start_paused = true)]
async fn at_most_four_peers_are_in_flight_and_online_resets_backoff() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let mut peers = Vec::new();
            for name in ["b", "c", "d", "e", "f"] {
                peers.push(
                    enrolled(
                        &root.path().join(name),
                        &hub,
                        name,
                        Some((&a, Profile::Issuer)),
                    )
                    .await?,
                );
            }
            targets(&a, &peers).await?;
            let before = hub.counts()[1];
            hub.stalled.store(true, Ordering::SeqCst);
            start(&a, true).await?;
            until(|| hub.counts()[1] == before + 4).await;
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }
            assert_eq!(hub.counts()[1], before + 4);
            start(&a, false).await?;
            hub.stalled.store(false, Ordering::SeqCst);
            for peer in &peers {
                peer.detach(None).await;
            }
            let mut status = a.sync_status.subscribe();
            start(&a, true).await?;
            status.changed().await?;
            settled(&a).await;
            assert!(
                a.retries
                    .lock()
                    .expect("retries")
                    .values()
                    .all(|state| !state.retry.ready())
            );
            let before = hub.counts()[1];
            a.dispatch(&*a.client(1)?, &json!({"type":"connection", "online":true}))
                .await?;
            until(|| hub.counts()[1] >= before + 5).await;
            start(&a, false).await?;
            a.detach(None).await;
            Ok(())
        }))
        .await
}
