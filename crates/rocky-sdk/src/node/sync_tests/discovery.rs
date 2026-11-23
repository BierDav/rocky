use super::*;

const OUTLASTS_A_LOADED_CI_HOST: Duration = Duration::from_secs(30);

fn writer() -> crate::Access {
    crate::Access::Full
}
async fn pass(node: &Node) -> Result<Value, Failure> {
    let link = node.network.read().await;
    Ok(node.sync(&*link.as_ref().ok_or("link")?.transport).await?.0)
}
fn key(node: &Node) -> Result<PublicKey, Failure> {
    Ok(PrivateKey::from_bytes(&node.vault.get("node")?).public_key())
}
async fn until(mut done: impl AsyncFnMut() -> bool) -> Result<(), Failure> {
    tokio::time::timeout(OUTLASTS_A_LOADED_CI_HOST, async {
        while !done().await {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .map_err(|_| "the node never reached the awaited state")?;
    Ok(())
}
async fn announced(hub: &Hub, ready: impl Fn(&Hub) -> bool) -> Result<(), Failure> {
    tokio::time::timeout(OUTLASTS_A_LOADED_CI_HOST, hub.wait_announced(ready))
        .await
        .map_err(|_| "the hub never announced the awaited state")?;
    Ok(())
}
async fn announce(node: &Node) -> Result<(), Failure> {
    let link = node.network.read().await;
    let discovery = link
        .as_ref()
        .and_then(|link| link.discovery.clone())
        .ok_or("discovery")?;
    node.announce(&*discovery).await;
    Ok(())
}
async fn titles(node: &Node) -> Result<Value, Failure> {
    let rows = read_rows(node, "SELECT key FROM items ORDER BY key").await?;
    Ok(json!(
        rows.into_iter()
            .map(|row| row
                .into_iter()
                .map(|value| json!({"type":"text","value":value.to_text().unwrap_or_default()}))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    ))
}

#[tokio::test]
async fn members_converge_with_discovery_as_their_only_source_of_contacts() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            hub.discovery.store(true, Ordering::SeqCst);
            let (b_route, c_route) = ("route-b-found-by-discovery", "route-c-found-by-discovery");
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(&root.path().join("b"), &hub, b_route, Some((&a, writer()))).await?;
            let c = enrolled(&root.path().join("c"), &hub, c_route, Some((&a, writer()))).await?;
            let (a_key, b_key, c_key) = (key(&a)?, key(&b)?, key(&c)?);
            announce(&b).await?;
            assert!(
                hub.topics(b_route).is_empty(),
                "before its first admitted session a device holds no root and derives no topic"
            );
            assert!(!b.discovery_status().announcing);
            pass(&c).await?;
            add(&a, 1).await?;
            pass(&b).await?;
            announced(&hub, |hub| hub.discoverable(b_route) == ["a", c_route]).await?;
            assert!(
                titles(&c).await?.as_array().is_some_and(Vec::is_empty),
                "C met the issuer before the row existed"
            );
            a.detach().await;
            for node in [&b, &c] {
                node.configure(|settings| {
                    settings.remove_peer(a_key)?;
                    settings.contact_meta.remove(&a_key);
                    Ok(())
                })
                .await?;
            }
            until(async || {
                let settings = b.settings.lock().await;
                settings
                    .contact_meta
                    .get(&c_key)
                    .is_some_and(|meta| meta.local && meta.source == "discovery")
            })
            .await?;
            until(async || c.settings.lock().await.peers.contains_key(&b_key)).await?;
            let dials = hub.dials_to("a");
            assert_eq!(completed_peers(&pass(&c).await?), 1);
            assert_eq!(completed_peers(&pass(&b).await?), 1);
            assert_eq!(titles(&b).await?, titles(&c).await?);
            assert_eq!(titles(&c).await?.as_array().map(Vec::len), Some(1));
            assert_eq!(hub.dials_to("a"), dials);
            let found = b
                .observed_peers()
                .await
                .into_iter()
                .find(|peer| peer.node == c_key.to_hex())
                .ok_or("found peer")?;
            assert_eq!(
                (found.source, found.is_local, found.choice),
                (
                    crate::PeerSource::Discovery,
                    true,
                    crate::PeerChoice::SelectedLocal
                )
            );
            let status = serde_json::to_value(b.observed_status().await?.network.discovery)?;
            assert_eq!(status["announcing"], true);
            assert_eq!(status["local"], json!({"state": "on", "reason": null}));
            assert_eq!(status["topic"]["state"], "unavailable");
            assert!(status["topic"]["reason"].is_string());
            assert!(
                rocky_secure::read_authority_entry(
                    &b.path,
                    crate::enrollment::AUTHORITY_IDENTITY_KEY,
                )?
                .is_some_and(|(bytes, _)| String::from_utf8_lossy(&bytes).contains(c_route)),
                "a found route stays in the identity file"
            );
            let db = rusqlite::Connection::open(root.path().join("b/rocky.sqlite"))?;
            let tables: Vec<String> = db
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            assert!(!tables.is_empty());
            for table in tables {
                if table == "secure_authority_state" {
                    continue;
                }
                let mut statement =
                    db.prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))?;
                let columns = statement.column_count();
                let mut rows = statement.query([])?;
                while let Some(row) = rows.next()? {
                    for index in 0..columns {
                        let bytes = match row.get_ref(index)? {
                            rusqlite::types::ValueRef::Text(bytes)
                            | rusqlite::types::ValueRef::Blob(bytes) => bytes,
                            _ => continue,
                        };
                        for needle in [b_route.as_bytes(), c_route.as_bytes()] {
                            assert!(!bytes.windows(needle.len()).any(|part| part == needle));
                            let hex = HEXLOWER.encode(needle);
                            assert!(!bytes.windows(hex.len()).any(|part| part == hex.as_bytes()));
                        }
                    }
                }
            }
            b.detach().await;
            assert!(
                hub.topics(b_route).is_empty(),
                "detaching withdraws the announcement"
            );
            assert_eq!(
                b.discovery_status().local.state,
                crate::DiscoveryPathState::Off
            );
            c.detach().await;
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_revoked_device_and_an_outsider_find_no_peer_after_the_window() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            hub.discovery.store(true, Ordering::SeqCst);
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(&root.path().join("b"), &hub, "b", Some((&a, writer()))).await?;
            let c = enrolled(&root.path().join("c"), &hub, "c", Some((&a, writer()))).await?;
            let outsider_domain = enrolled(&root.path().join("o"), &hub, "o", None).await?;
            pass(&b).await?;
            pass(&c).await?;
            announced(&hub, |hub| hub.discoverable("c") == ["a", "b"]).await?;
            assert!(hub.discoverable("o").is_empty());
            assert!(!hub.topics("o").is_empty());
            let old = hub.topics("c")[0];
            let c_key = key(&c)?;
            until(async || b.settings.lock().await.peers.contains_key(&c_key)).await?;
            let c_contact = b.settings.lock().await.peers[&c_key].clone();

            let c_port = c.current().ok_or("link")?.transport.clone();
            c_port.listen(None, false).expect("listen");
            let c_identity = c.settings.lock().await.identity;
            a.revoke(crate::enrollment::Revoke::Identity(c_identity))
                .await?;
            pass(&b).await?;
            announced(&hub, |hub| {
                hub.topics("a").len() == 2 && hub.topics("b").len() == 2
            })
            .await?;
            let current = hub.topics("a")[0];
            assert_eq!(hub.topics("b"), [current, old]);
            assert_ne!(current, old);
            announce(&c).await?;
            assert_eq!(
                hub.topics("c"),
                [old],
                "a revoked device derives no current topic"
            );
            assert_eq!(hub.discoverable("c"), ["a", "b"]);
            assert!(
                !b.settings.lock().await.peers.contains_key(&key(&c)?),
                "being found is not being admitted"
            );

            for node in [&a, &b, &c] {
                node.discovery_skew.store(86_400, Ordering::SeqCst);
                announce(node).await?;
            }
            assert_eq!(hub.topics("a"), [current]);
            assert_eq!(hub.topics("b"), [current]);
            assert_eq!(hub.topics("c"), [old]);
            assert!(hub.discoverable("c").is_empty());
            assert_eq!(hub.discoverable("a"), ["b"]);
            assert!(hub.discoverable("o").is_empty());
            for node in [&a, &b, &c] {
                node.discovery_skew.store(0, Ordering::SeqCst);
            }
            c_port.listen(None, true).expect("listen");
            announce(&b).await?;
            announce(&c).await?;
            until(async || b.settings.lock().await.revocations.revokes(&c_key)).await?;
            let b_port = b.current().ok_or("link")?.transport.clone();
            assert!(b.reconcile(&*b_port, c_key, &c_contact).await.is_err());
            let _ = pass(&c).await;
            announced(&hub, |hub| hub.topics("c").is_empty()).await?;
            for node in [&a, &b, &c, &outsider_domain] {
                node.detach().await;
            }
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_transport_without_the_capability_reports_each_path_unavailable() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(&root.path().join("b"), &hub, "b", Some((&a, writer()))).await?;
            pass(&b).await?;
            let status = b.discovery_status();
            assert!(!status.announcing);
            for path in [status.local, status.topic] {
                assert_eq!(path.state, crate::DiscoveryPathState::Unavailable);
                assert_eq!(
                    path.reason.as_deref(),
                    Some("the memory transport has no discovery")
                );
            }
            assert!(hub.topics("b").is_empty());
            assert_eq!(
                completed_peers(&pass(&b).await?),
                1,
                "contacts still carry the sync"
            );
            a.detach().await;
            b.detach().await;
            Ok(())
        })
        .await
}
