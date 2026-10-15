#[cfg(feature = "iroh")]
pub mod public;
use p2panda_core::PrivateKey;
use rocky_coordinator::{certified::*, service::*};
use rocky_core::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

/// A node certificate its node signed itself: the node key is its own
/// identity, which the controls these exercises sign trust.
pub fn certificate(key: &PrivateKey) -> rocky_credentials::SignedCapability {
    use rocky_credentials::{Capability, Extended, Extension, PeerId};
    let capability = Capability {
        subject: key.public_key(),
        kind: PrincipalKind::Node,
        grants: Vec::new(),
        not_before: 0,
        expires: u64::MAX,
        delegations: 0,
        extension: Extension::new(Extended {
            node: Some(PeerId::of(key.public_key())),
            ..Extended::default()
        })
        .unwrap(),
    };
    rocky_credentials::SignedCapability::sign(capability, key).unwrap()
}

#[allow(dead_code)] // Shared by independent multiprocess exercises.
pub struct Cluster {
    pub dir: TempDir,
    pub genesis: CertifiedGenesis,
    pub admin: PrivateKey,
    pub directory_key: PrivateKey,
    pub executor: PrivateKey,
    pub alternate: PrivateKey,
    pub user: PrivateKey,
    pub phone: PrivateKey,
    pub issuer: PrivateKey,
    pub scope: Scope,
    pub directory_scope: Scope,
    keys: Vec<PrivateKey>,
    children: Vec<Option<Child>>,
}
impl Cluster {
    pub async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
        let directory_key = PrivateKey::new();
        let executor = PrivateKey::new();
        let alternate = PrivateKey::new();
        let user = PrivateKey::new();
        let phone = PrivateKey::new();
        let issuer = PrivateKey::new();
        let keys: Vec<_> = (0..3).map(|_| PrivateKey::new()).collect();
        let listeners: Vec<_> = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        let voters: Vec<_> = listeners
            .iter()
            .zip(&keys)
            .enumerate()
            .map(|(i, (listener, key))| Voter {
                id: i as u64 + 1,
                address: listener.local_addr().unwrap().to_string(),
                key: key.public_key(),
            })
            .collect();
        let mesh = MeshId([0x91; 16]);
        let scope = Scope {
            mesh,
            app: inventory_app::app_id(),
            domain: DomainId([1; 16]),
        };
        let directory_app = directory_app::DirectoryApp {
            publisher: *directory_key.public_key().as_bytes(),
            domain: DomainId([2; 16]),
            bootstrap_registrar: directory_app::principal(*admin.public_key().as_bytes()),
            initial_ownership: vec![directory_app::Ownership {
                scope,
                owner: directory_app::principal(*user.public_key().as_bytes()),
            }],
        };
        let directory_scope = Scope {
            mesh,
            app: directory_app.manifest().app,
            domain: directory_app.domain,
        };
        let directory = ControlCertificate::sign(
            DomainControl {
                scope: directory_scope,
                configuration: [7; 16],
                revision: 1,
                trusted: vec![directory_key.public_key()],
                not_before: 0,
                expires: u64::MAX,
            },
            &directory_key,
        )
        .unwrap();
        // Valid certificates have an explicit finite lifetime.
        let mut control = directory.control;
        control.not_before = timestamp() - 1;
        control.expires = timestamp() + 3600;
        let directory = ControlCertificate::sign(control, &directory_key).unwrap();
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh,
                configuration: [7; 16],
                voters,
                initial_voters: BTreeSet::from([1, 2, 3]),
                membership_authority: admin.public_key(),
                directory_definition: directory_app.manifest().hash(),
                directory: Some(directory),
                authorities: vec![
                    Authority {
                        scope: directory_scope,
                        issuer: directory_key.public_key(),
                    },
                    Authority {
                        scope,
                        issuer: issuer.public_key(),
                    },
                ],
                max_control_lifetime: 86400,
                public: None,
            },
            &root,
        )
        .unwrap();
        std::fs::write(dir.path().join("genesis.cbor"), genesis.to_bytes().unwrap()).unwrap();
        // No root secret is written or retained in the running cluster.
        drop(root);
        drop(listeners);
        let mut cluster = Self {
            dir,
            genesis,
            admin,
            directory_key,
            executor,
            alternate,
            user,
            phone,
            issuer,
            scope,
            directory_scope,
            keys,
            children: (0..3).map(|_| None).collect(),
        };
        for id in 1..=3 {
            cluster.spawn(id, true, None);
        }
        let admin = cluster.client(&cluster.admin);
        for id in 1..=3 {
            cluster.wait_online(id, &admin).await;
        }
        admin.initialize(1).await.unwrap();
        cluster.wait_leader(&admin).await;
        cluster
    }
    pub fn client(&self, key: &PrivateKey) -> NetworkCoordinator {
        let client = NetworkCoordinator::open(
            self.genesis.clone(),
            self.genesis.root,
            self.genesis.genesis.mesh,
            key.clone(),
        )
        .unwrap();
        client.certify(certificate(key));
        client
    }
    pub fn app_control(&self, revision: u64) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope: self.scope,
                configuration: self.genesis.genesis.configuration,
                revision,
                trusted: vec![self.executor.public_key(), self.alternate.public_key()],
                not_before: timestamp() - 1,
                expires: timestamp() + 3600,
            },
            &self.issuer,
        )
        .unwrap()
    }
    pub fn spawn(&mut self, id: u64, provision: bool, fault: Option<&str>) {
        let index = id as usize - 1;
        let path = self.dir.path().join(format!("voter-{id}.json"));
        std::fs::write(&path,serde_json::to_vec(&serde_json::json!({"data":self.dir.path().join(format!("voter-{id}")),"genesis":self.dir.path().join("genesis.cbor"),"trusted_root":self.genesis.root,"mesh":self.genesis.genesis.mesh,"node":id,"key":self.keys[index]})).unwrap()).unwrap();
        let mut command = Command::new(public::voter());
        command.arg(path);
        command.env(
            "ROCKY_COORDINATOR_FAULT_FILE",
            self.dir.path().join(format!("voter-{id}.fault")),
        );
        if provision {
            command.arg("--provision");
        }
        if let Some(fault) = fault {
            command.env("ROCKY_COORDINATOR_CRASH", fault);
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join(format!("voter-{id}.log")))
            .unwrap();
        command
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        self.children[index] = Some(command.spawn().unwrap());
    }
    pub fn arm(&self, id: u64, stage: &str) {
        std::fs::write(self.dir.path().join(format!("voter-{id}.fault")), stage).unwrap();
    }
    pub fn stop(&mut self, id: u64) {
        if let Some(mut child) = self.children[id as usize - 1].take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    pub async fn wait_exit(&mut self, id: u64) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if self.children[id as usize - 1]
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_some()
            {
                self.children[id as usize - 1] = None;
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "fault did not trigger for voter {id}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub async fn wait_online(&self, id: u64, admin: &NetworkCoordinator) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if admin.status(id).await.is_ok() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "voter {id} unavailable: {}",
                std::fs::read_to_string(self.dir.path().join(format!("voter-{id}.log"))).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub async fn wait_leader(&self, admin: &NetworkCoordinator) -> u64 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            for id in 1..=3 {
                if let Ok(status) = admin.status(id).await
                    && status.leader == Some(id)
                {
                    return id;
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "no leader");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub fn store_path(&self, id: u64) -> PathBuf {
        self.dir.path().join(format!("voter-{id}/state.cbor"))
    }
}
impl Drop for Cluster {
    fn drop(&mut self) {
        for id in 1..=3 {
            self.stop(id);
        }
    }
}
pub fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
/// Joins `client` to `scope`'s log at its committed position.
pub async fn join(client: &NetworkCoordinator, scope: Scope, sequence: u64) {
    let head = client.coordinated_head(scope).await.unwrap();
    client
        .coordinated(
            scope,
            rocky_coordinator::coordinated::Action::Join {
                position: head.commit,
                running_hash: head.running_hash,
                sequence,
            },
        )
        .await
        .unwrap();
}
/// An append of `hash` at the log's current head.
pub async fn append(
    client: &NetworkCoordinator,
    scope: Scope,
    hash: u8,
    sequence: u64,
) -> rocky_coordinator::coordinated::Action {
    let head = client.coordinated_head(scope).await.unwrap();
    rocky_coordinator::coordinated::Action::Append {
        expected: head.head,
        epoch: head.epoch,
        hash: [hash; 32],
        deadline: timestamp() + 30,
        sequence,
    }
}

// Removing the leader can outlast one client call. Retry the same plan;
// a retained request must neither reapply nor reverse a later membership.
pub async fn membership_after_election(
    admin: &NetworkCoordinator,
    id: Commitment,
    operation: Operation,
) -> Result<rocky_coordinator::service::Outcome, CoordinationError> {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            match admin.call(id, operation.clone()).await {
                Err(CoordinationError::Unavailable) => continue,
                result => return result,
            }
        }
    })
    .await
    .unwrap_or(Err(CoordinationError::Unavailable))
}
