use super::*;
use rocky_secure::network::link;

#[derive(Debug, Default)]
struct PublicationTraffic {
    sessions: usize,
    families: BTreeMap<&'static str, usize>,
    messages: BTreeMap<&'static str, usize>,
    signed_bytes: usize,
    queue_us: Vec<u64>,
    receipts: usize,
    repair_scopes: usize,
    repair_all: usize,
    repair_reasons: BTreeMap<rocky_secure::network::PublicationRepairReason, usize>,
}

fn traffic_snapshot(a: &Link, b: &Link) -> Vec<link::channels::Traffic> {
    a.live
        .traffic()
        .into_iter()
        .chain(b.live.traffic())
        .collect()
}

impl PublicationTraffic {
    fn between(before: &[link::channels::Traffic], after: Vec<link::channels::Traffic>) -> Self {
        assert_eq!(
            before.len(),
            after.len(),
            "link replacement during publication"
        );
        let mut delta = Self::default();
        for (before, after) in before.iter().zip(after) {
            delta.sessions += after.sessions - before.sessions;
            delta.signed_bytes += after.signed_bytes - before.signed_bytes;
            delta.receipts += after.receipts - before.receipts;
            delta.repair_scopes += after.repair_scopes - before.repair_scopes;
            delta.repair_all += after.repair_all - before.repair_all;
            for (key, count) in after.families {
                let count = count - before.families.get(key).copied().unwrap_or_default();
                if count > 0 {
                    *delta.families.entry(key).or_default() += count;
                }
            }
            for (key, count) in after.publication_parts {
                let count = count
                    - before
                        .publication_parts
                        .get(key)
                        .copied()
                        .unwrap_or_default();
                if count > 0 {
                    *delta.messages.entry(key).or_default() += count;
                }
            }
            for (key, count) in after.repair_reasons {
                let count = count - before.repair_reasons.get(&key).copied().unwrap_or_default();
                if count > 0 {
                    *delta.repair_reasons.entry(key).or_default() += count;
                }
            }
            if after.intakes > before.intakes {
                delta
                    .queue_us
                    .push(after.last_queue_us.expect("intake queue measurement"));
            }
        }
        delta
    }
    fn assert_complete(&self) {
        assert_eq!(self.sessions, 2, "one channel's two endpoints: {self:?}");
        assert_eq!(
            self.families,
            [("chains", 1), ("escrow", 1), ("publication", 4)].into(),
            "unexpected control/discovery/row traffic: {self:?}"
        );
        assert_eq!(
            self.messages,
            ["start", "chains", "escrow", "page", "finish", "receipt"]
                .into_iter()
                .map(|part| (part, 1))
                .collect(),
            "{self:?}"
        );
        assert_eq!(self.receipts, 1, "{self:?}");
        assert_eq!(self.repair_scopes, 0, "{self:?}");
        assert_eq!(self.repair_all, 0, "{self:?}");
        assert!(self.repair_reasons.is_empty(), "{self:?}");
        assert_eq!(self.queue_us.len(), 1, "{self:?}");
    }
}

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
async fn committed_edit_publishes_on_the_link_without_receiver_reconcile() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            crate::escrow::keep(&a.replica, &*a.settings.lock().await, &a.vault, &a.domain())
                .await?;
            let service_a = a.current().ok_or("service")?;
            let service_b = b.current().ok_or("service")?;
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            let before = [hub.dials_to("a"), hub.dials_to("b")];
            let messages = [hub.link_messages("a"), hub.link_messages("b")];
            let activity = b.activity.after(0)["token"]
                .as_str()
                .ok_or("activity token")?
                .parse()?;
            let traffic_before = traffic_snapshot(&service_a, &service_b);
            let bytes_before = *hub.written().borrow();
            let mut changes = b.replica.observe_tables(Some(&["items"])).await?;
            let (committed, observed) = tokio::join!(
                async {
                    add(&a, 1).await?;
                    Ok::<_, Failure>(std::time::Instant::now())
                },
                async {
                    let change = changes.next().await?;
                    assert!(change.causes.contains(&rocky_storage::observe::Cause::Sync));
                    let notified = std::time::Instant::now();
                    assert_eq!(rows(&b).await?, ["1"]);
                    Ok::<_, Failure>((notified, std::time::Instant::now()))
                }
            );
            let committed = committed?;
            let (notified, snapshot) = observed?;
            settled(&a).await;
            settled(&b).await;
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            assert_eq!(rows(&b).await?, ["1"]);
            let traffic = PublicationTraffic::between(&traffic_before, traffic_snapshot(&service_a, &service_b));
            traffic.assert_complete();
            let notification_us = if notified >= committed { notified.duration_since(committed).as_micros() as i128 }
                else { -(committed.duration_since(notified).as_micros() as i128) };
            eprintln!("tiny publication diagnostic: commit_return_to_notification_us={notification_us} notification_to_snapshot_us={} framed_signed_link_bytes={} traffic={traffic:?}",
                snapshot.duration_since(notified).as_micros(), *hub.written().borrow() - bytes_before);
            eprintln!(
                "tiny publication complete link messages: {:?}",
                [
                    hub.link_messages("a") - messages[0],
                    hub.link_messages("b") - messages[1]
                ]
            );
            assert_eq!(hub.dials_to("b"), before[1], "publisher must not dial");
            assert_eq!(hub.dials_to("a"), before[0], "receiver must not dial");
            assert_eq!(
                hub.link_messages("a"),
                messages[0] + 7,
                "one declaration, five native messages including escrow, and channel close"
            );
            assert_eq!(
                hub.link_messages("b"),
                messages[1] + 2,
                "one receipt and channel close"
            );
            assert!(
                b.activity.after(activity)["events"]
                    .as_array()
                    .ok_or("events")?
                    .iter()
                    .all(|event| event["kind"] != "peer_reconciliation_completed"
                        && event["kind"] != "rows_synced")
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
            assert_eq!(completed_peers(&synced), 1);
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
                &json!({"type":"networking_control", "enabled":false}),
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
            assert!(status["network"]["peers"][0]["link"]["opened_at_epoch_seconds"].is_u64());
            let contact = a.settings.lock().await.peers.remove(&peer);
            let met = a
                .met
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&peer);
            let observed = a.observed_network().await?;
            assert_eq!(observed.connected_peer_count, 1);
            assert!(observed.peers.iter().any(
                |node| node.node == peer.to_hex() && node.link.state == crate::LinkState::Open
            ));
            if let Some(contact) = contact {
                a.settings.lock().await.peers.insert(peer, contact);
            }
            if let Some(met) = met {
                a.met
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(peer, met);
            }

            // The hub measures no paths; iroh's are covered by its liveness gate.
            assert_eq!(service.live.status(peer)["paths"], json!([]));
            assert!(service.live.status(peer)["round_trip_ms"].is_null());
            let sync_token = a.sync_status.borrow()["token"].clone();
            let client = a.client(1)?;
            let token = status["token"].as_str().ok_or("readiness token")?;
            let mut waiting = Box::pin(a.readiness_after(&client, token));
            assert!(futures_util::poll!(waiting.as_mut()).is_pending());
            let dials = hub.protocol_dials("a", "b", Protocol::Link);
            let reverse_dials = hub.protocol_dials("b", "a", Protocol::Link);
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
            let deadline = a.retries.lock().unwrap()[&peer]
                .link_retry
                .deadline()
                .ok_or("link retry deadline")?;
            tokio::time::advance(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .await;
            until(|| service.live.len() == 1).await;
            assert!(
                hub.protocol_dials("a", "b", Protocol::Link)
                    + hub.protocol_dials("b", "a", Protocol::Link)
                    > dials + reverse_dials
            );
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn saturated_link_budget_leaves_pair_admission_free() -> Result<(), Failure> {
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
                settings.credentials.clone(),
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
            until(|| service.served.load(Ordering::Relaxed) > before).await;
            assert_eq!(service.live_budget.available_permits(), 0);
            drop(pair);
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
            assert_eq!(completed_peers(&first?.0), 1);
            assert_eq!(completed_peers(&second?.0), 1);
            assert_eq!(rows(&b).await?, ["1"]);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test]
async fn simultaneous_short_requests_use_the_admitted_link_before_remote_reconcile_ends()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
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
            let ka = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let kb = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let (lower, higher, lower_key, higher_key) = if ka < kb {
                (&a, &b, ka, kb)
            } else {
                (&b, &a, kb, ka)
            };
            let low_service = lower.current().ok_or("service")?;
            let high_service = higher.current().ok_or("service")?;
            let low_contact = lower.settings.lock().await.peers[&higher_key].clone();
            let high_contact = higher.settings.lock().await.peers[&lower_key].clone();
            let (ready, waiting) = tokio::sync::oneshot::channel();
            let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(
                    async {
                        let handle = lower
                            .request_link(&*low_service.transport, higher_key, &low_contact)
                            .await?;
                        let reconcile = handle.begin_reconcile().await?;
                        waiting.await?;
                        reconcile.close().await?;
                        Ok::<_, Failure>(())
                    },
                    async {
                        let handle = higher
                            .request_link(&*high_service.transport, lower_key, &high_contact)
                            .await?;
                        ready.send(()).map_err(|_| "request ended")?;
                        Ok::<_, Failure>(handle)
                    },
                )
            })
            .await?;
            first?;
            second?;
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        })
        .await
}

