use super::*;

async fn pair(root: &std::path::Path, hub: &Arc<Hub>) -> Result<(Arc<Node>, Arc<Node>), Failure> {
    let a = enrolled(&root.join("a"), hub, "a", None).await?;
    let b = enrolled(
        &root.join("b"),
        hub,
        "b",
        Some((
            &a,
            Profile::Reader {
                scopes: ["items".into()].into(),
            },
        )),
    )
    .await?;
    targets(&a, std::slice::from_ref(&b)).await?;
    targets(&b, std::slice::from_ref(&a)).await?;
    start(&a, true).await?;
    start(&b, true).await?;
    until(|| {
        a.current().is_some_and(|s| s.live.len() == 1)
            && b.current().is_some_and(|s| s.live.len() == 1)
    })
    .await;
    settled(&a).await;
    settled(&b).await;
    Ok((a, b))
}

#[tokio::test(start_paused = true)]
async fn a_contact_request_reuses_the_open_link() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let requested = PrivateKey::new().public_key();
            a.configure(|settings| {
                settings.peers.insert(
                    requested,
                    Contact {
                        routes: vec![crate::enrollment::Route {
                            transport: "memory".into(),
                            address: "contact-only".into(),
                        }],
                    },
                );
                Ok(())
            })
            .await?;
            let before = [hub.dials_to("a"), hub.dials_to("b")];
            let reply = b
                .request_contact(&*transport(&b).await?, secure_node_id(requested))
                .await?;
            assert_eq!(reply["node"], requested.to_hex());
            assert!(b.settings.lock().await.peers.contains_key(&requested));
            assert_eq!([hub.dials_to("a"), hub.dials_to("b")], before);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn committed_edit_pushes_one_message_without_a_stream_or_receiver_reconcile()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let before = [hub.dials_to("a"), hub.dials_to("b")];
            let messages = [hub.link_messages("a"), hub.link_messages("b")];
            let activity = b.activity.after(0)["token"]
                .as_str()
                .ok_or("activity token")?
                .parse()?;
            add(&a, 1).await?;
            until(|| *b.replica.observe_revision().borrow() > 0).await;
            settled(&a).await;
            settled(&b).await;
            assert_eq!(rows(&b).await?, ["1"]);
            assert_eq!(hub.dials_to("b"), before[1], "publisher must not dial");
            assert_eq!(hub.dials_to("a"), before[0], "receiver must not dial");
            assert_eq!(
                hub.link_messages("a"),
                messages[0] + 1,
                "one committed push"
            );
            assert_eq!(
                hub.link_messages("b"),
                messages[1],
                "no receipt or reconcile traffic"
            );
            assert!(
                b.activity.after(activity)["events"]
                    .as_array()
                    .ok_or("events")?
                    .iter()
                    .all(|event| event["kind"] != "peer_reached" && event["kind"] != "rows_synced")
            );
            // An idle link opens no new streams as time passes.
            let idle = hub.counts();
            for _ in 0..4 {
                tokio::time::advance(Duration::from_secs(15)).await;
                for _ in 0..100 {
                    tokio::task::yield_now().await;
                }
            }
            assert_eq!(hub.counts(), idle);
            assert_eq!(a.current().ok_or("service")?.live.len(), 1);
            assert_eq!(b.current().ok_or("service")?.live.len(), 1);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn reverse_opened_link_delivers_when_receiver_cannot_be_dialed() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            targets(&b, std::slice::from_ref(&a)).await?;
            // Enable the publisher's link acceptor without initiating a pass.
            a.automatic.store(true, Ordering::SeqCst);
            hub.stall_peer("b", true);
            start(&b, true).await?;
            until(|| {
                a.current().is_some_and(|s| s.live.len() == 1)
                    && b.current().is_some_and(|s| s.live.len() == 1)
            })
            .await;
            settled(&b).await;
            let before = hub.dials_to("b");
            add(&a, 1).await?;
            until(|| *b.replica.observe_revision().borrow() > 0).await;
            settled(&b).await;
            assert_eq!(rows(&b).await?, ["1"]);
            assert_eq!(hub.dials_to("b"), before);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn explicit_sync_without_the_worker_closes_its_short_link() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            targets(&b, std::slice::from_ref(&a)).await?;
            add(&a, 1).await?;
            let links = hub.protocol_dials("b", "a", Protocol::Link);
            let synced = b.dispatch(&*b.client(1)?, &json!({"type":"sync"})).await?;
            assert_eq!(synced["state"], "connected");
            assert_eq!(rows(&b).await?, ["1"]);
            assert_eq!(hub.protocol_dials("b", "a", Protocol::Link), links + 1);
            // The reconcile ended the short link on both sides.
            until(|| {
                a.current().is_some_and(|s| s.live.len() == 0)
                    && b.current().is_some_and(|s| s.live.len() == 0)
            })
            .await;
            let counts = hub.counts();
            tokio::time::advance(Duration::from_secs(3600)).await;
            for _ in 0..100 {
                tokio::task::yield_now().await;
            }
            assert_eq!(hub.counts(), counts);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn authority_offline_disable_and_detach_end_owned_links() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = a.current().ok_or("service")?;
            let c = enrolled(
                &root.path().join("c"),
                &hub,
                "c",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            targets(&a, std::slice::from_ref(&c)).await?;
            start(&c, true).await?;
            let peers = [
                PrivateKey::from_bytes(&b.vault.get("node")?).public_key(),
                PrivateKey::from_bytes(&c.vault.get("node")?).public_key(),
            ];
            until(|| peers.iter().all(|peer| service.live.contains(*peer))).await;
            let links: Vec<_> = peers
                .iter()
                .map(|peer| (*peer, service.live.id(*peer)))
                .collect();
            // Renewal replaces every authenticated link, including inbound links.
            let chain = a.settings.lock().await.credentials.clone();
            let before = hub.counts()[1];
            a.replica.renew_credentials(chain).await?;
            until(|| hub.counts()[1] > before).await;
            until(|| {
                links
                    .iter()
                    .all(|(peer, id)| service.live.id(*peer).is_some_and(|new| Some(new) != *id))
            })
            .await;
            let before_pause = tokio::time::Instant::now();
            start(&a, false).await?;
            assert!(tokio::time::Instant::now() - before_pause < Duration::from_secs(1));
            assert_eq!(
                service.live.len(),
                0,
                "pause acknowledges only after every link closes"
            );
            assert!(
                a.peers()
                    .await
                    .iter()
                    .all(|peer| peer["link"]["state"] == "closed")
            );
            start(&a, true).await?;
            until(|| peers.iter().all(|peer| service.live.contains(*peer))).await;
            assert!(
                a.peers()
                    .await
                    .iter()
                    .all(|peer| peer["link"]["state"] == "open")
            );
            a.dispatch(
                &*a.client(1)?,
                &json!({"type":"connection", "online":false}),
            )
            .await?;
            until(|| service.live.len() == 0).await;
            a.detach(None).await;
            b.detach(None).await;
            c.detach(None).await;
            assert_eq!(service.live.len(), 0);
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn an_idle_linked_pair_dials_nothing_for_an_hour() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let (sa, sb) = (a.current().ok_or("service")?, b.current().ok_or("service")?);
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let link = sa.live.id(peer);
            let before = [hub.dials_to("a"), hub.dials_to("b")];
            // An idle link writes nothing; nothing dials, polls or replaces it.
            let written = *hub.written().borrow();
            for _ in 0..240 {
                tokio::time::advance(Duration::from_secs(15)).await;
                for _ in 0..100 {
                    tokio::task::yield_now().await;
                }
            }
            assert_eq!([hub.dials_to("a"), hub.dials_to("b")], before);
            assert_eq!(*hub.written().borrow(), written);
            assert_eq!((sa.live.len(), sb.live.len()), (1, 1));
            assert_eq!(sa.live.id(peer), link);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_transport_ended_link_closes_and_reconnects_with_backoff() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = a.current().ok_or("service")?;
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let status = a.readiness().await?;
            assert_eq!(status["network"]["peers"][0]["link"]["state"], "open");
            assert!(status["network"]["peers"][0]["link"]["opened"].is_string());
            // The hub measures no paths; iroh's are covered by its liveness gate.
            assert_eq!(service.live.status(peer)["paths"], json!([]));
            assert!(service.live.status(peer)["round_trip_ms"].is_null());
            let sync_token = a.sync_status.borrow()["token"].clone();
            let client = a.client(1)?;
            let token = status["token"].as_str().ok_or("readiness token")?;
            let mut waiting = Box::pin(a.readiness_after(&client, token));
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            let dials = hub.protocol_dials("a", "b", Protocol::Link);
            // As iroh's idle timeout ends a connection to a vanished peer.
            hub.sever_links("b");
            until(|| service.live.len() == 0).await;
            assert!(
                a.retries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&peer)
                    .is_some_and(|state| state.link_retry.deadline().is_some())
            );
            let closed = waiting.await?;
            assert_eq!(closed["network"]["peers"][0]["link"]["state"], "closed");
            assert_ne!(closed["token"], status["token"]);
            assert_ne!(a.sync_status.borrow()["token"], sync_token);
            assert!(
                a.activity.after(0)["events"]
                    .as_array()
                    .ok_or("events")?
                    .iter()
                    .any(|event| event["kind"] == "link_closed" && event["reason"] == "error")
            );
            assert_eq!(hub.protocol_dials("a", "b", Protocol::Link), dials);
            tokio::time::advance(Duration::from_secs(1)).await;
            until(|| service.live.len() == 1).await;
            assert!(hub.protocol_dials("a", "b", Protocol::Link) > dials);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn saturated_link_budget_leaves_pair_and_command_admission_free() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = a.current().ok_or("service")?;
            let remaining = service.live_budget.available_permits();
            let _full = service
                .live_budget
                .clone()
                .try_acquire_many_owned(u32::try_from(remaining)?)?;
            let transport = transport(&b).await?;
            let stream = transport.connect("a", Protocol::Link).await?;
            let settings = b.settings.lock().await.clone();
            let mut refused = rocky_secure::network::link::Link::open(
                stream,
                &b.replica,
                PrivateKey::from_bytes(&b.vault.get("node")?),
                settings.credentials,
                Some(PrivateKey::from_bytes(&a.vault.get("node")?).public_key()),
            )
            .await?;
            assert!(matches!(
                refused.reader.receive().await?.message,
                rocky_secure::network::link::Message::Close {
                    reason: rocky_secure::network::link::CloseReason::Busy
                }
            ));
            let before = service.served.load(Ordering::Relaxed);
            let pair = transport.connect("a", Protocol::Pair).await?;
            let command = transport.connect("a", Protocol::Command).await?;
            until(|| service.served.load(Ordering::Relaxed) >= before + 2).await;
            assert_eq!(service.live_budget.available_permits(), 0);
            drop(pair);
            drop(command);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn simultaneous_dials_keep_the_lower_nodes_stream() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            a.automatic.store(true, Ordering::SeqCst);
            b.automatic.store(true, Ordering::SeqCst);
            let sa = a.current().ok_or("service")?;
            let sb = b.current().ok_or("service")?;
            let ka = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let kb = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let (ab, ba) = tokio::join!(
                sa.transport.connect("b", Protocol::Link),
                sb.transport.connect("a", Protocol::Link)
            );
            let ca = a.settings.lock().await.clone();
            let cb = b.settings.lock().await.clone();
            let (ra, rb) = tokio::join!(
                a.admit_link(sa.clone(), ab?, Some(kb), ca, None),
                b.admit_link(sb.clone(), ba?, Some(ka), cb, None),
            );
            ra?;
            rb?;
            until(|| sa.live.dialed(kb) == Some(ka < kb) && sb.live.dialed(ka) == Some(kb < ka))
                .await;
            assert_eq!((sa.live.len(), sb.live.len()), (1, 1));
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn simultaneous_short_syncs_reconcile_after_a_duplicate_stream_ends() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            targets(&a, std::slice::from_ref(&b)).await?;
            targets(&b, std::slice::from_ref(&a)).await?;
            add(&a, 1).await?;
            let ta = transport(&a).await?;
            let tb = transport(&b).await?;
            let (first, second) = tokio::join!(a.sync(&*ta), b.sync(&*tb));
            assert_eq!(first?.0["state"], "connected");
            assert_eq!(second?.0["state"], "connected");
            assert_eq!(rows(&b).await?, ["1"]);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn losing_short_sync_cleanup_leaves_the_replacement_link_open() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            let ka = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let kb = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let (lower, higher, lower_key, higher_key, lower_address, higher_address) = if ka < kb {
                (&a, &b, ka, kb, "a", "b")
            } else {
                (&b, &a, kb, ka, "b", "a")
            };
            let low_service = lower.current().ok_or("service")?;
            let high_service = higher.current().ok_or("service")?;
            let stream = high_service
                .transport
                .connect(lower_address, Protocol::Link)
                .await?;
            let high_settings = higher.settings.lock().await.clone();
            higher
                .admit_link(
                    high_service.clone(),
                    stream,
                    Some(lower_key),
                    high_settings,
                    None,
                )
                .await?;
            until(|| low_service.live.contains(higher_key)).await;
            let old_id = high_service.live.id(lower_key);
            let cleanup = high_service.live.short(lower_key);
            let stream = low_service
                .transport
                .connect(higher_address, Protocol::Link)
                .await?;
            let low_settings = lower.settings.lock().await.clone();
            lower
                .admit_link(
                    low_service.clone(),
                    stream,
                    Some(higher_key),
                    low_settings,
                    None,
                )
                .await?;
            until(|| high_service.live.id(lower_key) != old_id).await;
            drop(cleanup);
            assert!(high_service.live.contains(lower_key));
            assert!(low_service.live.contains(higher_key));
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn learned_revocation_closes_the_revoked_peers_link() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = a.current().ok_or("service")?;
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            learn_revocation(&a, peer).await?;
            until(|| !service.live.contains(peer)).await;
            assert!(a.replica.revocations().await.revokes(&peer));
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

