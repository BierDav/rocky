use crate::{
    Attestation, CredentialChain, Error, PublicKey, Purpose, SignedCapability, Signer,
    decode_bounded, denied, encode, signing_bytes,
};
use rocky_core::{AppId, Commitment};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

pub const DAY: u64 = 24 * 60 * 60;
pub const MAX_CHAIN_LIFETIME: u64 = 366 * DAY;
pub const LIFETIME_SKEW: u64 = DAY;
pub const MAX_LIVE_REVOCATIONS: usize = 1024;
pub const MAX_REVOCATION_BYTES: usize = 8 * 1024;
pub const SIGNATURE_OVERHEAD: usize = 1024;
pub const MAX_REVOCATION_SET_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RESET_SET_BYTES: usize = 1024 * 1024;
pub const REVOCATION_SET_FULL: &str = "revocation set full: 1,024 live statements";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationMode {
    Lost,
    Compromised,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    pub app: AppId,
    pub issuer: PublicKey,
    pub time: u64,
    pub mode: RevocationMode,
    pub nodes: BTreeSet<crate::PeerId>,
    pub identities: BTreeSet<PublicKey>,
    pub capabilities: BTreeSet<Commitment>,
    pub cut: Vec<Cut>,
    pub recovery: Option<RootDecision>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootDecision {
    Grants(RootRecovery),
    Reset(Box<crate::Reset>),
}
impl RootDecision {
    pub fn grants(&self) -> Option<RootRecovery> {
        match self {
            Self::Grants(recovery) => Some(*recovery),
            Self::Reset(_) => None,
        }
    }
    pub fn reset(&self) -> Option<&crate::Reset> {
        match self {
            Self::Reset(reset) => Some(reset),
            Self::Grants(_) => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootRecovery {
    pub since: u64,
    pub keep: PublicKey,
}
impl RootRecovery {
    pub fn ends(&self, since: u64, identity: &PublicKey) -> bool {
        (since, *identity) < (self.since, self.keep)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Cut {
    pub identity: PublicKey,
    pub lane: PublicKey,
    pub statements: Vec<Head>,
    pub acknowledgments: Option<Head>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Head {
    pub sequence: u64,
    pub id: Commitment,
}
pub const MAX_CUT_LANES: usize = 32;
pub const MAX_CUT_HEADS: usize = 64;
impl Revocation {
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.revocation.id.v6",
            &encode(self)?,
        ))
    }
    pub fn expiry_day(&self) -> u64 {
        self.time
            .saturating_add(MAX_CHAIN_LIFETIME + LIFETIME_SKEW)
            .div_ceil(DAY)
    }
    pub fn live(&self, time: u64) -> bool {
        time / DAY < self.expiry_day()
    }
    pub fn subjects(&self) -> impl Iterator<Item = &PublicKey> {
        self.identities.iter()
    }
    fn empty(&self) -> bool {
        self.nodes.is_empty()
            && self.identities.is_empty()
            && self.capabilities.is_empty()
            && self.recovery.is_none()
    }
    pub fn grants(&self) -> Option<RootRecovery> {
        self.recovery.as_ref().and_then(RootDecision::grants)
    }
    pub fn reset(&self) -> Option<&crate::Reset> {
        self.recovery.as_ref().and_then(RootDecision::reset)
    }
    fn valid_recovery(&self, root: bool) -> bool {
        match &self.recovery {
            None => true,
            Some(RootDecision::Grants(_)) => root && self.mode == RevocationMode::Compromised,
            Some(RootDecision::Reset(reset)) => {
                root && self.mode == RevocationMode::Lost
                    && self.nodes.is_empty()
                    && self.identities.is_empty()
                    && self.capabilities.is_empty()
                    && reset.validate().is_ok()
            }
        }
    }
    fn max_bytes(&self) -> usize {
        if self.reset().is_some() {
            crate::MAX_RESET_BYTES
        } else {
            MAX_REVOCATION_BYTES
        }
    }
    fn valid_cut(&self, authorized: bool) -> bool {
        (authorized || self.cut.is_empty())
            && self.cut.len() <= MAX_CUT_LANES
            && self
                .cut
                .windows(2)
                .all(|pair| (pair[0].identity, pair[0].lane) < (pair[1].identity, pair[1].lane))
            && self
                .cut
                .iter()
                .map(|cut| cut.statements.len())
                .sum::<usize>()
                <= MAX_CUT_HEADS
            && self.cut.iter().all(|cut| {
                (self.identities.contains(&cut.identity)
                    || self.nodes.contains(&crate::PeerId::of(cut.lane))
                    || self
                        .grants()
                        .is_some_and(|recovery| recovery.keep != cut.identity)
                    || self
                        .reset()
                        .is_some_and(|reset| !reset.keep.contains(&crate::PeerId::of(cut.lane))))
                    && cut.statements.windows(2).all(|pair| pair[0] < pair[1])
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRevocation {
    pub statement: Revocation,
    pub chain: CredentialChain,
    attestation: Attestation,
    database: Option<crate::DatabaseContext>,
}
const SIGNED: &str = "rocky.revocation.v9";
impl SignedRevocation {
    pub fn sign<'s>(
        statement: Revocation,
        signer: impl Into<Signer<'s>>,
        chain: &CredentialChain,
    ) -> Result<Self, Error> {
        let database = if chain.0.is_empty() {
            None
        } else {
            Some(chain.database_context(chain.0[0].issuer)?)
        };
        Self::sign_scoped(statement, signer.into(), chain, database)
    }
    pub fn sign_in<'s>(
        statement: Revocation,
        signer: impl Into<Signer<'s>>,
        chain: &CredentialChain,
        database: crate::DatabaseContext,
    ) -> Result<Self, Error> {
        if !chain.0.is_empty() && chain.database_context(database.publisher)? != database {
            return Err(denied("revocation belongs to another database"));
        }
        Self::sign_scoped(statement, signer.into(), chain, Some(database))
    }
    pub fn database(&self) -> Option<crate::DatabaseContext> {
        self.database
    }
    fn sign_scoped(
        statement: Revocation,
        signer: Signer<'_>,
        chain: &CredentialChain,
        database: Option<crate::DatabaseContext>,
    ) -> Result<Self, Error> {
        if database.is_some_and(|context| {
            context.scope.app != statement.app
                || (chain.0.is_empty() && context.publisher != statement.issuer)
        }) {
            return Err(denied("revocation belongs to another application"));
        }
        if statement.issuer != signer.principal()?
            || (chain.0.is_empty() == signer.signing_node().is_some()
                && !(signer.signing_node().is_none() && Self::unlink_shape(&statement, chain)))
            || (!chain.0.is_empty()
                && !statement.nodes.is_empty()
                && (signer.signing_node().is_some() || !Self::unlink_shape(&statement, chain)))
            || statement.empty()
            || !statement.valid_cut(chain.0.is_empty() || Self::unlink_shape(&statement, chain))
            || !statement.valid_recovery(chain.0.is_empty())
            || (statement.mode == RevocationMode::Compromised && !chain.0.is_empty())
        {
            return Err(denied("invalid revocation"));
        }
        if let (Some(database), Signer::Node { certificate, .. }) = (database, &signer)
            && certificate.capability.extended()?.database != Some(database)
        {
            return Err(denied("revocation signer belongs to another database"));
        }
        let signed = Self {
            attestation: signer.attest(
                Purpose::Revocation,
                &signing_bytes(SIGNED, &(database, &statement))?,
            )?,
            database,
            chain: chain.clone(),
            statement,
        };
        if encode(&signed)?.len() > signed.statement.max_bytes() {
            return Err(denied("revocation exceeds its size limit"));
        }
        Ok(signed)
    }
    fn unlink_shape(statement: &Revocation, chain: &CredentialChain) -> bool {
        chain.certificate().is_some_and(|certificate| {
            certificate.issuer == statement.issuer
                && certificate.capability.not_before <= statement.time
                && statement.nodes == [crate::PeerId::of(certificate.capability.subject)].into()
                && statement.identities.is_empty()
                && statement.capabilities.is_empty()
                && statement.cut.iter().all(|cut| {
                    cut.identity == statement.issuer && cut.lane == certificate.capability.subject
                })
                && statement.recovery.is_none()
        })
    }
    fn identity_unlink(&self) -> bool {
        self.attestation.certificate().is_none() && Self::unlink_shape(&self.statement, &self.chain)
    }
    pub fn verify_historic(&self, publisher: PublicKey, app: AppId) -> Result<Commitment, Error> {
        let statement = &self.statement;
        if let Some(database) = self.database {
            if database.publisher != publisher || database.scope.app != app {
                return Err(denied("revocation belongs to another database"));
            }
            if !self.chain.0.is_empty() && self.chain.database_context(publisher)? != database {
                return Err(denied("revocation belongs to another database"));
            }
        }
        if let (Some(database), Some(certificate)) = (self.database, self.attestation.certificate())
            && certificate.capability.extended()?.database != Some(database)
        {
            return Err(denied("revocation signer belongs to another database"));
        }
        let time = statement.time;
        let unlink = self.identity_unlink();
        let signer = if self.chain.0.is_empty() {
            Ok(publisher)
        } else if unlink {
            let certificate = self
                .chain
                .certificate()
                .ok_or_else(|| denied("missing unlink certificate"))?;
            self.chain.verify_node_alias(
                publisher,
                crate::PeerId::of(certificate.capability.subject),
                certificate.capability.subject,
            )?;
            Ok(certificate.issuer)
        } else if self
            .chain
            .0
            .iter()
            .all(|link| link.capability.not_before <= time && time < link.capability.expires)
        {
            self.chain.issuer(publisher, None)
        } else {
            Err(denied(
                "the issuer chain was not valid at the statement's time",
            ))
        };
        if (!self.chain.0.is_empty() && !unlink && !statement.nodes.is_empty())
            || statement.app != app
            || statement.empty()
            || !statement.valid_cut(self.chain.0.is_empty() || unlink)
            || !statement.valid_recovery(self.chain.0.is_empty())
            || (statement.mode == RevocationMode::Compromised && !self.root())
            || statement
                .grants()
                .is_some_and(|recovery| recovery.keep == publisher)
            || statement.identities.contains(&publisher)
            || encode(self)?.len() > statement.max_bytes()
            || signer.ok() != Some(statement.issuer)
            || (!unlink && self.chain.0.is_empty() != self.attestation.certificate().is_none())
            || self
                .attestation
                .certificate()
                .is_some_and(|certificate| certificate.capability.not_before > time)
            || self
                .attestation
                .verify(
                    statement.issuer,
                    &signing_bytes(SIGNED, &(self.database, statement))?,
                )
                .is_err()
        {
            return Err(denied("invalid revocation signature"));
        }
        statement.id()
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        self.statement.id()
    }
    pub fn root(&self) -> bool {
        self.chain.0.is_empty()
    }
    pub fn live(&self, time: u64) -> bool {
        self.root() || self.identity_unlink() || self.statement.live(time)
    }
    pub fn expiry_day(&self) -> u64 {
        if self.root() || self.identity_unlink() {
            u64::MAX
        } else {
            self.statement.expiry_day()
        }
    }
    pub fn ends_at(&self, link: &SignedCapability, protected: bool) -> Result<bool, Error> {
        if protected && !self.root() && !self.identity_unlink() {
            return Ok(false);
        }
        let subject = &link.capability.subject;
        let extended = link.capability.extended()?;
        let capabilities = &self.statement.capabilities;
        let recovery = self.statement.grants().filter(|_| self.root());
        let reset = self.statement.reset().filter(|_| self.root());
        Ok(self.statement.subjects().any(|key| key == subject)
            || (self.root()
                && extended.origin.is_some_and(|origin| {
                    (self.statement.mode == RevocationMode::Compromised
                        && self.statement.identities.contains(&origin.identity))
                        || recovery.is_some_and(|r| r.ends(origin.since, &origin.identity))
                }))
            || (extended.trusted.is_some()
                && recovery.is_some_and(|r| r.ends(link.capability.not_before, subject)))
            || extended.node.is_some_and(|node| {
                (self.statement.nodes.contains(&node)
                    && (!self.identity_unlink() || link.issuer == self.statement.issuer))
                    || reset.is_some_and(|reset| !reset.keep.contains(&node))
            })
            || capabilities.contains(&link.id()?)
            || extended
                .claims
                .is_some_and(|right| capabilities.contains(&right.grant)))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, crate::MAX_RESET_BYTES)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "ReplicatedAuthority", try_from = "ReplicatedAuthority")]
pub struct RevocationSet {
    database: Option<Box<crate::DatabaseContext>>,
    certificates: BTreeMap<Commitment, SignedCapability>,
    admissions: BTreeMap<Commitment, crate::SignedResetAdmission>,
    statements: BTreeMap<Commitment, SignedRevocation>,
    subjects: BTreeSet<PublicKey>,
    references: BTreeSet<crate::PeerId>,
    capabilities: BTreeSet<Commitment>,
    root: Ended,
    compromised: BTreeSet<PublicKey>,
    cuts: BTreeMap<(PublicKey, PublicKey), Heads>,
    recovery: Option<RootRecovery>,
    derived: Derived,
    genesis: Pinned,
}
#[derive(Clone, Debug, Default)]
struct Pinned(Option<std::sync::Arc<crate::personal::Genesis>>);
impl PartialEq for Pinned {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Pinned {}
#[derive(Clone, Default)]
struct Derived {
    fork_ended: OnceLock<BTreeSet<(PublicKey, crate::PeerId)>>,
    fingerprint: OnceLock<Commitment>,
    resets: OnceLock<crate::reset::Resets>,
    unfenced: OnceLock<Box<RevocationSet>>,
}
impl PartialEq for Derived {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Derived {}
impl std::fmt::Debug for Derived {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Derived")
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Heads {
    pub statements: BTreeSet<Head>,
    pub acknowledgments: BTreeSet<Head>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Ended {
    subjects: BTreeSet<PublicKey>,
    references: BTreeSet<crate::PeerId>,
    capabilities: BTreeSet<Commitment>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplicatedAuthority {
    format: u32,
    database: Option<crate::DatabaseContext>,
    statements: Vec<SignedRevocation>,
    certificates: Vec<SignedCapability>,
    admissions: Vec<crate::SignedResetAdmission>,
}
const AUTHORITY_FORMAT: u32 = 6;
impl From<RevocationSet> for ReplicatedAuthority {
    fn from(set: RevocationSet) -> Self {
        Self {
            format: AUTHORITY_FORMAT,
            database: set.database.map(|database| *database),
            statements: set.statements.into_values().collect(),
            certificates: set.certificates.into_values().collect(),
            admissions: set.admissions.into_values().collect(),
        }
    }
}
impl TryFrom<ReplicatedAuthority> for RevocationSet {
    type Error = Error;
    fn try_from(value: ReplicatedAuthority) -> Result<Self, Error> {
        if value.format != AUTHORITY_FORMAT {
            return Err(denied("unsupported replicated authority format"));
        }
        let mut set = Self {
            database: value.database.map(Box::new),
            ..Self::default()
        };
        for statement in value.statements {
            set.insert(statement)?;
        }
        let mut named = BTreeSet::new();
        for certificate in value.certificates {
            if !named.insert(certificate.id()?) {
                return Err(denied("duplicate membership certificate"));
            }
            set.add_link(certificate)?;
        }
        set.prune_tombstoned();
        for admission in value.admissions {
            if set.add_admission(admission)?.is_none() {
                return Err(denied("duplicate admission"));
            }
        }
        if !set.certificates.is_empty() || !set.admissions.is_empty() {
            set.check_size()?;
        }
        Ok(set)
    }
}
impl From<RevocationSet> for Vec<SignedRevocation> {
    fn from(set: RevocationSet) -> Self {
        set.statements.into_values().collect()
    }
}
impl TryFrom<Vec<SignedRevocation>> for RevocationSet {
    type Error = Error;
    fn try_from(statements: Vec<SignedRevocation>) -> Result<Self, Error> {
        let mut set = Self::default();
        for signed in statements {
            if !set.insert(signed)? {
                return Err(denied("duplicate revocation"));
            }
        }
        if !set.certificates.is_empty() {
            set.check_size()?;
        }
        Ok(set)
    }
}
impl RevocationSet {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn new_in(database: crate::DatabaseContext) -> Self {
        Self {
            database: Some(Box::new(database)),
            ..Self::default()
        }
    }
    pub fn merge_in(
        &mut self,
        other: &Self,
        database: crate::DatabaseContext,
        time: u64,
    ) -> Result<Vec<SignedRevocation>, Error> {
        self.verify_in(database)?;
        other.verify_in(database)?;
        let mut next = self.clone();
        next.bind(Some(database))?;
        let added = next.merge(other, database.publisher, database.scope.app, time)?;
        *self = next;
        Ok(added)
    }
    pub fn verify_in(&self, database: crate::DatabaseContext) -> Result<(), Error> {
        if self
            .database
            .as_deref()
            .is_some_and(|held| *held != database)
        {
            return Err(denied("authority set belongs to another database"));
        }
        self.verify(database.publisher, database.scope.app)?;
        if self.certificates.values().any(|certificate| {
            certificate
                .capability
                .extended()
                .ok()
                .and_then(|extended| extended.database)
                != Some(database)
        }) || self
            .statements
            .values()
            .any(|statement| statement.database != Some(database))
            || self
                .admissions
                .values()
                .any(|signed| signed.admission.database != database)
        {
            return Err(denied("authority evidence belongs to another database"));
        }
        Ok(())
    }
    fn bind(&mut self, database: Option<crate::DatabaseContext>) -> Result<(), Error> {
        if let Some(database) = database {
            self.derived = Derived::default();
            if self
                .database
                .as_deref()
                .is_some_and(|held| *held != database)
            {
                return Err(denied("authority evidence belongs to another database"));
            }
            self.database = Some(Box::new(database));
        }
        Ok(())
    }
    pub fn pin(
        &mut self,
        genesis: &crate::SignedManifest,
        publisher: PublicKey,
    ) -> Result<(), Error> {
        let genesis = crate::personal::Genesis::of(genesis, publisher)?;
        if let Some(held) = &self.genesis.0 {
            return if **held == genesis {
                Ok(())
            } else {
                Err(denied("authority set is pinned to another genesis"))
            };
        }
        for certificate in self.certificates.values() {
            genesis.check_node(certificate.issuer, &certificate.capability)?;
        }
        self.genesis = Pinned(Some(std::sync::Arc::new(genesis)));
        self.derived = Derived::default();
        Ok(())
    }
    pub fn pinned_only(&self) -> Self {
        Self {
            genesis: self.genesis.clone(),
            ..Self::default()
        }
    }
    pub(crate) fn genesis(&self) -> Option<&crate::personal::Genesis> {
        self.genesis.0.as_deref()
    }
    pub fn link(&mut self, certificate: SignedCapability) -> Result<bool, Error> {
        if crate::PersonalLink::of(&certificate)?
            .is_some_and(|link| self.tombstones(link.owner, link.node))
        {
            return Ok(false);
        }
        let database = self.database.clone();
        let Some(id) = self.add_link(certificate)? else {
            return Ok(false);
        };
        if let Err(error) = self.check_size() {
            self.certificates.remove(&id);
            self.database = database;
            self.derived = Derived::default();
            return Err(error);
        }
        Ok(true)
    }
    fn add_link(&mut self, certificate: SignedCapability) -> Result<Option<Commitment>, Error> {
        certificate.certifies(
            certificate.capability.subject,
            certificate.capability.not_before,
        )?;
        crate::membership::node_database(&certificate)?;
        if let Some(genesis) = self.genesis() {
            genesis.check_node(certificate.issuer, &certificate.capability)?;
        }
        let id = certificate.id()?;
        if self.certificates.contains_key(&id) {
            return Ok(None);
        }
        let database = certificate
            .capability
            .extended()?
            .database
            .ok_or_else(|| denied("membership certificate has no database"))?;
        self.bind(Some(database))?;
        self.derived = Derived::default();
        self.certificates.insert(id, certificate);
        Ok(Some(id))
    }
    pub fn certificates(&self) -> impl Iterator<Item = &SignedCapability> {
        self.certificates.values()
    }
    pub fn admit(&mut self, admission: crate::SignedResetAdmission) -> Result<bool, Error> {
        let database = self.database.clone();
        let Some(id) = self.add_admission(admission)? else {
            return Ok(false);
        };
        if let Err(error) = self.check_size() {
            self.admissions.remove(&id);
            self.database = database;
            self.derived = Derived::default();
            return Err(error);
        }
        Ok(true)
    }
    fn add_admission(
        &mut self,
        admission: crate::SignedResetAdmission,
    ) -> Result<Option<Commitment>, Error> {
        admission.verify()?;
        let id = admission.id()?;
        if self.admissions.contains_key(&id) {
            return Ok(None);
        }
        self.bind(Some(admission.admission.database))?;
        self.derived = Derived::default();
        self.admissions.insert(id, admission);
        Ok(Some(id))
    }
    pub fn admissions(&self) -> impl Iterator<Item = &crate::SignedResetAdmission> {
        self.admissions.values()
    }
    pub fn membership(&self) -> Result<crate::MembershipSet, Error> {
        let mut membership = crate::MembershipSet::new();
        for certificate in self.certificates.values() {
            membership.record(certificate)?;
        }
        Ok(membership)
    }
    pub(crate) fn root_names(&self, node: PublicKey) -> bool {
        self.root.subjects.contains(&node)
            || self.root.references.contains(&crate::PeerId::of(node))
    }
    fn prune_tombstoned(&mut self) {
        let carried: BTreeSet<Commitment> = self
            .statements
            .values()
            .filter_map(|signed| signed.chain.certificate())
            .filter_map(|certificate| certificate.id().ok())
            .collect();
        let ended: Vec<Commitment> = self
            .certificates
            .iter()
            .filter(|(id, certificate)| {
                !carried.contains(*id)
                    && certificate
                        .capability
                        .extended()
                        .is_ok_and(|extended| extended.personal.is_some())
                    && self.tombstones(certificate.issuer, certificate.capability.subject)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ended {
            self.certificates.remove(&id);
        }
    }
    fn fork_ends(&self, identity: PublicKey, node: crate::PeerId) -> bool {
        self.fork_ended().contains(&(identity, node))
    }
    fn fork_ended(&self) -> &BTreeSet<(PublicKey, crate::PeerId)> {
        self.derived
            .fork_ended
            .get_or_init(|| self.compute_fork_ended())
    }
    fn compute_fork_ended(&self) -> BTreeSet<(PublicKey, crate::PeerId)> {
        #[cfg(test)]
        tests::count_derivation();
        let Ok(membership) = self.membership() else {
            return BTreeSet::new();
        };
        let void = membership.void_certificates();
        let mut counts = BTreeMap::<PublicKey, BTreeSet<crate::PeerId>>::new();
        let mut valid = BTreeSet::new();
        let mut links: Vec<_> = self
            .certificates
            .values()
            .filter_map(|certificate| crate::MembershipLink::from_certificate(certificate).ok())
            .collect();
        links.sort_by_key(|link| (link.identity, link.sequence, link.certificate));
        for link in &links {
            if void.contains(&link.certificate) || self.identity_unlinks(link.identity, link.node) {
                continue;
            }
            let nodes = counts.entry(link.identity).or_default();
            if nodes.contains(&link.node) || nodes.len() < crate::MAX_MEMBERSHIP_NODES {
                nodes.insert(link.node);
                valid.insert(link.certificate);
            }
        }
        links
            .iter()
            .filter(|link| !valid.contains(&link.certificate))
            .map(|link| (link.identity, link.node))
            .collect()
    }
    fn insert(&mut self, signed: SignedRevocation) -> Result<bool, Error> {
        self.bind(signed.database)?;
        let id = signed.id()?;
        if self.statements.contains_key(&id) {
            return Ok(false);
        }
        self.derived = Derived::default();
        self.subjects.extend(signed.statement.subjects().copied());
        if signed.identity_unlink() {
            if let Some(certificate) = signed.chain.certificate() {
                self.add_link(certificate.clone())?;
            }
        } else {
            self.references
                .extend(signed.statement.nodes.iter().copied());
        }
        self.capabilities
            .extend(signed.statement.capabilities.iter().copied());
        if signed.root() {
            if signed.statement.mode == RevocationMode::Compromised {
                self.compromised
                    .extend(signed.statement.subjects().copied());
            }
            let root = &mut self.root;
            root.subjects.extend(signed.statement.subjects().copied());
            root.references
                .extend(signed.statement.nodes.iter().copied());
            root.capabilities
                .extend(signed.statement.capabilities.iter().copied());
            self.recovery = self.recovery.max(signed.statement.grants());
        }
        let implicit = signed
            .chain
            .certificate()
            .filter(|_| signed.identity_unlink() && signed.statement.cut.is_empty())
            .map(|certificate| Cut {
                identity: signed.statement.issuer,
                lane: certificate.capability.subject,
                statements: Vec::new(),
                acknowledgments: None,
            });
        let cuts = implicit
            .as_ref()
            .map_or(signed.statement.cut.as_slice(), std::slice::from_ref);
        for cut in cuts {
            let key = (cut.identity, cut.lane);
            let root_cut = self.statements.values().any(|known| {
                known.root()
                    && known
                        .statement
                        .cut
                        .iter()
                        .any(|cut| (cut.identity, cut.lane) == key)
            });
            if !signed.root() {
                let earlier = self
                    .statements
                    .iter()
                    .filter(|(_, known)| {
                        known.identity_unlink()
                            && known.statement.issuer == cut.identity
                            && known.statement.nodes.contains(&crate::PeerId::of(cut.lane))
                    })
                    .map(|(id, known)| (known.statement.time, *id))
                    .min();
                let lane_already_fixed = root_cut
                    || earlier.is_some_and(|earlier| earlier <= (signed.statement.time, id));
                if lane_already_fixed {
                    continue;
                }
                self.cuts.remove(&key);
            }
            let root_supersedes_unlink = signed.root() && !root_cut;
            if root_supersedes_unlink {
                self.cuts.remove(&key);
            }
            let heads = self.cuts.entry(key).or_default();
            heads.statements.extend(cut.statements.iter().copied());
            heads.acknowledgments.extend(cut.acknowledgments);
        }
        let permanent = signed.root() || signed.identity_unlink();
        self.statements.insert(id, signed);
        if permanent {
            self.prune_tombstoned();
        }
        Ok(true)
    }
    pub fn verify(&self, publisher: PublicKey, app: AppId) -> Result<(), Error> {
        for signed in self.statements.values() {
            signed.verify_historic(publisher, app)?;
        }
        for certificate in self.certificates.values() {
            certificate.certifies(
                certificate.capability.subject,
                certificate.capability.not_before,
            )?;
            let database = crate::membership::node_database(certificate)?;
            if database.publisher != publisher || database.scope.app != app {
                return Err(denied("membership certificate belongs to another database"));
            }
            if let Some(genesis) = self.genesis() {
                genesis.check_node(certificate.issuer, &certificate.capability)?;
            }
        }
        for signed in self.admissions.values() {
            signed.verify()?;
            let database = signed.admission.database;
            if database.publisher != publisher || database.scope.app != app {
                return Err(denied("admission belongs to another database"));
            }
        }
        self.check_size()
    }
    fn check_size(&self) -> Result<(), Error> {
        let mut resets = 0;
        for signed in self.statements.values() {
            if signed.statement.reset().is_some() {
                resets += encode(signed)?.len();
            }
        }
        if resets > MAX_RESET_SET_BYTES {
            return Err(denied(
                "owner resets exceed their share of the revocation set",
            ));
        }
        if encode(self)?.len() > MAX_REVOCATION_SET_BYTES {
            return Err(denied("revocation set exceeds its size limit"));
        }
        Ok(())
    }
    pub fn merge(
        &mut self,
        other: &RevocationSet,
        publisher: PublicKey,
        app: AppId,
        time: u64,
    ) -> Result<Vec<SignedRevocation>, Error> {
        other.verify(publisher, app)?;
        let mut next = self.clone();
        next.bind(other.database.as_deref().copied())?;
        let mut added = Vec::new();
        for (id, signed) in &other.statements {
            if self.statements.contains_key(id) {
                continue;
            }
            signed.verify_historic(publisher, app)?;
            if signed.live(time) && next.insert(signed.clone())? {
                added.push(signed.clone());
            }
        }
        for certificate in other.certificates.values() {
            let replayed_after_tombstone = crate::PersonalLink::of(certificate)?
                .is_some_and(|link| next.tombstones(link.owner, link.node));
            if replayed_after_tombstone {
                continue;
            }
            next.add_link(certificate.clone())?;
        }
        for admission in other.admissions.values() {
            next.add_admission(admission.clone())?;
        }
        next = next.pruned(time);
        next.check_size()?;
        *self = next;
        Ok(added)
    }
    pub fn pruned(&self, time: u64) -> Self {
        let mut set = Self {
            database: self.database.clone(),
            certificates: self.certificates.clone(),
            admissions: self.admissions.clone(),
            genesis: self.genesis.clone(),
            ..Self::default()
        };
        for signed in self.statements.values() {
            if signed.live(time) {
                set.reinsert(signed.clone());
            }
        }
        set
    }
    pub fn statements(&self) -> impl Iterator<Item = &SignedRevocation> {
        self.statements.values()
    }
    pub fn get(&self, id: &Commitment) -> Option<&SignedRevocation> {
        self.statements.get(id)
    }
    pub fn len(&self) -> usize {
        self.statements.len()
    }
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty() && self.certificates.is_empty() && self.admissions.is_empty()
    }
    pub fn has_revocations(&self) -> bool {
        if !self.statements.is_empty() {
            return true;
        }
        let forked = self.fork_ended();
        self.certificates.values().any(|certificate| {
            forked.contains(&(
                certificate.issuer,
                crate::PeerId::of(certificate.capability.subject),
            ))
        })
    }
    pub fn live(&self, time: u64) -> usize {
        self.statements
            .values()
            .filter(|signed| signed.live(time))
            .count()
    }
    pub fn digest(&self, time: u64) -> Result<Commitment, Error> {
        let live: Vec<&Commitment> = self
            .statements
            .iter()
            .filter(|(_, signed)| signed.live(time))
            .map(|(id, _)| id)
            .collect();
        let forked = self.fork_ended();
        Ok(rocky_core::commitment(
            "rocky.revocation-set.v1",
            &encode(&(
                live,
                self.certificates
                    .values()
                    .filter(|certificate| {
                        forked.contains(&(
                            certificate.issuer,
                            crate::PeerId::of(certificate.capability.subject),
                        ))
                    })
                    .map(SignedCapability::id)
                    .collect::<Result<Vec<_>, _>>()?,
            ))?,
        ))
    }
    pub fn exchange_digest(&self, time: u64) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.authority-exchange.v3",
            &encode(&(
                self.digest(time)?,
                self.certificates.keys().collect::<Vec<_>>(),
                self.admissions.keys().collect::<Vec<_>>(),
            ))?,
        ))
    }
    pub fn fingerprint(&self) -> Result<Commitment, Error> {
        if let Some(fingerprint) = self.derived.fingerprint.get() {
            return Ok(*fingerprint);
        }
        #[cfg(test)]
        tests::count_derivation();
        let fingerprint = rocky_core::commitment(
            "rocky.revocation-set.pinned.v1",
            &encode(&(self, self.genesis()))?,
        );
        Ok(*self.derived.fingerprint.get_or_init(|| fingerprint))
    }
    pub fn replicated_commitment(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.revocation-set.held.v1",
            &self.to_bytes()?,
        ))
    }
    pub fn cut(&self, identity: &PublicKey, lane: &PublicKey) -> Option<&Heads> {
        self.cuts.get(&(*identity, *lane))
    }
    pub fn cut_lanes(&self) -> impl Iterator<Item = (&(PublicKey, PublicKey), &Heads)> {
        self.cuts.iter()
    }
    pub fn revokes_reference(&self, node: &crate::PeerId) -> bool {
        if self.references.contains(node) || self.reset_ends(node) {
            return true;
        }
        let mut known = self
            .certificates
            .values()
            .filter(|certificate| crate::PeerId::of(certificate.capability.subject) == *node)
            .peekable();
        known.peek().is_some()
            && known.all(|certificate| {
                self.subjects.contains(&certificate.issuer)
                    || self.ends_node_of(certificate.issuer, certificate.capability.subject, false)
            })
    }
    pub fn revokes(&self, key: &PublicKey) -> bool {
        self.subjects.contains(key) || self.revokes_reference(&crate::PeerId::of(*key))
    }
    pub fn revokes_capability(&self, id: &Commitment) -> bool {
        self.capabilities.contains(id)
    }
    pub fn revokes_trusted(&self, identity: &PublicKey) -> bool {
        self.root.subjects.contains(identity)
    }
    pub fn revokes_origin(&self, origin: &PublicKey) -> bool {
        self.compromised.contains(origin)
    }
    pub fn ends_origin(&self, origin: &crate::Origin) -> bool {
        self.revokes_origin(&origin.identity) || self.recovery_ends(origin.since, &origin.identity)
    }
    pub fn ends_trusted(&self, grant: &crate::Capability) -> bool {
        self.revokes_trusted(&grant.subject) || self.recovery_ends(grant.not_before, &grant.subject)
    }
    pub fn recovery_ends(&self, since: u64, identity: &PublicKey) -> bool {
        self.recovery
            .is_some_and(|recovery| recovery.ends(since, identity))
    }
    pub fn recovery(&self) -> Option<&RootRecovery> {
        self.recovery.as_ref()
    }
    pub fn recoveries(&self) -> Vec<RootRecovery> {
        let mut recoveries: Vec<RootRecovery> = self
            .statements
            .values()
            .filter(|signed| signed.root())
            .filter_map(|signed| signed.statement.grants())
            .collect();
        recoveries.sort_unstable();
        recoveries.dedup();
        recoveries
    }
    pub fn ends_at(&self, link: &SignedCapability, protected: bool) -> Result<bool, Error> {
        let extended = link.capability.extended()?;
        let (subjects, references, capabilities) = if protected {
            let root = &self.root;
            (&root.subjects, &root.references, &root.capabilities)
        } else {
            (&self.subjects, &self.references, &self.capabilities)
        };
        Ok(subjects.contains(&link.capability.subject)
            || extended.node.is_some_and(|node| {
                references.contains(&node)
                    || self.identity_unlinks(link.issuer, node)
                    || self.fork_ends(link.issuer, node)
            })
            || capabilities.contains(&link.id()?)
            || extended
                .claims
                .is_some_and(|right| capabilities.contains(&right.grant))
            || (extended.trusted.is_some()
                && self.recovery_ends(link.capability.not_before, &link.capability.subject))
            || extended
                .origin
                .is_some_and(|origin| self.ends_origin(&origin))
            || extended.node.is_some_and(|node| self.reset_ends(&node)))
    }
    pub fn ends_node(&self, node: PublicKey, protected: bool) -> bool {
        let mut known = self
            .certificates
            .values()
            .filter(|certificate| certificate.capability.subject == node)
            .peekable();
        let explicit = if protected {
            &self.root.references
        } else {
            &self.references
        };
        let subjects = if protected {
            &self.root.subjects
        } else {
            &self.subjects
        };
        subjects.contains(&node)
            || explicit.contains(&crate::PeerId::of(node))
            || self.reset_ends(&crate::PeerId::of(node))
            || (known.peek().is_some()
                && known.all(|certificate| self.ends_node_of(certificate.issuer, node, protected)))
    }
    pub fn ends_node_of(&self, identity: PublicKey, node: PublicKey, protected: bool) -> bool {
        let (subjects, references) = if protected {
            (&self.root.subjects, &self.root.references)
        } else {
            (&self.subjects, &self.references)
        };
        subjects.contains(&node)
            || references.contains(&crate::PeerId::of(node))
            || self.reset_ends(&crate::PeerId::of(node))
            || self.identity_unlinks(identity, crate::PeerId::of(node))
            || self.fork_ends(identity, crate::PeerId::of(node))
    }
    pub fn identity_unlinks(&self, identity: PublicKey, node: crate::PeerId) -> bool {
        self.statements.values().any(|signed| {
            signed.identity_unlink()
                && signed.statement.issuer == identity
                && signed.statement.nodes.contains(&node)
        })
    }
    pub fn revoking_holder(
        &self,
        chain: &CredentialChain,
    ) -> Result<Option<&SignedRevocation>, Error> {
        let start = chain.0.len().saturating_sub(2);
        self.first_ending(chain, start)
    }
    pub fn revoking(&self, chain: &CredentialChain) -> Result<Option<&SignedRevocation>, Error> {
        self.first_ending(chain, 0)
    }
    pub fn ends_chain(&self, chain: &CredentialChain) -> Result<bool, Error> {
        for (index, link) in chain.0.iter().enumerate() {
            if self.ends_at(link, chain.protected(index)?)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn reinsert(&mut self, signed: SignedRevocation) {
        let _ = self.insert(signed);
    }
    fn reset_still_ends(
        &self,
        id: &Commitment,
        signed: &SignedRevocation,
        link: &SignedCapability,
    ) -> Result<bool, Error> {
        Ok(signed.statement.reset().is_none()
            || link
                .capability
                .extended()?
                .node
                .is_some_and(|node| self.resets().ending(&node).any(|(ending, _)| ending == id)))
    }
    fn first_ending(
        &self,
        chain: &CredentialChain,
        start: usize,
    ) -> Result<Option<&SignedRevocation>, Error> {
        for (id, signed) in &self.statements {
            for (index, link) in chain.0.iter().enumerate().skip(start) {
                if signed.ends_at(link, chain.protected(index)?)?
                    && self.reset_still_ends(id, signed, link)?
                {
                    return Ok(Some(signed));
                }
            }
        }
        Ok(None)
    }
    fn resets(&self) -> &crate::reset::Resets {
        self.derived.resets.get_or_init(|| {
            let admits_nobody = |signer: &PublicKey| {
                self.subjects.contains(signer)
                    || self.references.contains(&crate::PeerId::of(*signer))
            };
            crate::reset::Resets::new(
                self.statements
                    .iter()
                    .filter(|(_, signed)| signed.root())
                    .filter_map(|(id, signed)| Some((*id, signed.statement.reset()?.clone())))
                    .collect(),
                self.admissions
                    .values()
                    .map(|signed| &signed.admission)
                    .filter(|admission| !admits_nobody(&admission.signer))
                    .map(|admission| {
                        (
                            admission.node,
                            admission.follows.clone(),
                            crate::PeerId::of(admission.signer),
                        )
                    }),
            )
        })
    }
    pub fn reset_ends(&self, node: &crate::PeerId) -> bool {
        !self.resets().is_empty() && self.resets().ends(node)
    }
    pub fn ending_resets(&self, node: &crate::PeerId) -> Vec<Commitment> {
        self.resets().ending(node).map(|(id, _)| *id).collect()
    }
    pub fn reset_standing(
        &self,
        scope: &rocky_core::ResourceScope,
        writer: &PublicKey,
        position: u64,
    ) -> crate::ResetStanding {
        self.resets().standing(scope, writer, position)
    }
    pub fn reset_heads(&self) -> BTreeSet<Commitment> {
        self.resets().heads()
    }
    pub fn reset_kept(&self) -> BTreeSet<crate::PeerId> {
        self.resets().kept()
    }
    pub fn unfenced(&self) -> &RevocationSet {
        if self.resets().is_empty() {
            return self;
        }
        self.derived.unfenced.get_or_init(|| {
            let mut set = Self {
                database: self.database.clone(),
                certificates: self.certificates.clone(),
                genesis: self.genesis.clone(),
                ..Self::default()
            };
            for signed in self.statements.values() {
                if signed.statement.reset().is_none() {
                    set.reinsert(signed.clone());
                }
            }
            Box::new(set)
        })
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_REVOCATION_SET_BYTES)
    }
}

#[cfg(test)]
mod tests;