/// Ends link handshakes after receiving their hello; renewal remains available.
struct EofLinks(Arc<dyn Transport>);
impl Transport for EofLinks {
    fn kind(&self) -> &str {
        self.0.kind()
    }
    fn address(&self) -> crate::transport::BoxFuture<'_, Result<String, Failure>> {
        self.0.address()
    }
    fn connect<'a>(
        &'a self,
        address: &'a str,
        protocol: Protocol,
    ) -> crate::transport::BoxFuture<'a, Result<Stream, Failure>> {
        if protocol != Protocol::Link {
            return self.0.connect(address, protocol);
        }
        Box::pin(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let (near, mut far) = tokio::io::duplex(64 * 1024);
            tokio::task::spawn_local(async move {
                let length = far.read_u32().await.unwrap() as usize;
                far.read_exact(&mut vec![0; length]).await.unwrap();
                far.shutdown().await.unwrap();
            });
            Ok(Stream::new(near))
        })
    }
    fn accept(&self) -> crate::transport::BoxFuture<'_, Option<(Protocol, Stream)>> {
        self.0.accept()
    }
    fn close(&self) -> crate::transport::BoxFuture<'_, ()> {
        self.0.close()
    }
}

#[tokio::test(start_paused = true)]
async fn stream_eof_does_not_renew_credentials() -> Result<(), Failure> {
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
            let authority = b.replica.observe_authority().await;
            let renewals = hub.protocol_dials("b", "a", Protocol::Renew);
            let (report, _) = b.sync(&EofLinks(transport(&b).await?)).await?;
            assert!(
                report["peers"]
                    .as_array()
                    .ok_or("peers")?
                    .iter()
                    .any(|peer| {
                        peer["attempts"].as_array().is_some_and(|attempts| {
                            attempts
                                .iter()
                                .any(|attempt| attempt["failure"]["kind"] == "transport")
                        })
                    })
            );
            assert!(
                !authority.has_changed()?,
                "a failed stream must leave authority intact"
            );
            assert_eq!(hub.protocol_dials("b", "a", Protocol::Renew), renewals);
            assert!(
                report["peers"]
                    .as_array()
                    .ok_or("peers")?
                    .iter()
                    .any(|peer| {
                        peer["attempts"].as_array().is_some_and(|attempts| {
                            attempts.iter().any(|attempt| {
                                attempt["operation"] == "reconcile"
                                    && attempt["stage"] == "admission"
                                    && attempt["outcome"] == "failed"
                            })
                        })
                    })
            );
            let peer = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let no_route = Contact { routes: Vec::new() };
            assert!(
                b.reconcile(&*transport(&b).await?, peer, &no_route)
                    .await
                    .is_err()
            );
            let last = b
                .observations
                .snapshot()
                .into_iter()
                .filter(|attempt| attempt.operation == crate::AttemptOperation::Reconcile)
                .max_by_key(|attempt| attempt.id)
                .ok_or("reconcile observation")?;
            assert_eq!(last.stage, crate::AttemptStage::ResolveRoute);
            assert_eq!(
                last.failure.ok_or("failure")?.code.as_deref(),
                Some("rejected")
            );
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
            let waiting = std::time::Instant::now();
            while *a.worker_running.borrow() || *b.worker_running.borrow() {
                assert!(
                    waiting.elapsed() < Duration::from_secs(5),
                    "workers did not settle: a={} b={} activities: a={} b={}",
                    *a.sync_status.borrow(),
                    *b.sync_status.borrow(),
                    a.activity.after(0),
                    b.activity.after(0)
                );
                tokio::task::yield_now().await;
            }
            // Real QUIC needs its ACK timers during reconciliation. Freeze
            // only the idle interval whose lack of traffic this test checks.
            tokio::time::pause();
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
#[tokio::test]
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
            // QUIC exposes a newly opened stream to the peer on its first write.
            tokio::io::AsyncWriteExt::write_all(&mut pair, &[0]).await?;
            until(|| sa.served.load(Ordering::Relaxed) > before).await;
            assert_eq!(sa.live_budget.available_permits(), 0);
            drop(pair);
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
            // Explicit sync tries the peer in its backoff and fails too;
            // nothing else carries the row.
            add(&a, 1).await?;
            let transport = transport(&c).await?;
            assert!(failed_report(&c.sync(&*transport).await?.0));
            assert!(rows(&c).await?.is_empty());
            // The renewal its failure prompts reached the peer, which clears
            // the backoff until the next busy refusal.
            until(|| backoff().is_some()).await;
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
    // Issuer authority targets the peer's identity, never its node.
    let identity = node
        .membership()
        .await?
        .identity_of(&peer)
        .ok_or("peer membership")?;
    let signer = PrivateKey::from_bytes(&node.vault.get("node")?);
    let settings = node.settings.lock().await;
    let links = settings.issuer_links();
    let credentials = settings.credentials.clone();
    let statement = rocky_credentials::SignedRevocation::sign(
        rocky_credentials::Revocation {
            app: node.domain().manifest().app,
            issuer: settings.identity,
            time: now()?,
            nodes: Default::default(),
            identities: [identity].into(),
            capabilities: Default::default(),
            cut: Default::default(),
            recovery: None,
        },
        rocky_credentials::Signer::node(&signer, &credentials)?,
        &links,
    )?;
    node.replica
        .learn_revocations(&RevocationSet::try_from(vec![statement])?)
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_in_budget_burst_spans_publication_pages_without_reconciliation() -> Result<(), Failure>
{
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let before = hub.counts();
            let activity = b.activity.after(0)["token"]
                .as_str()
                .ok_or("activity")?
                .parse()?;
            let service = a.current().ok_or("service")?;
            let publish = service.live.publishing.lock().await;
            for key in 1..=40 {
                add(&a, key).await?;
            }
            drop(publish);
            until(|| *b.replica.observe_revision().borrow() > 0).await;
            while rows(&b).await?.len() != 40 {
                tokio::task::yield_now().await;
            }
            settled(&a).await;
            settled(&b).await;
            assert_eq!(rows(&a).await?, rows(&b).await?);
            assert_eq!(hub.counts(), before, "publication reuses the stream");
            assert!(
                b.activity.after(activity)["events"]
                    .as_array()
                    .ok_or("events")?
                    .iter()
                    .all(|event| event["kind"] != "peer_reconciliation_completed"
                        && event["kind"] != "rows_synced")
            );
            let author = a.replica.frontier(a.app_scopes()[0]).await?.logs[0].author;
            let frontier = b.replica.frontier(b.app_scopes()[0]).await?;
            assert_eq!(frontier.log(author).ok_or("author log")?.applied, 40);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_local_commit_between_receipt_snapshots_remains_eligible_for_publication()
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
            let service_a = a.current().ok_or("service")?;
            let service_b = b.current().ok_or("service")?;
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            let gate = Arc::new(crate::node::live_links::ReceiptGate {
                entered: Semaphore::new(0),
                resume: Semaphore::new(0),
            });
            *service_b.live.receipt_gate.lock().unwrap() = Some(gate.clone());
            let dials = [hub.dials_to("a"), hub.dials_to("b")];
            let activity = a.activity.after(0)["token"]
                .as_str()
                .ok_or("activity")?
                .parse()?;
            add(&a, 1).await?;
            gate.entered.acquire().await?.forget();
            assert_eq!(rows(&b).await?, ["1"]);
            // Intake ownership prevents reverse publication until the receipt is processed.
            add(&b, 2).await?;
            gate.resume.add_permits(1);
            while rows(&a).await? != ["1", "2"] {
                tokio::task::yield_now().await;
            }
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            assert_eq!(rows(&b).await?, ["1", "2"]);
            assert_eq!([hub.dials_to("a"), hub.dials_to("b")], dials);
            assert!(
                a.activity.after(activity)["events"]
                    .as_array()
                    .ok_or("events")?
                    .iter()
                    .all(|event| event["kind"] != "peer_reconciliation_completed"
                        && event["kind"] != "rows_synced")
            );
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
            assert!(error.is::<crate::enrollment::Revoked>());
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
            assert!(failed_report(&b.sync(&*b_port).await?.0));
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

#[tokio::test(start_paused = true)]
async fn one_offline_peer_preserves_the_other_link_and_local_commits() -> Result<(), Failure> {
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
            let c = enrolled(
                &root.path().join("c"),
                &hub,
                "c",
                Some((
                    &a,
                    Profile::Writer {
                        scopes: ["items".into()].into(),
                    },
                )),
            )
            .await?;
            for (node, peers) in [
                (&a, vec![b.clone(), c.clone()]),
                (&b, vec![a.clone(), c.clone()]),
                (&c, vec![a.clone(), b.clone()]),
            ] {
                targets(node, &peers).await?;
                start(node, true).await?;
            }
            until(|| {
                [&a, &b, &c].iter().all(|node| {
                    node.current()
                        .is_some_and(|service| service.live.len() == 2)
                })
            })
            .await;
            for node in [&a, &b, &c] {
                settled(node).await;
            }
            let a_authority = a.replica.observe_authority().await;
            let b_authority = b.replica.observe_authority().await;
            let a_service = a.current().ok_or("service")?;
            let b_service = b.current().ok_or("service")?;
            let b_key = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let a_key = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let before = [
                hub.protocol_dials("a", "b", Protocol::Link),
                hub.protocol_dials("b", "a", Protocol::Link),
            ];
            c.dispatch(
                &*c.client(1)?,
                &json!({"type":"networking_control", "enabled":false}),
            )
            .await?;
            until(|| a_service.live.len() == 1 && b_service.live.len() == 1).await;
            let b_revision = *b.replica.observe_revision().borrow();
            let started = tokio::time::Instant::now();
            add(&a, 1).await?;
            assert_eq!(rows(&a).await?, ["1"]);
            assert_eq!(
                tokio::time::Instant::now(),
                started,
                "a local commit must not wait for an offline peer"
            );
            until(|| *b.replica.observe_revision().borrow() > b_revision).await;
            assert_eq!(rows(&b).await?, ["1"]);
            assert!(a_service.live.contains(b_key));
            assert!(b_service.live.contains(a_key));
            assert!(
                !a_authority.has_changed()?,
                "peer loss must not renew credentials"
            );
            assert!(
                !b_authority.has_changed()?,
                "peer loss must not renew credentials"
            );
            assert_eq!(
                [
                    hub.protocol_dials("a", "b", Protocol::Link),
                    hub.protocol_dials("b", "a", Protocol::Link)
                ],
                before,
                "the healthy link must not be redialed"
            );
            for node in [&a, &b, &c] {
                node.detach(None).await;
            }
            Ok(())
        }))
        .await
}

