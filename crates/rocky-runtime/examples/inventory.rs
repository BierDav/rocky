//! Composition and UI-author use of the typed inventory SDK.
#[path = "support/inventory_client.rs"]
mod inventory_client;
use inventory_app::{InventoryApp, Policy};
use inventory_client::InventoryClient;
use rocky_core::{DomainId, MeshId, NodeId, PrincipalId, Scope};
use rocky_runtime::{DomainInstallation, Identity, MutationHandle, Runtime, RuntimeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let caller = PrincipalId([7; 16]);
    let node = NodeId([3; 16]);
    let scopes: Vec<_> = [1, 2]
        .into_iter()
        .map(|domain| Scope {
            mesh: MeshId([11; 16]),
            app: inventory_app::app_id(),
            domain: DomainId([domain; 16]),
        })
        .collect();
    let runtime = Runtime::open(
        directory.path().join("inventory.db"),
        scopes
            .iter()
            .map(|scope| DomainInstallation {
                app: InventoryApp,
                policy: Policy {
                    scope: *scope,
                    caller,
                    reference: [9; 32],
                    note_prefix: None,
                },
                identity: Identity {
                    scope: *scope,
                    node,
                    caller,
                },
                config: RuntimeConfig {
                    authors: vec![(node, caller)],
                    delivery_targets: Vec::new(),
                    full_nodes: Vec::new(),
                },
            })
            .collect(),
    )
    .await?;
    let client = InventoryClient::new(runtime.domain(scopes[0])?, scopes[0]);
    let draft = client
        .note(MutationHandle([1; 32]), None, "local draft")?
        .committed()
        .await?;
    client
        .note(MutationHandle([2; 32]), None, "second draft")?
        .committed()
        .await?;
    client
        .note(
            MutationHandle([3; 32]),
            Some(draft.scoped_id()),
            "edited draft",
        )?
        .committed()
        .await?;
    let notes = client.notes().await?;
    assert_eq!(notes.notes, ["edited draft", "second draft"]);
    println!(
        "notes={:?} revision={} completeness={:?}",
        notes.notes,
        notes.revision.get(),
        notes.completeness
    );
    runtime.close().await?;
    Ok(())
}
