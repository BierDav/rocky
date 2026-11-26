use crate::{Failure, VoterService, provision::*, service::Faults};
use p2panda_core::PrivateKey;
use rocky_coordinator::{TrustBundle, certified::CertifiedGenesis, service::NetworkCoordinator};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};

const VOTERS: std::ops::RangeInclusive<usize> = 3..=7;
const LEADER_ELECTED_WITHIN: Duration = Duration::from_secs(30);
const GENESIS: &str = "genesis.cbor";

pub struct LocalCluster {
    bundle: TrustBundle,
    faults: Arc<Faults>,
    voters: Vec<Running>,
}
struct Running {
    service: Arc<VoterService>,
    clients: rocky_iroh::Attachment,
    tasks: Vec<JoinHandle<()>>,
}
struct Prepared {
    configuration: Configuration,
    peers: TcpListener,
    clients: rocky_iroh::Attachment,
}
fn configuration_path(dir: &Path, node: u64) -> PathBuf {
    dir.join(format!("voter-{node}.json"))
}
fn placed(configuration: Configuration, dir: &Path) -> Configuration {
    Configuration {
        data: dir.join(format!("voter-{}", configuration.node)),
        genesis: dir.join(GENESIS),
        ..configuration
    }
}
impl LocalCluster {
    pub async fn start(dir: &Path, voters: usize) -> Result<Self, Failure> {
        if !VOTERS.contains(&voters) {
            return Err("a local cluster runs 3 to 7 voters".into());
        }
        if dir.join(GENESIS).exists() || configuration_path(dir, 1).exists() {
            return Err(format!("{} holds a local cluster; open it instead", dir.display()).into());
        }
        std::fs::create_dir_all(dir)?;
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
        let mut planned = Vec::new();
        let mut prepared = Vec::new();
        for node in (1..).take(voters) {
            let secret = *PrivateKey::new().as_bytes();
            let lane = ClientLane::loopback(&secret, SocketAddr::from(([127, 0, 0, 1], 0)));
            let clients = lane.start().await?;
            let bound = clients
                .endpoint()
                .bound_sockets()
                .into_iter()
                .find(SocketAddr::is_ipv4)
                .ok_or("the client lane bound no IPv4 address")?;
            let peers = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
            let key = PrivateKey::new();
            planned.push(PlannedVoter {
                id: node,
                address: peers.local_addr()?.to_string(),
                key: key.public_key(),
                client: client_endpoint(&secret, Some(bound))?,
            });
            let configuration = placed(
                Configuration {
                    data: PathBuf::new(),
                    genesis: PathBuf::new(),
                    trusted_root: root.public_key(),
                    mesh: local_mesh(&root),
                    node,
                    key,
                    client: Some(ClientLane::loopback(&secret, bound)),
                },
                dir,
            );
            rocky_credentials::write_private(
                &configuration_path(dir, node),
                &serde_json::to_vec(&configuration)?,
            )
            .map_err(|error| -> Failure { error.to_string().into() })?;
            prepared.push(Prepared {
                configuration,
                peers,
                clients,
            });
        }
        let genesis = Plan::local(&root, admin.public_key(), planned).certify(&root)?;
        std::fs::write(dir.join(GENESIS), genesis.to_bytes()?)?;
        let cluster = Self::launch(&genesis, prepared, true).await?;
        NetworkCoordinator::open(
            genesis.clone(),
            root.public_key(),
            genesis.genesis.mesh,
            admin,
        )?
        .initialize(1)
        .await?;
        cluster.leader_elected().await?;
        Ok(cluster)
    }
    pub async fn open(dir: &Path) -> Result<Self, Failure> {
        let mut configurations = Vec::new();
        for node in 1.. {
            let path = configuration_path(dir, node);
            if !path.exists() {
                break;
            }
            let stored: Configuration = serde_json::from_slice(&std::fs::read(path)?)?;
            configurations.push(placed(stored, dir));
        }
        let Some(first) = configurations.first() else {
            return Err(format!("{} holds no local cluster", dir.display()).into());
        };
        let genesis = load(first)?;
        let mut prepared = Vec::new();
        for configuration in configurations {
            let address = &genesis
                .genesis
                .voters
                .iter()
                .find(|voter| voter.id == configuration.node)
                .ok_or("the genesis lists no such voter")?
                .address;
            let peers = TcpListener::bind(address.as_str()).await?;
            let clients = configuration
                .client
                .as_ref()
                .ok_or("a local voter needs a client lane")?
                .start()
                .await?;
            prepared.push(Prepared {
                configuration,
                peers,
                clients,
            });
        }
        let cluster = Self::launch(&genesis, prepared, false).await?;
        cluster.leader_elected().await?;
        Ok(cluster)
    }
    async fn launch(
        genesis: &CertifiedGenesis,
        prepared: Vec<Prepared>,
        provision: bool,
    ) -> Result<Self, Failure> {
        let mut cluster = Self {
            bundle: TrustBundle::new(genesis)?,
            faults: Arc::default(),
            voters: Vec::new(),
        };
        for voter in prepared {
            let service = open(&voter.configuration, genesis.clone(), provision).await?;
            let peers = tokio::spawn({
                let service = service.clone();
                async move {
                    let Err(stopped) = service.serve_voters(voter.peers).await;
                    tracing::warn!(%stopped, "local voter stopped");
                }
            });
            let clients = tokio::spawn(
                service
                    .clone()
                    .serve_clients_with(voter.clients.transport(), Some(cluster.faults.clone())),
            );
            cluster.voters.push(Running {
                service,
                clients: voter.clients,
                tasks: vec![peers, clients],
            });
        }
        Ok(cluster)
    }
    pub fn bundle(&self) -> &TrustBundle {
        &self.bundle
    }
    pub fn lose_next_claim_reply(&self) {
        self.faults.lose_next_claim_reply();
    }
    pub async fn shutdown(mut self) {
        stop(std::mem::take(&mut self.voters)).await;
    }
    async fn leader_elected(&self) -> Result<(), Failure> {
        let mut leading = tokio::task::JoinSet::new();
        for voter in &self.voters {
            let service = voter.service.clone();
            leading.spawn(async move { service.leads_a_quorum().await });
        }
        match tokio::time::timeout(LEADER_ELECTED_WITHIN, leading.join_next()).await {
            Ok(Some(Ok(true))) => Ok(()),
            _ => Err("no local voter came to lead a quorum".into()),
        }
    }
}
async fn stop(voters: Vec<Running>) {
    for voter in voters {
        voter.tasks.iter().for_each(JoinHandle::abort);
        voter.clients.close().await;
        voter.service.shutdown().await;
    }
}
impl Drop for LocalCluster {
    fn drop(&mut self) {
        let voters = std::mem::take(&mut self.voters);
        voters
            .iter()
            .flat_map(|voter| &voter.tasks)
            .for_each(JoinHandle::abort);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            drop(runtime.spawn(stop(voters)));
        }
    }
}