fn failed_report(report: &Value) -> bool {
    report["peers"].as_array().is_some_and(|peers| {
        peers.iter().any(|peer| {
            peer["skip"] == "backoff"
                || peer["attempts"].as_array().is_some_and(|attempts| {
                    attempts
                        .iter()
                        .any(|attempt| attempt["outcome"] == "failed")
                })
        })
    })
}

#[tokio::test(start_paused = true)]
async fn incoming_pushes_do_not_wait_for_page_publication() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = b.current().ok_or("service")?;
            let _publishing = service.live.publishing.lock().await;
            let revision = *b.replica.observe_revision().borrow();
            add(&a, 1).await?;
            until(|| *b.replica.observe_revision().borrow() > revision).await;
            assert_eq!(rows(&b).await?, ["1"]);
            drop(_publishing);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn publication_backpressure_preserves_committed_work_without_reopening_the_link()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service = b.current().ok_or("service")?;
            let gate = Arc::new(Semaphore::new(0));
            *service.live.push_gate.lock().unwrap() = Some(gate.clone());
            let before = [hub.dials_to("a"), hub.dials_to("b")];
            for index in 1..=24 {
                add(&a, index).await?;
                tokio::task::yield_now().await;
            }
            *service.live.push_gate.lock().unwrap() = None;
            gate.add_permits(1);
            b.sync(&*service.transport).await?;
            assert_eq!(rows(&b).await?.len(), 24);
            assert_eq!([hub.dials_to("a"), hub.dials_to("b")], before);
            assert_eq!(service.live.len(), 1);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn link_control_round_trip_runs_while_the_database_executor_is_blocked() -> Result<(), Failure>
{
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let (near, far) = opened_links(&a, &b).await?;
            let (answer, task) = crate::node::live_links::control_probe(near, far);
            // Block this LocalSet's OS thread. Framing and routing must finish on
            // the transport runtime without another database scheduler pass.
            let result = answer.recv_timeout(Duration::from_millis(500));
            task.abort();
            result.map_err(
                |_| "link heartbeat exceeded 500 ms while database executor was blocked",
            )??;
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        })
        .await
}

