use crate::{CoordinationError, Refusal};
use p2panda_core::{PrivateKey, PublicKey, Signature};
use rocky_core::*;

/// Identifies one coordinator deployment; control certificates must name it.
pub type Configuration = [u8; 16];
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::collections::BTreeSet;

/// The largest message on the wire, in bytes.
pub const MAX_FRAME: usize = 256 * 1024;
/// Retry records all tenants may hold together.
pub const MAX_RECORDS: usize = 65536;
/// The largest encoded state a decoder accepts, in bytes.
pub const MAX_STORE: usize = 1024 * 1024 * 1024;
const CONTROL_LABEL: &str = "rocky.control.v4";

/// One Raft voter of the coordinator.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Voter {
    /// Its nonzero id.
    pub id: u64,
    /// Its `host:port` for voter traffic.
    pub address: String,
    /// The key it signs messages with.
    pub key: PublicKey,
}
/// A key allowed to control one database without a public registration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Authority {
    /// The database it controls.
    pub scope: Scope,
    /// The controlling key.
    pub issuer: PublicKey,
}
/// What a control certificate says about one database.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomainControl {
    /// The database.
    pub scope: Scope,
    /// The coordinator deployment it is for.
    pub configuration: Configuration,
    /// Starts at 1 and rises with each renewal.
    pub revision: u64,
    /// The identity keys whose devices may call.
    pub trusted: Vec<PublicKey>,
    /// The database's root key, whose membership chain the coordinator checks; `None` admits no device
    /// calls.
    pub publisher: Option<Box<PublicKey>>,
    /// Unix seconds from which it is valid.
    pub not_before: u64,
    /// Unix seconds at which it expires.
    pub expires: u64,
}
/// A signed `DomainControl`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlCertificate {
    /// The signed statement.
    pub control: DomainControl,
    /// Its signer, a tenant or an authority.
    pub issuer: PublicKey,
    signature: Signature,
}
impl ControlCertificate {
    /// Signs a control statement.
    pub fn sign(control: DomainControl, issuer: &PrivateKey) -> Result<Self, CoordinationError> {
        Ok(Self {
            signature: issuer.sign(&bytes(CONTROL_LABEL, &control)?),
            control,
            issuer: issuer.public_key(),
        })
    }
    /// Checks the signer is the database's authority and the certificate is valid at `time`.
    pub fn verify(&self, genesis: &Genesis, time: u64) -> Result<(), CoordinationError> {
        if !genesis
            .authorities
            .iter()
            .any(|a| a.scope == self.control.scope && a.issuer == self.issuer)
            || self.control.scope.mesh != genesis.mesh
        {
            return Err(CoordinationError::Denied(Refusal::NotDomainAuthority));
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
            return Err(CoordinationError::Denied(Refusal::InvalidControl));
        }
        Ok(())
    }
    /// The digest receipts name this certificate by.
    pub fn reference(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment("rocky.control-reference.v1", &encode(self)?))
    }
}
/// Where clients reach one voter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClientEndpoint {
    /// The voter's id.
    pub voter: u64,
    /// The transport kind, such as `iroh` or `tcp`.
    pub kind: String,
    /// The route on that transport.
    pub address: String,
}
/// Terms on which a coordinator accepts registrations from any tenant.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicService {
    /// Where clients reach the voters.
    pub endpoints: Vec<ClientEndpoint>,
    /// The provider key whose admission tenants need, or `None` for open registration.
    pub admission: Option<PublicKey>,
    /// Storage and retry limits.
    pub limits: crate::Limits,
    /// The most tenants the service holds.
    pub tenants: u32,
    /// Registrations one tenant may make per day.
    pub registrations_per_day: u32,
    /// Client lanes one tenant may hold, at most half of `CLIENT_LANES`.
    pub tenant_lanes: u32,
}
/// Client requests a voter serves at once.
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
/// A coordinator deployment's founding configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    /// The mesh its databases belong to.
    pub mesh: MeshId,
    /// Its deployment id; all zeros is refused.
    pub configuration: Configuration,
    /// Three to seven voters.
    pub voters: Vec<Voter>,
    /// The voters that start the Raft group, at least three.
    pub initial_voters: BTreeSet<u64>,
    /// The key that may change the voters.
    pub membership_authority: PublicKey,
    /// Keys that control databases directly.
    pub authorities: Vec<Authority>,
    /// The longest certificate lifetime in seconds, at most 366 days.
    pub max_control_lifetime: u64,
    /// Terms for public registrations, or `None` for authorities only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<PublicService>,
}
/// How far a certificate's start is backdated to absorb clock skew.
pub const CONTROL_BACKDATE_SECONDS: u64 = 60;
/// The shortest certificate lifetime in seconds.
pub const MIN_CONTROL_LIFETIME: u64 = 60;
impl Genesis {
    /// The validity window for a certificate of `lifetime` seconds made at `time`.
    pub fn control_window(
        &self,
        time: u64,
        lifetime: u64,
    ) -> Result<(u64, u64), CoordinationError> {
        if !(MIN_CONTROL_LIFETIME..=self.max_control_lifetime).contains(&lifetime) {
            return Err(CoordinationError::Configuration);
        }
        let not_before = time.saturating_sub(CONTROL_BACKDATE_SECONDS);
        Ok((not_before, not_before.saturating_add(lifetime)))
    }
}
/// A genesis signed by the deployment's root key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CertifiedGenesis {
    /// The signed genesis.
    pub genesis: Genesis,
    /// The root key that signed it.
    pub root: PublicKey,
    signature: Signature,
}
impl CertifiedGenesis {
    /// Signs and checks a genesis.
    pub fn sign(genesis: Genesis, root: &PrivateKey) -> Result<Self, CoordinationError> {
        let result = Self {
            signature: root.sign(&bytes("rocky.genesis.v2", &genesis)?),
            genesis,
            root: root.public_key(),
        };
        result.verify(root.public_key(), result.genesis.mesh)?;
        Ok(result)
    }
    /// Checks the signature, the mesh and that the configuration is well formed.
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
            return Err(CoordinationError::Denied(Refusal::Signature));
        }
        if let Some(public) = &g.public {
            public.validate(&g.voters)?;
        }
        Ok(())
    }
    /// The public service's limits, or the defaults.
    pub fn limits(&self) -> crate::Limits {
        self.genesis
            .public
            .as_ref()
            .map_or_else(crate::Limits::default, |public| public.limits)
    }
    /// The canonical encoding.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CoordinationError> {
        encode(self)
    }
    /// Decodes the canonical encoding, refusing any other.
    pub fn from_bytes(data: &[u8]) -> Result<Self, CoordinationError> {
        decode(data)
    }
    /// The digest every request names, binding it to this deployment.
    pub fn id(&self) -> Result<Commitment, CoordinationError> {
        Ok(commitment("rocky.genesis-id.v1", &encode(self)?))
    }
}