async fn real_pair(
    root: &std::path::Path,
    hub: &Arc<Hub>,
) -> Result<(Arc<Node>, Arc<Node>, String, String), Failure> {
    let a = enrolled(&root.join("a"), hub, "a", None).await?;
    let b = enrolled(
        &root.join("b"),
        hub,
        "b",
        Some((
            &a,
            Profile::Reader {
                scopes: ["items".into()].into(),
            },
        )),
    )
    .await?;
    a.detach(None).await;
    b.detach(None).await;
    let connector: Arc<dyn Connector> = Arc::new(rocky_iroh::Options::isolated("127.0.0.1:0"));
    let ta = connector
        .start(true, Settings::transport_key(&a.vault)?)
        .await?;
    let tb = connector
        .start(true, Settings::transport_key(&b.vault)?)
        .await?;
    let aa = ta.address().await?;
    let ab = tb.address().await?;
    a.link(hub.wrap("a", ta), true).await;
    b.link(hub.wrap("b", tb), true).await;
    targets(&a, std::slice::from_ref(&b)).await?;
    targets(&b, std::slice::from_ref(&a)).await?;
    Ok((a, b, aa, ab))
}

async fn real_signal_gate() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b, aa, ab) = real_pair(root.path(), &hub).await?;
            start(&a, true).await?;
            start(&b, true).await?;
            until(|| {
                a.current().is_some_and(|s| s.live.len() == 1)
                    && b.current().is_some_and(|s| s.live.len() == 1)
            })
            .await;
            settled(&a).await;
            settled(&b).await;
            let before = [hub.dials_to(&aa), hub.dials_to(&ab)];
            add(&a, 1).await?;
            until(|| *b.replica.observe_revision().borrow() > 0).await;
            settled(&b).await;
            assert_eq!(rows(&b).await?, ["1"]);
            assert_eq!(hub.dials_to(&ab), before[1]);
            assert_eq!(hub.dials_to(&aa), before[0]);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}