#[tokio::test(start_paused = true)]
async fn publication_with_32_held_scopes_notifies_a_complete_contact_transaction()
-> Result<(), Failure> {
    tokio::task::LocalSet::new().run_until(frozen(async {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let names: Vec<String> = ["contacts", "receipts", "versions"].into_iter().map(str::to_owned)
            .chain((3..32).map(|n| format!("held_{n}"))).collect();
        let mut schema: Value = serde_json::from_str(SCHEMA)?;
        let mut table = schema["tables"][0].clone();
        for n in 0..6 {
            table["columns"].as_array_mut().ok_or("columns")?.push(json!({"name":format!("extra_{n}"),"type":"text"}));
        }
        schema["tables"] = Value::Array(names.iter().map(|name| {
            let mut table = table.clone();
            table["name"] = json!(name);
            table
        }).collect());
        let schema = Schema::parse(&schema.to_string())?;
        let a = enrolled_schema(&root.path().join("a"), &hub, "a", None, &schema).await?;
        crate::escrow::keep(&a.replica, &*a.settings.lock().await, &a.vault, &a.domain()).await?;
        let b = enrolled_schema(&root.path().join("b"), &hub, "b", Some((&a, Profile::Reader {
            scopes: names.iter().cloned().collect(),
        })), &schema).await?;
        targets(&a, std::slice::from_ref(&b)).await?;
        targets(&b, std::slice::from_ref(&a)).await?;
        start(&a, true).await?;
        start(&b, true).await?;
        until(|| a.current().is_some_and(|s| s.live.len() == 1)
            && b.current().is_some_and(|s| s.live.len() == 1)).await;
        settled(&a).await;
        settled(&b).await;
        assert_eq!(b.scopes().len(), 32);
        let service_a = a.current().ok_or("service")?;
        let service_b = b.current().ok_or("service")?;
        until(|| !service_a.live.publishing_or_receiving() && !service_b.live.publishing_or_receiving()).await;
        let dials = [hub.dials_to("a"), hub.dials_to("b")];
        let activity = b.activity.after(0)["token"].as_str().ok_or("activity")?.parse()?;
        for version in 1..=3 {
            let traffic_before = traffic_snapshot(&service_a, &service_b);
            let mut changes = b.replica.observe_tables(Some(&["contacts", "receipts", "versions"])).await?;
            let bytes_before = *hub.written().borrow();
            let messages_before = [hub.link_messages("a"), hub.link_messages("b")];
            let client = a.client(1)?;
            a.dispatch(&client, &json!({"type":"sql","sql":"BEGIN"})).await.map_err(|error| format!("source begin: {error}"))?;
            for table in ["contacts", "receipts", "versions"] {
                let id = a.dispatch(&client, &json!({"type":"next_id","table":table})).await.map_err(|error| format!("source allocation in {table}: {error}"))?["id"].clone();
                a.dispatch(&client, &json!({"type":"sql", "sql":format!("INSERT INTO {table}(key,title,id) VALUES(?,'associated',?)"),
                    "values":[{"type":"text","value":version.to_string()},{"type":"integer","value":id}]})).await?;
            }
            let commit = async {
                a.dispatch(&client, &json!({"type":"sql","sql":"COMMIT","handle":vec![version;32]})).await?;
                Ok::<_, Failure>(std::time::Instant::now())
            };
            let observe = async {
                let change = changes.next().await?;
                assert_eq!(change.tables, ["contacts", "receipts", "versions"].into_iter().map(str::to_owned).collect());
                assert!(change.causes.contains(&rocky_storage::observe::Cause::Sync), "{change:?}");
                let notified = std::time::Instant::now();
                let snapshot = b.dispatch(&*b.client(1)?, &json!({"type":"read", "query":{
                    "sql":"SELECT (SELECT count(*) FROM contacts),(SELECT count(*) FROM receipts),(SELECT count(*) FROM versions)", "max_rows":1
                }})).await.map_err(|error| format!("notification snapshot: {error}"))?;
                Ok::<_, Failure>((notified, snapshot, std::time::Instant::now()))
            };
            let (committed, observed) = tokio::join!(commit, observe);
            let committed = committed?;
            let (notified, snapshot, read) = observed?;
            assert_eq!(snapshot["rows"][0], json!([
                {"type":"integer","value":version.to_string()},
                {"type":"integer","value":version.to_string()},
                {"type":"integer","value":version.to_string()}
            ]));
            until(|| !service_a.live.publishing_or_receiving() && !service_b.live.publishing_or_receiving()).await;
            settled(&a).await;
            settled(&b).await;
            let traffic = PublicationTraffic::between(&traffic_before, traffic_snapshot(&service_a, &service_b));
            traffic.assert_complete();
            eprintln!("publication native traffic: {traffic:?}");
            assert_eq!(
                [hub.link_messages("a") - messages_before[0], hub.link_messages("b") - messages_before[1]],
                [9, 2],
                "one fragmented publication and receipt"
            );
            let notification_us = if notified >= committed {
                notified.duration_since(committed).as_micros() as i128
            } else {
                -(committed.duration_since(notified).as_micros() as i128)
            };
            eprintln!("publication diagnostic: scopes=32 edit={version} commit_return_to_notification_us={notification_us} notification_to_snapshot_us={} framed_signed_link_bytes={} link_messages={:?}",
                read.duration_since(notified).as_micros(),
                *hub.written().borrow() - bytes_before,
                [hub.link_messages("a") - messages_before[0], hub.link_messages("b") - messages_before[1]]);
        }
        assert_eq!([hub.dials_to("a"), hub.dials_to("b")], dials);
        assert!(b.activity.after(activity)["events"].as_array().ok_or("events")?.iter().all(|event|
            event["kind"] != "peer_reconciliation_completed" && event["kind"] != "rows_synced"));
        a.detach(None).await;
        b.detach(None).await;
        Ok(())
    })).await
}

