pub mod public;
use p2panda_core::PrivateKey;
use rocky_coordinator::CoordinationError;
use rocky_coordinator::{certified::*, service::*};
use rocky_core::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tempfile::TempDir;

pub fn certificate(key: &PrivateKey) -> rocky_credentials::SignedCapability {
    use rocky_credentials::{Capability, Extended, Extension, PeerId};
    let database = rocky_credentials::DatabaseContext {
        publisher: key.public_key(),
        scope: rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        },
    };
    let capability = Capability {
        subject: key.public_key(),
        kind: PrincipalKind::Node,
        grants: Vec::new(),
        not_before: 0,
        expires: u64::MAX,
        delegations: 0,
        extension: Extension::new(Extended {
            node: Some(PeerId::of(key.public_key())),
            database: Some(database),
            sequence: Some(0),
            possession: Some(
                rocky_credentials::NodePossession::sign(key, key.public_key(), database).unwrap(),
            ),
            ..Extended::default()
        })
        .unwrap(),
    };
    rocky_credentials::SignedCapability::sign(capability, key).unwrap()
}

#[expect(dead_code, reason = "Shared by independent multiprocess exercises.")]
pub struct Cluster {
    pub dir: TempDir,
    pub genesis: CertifiedGenesis,
    pub admin: PrivateKey,
    pub executor: PrivateKey,
    pub alternate: PrivateKey,
    pub user: PrivateKey,
    pub phone: PrivateKey,
    pub issuer: PrivateKey,
    pub scope: Scope,
    keys: Vec<PrivateKey>,
    children: Vec<Option<Child>>,
}
impl Cluster {
    pub async fn start() -> Self {
        let dir = TempDir::new().unwrap();
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
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
            app: rocky_core::ApplicationManifest::app_id([0x81; 32], "inventory"),
            domain: DomainId([1; 16]),
        };
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh,
                configuration: [7; 16],
                voters,
                initial_voters: BTreeSet::from([1, 2, 3]),
                membership_authority: admin.public_key(),
                authorities: vec![Authority {
                    scope,
                    issuer: issuer.public_key(),
                }],
                max_control_lifetime: 86400,
                public: None,
            },
            &root,
        )
        .unwrap();
        std::fs::write(dir.path().join("genesis.cbor"), genesis.to_bytes().unwrap()).unwrap();
        drop(root);
        drop(listeners);
        let mut cluster = Self {
            dir,
            genesis,
            admin,
            executor,
            alternate,
            user,
            phone,
            issuer,
            scope,
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
        cluster.wait_unfenced_leader(&admin).await;
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
                publisher: None,
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
    pub async fn wait_unfenced_leader(&self, admin: &NetworkCoordinator) -> u64 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(leader) = leader_recognized_by_majority(admin).await {
                return leader;
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
const VOTERS: u64 = 3;
pub async fn leader_recognized_by_majority(admin: &NetworkCoordinator) -> Option<u64> {
    let mut statuses = Vec::new();
    for id in 1..=VOTERS {
        if let Ok(status) = admin.status(id).await {
            statuses.push((id, status));
        }
    }
    statuses.iter().find_map(|(id, status)| {
        let recognizing = statuses
            .iter()
            .filter(|(_, other)| other.leader == Some(*id))
            .count() as u64;
        (status.leader == Some(*id) && !status.fenced && 2 * recognizing > VOTERS).then_some(*id)
    })
}
pub fn timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
pub fn invoices() -> rocky_coordinator::counters::Name {
    rocky_coordinator::counters::Name::new("invoices").unwrap()
}
pub fn fresh_claim(n: u8) -> rocky_coordinator::counters::Claim {
    rocky_coordinator::counters::Claim {
        id: [n; 32],
        issued: timestamp(),
    }
}
pub async fn reading(client: &NetworkCoordinator, scope: Scope) -> Result<u64, CoordinationError> {
    Ok(client.counter(scope, invoices(), None).await?.value)
}

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
