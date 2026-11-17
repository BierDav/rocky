//! Credentialed strangers against the inbound link budget.
use super::*;
use crate::test_snapshot as snap;
use rocky_credentials::Authority;
use rocky_secure::network::link::Link;

#[tokio::test]
async fn credentialed_strangers_over_the_link_budget_are_refused_and_a_configured_peer_syncs_after()
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
            let issuer_node = PrivateKey::from_bytes(&a.vault.get("node")?);
            let grants = crate::enrollment::grants_of(&writer, &a.domain())?;
            let issuer_links = a.settings.lock().await.issuer_links();
            let credentials = a.settings.lock().await.credentials.clone();
            let authority = Authority::issuer(&issuer_node, &credentials)?;
            let strangers = (0..64)
                .map(|_| {
                    let (user, node) = (PrivateKey::new(), PrivateKey::new());
                    let capability = authority.issue_partition_terms(
                        user.public_key(),
                        grants.clone(),
                        Vec::new(),
                        rocky_credentials::DelegationTerms { delegations: 1 },
                        rocky_credentials::Validity {
                            issued_at: now()?,
                            lifetime: std::time::Duration::from_secs(
                                crate::enrollment::DEFAULT_TTL,
                            ),
                            clock_skew: std::time::Duration::from_secs(300),
                        },
                    )?;
                    let mut chain = issuer_links.clone();
                    chain.0.push(capability);
                    Ok((node.clone(), chain.complete(&user, node.public_key())?))
                })
                .collect::<Result<Vec<_>, Failure>>()?;
            let config = a.settings.lock().await.config(&a.vault, &a.domain())?;
            let replica = EncryptedReplica::open(
                root.path().join("strangers.sqlite"),
                a.domain().app.clone(),
                config,
            )
            .await?;
            let dialer = hub.connector("strangers").start(false, [7; 32]).await?;
            let own = PrivateKey::from_bytes(&a.vault.get("node")?).public_key();
            let link = a.current().ok_or("link")?;
            let (served, refused) = (
                link.served.load(Ordering::Relaxed),
                link.refused.load(Ordering::Relaxed),
            );
            let mut links = Vec::new();
            for (node, chain) in strangers {
                let stream = dialer.connect("a", Protocol::Link, None).await?;
                let mut opened = Link::open(stream, &replica, node, chain, Some(own)).await?;
                links.push(opened.ready(false).await.map(|_| opened));
            }
            assert_eq!(links.iter().filter(|link| link.is_ok()).count(), 32);
            for refused in links.iter().skip(32) {
                let Err(rocky_secure::Error::Application(cause)) = refused else {
                    return Err("an over-budget link was not refused".into());
                };
                assert!(cause.is::<rocky_secure::network::link::Busy>());
            }
            assert_eq!(link.served.load(Ordering::Relaxed), served + 64);
            assert_eq!(link.refused.load(Ordering::Relaxed), refused + 32);
            assert_eq!(
                link.live_budget.available_permits(),
                0,
                "each admitted link holds its slot"
            );
            let peers = a.observed_peers().await;
            assert_eq!(
                peers.iter().filter(|peer| !peer.is_configured).count(),
                32,
                "`peers` lists the served strangers, met without a contact"
            );
            drop(links);
            while link.live_budget.available_permits() < live_links::BUDGET {
                tokio::task::yield_now().await;
            }
            let synced = snap::value(b.sync_once()).await?;
            assert_eq!(
                completed_peers(&synced),
                1,
                "a configured peer syncs afterwards"
            );
            assert!(link.served.load(Ordering::Relaxed) > served + 64);
            assert_eq!(link.refused.load(Ordering::Relaxed), refused + 32);
            dialer.close().await;
            b.detach(None).await;
            a.detach(None).await;
            Ok(())
        })
        .await
}