#[tokio::test(start_paused = true)]
async fn a_peer_holding_more_than_one_frontier_batch_publishes_only_the_changed_scope()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            // Partition selectors cover several concrete scopes without extra grants.
            let names: Vec<_> = std::iter::once("items".to_owned())
                .chain((1..62).map(|n| format!("held_{n}")))
                .collect();
            let mut schema: Value = serde_json::from_str(SCHEMA)?;
            let table = schema["tables"][0].clone();
            schema["tables"] = Value::Array(
                names
                    .iter()
                    .map(|name| {
                        let mut table = table.clone();
                        table["name"] = json!(name);
                        if name == "held_1" {
                            table["partition_by"] = json!(["key"]);
                            table["partition_group"] = json!("held");
                        }
                        table
                    })
                    .collect(),
            );
            let schema = Schema::parse(&schema.to_string())?;
            let a = enrolled_schema(&root.path().join("a"), &hub, "a", None, &schema).await.map_err(|error| format!("multi-batch source enrollment: {error}"))?;
            crate::escrow::keep(&a.replica, &*a.settings.lock().await, &a.vault, &a.domain())
                .await?;
            let b = enrolled_schema(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, Profile::Issuer)),
                &schema,
            )
            .await.map_err(|error| format!("multi-batch receiver enrollment: {error}"))?;
            let peer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let (client_a, client_b) = (a.client(1)?, b.client(1)?);
            for n in 0..3 {
                let values = json!([{"type":"text","value":format!("partition_{n}")}]);
                a.dispatch(&client_a, &json!({"type":"partition_create","group":"held","values":values})).await.map_err(|error| format!("create partition {n}: {error}"))?;
                let shared = a.dispatch(&client_a, &json!({"type":"partition_share","group":"held","values":values,"recipients":[peer.to_hex()]})).await.map_err(|error| format!("share partition {n}: {error}"))?;
                b.dispatch(&client_b, &json!({"type":"partition_install","group":"held","values":values,
                    "envelope":shared["envelopes"][0]["envelope"],"sender":shared["sender"]})).await.map_err(|error| format!("install partition {n}: {error}"))?;
            }
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
            assert_eq!(b.scopes().len(), 66);
            assert_eq!(b.scopes().iter().filter(|scope| !b.domain().partitioned_table(scope)).count(), 65);
            let service_a = a.current().ok_or("service")?;
            let service_b = b.current().ok_or("service")?;
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            let before = traffic_snapshot(&service_a, &service_b);
            let dials = [hub.dials_to("a"), hub.dials_to("b")];
            add(&a, 1).await?;
            while rows(&b).await? != ["1"] {
                tokio::task::yield_now().await;
            }
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            PublicationTraffic::between(&before, traffic_snapshot(&service_a, &service_b))
                .assert_complete();
            assert_eq!([hub.dials_to("a"), hub.dials_to("b")], dials);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