#[tokio::test]
async fn iroh_signal_gate() -> Result<(), Failure> {
    real_signal_gate().await
}

async fn real_reverse_gate() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b, _, ab) = real_pair(root.path(), &hub).await?;
            a.automatic.store(true, Ordering::SeqCst);
            hub.stall_peer(&ab, true);
            start(&b, true).await?;
            until(|| {
                a.current().is_some_and(|s| s.live.len() == 1)
                    && b.current().is_some_and(|s| s.live.len() == 1)
            })
            .await;
            settled(&b).await;
            let before = hub.dials_to(&ab);
            add(&a, 1).await?;
            until(|| *b.replica.observe_revision().borrow() > 0).await;
            settled(&b).await;
            assert_eq!(rows(&b).await?, ["1"]);
            assert_eq!(hub.dials_to(&ab), before);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}
#[tokio::test]
async fn iroh_reverse_gate() -> Result<(), Failure> {
    real_reverse_gate().await
}

// Stop both workers before either endpoint closes, so neither redials a closing peer.
async fn detach_real_pair(a: &Node, b: &Node) -> Result<(), Failure> {
    let (stopped_a, stopped_b) = tokio::join!(start(a, false), start(b, false));
    stopped_a?;
    stopped_b?;
    tokio::join!(a.detach(None), b.detach(None));
    Ok(())
}