/// Encodes a value in the coordinator's canonical CBOR.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CoordinationError> {
    p2panda_core::cbor::encode_cbor(value).map_err(|_| CoordinationError::Corrupt)
}
/// Decodes canonical CBOR, refusing a non-canonical encoding.
pub fn decode<T: Serialize + DeserializeOwned>(data: &[u8]) -> Result<T, CoordinationError> {
    if data.len() > MAX_STORE {
        return Err(CoordinationError::Denied(Refusal::Oversized));
    }
    let value = p2panda_core::cbor::decode_cbor(data).map_err(|_| CoordinationError::Corrupt)?;
    if encode(&value)? != data {
        return Err(CoordinationError::Corrupt);
    }
    Ok(value)
}
/// The bytes a signature covers: a domain label and a value.
pub fn bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, CoordinationError> {
    encode(&(label, value))
}
/// The current Unix time in seconds.
pub fn now() -> Result<u64, CoordinationError> {
    let time = rocky_platform::time::SystemTime::now()
        .duration_since(rocky_platform::time::UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| CoordinationError::Corrupt)?;
    Ok(time)
}
/// What a client needs to reach and trust a coordinator, as `scripts/coordinator-local` prints it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustBundle {
    /// The bundle format, `BUNDLE_FORMAT`.
    pub format: u32,
    /// The deployment's root key.
    pub root: PublicKey,
    /// The deployment's mesh.
    pub mesh: MeshId,
    /// The certified genesis in hexadecimal.
    pub genesis: String,
}
/// The bundle format this version reads.
pub const BUNDLE_FORMAT: u32 = 2;
impl TrustBundle {
    /// The bundle for a certified genesis.
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
    /// The verified genesis, refusing one without a public service.
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
    /// The bundle as JSON, for configuration files.
    pub fn to_json(&self) -> Result<String, CoordinationError> {
        serde_json::to_string_pretty(self).map_err(|_| CoordinationError::Corrupt)
    }
    /// Reads a bundle from JSON.
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
