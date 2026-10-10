//! Credentialed strangers against the inbound limit
//! (`docs/decisions/0051-roster-free-admission.md`).
use super::*;
use rocky_credentials::{Authority, device_chain};
use rocky_secure::{MAX_SECURE_BYTES, network::NativeSession};

#[tokio::test]
async fn credentialed_strangers_over_the_inbound_limit_are_refused_and_a_configured_peer_syncs_after()
-> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
            let writer = Profile::Writer {
                scopes: ["items".into()].into(),
            };
            let b = enrolled(
                &root.path().join("b"),
                &hub,
                "b",
                Some((&a, writer.clone())),
            )
            .await?;
            // 64 devices of the domain, none of them a contact of A.
            let publisher = PrivateKey::from_bytes(&a.vault.get("publisher")?);
            let grants = crate::enrollment::grants_of(&writer, &a.domain)?;
            let authority = Authority {
                publisher: &publisher,
                manifest: a.domain.manifest().hash(),
            };
            let strangers = (0..64)
                .map(|_| {
                    let (user, node) = (PrivateKey::new(), PrivateKey::new());
                    let capability = authority.issue_partition_terms(
                        user.public_key(),
                        grants.clone(),
                        Vec::new(),
                        (1, None),
                        (now()?, crate::enrollment::DEFAULT_TTL, 300),
                    )?;
                    Ok((
                        node.clone(),
                        device_chain(capability, &user, node.public_key())?,
                    ))
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            // The strangers share one replica: it only checks A's chain.
            let config = a.settings.lock().await.config(&a.vault, &a.domain)?;
            let replica = EncryptedReplica::open(
                root.path().join("strangers.sqlite"),
                a.domain.app.clone(),
                config,
            )
            .await?;
            let dialer = hub.connector("strangers").start(false, [7; 32]).await?;
            let own = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let link = a.current().ok_or("link")?;
            // B's enrollment was served already.
            let (served, refused) = (
                link.served.load(Ordering::Relaxed),
                link.refused.load(Ordering::Relaxed),
            );
            // All 64 are open at once; each served one holds its slot. The hub
            // queues few dials, so each waits until A took the one before.
            let mut attempts = Vec::new();
            for (index, (node, chain)) in strangers.into_iter().enumerate() {
                let stream = dialer.connect("a", Protocol::Sync).await?;
                while link.served.load(Ordering::Relaxed) + link.refused.load(Ordering::Relaxed)
                    <= served + refused + index as u64
                {
                    tokio::task::yield_now().await;
                }
                let replica = replica.clone();
                attempts.push(tokio::task::spawn_local(async move {
                    NativeSession::authenticate(
                        stream,
                        &replica,
                        node,
                        chain,
                        own,
                        MAX_SECURE_BYTES,
                    )
                    .await
                }));
            }
            let mut sessions = Vec::new();
            for attempt in attempts {
                sessions.push(attempt.await?);
            }
            assert_eq!(
                sessions.iter().filter(|session| session.is_ok()).count(),
                32
            );
            assert_eq!(link.served.load(Ordering::Relaxed), served + 32);
            assert_eq!(link.refused.load(Ordering::Relaxed), refused + 32);
            assert_eq!(link.active.available_permits(), 0);
            // `peers` lists the served strangers, met without a contact.
            let peers = a.peers().await;
            assert_eq!(
                peers
                    .iter()
                    .filter(|peer| peer["configured"] == false)
                    .count(),
                32
            );
            drop(sessions);
            while link.active.available_permits() < INBOUND {
                tokio::task::yield_now().await;
            }
            // A configured peer syncs afterwards.
            let synced = b.dispatch(&*b.client(1)?, &json!({"type":"sync"})).await?;
            assert_eq!(synced["state"], "connected");
            assert_eq!(synced["peers"], 1);
            assert!(link.served.load(Ordering::Relaxed) > served + 32);
            assert_eq!(link.refused.load(Ordering::Relaxed), refused + 32);
            dialer.close().await;
            b.detach(None).await;
            a.detach(None).await;
            Ok(())
        })
        .await
}
