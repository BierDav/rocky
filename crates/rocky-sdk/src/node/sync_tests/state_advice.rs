use super::*;

#[tokio::test]
async fn ordinary_sync_waits_for_the_shared_import_slot_without_a_bootstrap_marker()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            a.tips(&json!({"action":"create", "name":"main"})).await?;
            a.tips(&json!({"action":"follow", "name":"main"})).await?;
            for row in 1..=4 {
                add(&a, row).await?;
            }
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
            c.replica
                .receive_tip_statements(&a.replica.tip_statements(None, 32).await?)
                .await?;
            c.replica.follow_bookmark(a.domain().scope, "main").await?;
            assert!(!c.path.with_file_name("bootstrap.json").exists());
            let key = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let contact = c
                .settings
                .lock()
                .await
                .targets()
                .into_iter()
                .find(|(peer, _)| *peer == key)
                .ok_or("contact")?
                .1;
            let transport = change_driven::transport(&c).await?;
            let importing = c.bootstrap.lock().await;
            let mut exchange = Box::pin(c.reconcile(&*transport, key, &contact));
            assert!(
                tokio::time::timeout(Duration::from_secs(1), exchange.as_mut())
                    .await
                    .is_err(),
                "ordinary sync bypassed an active import"
            );
            drop(importing);
            exchange.await?;
            let scope = a.app_scopes()[0];
            let rows = c
                .replica
                .session([scope].into())
                .await?
                .sql(Default::default())
                .await?
                .run("SELECT count(*) FROM items", vec![])
                .await?;
            assert_eq!(rows.rows, vec![vec![SqlValue::Integer(4)]]);
            for node in [&a, &c] {
                node.detach(None).await;
            }
            Ok(())
        })
        .await
}

