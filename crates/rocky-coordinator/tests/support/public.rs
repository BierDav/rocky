//! A public service of three voter processes with iroh client lanes on
//! loopback, set up with the same `rocky-voter` commands an operator uses.
#![allow(dead_code)] // Shared by independent multiprocess exercises.
use super::timestamp;
use p2panda_core::{PrivateKey, PublicKey};
use rocky_coordinator::{certified::*, service::*};
use rocky_core::{net::Transport, *};
use rocky_credentials::{
    CoordinationGrant, CoordinatorControl, Publisher, SignedCoordinationGrant,
    SignedTenantAdmission, TenantAdmission,
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tempfile::TempDir;

pub const DAY: u64 = 86_400;

pub struct PublicCluster {
    pub dir: TempDir,
    pub bundle: TrustBundle,
    pub genesis: CertifiedGenesis,
    pub admin: PrivateKey,
    pub provider: PrivateKey,
    pub limits: rocky_coordinator::Limits,
    children: Vec<Option<Child>>,
}
/// One application owner: its tenant control key, its publisher and devices.
pub struct Tenant {
    pub name: &'static str,
    pub key: PrivateKey,
    pub publisher: PrivateKey,
    pub executor: PrivateKey,
    pub alternate: PrivateKey,
    pub phone: PrivateKey,
}
pub fn voter() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rocky-voter"))
}
fn run(arguments: &[&Path]) -> String {
    let output = Command::new(voter()).args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "rocky-voter {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
impl PublicCluster {
    /// Each voter generates its own secrets; the root certifies the plan
    /// offline and is discarded before any voter starts.
    pub async fn start(limits: rocky_coordinator::Limits, tenants: u32) -> Self {
        let dir = TempDir::new().unwrap();
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
        let provider = PrivateKey::new();
        let mut voters = Vec::new();
        for id in 1..=3u64 {
            let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let (tcp_address, udp_address) = (tcp.local_addr().unwrap(), udp.local_addr().unwrap());
            drop((tcp, udp));
            let keys = dir.path().join(format!("keys-{id}"));
            let printed: serde_json::Value = serde_json::from_str(&run(&[
                Path::new("keys"),
                &keys,
                Path::new(&udp_address.to_string()),
            ]))
            .unwrap();
            voters.push(serde_json::json!({
                "id": id,
                "address": tcp_address.to_string(),
                "key": printed["key"],
                "client": printed["client"],
            }));
            let config = serde_json::json!({
                "data": dir.path().join(format!("voter-{id}")),
                "genesis": dir.path().join("service/genesis.cbor"),
                "trusted_root": root.public_key(),
                "mesh": MeshId([0x71; 16]),
                "node": id,
                "key": std::fs::read_to_string(keys.join("voter.key")).unwrap(),
                "client": {
                    "secret": std::fs::read_to_string(keys.join("client.secret")).unwrap(),
                    "relay": "disabled",
                    "discovery": false,
                    "bind": udp_address.to_string(),
                },
            });
            std::fs::write(
                dir.path().join(format!("voter-{id}.json")),
                serde_json::to_vec(&config).unwrap(),
            )
            .unwrap();
        }
        let plan = serde_json::json!({
            "mesh": "71".repeat(16),
            "configuration": "07".repeat(16),
            "membership_authority": admin.public_key(),
            "admission": provider.public_key(),
            "voters": voters,
            "max_control_lifetime": 30 * DAY,
            "tenants": tenants,
            "registrations_per_day": 64,
            "tenant_lanes": 2,
            "limits": limits,
        });
        std::fs::write(dir.path().join("plan.json"), plan.to_string()).unwrap();
        std::fs::write(dir.path().join("root.key"), root.to_hex()).unwrap();
        run(&[
            Path::new("certify"),
            &dir.path().join("plan.json"),
            &dir.path().join("root.key"),
            &dir.path().join("service"),
        ]);
        std::fs::remove_file(dir.path().join("root.key")).unwrap();
        drop(root);
        let bundle = TrustBundle::from_json(
            &std::fs::read_to_string(dir.path().join("service/bundle.json")).unwrap(),
        )
        .unwrap();
        let genesis = bundle.genesis().unwrap();
        let mut cluster = Self {
            dir,
            bundle,
            genesis,
            admin,
            provider,
            limits,
            children: (0..3).map(|_| None).collect(),
        };
        for id in 1..=3 {
            cluster.spawn(id, true);
        }
        let admin = cluster.admin();
        for id in 1..=3 {
            cluster.wait_online(id, &admin).await;
        }
        admin.initialize(1).await.unwrap();
        cluster.wait_leader(&admin).await;
        cluster
    }
    pub fn config(&self, id: u64) -> PathBuf {
        self.dir.path().join(format!("voter-{id}.json"))
    }
    pub fn data(&self, id: u64) -> PathBuf {
        self.dir.path().join(format!("voter-{id}"))
    }
    pub fn spawn(&mut self, id: u64, provision: bool) {
        let mut command = Command::new(voter());
        command.arg(self.config(id));
        if provision {
            command.arg("--provision");
        }
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.path().join(format!("voter-{id}.log")))
            .unwrap();
        command
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        self.children[id as usize - 1] = Some(command.spawn().unwrap());
    }
    pub fn stop(&mut self, id: u64) {
        if let Some(mut child) = self.children[id as usize - 1].take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    pub fn log(&self, id: u64) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("voter-{id}.log"))).unwrap_or_default()
    }
    /// The membership authority administers voters over provider TCP.
    pub fn admin(&self) -> NetworkCoordinator {
        NetworkCoordinator::open(
            self.genesis.clone(),
            self.genesis.root,
            self.genesis.genesis.mesh,
            self.admin.clone(),
            self.dir.path().join("admin.evidence"),
        )
        .unwrap()
    }
    pub async fn wait_online(&self, id: u64, admin: &NetworkCoordinator) {
        self.wait(&format!("voter {id} online"), async || {
            admin.status(id).await.is_ok()
        })
        .await;
    }
    pub async fn wait_leader(&self, admin: &NetworkCoordinator) -> u64 {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            for id in 1..=3 {
                // A fenced voter may report the leadership of its backup.
                if let Ok(status) = admin.status(id).await
                    && status.leader == Some(id)
                    && !status.fenced
                {
                    return id;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let mut statuses = Vec::new();
                for id in 1..=3 {
                    statuses.push(
                        admin
                            .status(id)
                            .await
                            .map(|s| (s.leader, s.voters, s.fenced, s.applied)),
                    );
                }
                panic!(
                    "no leader: {statuses:?}\n{}\n{}\n{}",
                    self.log(1),
                    self.log(2),
                    self.log(3)
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    pub async fn wait(&self, what: &str, mut condition: impl AsyncFnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !condition().await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {what}\n{}\n{}\n{}",
                self.log(1),
                self.log(2),
                self.log(3)
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    pub fn admission(&self, tenant: &Tenant) -> SignedTenantAdmission {
        SignedTenantAdmission::sign(
            TenantAdmission {
                provider: self.provider.public_key(),
                tenant: CoordinatorControl(tenant.key.public_key()),
                not_before: timestamp() - 60,
                expires: timestamp() + DAY,
            },
            &self.provider,
        )
        .unwrap()
    }
}
impl Drop for PublicCluster {
    fn drop(&mut self) {
        for id in 1..=3 {
            self.stop(id);
        }
    }
}

/// A client endpoint the device owns: dial-only, relays and discovery off.
pub async fn transport() -> Arc<dyn Transport> {
    let options = rocky_iroh::Options {
        relay: Some("disabled".into()),
        discovery: false,
        bind: Some("127.0.0.1:0".into()),
    };
    let endpoint = options.bind(*PrivateKey::new().as_bytes()).await.unwrap();
    rocky_iroh::Attachment::serving(endpoint, Vec::new()).transport()
}

impl Tenant {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            key: PrivateKey::new(),
            publisher: PrivateKey::new(),
            executor: PrivateKey::new(),
            alternate: PrivateKey::new(),
            phone: PrivateKey::new(),
        }
    }
    pub fn app(&self) -> AppId {
        ApplicationManifest::app_id(*self.publisher.public_key().as_bytes(), self.name)
    }
    /// A domain as the SDK creates one: its own random mesh and domain.
    pub fn scope(&self, n: u8) -> Scope {
        Scope {
            mesh: MeshId([n; 16]),
            app: self.app(),
            domain: DomainId([n; 16]),
        }
    }
    pub fn principal(&self) -> PrincipalId {
        PrincipalId(self.key.public_key().as_bytes()[..16].try_into().unwrap())
    }
    pub fn grant(&self) -> SignedCoordinationGrant {
        SignedCoordinationGrant::sign(
            CoordinationGrant {
                publisher: Publisher(self.publisher.public_key()),
                application: self.name.into(),
                tenant: CoordinatorControl(self.key.public_key()),
                not_before: timestamp() - 60,
                expires: timestamp() + DAY,
            },
            &self.publisher,
        )
        .unwrap()
    }
    pub fn control(
        &self,
        genesis: &CertifiedGenesis,
        scope: Scope,
        revision: u64,
        executors: &[&PrivateKey],
    ) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: genesis.genesis.configuration,
                revision,
                owner: self.principal(),
                executors: executors.iter().map(|k| k.public_key()).collect(),
                invokers: vec![
                    (self.executor.public_key(), self.principal()),
                    (self.alternate.public_key(), self.principal()),
                    (self.phone.public_key(), self.principal()),
                ],
                schema: 1,
                handler: 1,
                not_before: timestamp() - 60,
                expires: timestamp() + DAY,
            },
            &self.key,
        )
        .unwrap()
    }
    pub async fn client(
        &self,
        cluster: &PublicCluster,
        key: &PrivateKey,
        name: &str,
    ) -> NetworkCoordinator {
        NetworkCoordinator::connect(
            &cluster.bundle,
            key.clone(),
            cluster
                .dir
                .path()
                .join(format!("{}-{name}.evidence", self.name)),
            transport().await,
        )
        .unwrap()
    }
    pub fn public_key(&self) -> PublicKey {
        self.key.public_key()
    }
}
