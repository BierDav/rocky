use super::*;
use rocky_coordinator::coordinated::MemoryLog;

#[tokio::test]
async fn late_coordinator_attachment_binds_independent_database_clients() -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let directory = tempfile::tempdir()?;
            let hub = Arc::new(Hub::default());
            let a = enrolled(&directory.path().join("a"), &hub, "a", None).await?;
            let b = enrolled(&directory.path().join("b"), &hub, "b", None).await?;
            let logs = MemoryLog::new();
            let client = Arc::new(logs.client(PrivateKey::new().public_key()));
            for node in [&a, &b] {
                node.coordinate(Some(client.clone())).await?;
                let bound = node.coordinator().ok_or("missing coordinator")?;
                assert_eq!(
                    bound.node(),
                    *Settings::node_key(&node.vault)?.public_key().as_bytes()
                );
                assert_ne!(bound.node(), rocky_core::DomainLog::node(&*client));
                assert_eq!(
                    node.replica
                        .domain_log()
                        .ok_or("missing domain log")?
                        .node(),
                    bound.node()
                );
            }
            assert_ne!(
                a.coordinator().ok_or("a")?.node(),
                b.coordinator().ok_or("b")?.node()
            );
            a.coordinate(None).await?;
            assert!(a.replica.domain_log().is_none());
            assert_eq!(
                b.coordinator().ok_or("b")?.node(),
                *Settings::node_key(&b.vault)?.public_key().as_bytes()
            );
            a.detach(None).await;
            b.detach(None).await;
            Ok(())
        })
        .await
}
