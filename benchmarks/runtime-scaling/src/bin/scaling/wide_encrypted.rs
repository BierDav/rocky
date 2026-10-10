//! Entity-count sweep through signed manifests, grant verification and encrypted scopes.
use super::{Failure, line, resident_mib};
use employees_app::Employees;
use rocky_core::*;
use rocky_secure::*;
use std::{collections::BTreeSet, sync::Arc, time::Instant};

struct WideEmployees {
    app: Employees,
    manifest: Arc<ApplicationManifest>,
}
impl RowApplication for WideEmployees {
    fn manifest(&self) -> ApplicationManifest {
        (*self.manifest).clone()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        self.app.permission_fields()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        self.app.validate_changes(changes)
    }
}
fn config(
    publisher: &PrivateKey,
    node: &PrivateKey,
    manifest: &ApplicationManifest,
    scope: ResourceScope,
    field: Commitment,
) -> Result<SecureConfig, Failure> {
    let hash = manifest.hash();
    let grant = ResourceGrant {
        scope,
        rows: RowSelection::All,
        rights: [Right::Read, Right::Insert, Right::Update, Right::Delete].into(),
        fields: [field].into(),
    };
    let user = PrivateKey::new();
    let deployment = PrivateKey::new();
    let extension = credentials::Extension::new(credentials::Extended::default())?;
    let capability = |subject, kind, delegations| Capability {
        manifest: hash,
        subject,
        kind,
        grants: vec![grant.clone()],
        not_before: super::lifetime().0,
        expires: super::lifetime().1,
        delegations,
        extension: extension.clone(),
    };
    Ok(SecureConfig {
        publisher: publisher.public_key(),
        manifest: SignedManifest::sign(manifest, publisher)?,
        revocations: RevocationSet::new(),
        node: node.clone(),
        credentials: CredentialChain(vec![
            SignedCapability::sign(
                capability(user.public_key(), PrincipalKind::User, 2),
                publisher,
            )?,
            SignedCapability::sign(
                capability(deployment.public_key(), PrincipalKind::Deployment, 1),
                &user,
            )?,
            SignedCapability::sign(
                capability(node.public_key(), PrincipalKind::Node, 0),
                &deployment,
            )?,
        ]),
        keys: vec![KeyMaterial {
            scope,
            generation: 1,
            key: [11; 32],
        }],
    })
}
pub async fn run() -> Result<(), Failure> {
    for entities in [10, 100, 500, 2000] {
        let publisher = PrivateKey::new();
        let app = Employees::new(*publisher.public_key().as_bytes());
        let scope = app.scope(MeshId([1; 16]), false);
        let field = app.field("display_name");
        let mut manifest = app.definition();
        for table in 2..entities {
            let name = format!("unused_{table}");
            manifest.entities.push(DefinitionItem {
                id: manifest.item_id("entity", &name),
                name: name.clone(),
                shape: "row".into(),
            });
            for column in 0..3 {
                let name = format!("{name}.field_{column}");
                manifest.fields.push(DefinitionItem {
                    id: manifest.item_id("field", &name),
                    name,
                    shape: "text".into(),
                });
            }
        }
        let manifest = Arc::new(manifest);
        let author = PrivateKey::new();
        let receiver = PrivateKey::new();
        let source_config = config(&publisher, &author, &manifest, scope, field)?;
        let target_config = config(&publisher, &receiver, &manifest, scope, field)?;
        let credentials = source_config.credentials.clone();
        let policy = source_config.revocations.clone();
        let time = super::lifetime().0 + rocky_secure::credentials::DAY;
        let start = Instant::now();
        let encoded = source_config.manifest.to_bytes()?;
        assert_eq!(
            SignedManifest::from_bytes(&encoded)?
                .verify(publisher.public_key())?
                .hash(),
            manifest.hash()
        );
        println!(
            "encrypted entities={entities} signed_manifest_bytes={} verify_us={}",
            encoded.len(),
            start.elapsed().as_micros()
        );
        let dir = tempfile::tempdir()?;
        let start = Instant::now();
        let source = EncryptedReplica::open(
            dir.path().join("source.db"),
            WideEmployees {
                app: Employees::new(*publisher.public_key().as_bytes()),
                manifest: manifest.clone(),
            },
            source_config,
        )
        .await?;
        let target = EncryptedReplica::open(
            dir.path().join("target.db"),
            WideEmployees {
                app: Employees::new(*publisher.public_key().as_bytes()),
                manifest: manifest.clone(),
            },
            target_config,
        )
        .await?;
        println!(
            "encrypted entities={entities} install_ms={} resident_mib={}",
            start.elapsed().as_millis(),
            resident_mib()
        );
        let start = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(credentials.verify(
                publisher.public_key(),
                manifest.hash(),
                author.public_key(),
                &policy,
                time,
            )?);
        }
        // Report pure verification separately from manifest hashing.
        let hash = manifest.hash();
        let pure = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(credentials.verify(
                publisher.public_key(),
                hash,
                author.public_key(),
                &policy,
                time,
            )?);
        }
        println!(
            "encrypted entities={entities} grant_us={} grant_plus_manifest_us={}",
            pure.elapsed().as_micros() / 1000,
            (start.elapsed() - pure.elapsed()).as_micros() / 1000
        );
        let mut author_times = vec![];
        let mut incoming_times = vec![];
        let mut read_times = vec![];
        for index in 0u64..1000 {
            let row = commitment("rocky.wide-encrypted.row", &index.to_le_bytes());
            let changes = vec![RowChange {
                scope,
                row,
                before: None,
                after: Some(app.name(&format!("row-{index}"))),
            }];
            let start = Instant::now();
            let frame = source.author(row, changes, vec![]).await?;
            author_times.push(start.elapsed());
            let start = Instant::now();
            target.receive(frame).await?;
            incoming_times.push(start.elapsed());
            let start = Instant::now();
            target.snapshot_page(scope, None, 1).await?;
            read_times.push(start.elapsed());
        }
        for (label, times) in [
            ("author", author_times),
            ("incoming", incoming_times),
            ("scope-read", read_times),
        ] {
            let total = times.iter().sum();
            line(
                &format!("encrypted entities={entities} {label}"),
                times,
                total,
            );
        }
        source.flush().await?;
        target.flush().await?;
        drop(source);
        drop(target);
    }
    Ok(())
}
