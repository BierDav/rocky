use crate::{Failure, VoterService, provision::*};
use p2panda_core::PrivateKey;
use rocky_coordinator::{TrustBundle, service::NetworkCoordinator};
use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};
use tokio::{net::TcpListener, task::JoinHandle};

const VOTERS: std::ops::RangeInclusive<usize> = 3..=7;
const LEADER_ELECTED_WITHIN: Duration = Duration::from_secs(30);

pub struct LocalCluster {
    bundle: TrustBundle,
    voters: Vec<Running>,
}
struct Running {
    service: Arc<VoterService>,
    clients: rocky_iroh::Attachment,
    tasks: Vec<JoinHandle<()>>,
}
struct Prepared {
    id: u64,
    key: PrivateKey,
    peers: TcpListener,
    clients: rocky_iroh::Attachment,
}
impl LocalCluster {
    pub async fn start(dir: &Path, voters: usize) -> Result<Self, Failure> {
        if !VOTERS.contains(&voters) {
            return Err("a local cluster runs 3 to 7 voters".into());
        }
        let root = PrivateKey::new();
        let admin = PrivateKey::new();
        let mut planned = Vec::new();
        let mut prepared = Vec::new();
        for id in (1..).take(voters) {
            let key = PrivateKey::new();
            let secret = *PrivateKey::new().as_bytes();
            let clients = ClientLane::loopback(&secret, SocketAddr::from(([127, 0, 0, 1], 0)))
                .start()
                .await?;
            let bound = clients
                .endpoint()
                .bound_sockets()
                .into_iter()
                .find(SocketAddr::is_ipv4)
                .ok_or("the client lane bound no IPv4 address")?;
            let peers = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
            planned.push(PlannedVoter {
                id,
                address: peers.local_addr()?.to_string(),
                key: key.public_key(),
                client: client_endpoint(&secret, Some(bound))?,
            });
            prepared.push(Prepared {
                id,
                key,
                peers,
                clients,
            });
        }
        let genesis = Plan::local(&root, admin.public_key(), planned).certify(&root)?;
        std::fs::create_dir_all(dir)?;
        let genesis_path = dir.join("genesis.cbor");
        std::fs::write(&genesis_path, genesis.to_bytes()?)?;
        let mut cluster = Self {
            bundle: TrustBundle::new(&genesis)?,
            voters: Vec::new(),
        };
        for voter in prepared {
            let configuration = Configuration {
                data: dir.join(format!("voter-{}", voter.id)),
                genesis: genesis_path.clone(),
                trusted_root: root.public_key(),
                mesh: genesis.genesis.mesh,
                node: voter.id,
                key: voter.key,
                client: None,
            };
            let service = open(&configuration, genesis.clone(), true).await?;
            let peers = tokio::spawn({
                let service = service.clone();
                async move {
                    let Err(stopped) = service.serve_voters(voter.peers).await;
                    tracing::warn!(%stopped, "local voter stopped");
                }
            });
            let clients = tokio::spawn(service.clone().serve_clients(voter.clients.transport()));
            cluster.voters.push(Running {
                service,
                clients: voter.clients,
                tasks: vec![peers, clients],
            });
        }
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
    pub fn bundle(&self) -> &TrustBundle {
        &self.bundle
    }
    pub async fn shutdown(mut self) {
        for voter in self.voters.drain(..) {
            voter.tasks.iter().for_each(JoinHandle::abort);
            voter.clients.transport().close().await;
            voter.service.shutdown().await;
        }
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
impl Drop for LocalCluster {
    fn drop(&mut self) {
        for voter in &self.voters {
            voter.tasks.iter().for_each(JoinHandle::abort);
        }
    }
}
