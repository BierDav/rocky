//! Credentialed strangers against the inbound link budget
//! (`docs/decisions/0051-roster-free-admission.md`).
use super::*;
use rocky_credentials::{Authority, device_chain};
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
            // One after another, each kept open: an admitted link holds its
            // slot, so the 33rd and later are refused as busy.
            let mut links = Vec::new();
            for (node, chain) in strangers {
                let stream = dialer.connect("a", Protocol::Link).await?;
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
            assert_eq!(link.live_budget.available_permits(), 0);
            // `peers` lists the served strangers, met without a contact.
            let peers = a.peers().await;
            assert_eq!(
                peers
                    .iter()
                    .filter(|peer| peer["configured"] == false)
                    .count(),
                32
            );
            drop(links);
            while link.live_budget.available_permits() < live_links::BUDGET {
                tokio::task::yield_now().await;
            }
            // A configured peer syncs afterwards.
            let synced = b.dispatch(&*b.client(1)?, &json!({"type":"sync"})).await?;
            assert_eq!(synced["state"], "connected");
            assert_eq!(synced["peers"], 1);
            assert!(link.served.load(Ordering::Relaxed) > served + 64);
            assert_eq!(link.refused.load(Ordering::Relaxed), refused + 32);
            dialer.close().await;
            b.detach(None).await;
            a.detach(None).await;
            Ok(())
        })
        .await
}