#[tokio::test]
async fn three_failed_advised_imports_fall_back_only_for_that_scope_and_checkpoint()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            a.tips(&json!({"action":"create", "name":"main"})).await?;
            a.tips(&json!({"action":"follow", "name":"main"})).await?;
            for handle in 1..=64 {
                add(&a, handle).await?;
            }
            a.tips(&json!({"action":"seal"})).await?;
            a.tips(&json!({"action":"compact"})).await?;
            let profile = Profile::Writer {
                scopes: ["items".into()].into(),
            };
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, profile.clone())),
            )
            .await?;
            let c = enrolled(&root.path().join("c"), &hub, "c", Some((&a, profile))).await?;
            let scope = a.app_scopes()[0];
            for peer in [&b, &c] {
                peer.replica
                    .receive_tip_statements(&a.replica.tip_statements(None, 32).await?)
                    .await?;
                peer.replica
                    .follow_bookmark(a.domain().scope, "main")
                    .await?;
            }
            drop(link_sessions(&b, &a).await?);
            let signer = PrivateKey::from_bytes(&b.vault.get("node")?).public_key();
            let signed = a
                .replica
                .checkpoint_offer(signer, &b.settings.lock().await.credentials, scope)
                .await?
                .ok_or("base")?;
            let checkpoint = signed.id()?;
            b.replica.begin_rebootstrap(signed.clone()).await?;
            let page = a
                .replica
                .checkpoint_page(
                    signer,
                    &b.settings.lock().await.credentials,
                    scope,
                    checkpoint,
                    None,
                    64 * 1024,
                )
                .await?;
            b.replica.stage_checkpoint_page(page).await?;
            b.state_failures
                .lock()
                .unwrap()
                .insert((scope, checkpoint), 3);
            // Other checkpoint and scope failures do not suppress this offer.
            c.state_failures.lock().unwrap().insert((scope, [0; 32]), 3);
            c.state_failures
                .lock()
                .unwrap()
                .insert((a.domain().registry(), checkpoint), 3);
            for (peer, imported) in [(&b, false), (&c, true)] {
                let transport = peer.current().ok_or("service")?.transport.clone();
                let (report, _) = peer.sync(&*transport).await?;
                let transfers: Vec<_> = report["peers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|peer| peer["transfers"].as_array().into_iter().flatten())
                    .collect();
                assert_eq!(
                    transfers
                        .iter()
                        .any(|moved| moved["checkpoint"] == HEXLOWER.encode(&checkpoint)),
                    imported,
                    "{report}"
                );
                let rows = peer
                    .replica
                    .session([scope].into())
                    .await?
                    .sql(Default::default())
                    .await?
                    .run("SELECT count(*) FROM items", vec![])
                    .await?;
                assert_eq!(rows.rows, vec![vec![SqlValue::Integer(64)]]);
                if !imported {
                    // An import already progressing with another signer is kept.
                    b.replica.compact(a.domain().scope, 0).await?;
                    let key = PrivateKey::from_bytes(&c.vault.get("node")?).public_key();
                    let credentials = c.settings.lock().await.credentials.clone();
                    drop(link_sessions(&c, &b).await?);
                    let staged = b
                        .replica
                        .checkpoint_offer(key, &credentials, scope)
                        .await?
                        .ok_or("second signer's base")?;
                    let staged_id = staged.id()?;
                    c.replica.begin_rebootstrap(staged).await?;
                    let page = b
                        .replica
                        .checkpoint_page(key, &credentials, scope, staged_id, None, 16 * 1024)
                        .await?;
                    c.replica.stage_checkpoint_page(page).await?;
                    let before = c.replica.rebootstrap_progress(scope).await?;
                    assert!(
                        before
                            .as_ref()
                            .is_some_and(|(_, cursor, _)| cursor.is_some())
                    );
                    let service = c.current().ok_or("service")?;
                    let owner = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
                    let contact = c
                        .settings
                        .lock()
                        .await
                        .targets()
                        .into_iter()
                        .find(|(key, _)| *key == owner)
                        .ok_or("owner contact")?
                        .1;
                    let keep = |_, _, _| true;
                    let upstream = Upstream {
                        link: c.request_link(&*service.transport, owner, &contact).await?,
                        observations: &c.observations,
                        peer: owner,
                        contacts: None,
                        renew_from: None,
                        state_failures: &c.state_failures,
                        keep: Some(&keep),
                    };
                    sync::reconcile(&c.replica, &upstream, &[scope], |_| {}, None).await?;
                    assert_eq!(c.replica.rebootstrap_progress(scope).await?, before);
                    c.replica.cancel_rebootstrap(scope, staged_id).await?;
                }
            }
            for node in [&a, &b, &c] {
                node.detach(None).await;
            }
            Ok(())
        })
        .await
}

#[tokio::test]
async fn a_failed_scope_does_not_skip_the_next_scope_on_the_same_link() -> Result<(), Failure> {
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
            add(&a, 1).await?;
            let peer = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let contact = b
                .settings
                .lock()
                .await
                .targets()
                .into_iter()
                .find(|(node, _)| *node == peer)
                .ok_or("owner contact")?
                .1;
            let service = b.current().ok_or("service")?;
            let upstream = Upstream {
                link: b.request_link(&*service.transport, peer, &contact).await?,
                observations: &b.observations,
                peer,
                contacts: None,
                renew_from: None,
                state_failures: &b.state_failures,
                keep: None,
            };
            let good = a.app_scopes()[0];
            let mut bad = good;
            bad.table = [255; 32];
            let (_, transfer) =
                sync::reconcile(&b.replica, &upstream, &[bad, good], |_| {}, None).await?;
            assert!(transfer.failed.contains(&bad) || transfer.refused.contains(&bad));
            assert!(!transfer.failed.contains(&good));
            assert!(
                transfer
                    .scopes
                    .iter()
                    .any(|(scope, moved)| *scope == good && moved.pulled == 1)
            );
            assert!(!upstream.link.is_closed());
            for node in [&a, &b] {
                node.detach(None).await;
            }
            Ok(())
        })
        .await
}