async fn real_liveness_gate() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b, _, ab) = real_pair(root.path(), &hub).await?;
            start(&a, true).await?;
            start(&b, true).await?;
            let sa = a.current().ok_or("service")?;
            let sb = b.current().ok_or("service")?;
            until(|| sa.live.len() == 1 && sb.live.len() == 1).await;
            settled(&a).await;
            settled(&b).await;
            let before = hub.counts();
            assert!(
                a.retry_deadline().is_none(),
                "retry while idle: {} {}",
                *a.sync_status.borrow(),
                a.activity.after(0)
            );
            assert!(
                b.retry_deadline().is_none(),
                "retry while idle: {} {}",
                *b.sync_status.borrow(),
                b.activity.after(0)
            );
            // iroh keeps the idle connection alive below the stream: the
            // link itself writes nothing and nothing redials.
            let written = *hub.written().borrow();
            for _ in 0..4 {
                tokio::time::advance(Duration::from_secs(15)).await;
                for _ in 0..100 {
                    tokio::task::yield_now().await;
                }
            }
            assert_eq!(hub.counts(), before);
            assert_eq!(*hub.written().borrow(), written);
            assert_eq!((sa.live.len(), sb.live.len()), (1, 1));
            assert_eq!(hub.protocol_dials("a", &ab, Protocol::Link), 1);
            // Status measures the connection's paths when read.
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let link = sa.live.status(peer);
            let paths = link["paths"].as_array().ok_or("paths")?;
            let selected = paths.first().ok_or("no measured path")?;
            assert_eq!(selected["selected"], true);
            assert_eq!(selected["kind"], "direct");
            assert!(
                selected["remote"]
                    .as_str()
                    .is_some_and(|remote| remote.starts_with("127.0.0.1:"))
            );
            assert!(selected["rtt_us"].is_string());
            assert!(
                selected["sent_bytes"]
                    .as_str()
                    .ok_or("sent")?
                    .parse::<u64>()?
                    > 0
            );
            assert!(link["round_trip_ms"].is_string());
            detach_real_pair(&a, &b).await?;
            Ok(())
        }))
        .await
}
#[tokio::test(start_paused = true)]
async fn iroh_liveness_gate() -> Result<(), Failure> {
    real_liveness_gate().await
}

