//! Merge-only application with two tables and state-aware admission.
#[path = "support/ledger_app.rs"]
mod ledger_app;
use ledger_app::{LedgerApp, payload};
use rocky_core::{AppId, DomainId, MeshId, NodeId, PrincipalId, Scope};
use rocky_runtime::{Identity, Mutation, Replica};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let scope = Scope {
        mesh: MeshId([9; 16]),
        app: AppId([8; 16]),
        domain: DomainId([7; 16]),
    };
    let caller = PrincipalId([6; 16]);
    let replica = Replica::open(
        directory.path().join("ledger.db"),
        LedgerApp,
        (scope, caller),
        Identity {
            scope,
            caller,
            node: NodeId([5; 16]),
        },
    )
    .await?;
    let receipt = replica.mutate(Mutation {
        entity: None,
        payload: payload(0, 10, caller),
    })?;
    let handle = receipt.handle();
    let created = receipt.committed().await?;
    replica
        .mutate(Mutation {
            entity: Some(created.id),
            payload: payload(1, -3, caller),
        })?
        .committed()
        .await?;
    assert!(replica.lookup_mutation(handle).await?.is_some());
    assert_eq!(replica.query().await?.rows[0].1, 7);
    replica.close().await?;
    Ok(())
}
