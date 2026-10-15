//! Discovery on the hub's deterministic capability; see decision 0057.
use super::*;

const WAIT: Duration = Duration::from_secs(5);

fn writer() -> Profile {
    Profile::Writer {
        scopes: ["items".into()].into(),
    }
}
async fn pass(node: &Node) -> Result<Value, Failure> {
    let link = node.network.read().await;
    Ok(node.sync(&*link.as_ref().ok_or("link")?.transport).await?.0)
}
fn key(node: &Node) -> Result<PublicKey, Failure> {
    Ok(PrivateKey::from_bytes(&node.vault.get("node")?).public_key())
}
/// Lets the discovery workers run until `done` holds.
async fn until(mut done: impl AsyncFnMut() -> bool) -> Result<(), Failure> {
    tokio::time::timeout(WAIT, async {
        while !done().await {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await?;
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
    let page = node
        .dispatch(
            &*node.client(1)?,
            &json!({"type":"read","query":{"sql":"SELECT key FROM items ORDER BY key","max_rows":8}}),
        )
        .await?;
    Ok(page["rows"].clone())
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
            // Before its first admitted session a device holds no root: it
            // derives no topic and announces nothing.
            announce(&b).await?;
            assert!(hub.topics(b_route).is_empty());
            assert_eq!(b.discovery_status()["announcing"], false);
            // C meets the issuer before the row exists; only B receives it.
            pass(&c).await?;
            add(&a, 1).await?;
            pass(&b).await?;
            until(async || hub.discoverable(b_route) == ["a", c_route]).await?;
            assert!(titles(&c).await?.as_array().is_some_and(Vec::is_empty));
            // The trusted node leaves; the members know no route to each other
            // but what discovery reports.
            a.detach(None).await;
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
                .peers()
                .await
                .into_iter()
                .find(|peer| peer["node"] == c_key.to_hex())
                .ok_or("found peer")?;
            assert_eq!(
                (&found["source"], &found["local"], &found["choice"]),
                (&json!("discovery"), &json!(true), &json!("selected_local"))
            );
            let status = b.readiness().await?["network"]["discovery"].clone();
            assert_eq!(status["announcing"], true);
            assert_eq!(status["local"], json!({"state": "on", "reason": null}));
            assert_eq!(status["topic"]["state"], "unavailable");
            assert!(status["topic"]["reason"].is_string());
            // A found route stays in the identity file; no replica table holds it.
            assert!(std::fs::read_to_string(&b.path)?.contains(c_route));
            let db = rusqlite::Connection::open(root.path().join("b/rocky.sqlite"))?;
            let tables: Vec<String> = db
                .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
                .query_map([], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            assert!(!tables.is_empty());
            for table in tables {
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
            // Detaching withdraws the announcement.
            b.detach(None).await;
            assert!(hub.topics(b_route).is_empty());
            assert_eq!(
                b.discovery_status()["paths"]["local"],
                json!({"state": "off"})
            );
            c.detach(None).await;
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
            // Another domain on the same network, announcing under its own root.
            let outsider = enrolled(&root.path().join("o"), &hub, "o", None).await?;
            pass(&b).await?;
            pass(&c).await?;
            until(async || hub.discoverable("c") == ["a", "b"]).await?;
            assert!(hub.discoverable("o").is_empty());
            assert!(!hub.topics("o").is_empty());
            let old = hub.topics("c")[0];
            let c_key = key(&c)?;
            until(async || b.settings.lock().await.peers.contains_key(&c_key)).await?;
            let c_contact = b.settings.lock().await.peers[&c_key].clone();

            // The revoked device accepts no session, so nobody tells it; the
            // others come to hold the statement and its discovery value.
            let c_port = c.current().ok_or("link")?.transport.clone();
            c_port.listen(false);
            a.revoke(crate::enrollment::Revoke::Node(key(&c)?)).await?;
            pass(&b).await?;
            until(async || hub.topics("a").len() == 2 && hub.topics("b").len() == 2).await?;
            let current = hub.topics("a")[0];
            assert_eq!(hub.topics("b"), [current, old]);
            assert_ne!(current, old);
            // It derives no current topic, and is answered on the previous
            // one only while the window lasts.
            announce(&c).await?;
            assert_eq!(hub.topics("c"), [old]);
            assert_eq!(hub.discoverable("c"), ["a", "b"]);
            // Being found is not being admitted.
            assert!(!b.settings.lock().await.peers.contains_key(&key(&c)?));

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
            // Once a peer reaches it, it is told and announces nothing more.
            for node in [&a, &b, &c] {
                node.discovery_skew.store(0, Ordering::SeqCst);
            }
            c_port.listen(true);
            announce(&b).await?;
            announce(&c).await?;
            until(async || b.settings.lock().await.revocations.revokes(&c_key)).await?;
            let b_port = b.current().ok_or("link")?.transport.clone();
            assert!(b.reconcile(&*b_port, c_key, &c_contact).await.is_err());
            let _ = pass(&c).await;
            until(async || hub.topics("c").is_empty()).await?;
            for node in [&a, &b, &c, &outsider] {
                node.detach(None).await;
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
            assert_eq!(status["announcing"], false);
            for path in ["local", "topic"] {
                assert_eq!(status["paths"][path]["state"], "unavailable");
                assert_eq!(
                    status["paths"][path]["reason"],
                    "the memory transport has no discovery"
                );
            }
            assert!(hub.topics("b").is_empty());
            // Contacts still carry the sync.
            assert_eq!(completed_peers(&pass(&b).await?), 1);
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        })
        .await
}