async fn real_authority_gate() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b, _, _) = real_pair(root.path(), &hub).await?;
            start(&a, true).await?;
            start(&b, true).await?;
            let sa = a.current().ok_or("service")?;
            let sb = b.current().ok_or("service")?;
            until(|| sa.live.len() == 1 && sb.live.len() == 1).await;
            settled(&a).await;
            settled(&b).await;
            let dials = hub.counts()[1];
            let credentials = a.settings.lock().await.credentials.clone();
            a.replica.renew_credentials(credentials).await?;
            until(|| hub.counts()[1] > dials).await;
            until(|| sa.live.len() == 1 && sb.live.len() == 1).await;
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            learn_revocation(&a, peer).await?;
            until(|| !sa.live.contains(peer)).await;
            assert!(a.replica.revocations().await.revokes(&peer));
            detach_real_pair(&a, &b).await?;
            Ok(())
        }))
        .await
}
#[tokio::test]
async fn iroh_authority_gate() -> Result<(), Failure> {
    real_authority_gate().await
}

async fn real_tie_gate() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b, aa, ab) = real_pair(root.path(), &hub).await?;
            a.automatic.store(true, Ordering::SeqCst);
            b.automatic.store(true, Ordering::SeqCst);
            let sa = a.current().ok_or("service")?;
            let sb = b.current().ok_or("service")?;
            let ka = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let kb = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let ca = a.settings.lock().await.clone();
            let cb = b.settings.lock().await.clone();
            let (near, far) = tokio::join!(
                sa.transport.connect(&ab, Protocol::Link),
                sb.transport.connect(&aa, Protocol::Link)
            );
            let (one, two) = tokio::join!(
                a.admit_link(sa.clone(), near?, Some(kb), ca, None),
                b.admit_link(sb.clone(), far?, Some(ka), cb, None)
            );
            one?;
            two?;
            until(|| sa.live.dialed(kb) == Some(ka < kb) && sb.live.dialed(ka) == Some(kb < ka))
                .await;
            assert_eq!((sa.live.len(), sb.live.len()), (1, 1));
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}
#[tokio::test]
async fn iroh_tie_gate() -> Result<(), Failure> {
    real_tie_gate().await
}