async fn count(node: &Node) -> Result<Value, Failure> {
    let page = node
        .dispatch(
            &*node.client(1)?,
            &json!({"type":"read", "query":{"sql":"SELECT count(*) FROM items", "max_rows":1}}),
        )
        .await?;
    Ok(page["rows"][0][0]["value"].clone())
}

#[tokio::test(start_paused = true)]
async fn an_over_budget_burst_is_repaired_once_and_later_edits_publish() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let (a, b) = pair(root.path(), &hub).await?;
            let service_a = a.current().ok_or("service")?;
            let service_b = b.current().ok_or("service")?;
            until(|| {
                !service_a.live.publishing_or_receiving()
                    && !service_b.live.publishing_or_receiving()
            })
            .await;
            // B never publishes back, so only A's own outcomes move its cursor.
            let quiet = service_b.live.publishing.lock().await;
            let idle = || !service_a.live.publishing_or_receiving();
            // One capture over the 256 KiB publication budget fails preflight.
            let title = "x".repeat(16 * 1024);
            let client = a.client(1)?;
            let publish = service_a.live.publishing.lock().await;
            for key in 0..24u8 {
                a.dispatch(&client, &json!({"type":"sql","sql":"BEGIN"}))
                    .await?;
                let id = a
                    .dispatch(&client, &json!({"type":"next_id","table":"items"}))
                    .await?["id"]
                    .clone();
                a.dispatch(&client, &json!({"type":"sql","sql":"INSERT INTO items(key,title,id) VALUES(?,?,?)",
                    "values":[{"type":"text","value":format!("burst_{key}")},{"type":"text","value":title},{"type":"integer","value":id}]})).await?;
                a.dispatch(
                    &client,
                    &json!({"type":"sql","sql":"COMMIT","handle":vec![100 + key; 32]}),
                )
                .await?;
            }
            drop(publish);
            while count(&b).await? != "24" {
                tokio::task::yield_now().await;
            }
            settled(&a).await;
            settled(&b).await;
            until(idle).await;
            let activity = b.activity.after(0)["token"]
                .as_str()
                .ok_or("activity")?
                .parse()?;
            // The repaired range is not exported again with the next edit.
            add(&a, 1).await?;
            while count(&b).await? != "25" {
                tokio::task::yield_now().await;
            }
            settled(&a).await;
            settled(&b).await;
            until(idle).await;
            let events = b.activity.after(activity)["events"].clone();
            assert!(
                events.as_array().ok_or("events")?.iter().all(|event| ![
                    "peer_reconciliation_completed",
                    "rows_synced",
                    "synced"
                ]
                .contains(&event["kind"].as_str().unwrap_or_default())),
                "{events}"
            );
            drop(quiet);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        }))
        .await
}

