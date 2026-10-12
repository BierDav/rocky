//! Development employees composition over authenticated loopback links and an opaque relay.
use employees_app::Employees;
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{
    Commitment, MeshId, PrincipalKind, ResourceGrant, ResourceScope, Right, RowChange, RowSelection,
};
use rocky_secure::{
    Capability, CredentialChain, EncryptedReplica, Error, KeyMaterial, PrivateKey, RevocationSet,
    SecureConfig, SignedCapability, SignedManifest,
    network::{NativeSession, link::Link},
    secure_node_id, secure_principal_id,
};
use std::{collections::BTreeSet, path::Path, sync::Arc};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
pub struct SecureDemo {
    pub admin: EncryptedReplica<Employees>,
    pub editor: EncryptedReplica<Employees>,
    pub relay: EncryptedReplica<Employees>,
    pub coordinator: Arc<MemoryCoordinator>,
    pub scopes: [ResourceScope; 2],
    identities: [(PrivateKey, CredentialChain); 3],
    publisher: [u8; 32],
}
impl SecureDemo {
    pub async fn open(dir: &Path) -> Result<Self, Error> {
        // Public fixture secrets make this reproducible development data only.
        let publisher = PrivateKey::from_bytes(&[81; 32]);
        let user = PrivateKey::from_bytes(&[82; 32]);
        let app = Employees::new(*publisher.public_key().as_bytes());
        let scopes = [
            app.scope(MeshId([1; 16]), false),
            app.scope(MeshId([1; 16]), true),
        ];
        let fields: BTreeSet<_> = app
            .definition()
            .fields
            .iter()
            .map(|field| field.id)
            .chain([app.definition().item_id("method", "SetEmployee")])
            .collect();
        let full: Vec<_> = scopes
            .iter()
            .map(|scope| ResourceGrant {
                scope: *scope,
                rows: RowSelection::All,
                rights: [
                    Right::Read,
                    Right::Update,
                    Right::Insert,
                    Right::Delete,
                    Right::Invoke,
                    Right::Execute,
                    Right::Custody,
                ]
                .into(),
                fields: fields.clone(),
            })
            .collect();
        let editor_grants = vec![ResourceGrant {
            scope: scopes[0],
            rows: RowSelection::All,
            rights: [Right::Read, Right::Insert, Right::Update].into(),
            fields: [app.field("display_name")].into(),
        }];
        let relay_grants = scopes
            .iter()
            .map(|scope| ResourceGrant {
                scope: *scope,
                rows: RowSelection::All,
                rights: [Right::Custody].into(),
                fields: BTreeSet::new(),
            })
            .collect();
        let manifest = SignedManifest::sign(&app.definition(), &publisher).map_err(signing)?;
        // Credentials live at most a year; each start issues them afresh.
        const DAY: u64 = 24 * 60 * 60;
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Rejected("clock before the epoch".into()))?
            .as_secs();
        let (not_before, expires) = (time.saturating_sub(DAY), time + 300 * DAY);
        let root = SignedCapability::sign(
            Capability {
                manifest: app.definition().hash(),
                subject: user.public_key(),
                kind: PrincipalKind::User,
                grants: full.clone(),
                not_before,
                expires,
                delegations: 1,
                extension: rocky_secure::credentials::Extension::new(
                    rocky_secure::credentials::Extended::default(),
                )
                .map_err(signing)?,
            },
            &publisher,
        )
        .map_err(signing)?;
        let config = |index: u8,
                      grants: Vec<ResourceGrant>,
                      keys: Vec<KeyMaterial>|
         -> Result<SecureConfig, Error> {
            let node = PrivateKey::from_bytes(&[index; 32]);
            let leaf = SignedCapability::sign(
                Capability {
                    manifest: app.definition().hash(),
                    subject: node.public_key(),
                    kind: PrincipalKind::Node,
                    grants,
                    not_before,
                    expires,
                    delegations: 0,
                    extension: rocky_secure::credentials::Extension::new(
                        rocky_secure::credentials::Extended {
                            node: Some(rocky_secure::PeerId::of(node.public_key())),
                            ..Default::default()
                        },
                    )
                    .map_err(signing)?,
                },
                &user,
            )
            .map_err(signing)?;
            Ok(SecureConfig {
                publisher: publisher.public_key(),
                manifest: manifest.clone(),
                revocations: RevocationSet::new(),
                node,
                credentials: CredentialChain(vec![root.clone(), leaf]),
                keys,
            })
        };
        let key = |scope| {
            KeyMaterial::new(
                scope,
                1,
                if scope == scopes[0] {
                    [11; 32]
                } else {
                    [22; 32]
                },
            )
        };
        let admin_config = config(83, full, scopes.iter().copied().map(key).collect())?;
        let editor_config = config(84, editor_grants, vec![key(scopes[0])])?;
        let relay_config = config(85, relay_grants, vec![])?;
        let identities = [&admin_config, &editor_config, &relay_config]
            .map(|config| (config.node.clone(), config.credentials.clone()));
        let coordinator = Arc::new(MemoryCoordinator::default());
        let caller = secure_principal_id(user.public_key());
        let executor = secure_node_id(admin_config.node.public_key());
        coordinator.register(scopes[0].domain, caller, [1; 16], vec![executor], 1, 1)?;
        coordinator.acquire(scopes[0].domain, caller, executor)?;
        let publisher_bytes = *publisher.public_key().as_bytes();
        let admin = EncryptedReplica::open(
            dir.join("admin-secure.db"),
            Employees::new(publisher_bytes),
            admin_config,
        )
        .await?;
        let editor = EncryptedReplica::open(
            dir.join("editor-secure.db"),
            Employees::new(publisher_bytes),
            editor_config,
        )
        .await?;
        let relay = EncryptedReplica::open(
            dir.join("relay-secure.db"),
            Employees::new(publisher_bytes),
            relay_config,
        )
        .await?;
        Ok(Self {
            admin,
            editor,
            relay,
            coordinator,
            scopes,
            identities,
            publisher: publisher_bytes,
        })
    }
    pub fn app(&self) -> Employees {
        Employees::new(self.publisher)
    }
    pub fn employee_id(&self) -> Commitment {
        rocky_core::commitment("employees.id.v1", b"demo-employee")
    }
    pub async fn set_employee(&self, name: &str, salary: &str) -> Result<(), Error> {
        let args = format!("demo-employee\n{name}\n{salary}");
        let handle = rocky_core::commitment("employees.demo-command.v1", args.as_bytes());
        let command = self
            .admin
            .invoke_encrypted(handle, self.scopes[1], "SetEmployee", args.as_bytes())
            .await?;
        self.admin
            .execute_encrypted(command, self.coordinator.as_ref())
            .await?;
        Ok(())
    }
    pub async fn edit_public_name(&self, name: &str) -> Result<(), Error> {
        let snapshot = self.editor.snapshot(self.scopes[0]).await?;
        let row = self.employee_id();
        let before = snapshot.rows.get(&row).cloned();
        let after = self.app().name(name);
        let handle = rocky_core::commitment(
            "employees.demo-edit.v1",
            &format!("{}:{name}", snapshot.revision).into_bytes(),
        );
        self.editor
            .author(
                handle,
                vec![RowChange {
                    scope: self.scopes[0],
                    row,
                    before,
                    after: Some(after),
                }],
                vec![],
            )
            .await?;
        Ok(())
    }
    /// Every hop uses mutual authentication and subscription-filtered native TCP.
    pub async fn pump(&self) -> Result<(), Error> {
        self.copy_rows(1, 2, vec![self.scopes[0]]).await?;
        self.copy_rows(2, 0, vec![self.scopes[0]]).await?;
        for sequence in 1..=rocky_secure::MAX_SECURE_RECORDS as u64 {
            let result = self
                .admin
                .encrypted_result_for(
                    self.identities[2].0.public_key(),
                    &self.identities[2].1,
                    self.scopes[0].domain,
                    sequence,
                    &self.scopes,
                )
                .await?;
            let Some(result) = result else {
                break;
            };
            let (mut admin, mut relay, _link) = self.channel(0, 2).await?;
            let (send, receive) = tokio::join!(
                admin.push_result(&self.admin, result),
                relay.serve_command_once(&self.relay, self.coordinator.as_ref())
            );
            send?;
            receive?;
            let (mut editor, mut relay, _link) = self.channel(1, 2).await?;
            let (receive, send) = tokio::join!(
                editor.pull_result(
                    &self.editor,
                    self.scopes[0].domain,
                    sequence,
                    vec![self.scopes[0]],
                    self.coordinator.as_ref()
                ),
                relay.serve_command_once(&self.relay, self.coordinator.as_ref())
            );
            receive?;
            send?;
        }
        Ok(())
    }
    fn replica(&self, index: usize) -> &EncryptedReplica<Employees> {
        [&self.admin, &self.editor, &self.relay][index]
    }
    async fn copy_rows(
        &self,
        source: usize,
        target: usize,
        scopes: Vec<ResourceScope>,
    ) -> Result<(), Error> {
        let mut cursor = None;
        loop {
            let (mut target_session, mut source_session, _link) =
                self.channel(target, source).await?;
            let serve = async {
                source_session.serve(self.replica(source)).await?;
                source_session.close().await
            };
            let pull = async {
                let pulled = target_session
                    .pull(self.replica(target), scopes.clone(), cursor, 128 * 1024)
                    .await;
                target_session.close().await?;
                pulled
            };
            let (serve, pull) = tokio::join!(serve, pull);
            serve?;
            cursor = pull?;
            if cursor.is_none() {
                return Ok(());
            }
        }
    }
    /// A request channel `opener` opens on a loopback link to `answerer`,
    /// with the session answering it. Dropping the guard ends the link.
    async fn channel(
        &self,
        opener: usize,
        answerer: usize,
    ) -> Result<(NativeSession, NativeSession, Driven), Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(Error::Io)?;
        let address = listener.local_addr().map_err(Error::Io)?;
        let accepted = async {
            let (stream, _) = listener.accept().await.map_err(Error::Io)?;
            Link::open(
                stream,
                self.replica(answerer),
                self.identities[answerer].0.clone(),
                self.identities[answerer].1.clone(),
                None,
            )
            .await
        };
        let dialed = async {
            let stream = TcpStream::connect(address).await.map_err(Error::Io)?;
            Link::open(
                stream,
                self.replica(opener),
                self.identities[opener].0.clone(),
                self.identities[opener].1.clone(),
                Some(self.identities[answerer].0.public_key()),
            )
            .await
        };
        let (accepted, dialed) = tokio::join!(accepted, dialed);
        let (mut accepted, mut dialed) = (accepted?, dialed?);
        let (near, far) = tokio::join!(dialed.ready(false), accepted.ready(false));
        near?;
        far?;
        let handle = dialed.channels.handle.clone();
        let (served, mut incoming) = tokio::sync::mpsc::channel(1);
        let driven = Driven([
            tokio::spawn(dialed.drive(tokio::sync::mpsc::channel(1).0)),
            tokio::spawn(accepted.drive(served)),
        ]);
        let session = handle.open().await?;
        let answering = incoming
            .recv()
            .await
            .ok_or_else(|| Error::Rejected("link ended".into()))?;
        Ok((session, answering, driven))
    }
}