async fn real_budget_gate() -> Result<(), Failure> {
    use rocky_secure::network::link::{CloseReason, Message};
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b, aa, _) = real_pair(root.path(), &hub).await?;
            start(&a, true).await?;
            start(&b, true).await?;
            let sa = a.current().ok_or("service")?;
            until(|| sa.live.len() == 1 && b.current().is_some_and(|s| s.live.len() == 1)).await;
            settled(&a).await;
            settled(&b).await;
            let full = sa
                .live_budget
                .clone()
                .try_acquire_many_owned(u32::try_from(sa.live_budget.available_permits())?)?;
            let transport = transport(&b).await?;
            let stream = transport.connect(&aa, Protocol::Link).await?;
            let mut refused = rocky_secure::network::link::Link::open(
                stream,
                &b.replica,
                PrivateKey::from_bytes(&b.vault.get("node")?),
                b.settings.lock().await.credentials.clone(),
                Some(PrivateKey::from_bytes(&a.vault.get("node")?).public_key()),
            )
            .await?;
            assert!(matches!(
                refused.reader.receive().await?.message,
                Message::Close {
                    reason: CloseReason::Busy
                }
            ));
            let before = sa.served.load(Ordering::Relaxed);
            let mut pair = transport.connect(&aa, Protocol::Pair).await?;
            let mut command = transport.connect(&aa, Protocol::Command).await?;
            // QUIC exposes a newly opened stream to the peer on its first write.
            tokio::io::AsyncWriteExt::write_all(&mut pair, &[0]).await?;
            tokio::io::AsyncWriteExt::write_all(&mut command, &[0]).await?;
            until(|| sa.served.load(Ordering::Relaxed) >= before + 2).await;
            assert_eq!(sa.live_budget.available_permits(), 0);
            drop(pair);
            drop(command);
            drop(full);
            assert!(sa.live_budget.available_permits() > 0);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}
#[tokio::test]
async fn iroh_budget_gate() -> Result<(), Failure> {
    real_budget_gate().await
}

#[tokio::test(start_paused = true)]
async fn a_busy_link_backs_off_without_another_sync_path_and_reopens_when_a_slot_frees()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = a.current().ok_or("service")?;
            let full = service
                .live_budget
                .clone()
                .try_acquire_many_owned(u32::try_from(service.live_budget.available_permits())?)?;
            hub.stall_peer("c", true);
            let c = enrolled(
                &root.path().join("c"),
                &hub,
                "c",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            start(&c, true).await?;
            let peer = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let backoff = || {
                c.retries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&peer)
                    .and_then(|retry| retry.retry.deadline().max(retry.link_retry.deadline()))
            };
            until(|| c.current().is_some_and(|s| !s.live.contains(peer)) && backoff().is_some())
                .await;
            // Explicit sync fails too; nothing else carries the row.
            add(&a, 1).await?;
            let transport = transport(&c).await?;
            assert!(c.sync(&*transport).await.is_err());
            assert!(rows(&c).await?.is_empty());
            drop(full);
            let deadline = backoff().ok_or("back-off")?;
            tokio::time::advance(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .await;
            until(|| c.current().is_some_and(|s| s.live.contains(peer))).await;
            c.sync(&*transport).await?;
            assert_eq!(rows(&c).await?, ["1"]);
            a.detach(None).await;
            b.detach(None).await;
            c.detach(None).await;
            Ok(())
        }))
        .await
}

