use crate::CoordinationError;
use p2panda_core::{PrivateKey, PublicKey, Signature};
use rocky_core::*;

pub type Configuration = [u8; 16];
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

pub const MAX_FRAME: usize = 256 * 1024;
pub const MAX_RECORDS: usize = 65536;
pub const MAX_STORE: usize = 1024 * 1024 * 1024;
const CONTROL_LABEL: &str = "rocky.control.v4";

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
    pub trusted: Vec<PublicKey>,
    pub publisher: Option<Box<PublicKey>>,
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
    pub fn verify(&self, genesis: &Genesis, time: u64) -> Result<(), CoordinationError> {
        if !genesis
            .authorities
            .iter()
            .any(|a| a.scope == self.control.scope && a.issuer == self.issuer)
            || self.control.scope.mesh != genesis.mesh
        {
            return Err(CoordinationError::Denied);
        }
        self.check_without_authority(genesis, time)
    }
    pub(crate) fn check_without_authority(
        &self,
        genesis: &Genesis,
        time: u64,
    ) -> Result<(), CoordinationError> {
        let c = &self.control;
        if c.configuration != genesis.configuration
            || c.revision == 0
            || c.trusted.is_empty()
            || c.trusted.len() > crate::machine::TRUSTED_NODES
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientEndpoint {
    pub voter: u64,
    pub kind: String,
    pub address: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicService {
    pub endpoints: Vec<ClientEndpoint>,
    pub admission: Option<PublicKey>,
    pub limits: crate::Limits,
    pub tenants: u32,
    pub registrations_per_day: u32,
    pub tenant_lanes: u32,
}
pub const CLIENT_LANES: u32 = 64;
impl PublicService {
    pub(crate) fn validate(&self, voters: &[Voter]) -> Result<(), CoordinationError> {
        self.limits.validate()?;
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
            || self.tenants == 0
            || !self.every_tenant_budget_fits()
            || self.registrations_per_day == 0
            || self.tenant_lanes == 0
            || self.tenant_lanes > CLIENT_LANES / 2
        {
            return Err(CoordinationError::Configuration);
        }
        Ok(())
    }
    fn every_tenant_budget_fits(&self) -> bool {
        let tenants = self.tenants as usize;
        tenants.saturating_mul(self.limits.tenant_bytes) <= self.limits.total_bytes
            && MAX_RECORDS
                .checked_sub(self.limits.retry_records)
                .is_some_and(|room| tenants.saturating_mul(self.limits.retry_records) <= room)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    pub mesh: MeshId,
    pub configuration: Configuration,
    pub voters: Vec<Voter>,
    pub initial_voters: BTreeSet<u64>,
    pub membership_authority: PublicKey,
    pub authorities: Vec<Authority>,
    pub max_control_lifetime: u64,
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
        Ok(())
    }
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

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CoordinationError> {
    p2panda_core::cbor::encode_cbor(value).map_err(|_| CoordinationError::Corrupt)
}
pub fn decode<T: Serialize + DeserializeOwned>(data: &[u8]) -> Result<T, CoordinationError> {
    if data.len() > MAX_STORE {
        return Err(CoordinationError::Denied);
    }
    let value = p2panda_core::cbor::decode_cbor(data).map_err(|_| CoordinationError::Corrupt)?;
    if encode(&value)? != data {
        return Err(CoordinationError::Corrupt);
    }
    Ok(value)
}
pub fn bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, CoordinationError> {
    encode(&(label, value))
}
pub fn now() -> Result<u64, CoordinationError> {
    let time = rocky_platform::time::SystemTime::now()
        .duration_since(rocky_platform::time::UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| CoordinationError::Corrupt)?;
    Ok(time)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBundle {
    pub format: u32,
    pub root: PublicKey,
    pub mesh: MeshId,
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
