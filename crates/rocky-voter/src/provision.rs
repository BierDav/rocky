use crate::Failure;
use p2panda_core::{PrivateKey, PublicKey};
use rocky_coordinator::{Limits, certified::*};
use rocky_core::MeshId;
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub mesh: String,
    pub configuration: String,
    pub membership_authority: PublicKey,
    #[serde(default)]
    pub admission: Option<PublicKey>,
    pub voters: Vec<PlannedVoter>,
    pub max_control_lifetime: u64,
    pub tenants: u32,
    pub registrations_per_day: u32,
    pub tenant_lanes: u32,
    #[serde(default)]
    pub limits: Option<Limits>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedVoter {
    pub id: u64,
    pub address: String,
    pub key: PublicKey,
    pub client: String,
}
impl Plan {
    pub fn local(
        root: &PrivateKey,
        membership_authority: PublicKey,
        voters: Vec<PlannedVoter>,
    ) -> Self {
        let mesh = local_mesh(root);
        Self {
            mesh: hex(&mesh.0),
            configuration: hex(&rocky_core::commitment("rocky.local-configuration", &mesh.0)[..16]),
            membership_authority,
            admission: None,
            voters,
            max_control_lifetime: 30 * 86_400,
            tenants: 6,
            registrations_per_day: 64,
            tenant_lanes: 4,
            limits: None,
        }
    }
    pub fn certify(&self, root: &PrivateKey) -> Result<CertifiedGenesis, Failure> {
        let genesis = CertifiedGenesis::sign(
            Genesis {
                mesh: MeshId(unhex(&self.mesh)?),
                configuration: unhex(&self.configuration)?,
                voters: self
                    .voters
                    .iter()
                    .map(|v| Voter {
                        id: v.id,
                        address: v.address.clone(),
                        key: v.key,
                    })
                    .collect(),
                initial_voters: self.voters.iter().map(|v| v.id).collect(),
                membership_authority: self.membership_authority,
                authorities: Vec::new(),
                max_control_lifetime: self.max_control_lifetime,
                public: Some(PublicService {
                    endpoints: self
                        .voters
                        .iter()
                        .map(|v| ClientEndpoint {
                            voter: v.id,
                            route: v.client.clone(),
                        })
                        .collect(),
                    admission: self.admission,
                    limits: self.limits.unwrap_or_default(),
                    tenants: self.tenants,
                    registrations_per_day: self.registrations_per_day,
                    tenant_lanes: self.tenant_lanes,
                }),
            },
            root,
        )?;
        Ok(genesis)
    }
}
pub fn local_mesh(root: &PrivateKey) -> MeshId {
    let digest = rocky_core::commitment("rocky.local-mesh", root.public_key().as_bytes());
    let mut mesh = [0; 16];
    mesh.copy_from_slice(&digest[..16]);
    MeshId(mesh)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub data: PathBuf,
    pub genesis: PathBuf,
    pub trusted_root: PublicKey,
    pub mesh: MeshId,
    pub node: u64,
    pub key: PrivateKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<ClientLane>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientLane {
    pub secret: String,
    #[serde(default)]
    pub relay: Option<String>,
    #[serde(default)]
    pub discovery: bool,
    #[serde(default)]
    pub bind: Option<String>,
}
const ANNOUNCES_A_TOPIC: bool = false;
impl ClientLane {
    pub fn loopback(secret: &[u8; 32], bind: SocketAddr) -> Self {
        Self {
            secret: hex(secret),
            relay: Some("disabled".into()),
            discovery: false,
            bind: Some(bind.to_string()),
        }
    }
    pub async fn start(&self) -> Result<rocky_iroh::Attachment, Failure> {
        let options = rocky_iroh::Options {
            relay: self.relay.clone(),
            lookup: (!self.discovery).then(|| "disabled".into()),
            topic: ANNOUNCES_A_TOPIC,
            local: false,
            bind: self.bind.clone(),
        };
        options
            .start_serving(
                unhex(&self.secret)?,
                vec![rocky_core::net::Protocol::Coordinate],
            )
            .await
            .map_err(|error| -> Failure { error.to_string().into() })
    }
}
pub fn client_endpoint(secret: &[u8; 32], address: Option<SocketAddr>) -> Result<String, Failure> {
    use rocky_iroh::iroh::{EndpointAddr, SecretKey};
    let mut endpoint = EndpointAddr::new(SecretKey::from_bytes(secret).public());
    if let Some(address) = address {
        endpoint = endpoint.with_ip_addr(address);
    }
    rocky_iroh::route(&endpoint)
}
pub fn load(configuration: &Configuration) -> Result<CertifiedGenesis, Failure> {
    let genesis = CertifiedGenesis::from_bytes(&std::fs::read(&configuration.genesis)?)?;
    genesis.verify(configuration.trusted_root, configuration.mesh)?;
    Ok(genesis)
}
pub async fn open(
    configuration: &Configuration,
    genesis: CertifiedGenesis,
    provision: bool,
) -> Result<Arc<crate::VoterService>, Failure> {
    Ok(crate::VoterService::open(
        &configuration.data,
        genesis,
        configuration.trusted_root,
        configuration.mesh,
        configuration.node,
        configuration.key.clone(),
        provision,
    )
    .await?)
}
pub fn check_client_lane(
    configuration: &Configuration,
    genesis: &CertifiedGenesis,
) -> Result<(), Failure> {
    match (&configuration.client, genesis.genesis.public.is_some()) {
        (None, true) => Err("a public service needs a client lane".into()),
        (Some(_), false) => Err("only a public service has a client lane".into()),
        _ => Ok(()),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn unhex<const N: usize>(text: &str) -> Result<[u8; N], Failure> {
    let text = text.trim();
    if text.len() != N * 2 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("expected {} hexadecimal digits", N * 2).into());
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
    }
    Ok(bytes)
}