#[tokio::test(start_paused = true)]
async fn a_control_hint_matching_local_state_schedules_no_repair() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(frozen(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let work = |peer: PublicKey| {
                a.retries
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&peer)
                    .filter(|retry| retry.pending.pending())
                    .map(|retry| retry.pending.selected().cloned())
            };
            let scopes = a.app_scopes();
            let token = a.signal_token(&scopes).await?;
            let peer = PrivateKey::new().public_key();
            a.changed_signal(peer, &scopes, token, true).await?;
            assert_eq!(work(peer), None);
            a.changed_signal(peer, &scopes, [0; 32], false).await?;
            assert_eq!(work(peer), Some(Some(scopes.iter().copied().collect())));
            a.changed_signal(peer, &scopes, [0; 32], true).await?;
            assert_eq!(work(peer), Some(None));
            // A full control batch may have been truncated: it widens even when equal.
            let full: Vec<_> = (0..link::SIGNAL_SCOPES)
                .map(|index| {
                    let mut scope = scopes[0];
                    scope.table[0] = index as u8;
                    scope
                })
                .collect();
            let token = a.signal_token(&full).await?;
            let peer = PrivateKey::new().public_key();
            a.changed_signal(peer, &full, token, true).await?;
            assert_eq!(work(peer), Some(None));
            a.detach(None).await;
            Ok(())
        }))
        .await
}