// Inject a verified incoming statement without renewal or key installation.
async fn learn_revocation(node: &Node, peer: PublicKey) -> Result<(), Failure> {
    let issuer = PrivateKey::from_bytes(&node.vault.get("node")?);
    let publisher = PrivateKey::from_bytes(&node.vault.get("publisher")?);
    let statement = rocky_credentials::SignedRevocation::sign(
        rocky_credentials::Revocation {
            app: node.domain.manifest().app,
            issuer: issuer.public_key(),
            time: now()?,
            nodes: [rocky_credentials::PeerId::of(peer)].into(),
            users: Default::default(),
            capabilities: Default::default(),
            pseudonyms: Default::default(),
        },
        &publisher,
        &issuer,
    )?;
    node.replica
        .learn_revocations(&RevocationSet::try_from(vec![statement])?)
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_burst_exceeds_push_bound_and_finishes_by_link_catch_up_in_log_order()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let before = hub.counts();
            let reconcile = b.syncing.lock().await;
            let service = a.current().ok_or("service")?;
            let publish = service.live.publishing.lock().await;
            for key in 1..=40 {
                add(&a, key).await?;
            }
            drop(publish);
            until(|| *b.replica.observe_revision().borrow() > 0).await;
            let partial = rows(&b).await?;
            assert!(
                !partial.is_empty() && partial.len() < 40,
                "the bounded push sends only a prefix"
            );
            let mut numbers = partial
                .iter()
                .map(|value| value.parse::<usize>())
                .collect::<Result<Vec<_>, _>>()?;
            numbers.sort();
            assert_eq!(numbers, (1..=partial.len()).collect::<Vec<_>>());
            let frontier = b.replica.frontier(b.app_scopes()[0]).await?;
            let author = a.replica.frontier(a.app_scopes()[0]).await?.logs[0].author;
            assert_eq!(
                frontier.log(author).ok_or("author log")?.applied,
                partial.len() as u64
            );
            drop(reconcile);
            while rows(&b).await?.len() != 40 {
                tokio::task::yield_now().await;
            }
            settled(&a).await;
            settled(&b).await;
            assert_eq!(rows(&a).await?, rows(&b).await?);
            assert_eq!(hub.counts(), before, "push and catch-up reuse the stream");
            let frontier = b.replica.frontier(b.app_scopes()[0]).await?;
            assert_eq!(frontier.log(author).ok_or("author log")?.applied, 40);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn two_writers_push_concurrently_and_converge_on_the_same_scope() -> Result<(), Failure> {
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
            targets(&b, std::slice::from_ref(&a)).await?;
            start(&a, true).await?;
            start(&b, true).await?;
            until(|| {
                a.current().is_some_and(|s| s.live.len() == 1)
                    && b.current().is_some_and(|s| s.live.len() == 1)
            })
            .await;
            settled(&a).await;
            settled(&b).await;
            let before = hub.counts();
            let (left, right) = tokio::join!(add(&a, 1), add(&b, 2));
            left?;
            right?;
            while rows(&a).await? != ["1", "2"] || rows(&b).await? != ["1", "2"] {
                tokio::task::yield_now().await;
            }
            settled(&a).await;
            settled(&b).await;
            assert_eq!(hub.counts(), before);
            assert_eq!(
                a.replica.frontier(a.app_scopes()[0]).await?,
                b.replica.frontier(b.app_scopes()[0]).await?
            );
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn revocation_learned_mid_push_cancels_the_partial_frame_and_its_authority()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = a.current().ok_or("service")?;
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let closing = service.live.closing.load(Ordering::SeqCst);
            let mut written = hub.written();
            let before = *written.borrow_and_update();
            hub.hold_cut.store(true, Ordering::SeqCst);
            hub.cut_after(Some(36));
            add(&a, 1).await?;
            written.wait_for(|bytes| *bytes == before + 36).await?;
            assert!(
                rows(&b).await?.is_empty(),
                "an unfinished signed frame cannot apply"
            );
            learn_revocation(&a, peer).await?;
            until(|| service.live.closing.load(Ordering::SeqCst) > closing).await;
            tokio::time::advance(Duration::from_millis(100)).await;
            until(|| service.live.len() == 0).await;
            assert!(a.replica.revocations().await.revokes(&peer));
            hub.cut_after(None);
            hub.hold_cut.store(false, Ordering::SeqCst);
            let error = b.sync(&*transport(&b).await?).await.unwrap_err();
            assert!(
                error.is::<crate::enrollment::Revoked>() || error.to_string().contains("revoked")
            );
            assert!(rows(&b).await?.is_empty());
            assert_eq!(
                rows(&a).await?,
                ["1"],
                "the local commit survives cancelled publication"
            );
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_verified_renewal_releases_only_its_issuers_data_backoff() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((
                    &a,
                    Profile::Reader {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            add(&a, 1).await?;
            let peer = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let other = PrivateKey::new().public_key();
            let a_port = transport(&a).await?;
            let b_port = transport(&b).await?;
            a_port.listen(false);
            assert!(b.sync(&*b_port).await.is_err());
            assert!(
                b.retries.lock().unwrap_or_else(PoisonError::into_inner)[&peer]
                    .retry
                    .deadline()
                    .is_some()
            );
            a_port.listen(true);
            b.retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(other)
                .or_default()
                .retry
                .failed();
            let other_deadline = b.retries.lock().unwrap_or_else(PoisonError::into_inner)[&other]
                .retry
                .deadline();
            let before = tokio::time::Instant::now();
            b.renew(Some(&*b_port), false, Some(peer), false).await?;
            {
                let retries = b.retries.lock().unwrap_or_else(PoisonError::into_inner);
                assert!(retries[&peer].retry.ready());
                assert_eq!(retries[&other].retry.deadline(), other_deadline);
            }
            b.sync(&*b_port).await?;
            assert_eq!(rows(&b).await?, ["1"]);
            assert_eq!(tokio::time::Instant::now(), before);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn unchanged_epoch_probe_ignores_contacts_changed_while_the_reply_was_in_flight()
-> Result<(), Failure> {
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
            a.sync(&*transport(&a).await?).await?;
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let authority = a.replica.observe_authority().await;
            let before = hub.protocol_dials("a", "b", Protocol::Renew);
            hub.hold_cut.store(true, Ordering::SeqCst);
            hub.cut_after(Some(0));
            let transport = transport(&a).await?;
            let renew = a.renew(Some(&*transport), true, Some(peer), true);
            let metadata = async {
                until(|| hub.protocol_dials("a", "b", Protocol::Renew) > before).await;
                a.configure(|settings| {
                    let meta = settings
                        .contact_meta
                        .get_mut(&peer)
                        .ok_or("contact metadata")?;
                    meta.learned = meta.learned.saturating_add(1);
                    Ok(())
                })
                .await?;
                hub.cut_after(None);
                Ok::<_, Failure>(())
            };
            let (renewed, changed) = tokio::join!(renew, metadata);
            changed?;
            renewed?;
            assert!(
                !authority.has_changed()?,
                "an unchanged epoch response must not invalidate admission"
            );
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn link_status_reports_opening_and_clears_cancelled_dials() -> Result<(), Failure> {
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
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            hub.stall_peer("b", true);
            let transport = transport(&a).await?;
            let contact = a.settings.lock().await.peers[&peer].clone();
            let mut opening = Box::pin(a.request_link(&*transport, peer, &contact));
            assert!(futures_util::poll!(opening.as_mut()).is_pending());
            assert_eq!(a.peers().await[0]["link"]["state"], "opening");
            let token = a.readiness().await?["token"].clone();
            drop(opening);
            assert_eq!(a.peers().await[0]["link"]["state"], "closed");
            assert_ne!(a.readiness().await?["token"], token);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn background_refuses_new_links_until_foreground() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let client = a.client(1)?;
            let before = tokio::time::Instant::now();
            start(&a, false).await?;
            a.dispatch(&client, &json!({"type":"network_background"}))
                .await?;
            assert!(tokio::time::Instant::now() - before < Duration::from_secs(1));
            assert!(
                a.peers()
                    .await
                    .iter()
                    .all(|peer| peer["link"]["state"] == "closed")
            );
            let peer = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let service = b.current().ok_or("service")?;
            until(|| !service.live.contains(peer)).await;
            let contact = b.settings.lock().await.peers[&peer].clone();
            assert!(
                b.request_link(&*transport(&b).await?, peer, &contact)
                    .await
                    .is_err()
            );
            assert_eq!(a.current().ok_or("service")?.live.len(), 0);
            a.dispatch(&client, &json!({"type":"network_foreground"}))
                .await?;
            start(&a, true).await?;
            until(|| a.current().is_some_and(|service| service.live.len() == 1)).await;
            assert_eq!(a.peers().await[0]["link"]["state"], "open");
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}
