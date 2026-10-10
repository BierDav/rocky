use directory_app::{Directory, DirectoryApp, Disposition, Transition};
use p2panda_core::PrivateKey;
use rocky_core::*;
use rocky_secure::*;
use std::collections::BTreeSet;
#[derive(Clone)]
struct Accounting {
    publisher: Commitment,
}
impl RowApplication for Accounting {
    fn manifest(&self) -> ApplicationManifest {
        let mut m = ApplicationManifest {
            publisher: self.publisher,
            name: "Accounting".into(),
            app: ApplicationManifest::app_id(self.publisher, "Accounting"),
            schema: 1,
            projection: 1,
            handler: 1,
            entities: vec![],
            fields: vec![],
            methods: vec![],
            operations: vec![],
            domains: vec![DomainRule {
                domain: DomainId([1; 16]),
                author_log: AuthorLogScope::Domain,
                rule: "Accounting owns data authority".into(),
            }],
        };
        m.entities.push(DefinitionItem {
            id: m.item_id("entity", "Account"),
            name: "Account".into(),
            shape: "row identity".into(),
        });
        m.fields.push(DefinitionItem {
            id: m.item_id("field", "Balance"),
            name: "Balance".into(),
            shape: "u64-le".into(),
        });
        m
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        BTreeSet::new()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        let field = self.manifest().item_id("field", "Balance");
        if changes.iter().any(|c| {
            c.after.as_ref().is_some_and(|fields| {
                fields.len() != 1 || fields.get(&field).is_none_or(|v| v.len() != 8)
            })
        }) {
            return Err("invalid balance".into());
        }
        Ok(())
    }
}
#[tokio::test]
async fn registration_does_not_authorize_accounting_and_learned_deletion_revokes_grants() {
    let root = PrivateKey::new();
    let accounting = PrivateKey::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let phone = PrivateKey::new();
    let caller = directory_app::principal(*user.public_key().as_bytes());
    let directory = DirectoryApp {
        publisher: *root.public_key().as_bytes(),
        domain: DomainId([2; 16]),
        bootstrap_registrar: directory_app::principal(*root.public_key().as_bytes()),
        initial_ownership: vec![],
    };
    let registry = directory
        .transition(
            Directory::default(),
            directory.bootstrap_registrar,
            Transition::RegisterUser {
                key: *user.public_key().as_bytes(),
            },
        )
        .unwrap();
    let registry = directory
        .transition(
            registry,
            caller,
            Transition::RegisterNode {
                key: *node.public_key().as_bytes(),
                owner: caller,
                deployment: None,
                time: 10,
            },
        )
        .unwrap();
    let registry = directory
        .transition(
            registry,
            caller,
            Transition::RegisterNode {
                key: *phone.public_key().as_bytes(),
                owner: caller,
                deployment: None,
                time: 10,
            },
        )
        .unwrap();
    assert!(registry.registered_node(*node.public_key().as_bytes()));
    let app = Accounting {
        publisher: *accounting.public_key().as_bytes(),
    };
    let manifest = app.manifest();
    let hash = manifest.hash();
    let field = manifest.item_id("field", "Balance");
    let resource = ResourceScope {
        domain: Scope {
            mesh: MeshId([3; 16]),
            app: manifest.app,
            domain: DomainId([1; 16]),
        },
        encryption: [4; 32],
        table: manifest.item_id("entity", "Account"),
    };
    let allowed = [5; 32];
    let grants = vec![
        ResourceGrant {
            scope: resource,
            rows: RowSelection::All,
            rights: BTreeSet::from([Right::Read]),
            fields: BTreeSet::from([field]),
        },
        ResourceGrant {
            scope: resource,
            rows: RowSelection::Only(BTreeSet::from([allowed])),
            rights: BTreeSet::from([Right::Insert, Right::Update]),
            fields: BTreeSet::from([field]),
        },
    ];
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let capability = |subject, kind, delegations| Capability {
        manifest: hash,
        subject,
        kind,
        grants: grants.clone(),
        not_before: time - rocky_credentials::DAY,
        expires: time + 300 * rocky_credentials::DAY,
        delegations,
        extension: rocky_credentials::Extension::new(rocky_credentials::Extended {
            node: (kind == PrincipalKind::Node).then(|| rocky_credentials::PeerId::of(subject)),
            ..Default::default()
        })
        .unwrap(),
    };
    let signed = SignedManifest::sign(&manifest, &accounting).unwrap();
    let config = |credentials| SecureConfig {
        publisher: accounting.public_key(),
        manifest: signed.clone(),
        revocations: RevocationSet::new(),
        node: node.clone(),
        credentials,
        keys: vec![KeyMaterial::new(resource, 1, [6; 32])],
    };
    let delegated =
        SignedCapability::sign(capability(node.public_key(), PrincipalKind::Node, 0), &user)
            .unwrap();
    let registration_only = CredentialChain(vec![
        SignedCapability::sign(capability(user.public_key(), PrincipalKind::User, 1), &root)
            .unwrap(),
        delegated.clone(),
    ]);
    let path = tempfile::tempdir().unwrap();
    assert!(
        EncryptedReplica::open(
            path.path().join("denied.db"),
            app.clone(),
            config(registration_only)
        )
        .await
        .is_err()
    );
    let app_grant = SignedCapability::sign(
        capability(user.public_key(), PrincipalKind::User, 1),
        &accounting,
    )
    .unwrap();
    let grant_id = app_grant.id().unwrap();
    let replica = EncryptedReplica::open(
        path.path().join("accounting.db"),
        app,
        config(CredentialChain(vec![app_grant, delegated])),
    )
    .await
    .unwrap();
    let change = |row| RowChange {
        scope: resource,
        row,
        before: None,
        after: Some(RowFields::from([(field, 10u64.to_le_bytes().to_vec())])),
    };
    assert!(
        replica
            .author([1; 32], vec![change([7; 32])], vec![])
            .await
            .is_err()
    );
    replica
        .author([2; 32], vec![change(allowed)], vec![])
        .await
        .unwrap();
    let registry = directory
        .transition(
            registry,
            caller,
            Transition::TrackArtifacts(directory_app::Artifacts {
                owner: caller,
                app_grants: vec![grant_id],
                keys: vec![resource],
                pending: vec![],
            }),
        )
        .unwrap();
    let registry = directory
        .transition(
            registry,
            caller,
            Transition::DeleteUser {
                disposition: Vec::<(Scope, Disposition)>::new(),
            },
        )
        .unwrap();
    assert!(!registry.registered_node(*phone.public_key().as_bytes()));
    assert_eq!(registry.deletions[0].rotate_keys, vec![resource]);
    let issuer = PrivateKey::new();
    let statement = Revocation {
        app: manifest.app,
        issuer: issuer.public_key(),
        time,
        nodes: BTreeSet::new(),
        users: BTreeSet::new(),
        capabilities: registry.deletions[0]
            .revoked_grants
            .iter()
            .copied()
            .collect(),
        pseudonyms: BTreeSet::new(),
    };
    replica
        .learn_revocations(
            &RevocationSet::try_from(vec![
                SignedRevocation::sign(statement, &accounting, &issuer).unwrap(),
            ])
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        replica
            .author(
                [3; 32],
                vec![RowChange {
                    scope: resource,
                    row: allowed,
                    before: Some(RowFields::from([(field, 10u64.to_le_bytes().to_vec())])),
                    after: Some(RowFields::from([(field, 11u64.to_le_bytes().to_vec())]))
                }],
                vec![]
            )
            .await
            .is_err()
    );
}
