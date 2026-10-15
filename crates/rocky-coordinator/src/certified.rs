//! Root-certified, bounded mesh authority. Root secrets are never needed by voters.
use p2panda_core::{PrivateKey, PublicKey, Signature};
use rocky_core::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

pub const MAX_FRAME: usize = 256 * 1024;
pub const MAX_RECORDS: usize = 4096;
pub const MAX_STORE: usize = 32 * 1024 * 1024;
/// The one domain control format; a control signed under another is refused.
const CONTROL_LABEL: &str = "rocky.control.v2";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Voter {
    pub id: u64,
    pub address: String,
    pub key: PublicKey,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Authority {
    pub scope: Scope,
    pub issuer: PublicKey,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainControl {
    pub scope: Scope,
    pub configuration: Configuration,
    pub revision: u64,
    /// The trusted nodes that may join the domain's coordinated log.
    pub trusted: Vec<PublicKey>,
    pub not_before: u64,
    pub expires: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCertificate {
    pub control: DomainControl,
    pub issuer: PublicKey,
    signature: Signature,
}
impl ControlCertificate {
    pub fn sign(control: DomainControl, issuer: &PrivateKey) -> Result<Self, CoordinationError> {
        Ok(Self {
            signature: issuer.sign(&bytes(CONTROL_LABEL, &control)?),
            control,
            issuer: issuer.public_key(),
        })
    }
    /// A root-bound control: its issuer is the genesis authority of its
    /// scope in the certified mesh.
    pub fn verify(&self, genesis: &Genesis, time: u64) -> Result<(), CoordinationError> {
        if !genesis
            .authorities
            .iter()
            .any(|a| a.scope == self.control.scope && a.issuer == self.issuer)
            || self.control.scope.mesh != genesis.mesh
        {
            return Err(CoordinationError::Denied);
        }
        self.check(genesis, time)
    }
    /// Signature, bounds, configuration and lifetime, whoever the authority.
    pub(crate) fn check(&self, genesis: &Genesis, time: u64) -> Result<(), CoordinationError> {
        let c = &self.control;
        if c.configuration != genesis.configuration
            || c.revision == 0
            || c.trusted.is_empty()
            || c.trusted.len() > crate::coordinated::TRUSTED_NODES
            || c.not_before > time
            || c.expires <= time
            || c.expires - c.not_before > genesis.max_control_lifetime
            || c.trusted.iter().collect::<BTreeSet<_>>().len() != c.trusted.len()
            || !self
                .issuer
                .verify(&bytes(CONTROL_LABEL, c)?, &self.signature)
        {
            return Err(CoordinationError::Denied);
        }
        Ok(())
    }
    pub fn reference(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment("rocky.control-reference.v1", &encode(self)?))
    }
}
/// Where clients reach one voter: an address of transport `kind`, such as
/// an iroh endpoint address whose key the transport authenticates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientEndpoint {
    pub voter: u64,
    pub kind: String,
    pub address: String,
}
/// The provider's terms for tenants registering their own domains. A
/// genesis without it controls only its root-bound scopes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicService {
    /// The only client endpoints; tenant requests never use voter TCP.
    pub endpoints: Vec<ClientEndpoint>,
    /// Signs tenant admissions; `None` admits any tenant key.
    pub admission: Option<PublicKey>,
    /// Retention and quota budgets every voter applies.
    pub limits: crate::Limits,
    /// Tenants the service holds, each with the full tenant budgets.
    pub tenants: u32,
    /// Registrations, including replacements, per tenant per day.
    pub registrations_per_day: u32,
    /// Concurrent client requests per tenant at one voter.
    pub tenant_lanes: u32,
}
/// Most concurrent client requests one voter serves.
pub const CLIENT_LANES: u32 = 64;
impl PublicService {
    fn validate(&self, voters: &[Voter]) -> Result<(), CoordinationError> {
        self.limits.validate()?;
        let tenants = self.tenants as usize;
        if self.endpoints.is_empty()
            || self.endpoints.len() > voters.len()
            || self
                .endpoints
                .iter()
                .map(|e| e.voter)
                .collect::<BTreeSet<_>>()
                .len()
                != self.endpoints.len()
            || self.endpoints.iter().any(|e| {
                !voters.iter().any(|v| v.id == e.voter)
                    || e.kind.is_empty()
                    || e.kind.len() > 16
                    || e.address.is_empty()
                    || e.address.len() > 1024
            })
            || tenants == 0
            // No tenant can take another's state or retry budget.
            || tenants.saturating_mul(self.limits.tenant_bytes) > self.limits.total_bytes
            || tenants.saturating_mul(self.limits.retry_records)
                > MAX_RECORDS - self.limits.retry_records
            || self.registrations_per_day == 0
            || self.tenant_lanes == 0
            || self.tenant_lanes > CLIENT_LANES / 2
        {
            return Err(CoordinationError::Configuration);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    pub mesh: MeshId,
    pub configuration: Configuration,
    pub voters: Vec<Voter>,
    pub initial_voters: BTreeSet<u64>,
    pub membership_authority: PublicKey,
    /// Zero without a Directory.
    pub directory_definition: Commitment,
    /// Required unless the genesis is a public service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<ControlCertificate>,
    /// Exact scope bindings: these authorize metadata control, never row access.
    pub authorities: Vec<Authority>,
    pub max_control_lifetime: u64,
    /// Present for a service where tenants register their own domains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<PublicService>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertifiedGenesis {
    pub genesis: Genesis,
    pub root: PublicKey,
    signature: Signature,
}
impl CertifiedGenesis {
    pub fn sign(genesis: Genesis, root: &PrivateKey) -> Result<Self, CoordinationError> {
        let result = Self {
            signature: root.sign(&bytes("rocky.genesis.v2", &genesis)?),
            genesis,
            root: root.public_key(),
        };
        result.verify(root.public_key(), result.genesis.mesh)?;
        Ok(result)
    }
    pub fn verify(&self, root: PublicKey, mesh: MeshId) -> Result<(), CoordinationError> {
        let g = &self.genesis;
        if self.root != root
            || g.mesh != mesh
            || g.voters.len() < 3
            || g.voters.len() > 7
            || g.initial_voters.len() < 3
            || (g.authorities.is_empty() && g.public.is_none())
            || g.authorities.len() > 64
            || (g.directory_definition == [0; 32]) != g.directory.is_none()
            || (g.directory.is_none() && g.public.is_none())
            || g.configuration == [0; 16]
            || g.max_control_lifetime == 0
            || g.max_control_lifetime > 366 * 86400
            || g.voters.iter().map(|v| v.id).collect::<BTreeSet<_>>().len() != g.voters.len()
            || g.voters
                .iter()
                .map(|v| v.key)
                .collect::<BTreeSet<_>>()
                .len()
                != g.voters.len()
            || g.voters
                .iter()
                .any(|v| v.id == 0 || v.address.parse::<std::net::SocketAddr>().is_err())
            || !g
                .initial_voters
                .iter()
                .all(|id| g.voters.iter().any(|v| v.id == *id))
            || g.authorities
                .iter()
                .map(|a| (a.scope.mesh, a.scope.app, a.scope.domain))
                .collect::<BTreeSet<_>>()
                .len()
                != g.authorities.len()
            || !root.verify(&bytes("rocky.genesis.v2", g)?, &self.signature)
        {
            return Err(CoordinationError::Denied);
        }
        if let Some(public) = &g.public {
            public.validate(&g.voters)?;
        }
        match &g.directory {
            Some(directory) => directory.verify(g, directory.control.not_before),
            None => Ok(()),
        }
    }
    /// The retention budgets of this genesis: the public service's or the defaults.
    pub fn limits(&self) -> crate::Limits {
        self.genesis
            .public
            .as_ref()
            .map_or_else(crate::Limits::default, |public| public.limits)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, CoordinationError> {
        encode(self)
    }
    pub fn from_bytes(data: &[u8]) -> Result<Self, CoordinationError> {
        decode(data)
    }
    pub fn id(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment("rocky.genesis-id.v1", &encode(self)?))
    }
}

pub fn node_id(key: PublicKey) -> NodeId {
    {
        let digest = commitment("rocky.secure-node.v1", key.as_bytes());
        let mut id = [0; 16];
        id.copy_from_slice(&digest[..16]);
        NodeId(id)
    }
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CoordinationError> {
    p2panda_core::cbor::encode_cbor(value).map_err(|_| CoordinationError::Corrupt)
}
pub(crate) fn decode<T: Serialize + DeserializeOwned>(data: &[u8]) -> Result<T, CoordinationError> {
    if data.len() > MAX_STORE {
        return Err(CoordinationError::Denied);
    }
    let value = p2panda_core::cbor::decode_cbor(data).map_err(|_| CoordinationError::Corrupt)?;
    if encode(&value)? != data {
        return Err(CoordinationError::Corrupt);
    }
    Ok(value)
}
pub(crate) fn bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, CoordinationError> {
    encode(&(label, value))
}
pub(crate) fn now() -> Result<u64, CoordinationError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| CoordinationError::Corrupt)
}

/// What a client pins to use a coordination service: the operator's root
/// key, the service's mesh and its certified genesis. A provider publishes
/// one; a self-hosted service writes its own in the same format.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBundle {
    pub format: u32,
    pub root: PublicKey,
    pub mesh: MeshId,
    /// The certified genesis, as lowercase hexadecimal CBOR.
    pub genesis: String,
}
pub const BUNDLE_FORMAT: u32 = 2;
impl TrustBundle {
    pub fn new(genesis: &CertifiedGenesis) -> Result<Self, CoordinationError> {
        Ok(Self {
            format: BUNDLE_FORMAT,
            root: genesis.root,
            mesh: genesis.genesis.mesh,
            genesis: genesis
                .to_bytes()?
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        })
    }
    /// The verified genesis of a public service.
    pub fn genesis(&self) -> Result<CertifiedGenesis, CoordinationError> {
        if self.format != BUNDLE_FORMAT {
            return Err(CoordinationError::UnsupportedFormat);
        }
        let hex = self.genesis.as_bytes();
        if !hex.len().is_multiple_of(2) || hex.len() > MAX_FRAME * 2 {
            return Err(CoordinationError::Configuration);
        }
        let digit = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(CoordinationError::Configuration),
        };
        let bytes = hex
            .chunks(2)
            .map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?))
            .collect::<Result<Vec<u8>, CoordinationError>>()?;
        let genesis = CertifiedGenesis::from_bytes(&bytes)?;
        genesis.verify(self.root, self.mesh)?;
        if genesis.genesis.public.is_none() {
            return Err(CoordinationError::Configuration);
        }
        Ok(genesis)
    }
    pub fn to_json(&self) -> Result<String, CoordinationError> {
        serde_json::to_string_pretty(self).map_err(|_| CoordinationError::Corrupt)
    }
    pub fn from_json(json: &str) -> Result<Self, CoordinationError> {
        if json.len() > MAX_FRAME * 4 {
            return Err(CoordinationError::Configuration);
        }
        let bundle: Self =
            serde_json::from_str(json).map_err(|_| CoordinationError::Configuration)?;
        if bundle.format != BUNDLE_FORMAT {
            return Err(CoordinationError::UnsupportedFormat);
        }
        Ok(bundle)
    }
}