/// Both ends of a demo link; neither outlives the exchange.
struct Driven([tokio::task::JoinHandle<Result<(), Error>>; 2]);
impl Drop for Driven {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// A signing failure as the runtime reports it.
fn signing(error: rocky_secure::credentials::Error) -> Error {
    Error::Rejected(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn desktop_native_composition_keeps_salary_from_editor_and_relay() {
        let dir = tempfile::TempDir::new().unwrap();
        let demo = SecureDemo::open(dir.path()).await.unwrap();
        demo.set_employee("Alice", "918273").await.unwrap();
        demo.edit_public_name("Alicia").await.unwrap();
        demo.pump().await.unwrap();
        assert_eq!(
            demo.admin.snapshot(demo.scopes[0]).await.unwrap().rows,
            demo.editor.snapshot(demo.scopes[0]).await.unwrap().rows
        );
        assert_eq!(
            demo.editor
                .command_snapshot(demo.scopes[0])
                .await
                .unwrap()
                .rows
                .len(),
            1
        );
        assert!(demo.editor.command_snapshot(demo.scopes[1]).await.is_err());
        assert!(demo.relay.command_snapshot(demo.scopes[1]).await.is_err());
        assert_eq!(
            demo.admin
                .command_snapshot(demo.scopes[1])
                .await
                .unwrap()
                .rows[&demo.employee_id()],
            demo.app().salary("918273")
        );
        assert!(
            demo.editor
                .author(
                    [90; 32],
                    vec![RowChange {
                        scope: demo.scopes[1],
                        row: demo.employee_id(),
                        before: None,
                        after: Some(demo.app().salary("1"))
                    }],
                    vec![]
                )
                .await
                .is_err()
        );
    }
}
