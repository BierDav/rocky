//! Self-enrollment: a new node of an identity that already holds grants
//! certifies itself with its identity provider. A node of the same identity
//! hands it what the identity holds; no issuer and no invitation is
//! involved. Both devices confirm the request code before the bundle is sent.
use crate::{
    Failure,
    enrollment::{IDENTITY, Intent, Settings, exchange, now},
    groups::GroupRecord,
    secrets::Vault,
    service::ServiceError,
    transport::{Protocol, Transport},
};
use data_encoding::HEXLOWER;
use rocky_core::{Commitment, DomainId, MeshId};
use rocky_credentials::{IdentityAnswer, IdentityProvider, IdentityRequest, Profile};
use rocky_secure::{CredentialChain, PrivateKey, RevocationSet};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, sync::Arc};

/// Clock difference a request may carry.
const SKEW: u64 = 300;

/// What a node of an identity hands another node of it, sealed to that
/// node alone.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    format: u32,
    mesh: MeshId,
    domain: DomainId,
    record: crate::tip_schemas::Record,
    genesis: String,
    bootstrap: crate::tip_schemas::Bootstrap,
    revocations: RevocationSet,
    /// The identity's links: issuer links and its own capability.
    links: CredentialChain,
    profile: Profile,
    ttl: u64,
    /// Every held epoch of each collection the identity reads.
    keys: BTreeMap<String, Vec<(rocky_core::KeyEpoch, String)>>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    groups: Vec<(GroupRecord, Vec<Vec<u8>>)>,
    /// The partition naming root, hex, when the identity issues.
    naming: Option<String>,
    /// Contacts of the issuers the answering node reaches.
    issuers: Vec<String>,
}

impl Bundle {
    fn verify_database(&self, database: rocky_credentials::DatabaseContext) -> Result<(), Failure> {
        if self.mesh != database.scope.mesh
            || self.domain != database.scope.domain
            || self.links.database_context(database.publisher)? != database
        {
            return Err("identity enrollment belongs to another database".into());
        }
        self.revocations.verify_in(database)?;
        Ok(())
    }
}

/// A linking request awaiting confirmation on both devices. Contains no identity signer.
pub struct AwaitingLink {
    pub code: String,
    pub node: String,
    pub(crate) request: IdentityRequest,
    pub(crate) secret: rocky_credentials::Exchange,
}
impl std::fmt::Debug for AwaitingLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwaitingLink")
            .field("code", &self.code)
            .field("node", &self.node)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Display for AwaitingLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "confirm linking code {} on both devices", self.code)
    }
}
impl std::error::Error for AwaitingLink {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkRequest {
    pub request: Vec<u8>,
    pub code: Option<String>,
}

pub(crate) struct PendingLink {
    pub request: Vec<u8>,
    pub code: String,
    pub received: u64,
    pub confirmed: bool,
}

impl Settings {
    /// Answers a node of this node's own identity: everything the identity
    /// holds, sealed to that node. A request for another identity, or from
    /// a revoked node, is refused.
    pub(crate) fn answer_identity(
        &self,
        vault: &Vault,
        request: &[u8],
    ) -> Result<Vec<u8>, Failure> {
        if self.single_use {
            return Err("a single-use identity cannot link nodes".into());
        }
        let request = IdentityRequest::from_bytes(request)?;
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        request.verify(
            node.public_key(),
            self.identity,
            (now()?, SKEW),
            self.database_context()?,
        )?;
        if self
            .revocations
            .ends_node_of(self.identity, request.node, false)
            || self.revocations.revokes(&self.identity)
        {
            return Err("the node or its identity was revoked".into());
        }
        let mut keys = BTreeMap::<String, Vec<_>>::new();
        for (name, epochs) in &self.keys {
            for epoch in epochs {
                keys.entry(name.clone()).or_default().push((
                    *epoch,
                    HEXLOWER.encode(&vault.get(&crate::enrollment::key_role(name, *epoch))?),
                ));
            }
        }
        let leaf = self
            .credentials
            .held()
            .ok_or("missing identity capability")?;
        let bundle = Bundle {
            format: 6,
            mesh: self.mesh,
            domain: self.domain,
            record: crate::tip_schemas::Record::new(&self.schema, self.generation, self.engine),
            genesis: self.genesis.clone(),
            bootstrap: self.schema_bootstrap()?,
            revocations: {
                let mut known = self.revocations.clone();
                if let Some(certificate) = self.credentials.certificate() {
                    known.link(certificate.clone())?;
                }
                known
            },
            links: self.identity_links(),
            profile: self.profile.clone(),
            ttl: self.ttl,
            keys,
            epoch_statements: self.epoch_statements.clone(),
            groups: self.groups_for(vault, request.node, leaf, true)?,
            naming: if self.issuer() {
                crate::enrollment::naming_for(vault, &Profile::Issuer)?
            } else {
                None
            },
            issuers: self.issuer_contacts(&[request.node])?,
        };
        Ok(request.answer(&serde_json::to_vec(&bundle)?)?.to_bytes()?)
    }

    /// The node keys of a new node of `provider`'s identity, saved before
    /// anything is sent, so a retry is the same node.
    pub(crate) fn prepare_identity(
        path: &Path,
        store: &Arc<dyn crate::SecretStore>,
        provider: &Arc<dyn IdentityProvider>,
        contact: &str,
    ) -> Result<(Intent, Vault), Failure> {
        let record = crate::enrollment::ContactRecord::parse(contact)?;
        Intent::resume(
            &Intent::path(path, false),
            Some(rocky_core::commitment(
                "rocky.sdk.identity-join.v1",
                record.node.as_bytes(),
            )),
            store,
            Some(provider),
            false,
        )
    }
    /// Installs a new node of `provider`'s identity from a node of the
    /// same identity that `contact` names.
    pub(crate) async fn enroll_identity(
        path: &Path,
        (intent, vault): (Intent, Vault),
        provider: Arc<dyn IdentityProvider>,
        contact: &str,
        transport: &dyn Transport,
        confirmed: Option<Arc<AwaitingLink>>,
    ) -> Result<Self, Failure> {
        let record = crate::enrollment::ContactRecord::parse(contact)?;
        let addresses = record
            .routes
            .iter()
            .filter(|route| route.transport == transport.kind())
            .map(|route| route.address.clone())
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            return Err(ServiceError::NoRoute {
                peer: "the identity's node",
                network: transport.kind().into(),
            }
            .into());
        }
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        let pending = match confirmed.as_ref() {
            Some(pending) => {
                pending.request.verify(
                    record.node,
                    provider.public_key()?,
                    (now()?, SKEW),
                    record.database,
                )?;
                if pending.request.node != node.public_key() {
                    return Err("link continuation belongs to another installation".into());
                }
                None
            }
            None => {
                let (request, secret) =
                    IdentityRequest::sign(&*provider, &node, record.node, now()?, record.database)?;
                Some(AwaitingLink {
                    code: request.confirmation_code()?,
                    node: node.public_key().to_hex(),
                    request,
                    secret,
                })
            }
        };
        let linking = confirmed
            .as_deref()
            .or(pending.as_ref())
            .ok_or("missing link request")?;
        let request = &linking.request;
        let wire = LinkRequest {
            request: request.to_bytes()?,
            code: confirmed.as_ref().map(|pending| pending.code.clone()),
        };
        let reply = exchange(
            transport,
            &addresses,
            Protocol::Renew,
            &[IDENTITY, &serde_json::to_vec(&wire)?].concat(),
        )
        .await?;
        if confirmed.is_none() {
            if reply != b"confirmation required" {
                return Err("unexpected link challenge".into());
            }
            return Err(Box::new(pending.ok_or("missing link continuation")?));
        }
        let secret = &linking.secret;
        let bundle: Bundle =
            serde_json::from_slice(&IdentityAnswer::from_bytes(&reply)?.open(request, secret)?)?;
        if bundle.format != 6 {
            return Err("unsupported identity enrollment format".into());
        }
        bundle.record.bytes()?;
        bundle.verify_database(record.database)?;
        let identity = provider.public_key()?;
        if bundle
            .links
            .0
            .last()
            .is_none_or(|link| link.capability.subject != identity)
        {
            return Err("the answer names another identity".into());
        }
        if bundle
            .revocations
            .ends_node_of(identity, node.public_key(), false)
        {
            rocky_platform::fs::remove_file(Intent::path(path, false))?;
            vault.forget(["node", "transport"].into_iter().map(String::from))?;
            return Err("the previous membership link is void; retry to link a fresh node".into());
        }
        let mut settings = Settings::installed(
            (intent.installation.clone(), intent.single_use),
            (record.publisher, identity),
            (bundle.mesh, bundle.domain, bundle.record.schema.clone()),
            bundle.genesis.clone(),
            bundle.ttl,
        );
        settings.generation = bundle.record.generation;
        settings.bootstrap = Some(bundle.bootstrap);
        // The followed graph authenticates the schema record.
        settings.verify_schema()?;
        settings.learn_revocations(&bundle.revocations)?;
        let domain = settings.domain()?;
        let issues = bundle
            .links
            .0
            .last()
            .map(|link| link.capability.extended())
            .transpose()?
            .is_some_and(|extended| extended.issues);
        settings.profile = bundle.profile;
        settings.credentials = crate::identities::with_identity(provider, || {
            settings.certify(&vault, bundle.links, &domain)
        })?;
        settings.epoch_statements = bundle.epoch_statements;
        settings.adopt_groups(&vault, &bundle.groups)?;
        for (name, scope) in domain.scopes() {
            if !settings.reads_whole(&domain, &scope) {
                continue;
            }
            for (epoch, key) in bundle.keys.get(name).into_iter().flatten() {
                settings.hold(&vault, name, *epoch, &crate::enrollment::hex32(key)?)?;
            }
            if !settings.keys.contains_key(name) {
                return Err("missing reader key".into());
            }
        }
        if issues {
            // Its identity's authority makes this node an issuer too.
            settings.authority = Some(crate::enrollment::Issuing::default());
            settings.learn_revocations(&RevocationSet::new())?;
            crate::enrollment::hold_naming(&vault, bundle.naming.as_deref())?;
        }
        // An identity may relink through its last, now-unlinked installation.
        // Its sealed bundle is provisioning evidence, not a live data peer.
        if !settings
            .revocations
            .ends_node_of(identity, record.node, false)
        {
            settings.add_peer(&vault, contact)?;
        }
        settings.learn_issuers(&vault, &bundle.issuers);
        let certificate = settings
            .credentials
            .certificate()
            .ok_or("missing linked certificate")?;
        // The answering node persists the identity's certificate before any sync.
        let reply = exchange(
            transport,
            &addresses,
            Protocol::Renew,
            &[crate::enrollment::LINK, &serde_json::to_vec(certificate)?].concat(),
        )
        .await?;
        let accepted = RevocationSet::from_bytes(&reply)
            .map_err(|error| format!("link acknowledgment: {error}"))?;
        settings.learn_revocations(&accepted)?;
        let linked = settings
            .credentials
            .certificate()
            .ok_or("missing linked certificate")?
            .id()?;
        if !settings.revocations.membership()?.accepts(linked)
            || settings
                .revocations
                .ends_node_of(identity, node.public_key(), false)
        {
            rocky_platform::fs::remove_file(Intent::path(path, false))?;
            vault.forget(settings.roles())?;
            return Err("the membership link is void; retry to link a fresh node".into());
        }
        settings.save(path)?;
        rocky_platform::fs::remove_file(Intent::path(path, false))?;
        Ok(settings)
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;
    use crate::{FileSecretStore, IdentityKey, RootKey, Schema, SecretStore};
    const SCHEMA: &str = r#"{"format":6,"application":"scoped_enrollment","version":1,"tables":[{"name":"items","class":"replayed","columns":[{"name":"key","type":"text","not_null":true}],"primary_key":["key"]}]}"#;

    #[test]
    fn foreign_identity_answers_are_refused_before_installation() -> Result<(), Failure> {
        let directory = tempfile::tempdir()?;
        let root = RootKey::generate();
        let person: Arc<dyn IdentityProvider> = Arc::new(IdentityKey::generate());
        let store: Arc<dyn SecretStore> = Arc::new(FileSecretStore::new(directory.path()));
        let a = Settings::create(
            &Schema::parse(SCHEMA)?,
            &store,
            3600,
            Some(person.clone()),
            &root,
        )?;
        let b = Settings::create(
            &Schema::parse(SCHEMA)?,
            &store,
            3600,
            Some(person.clone()),
            &root,
        )?;
        let vault = a.vault(&store);
        let node = PrivateKey::new();
        let audience = Settings::node_key(&vault)?.public_key();
        let (request, secret) =
            IdentityRequest::sign(&*person, &node, audience, now()?, a.database_context()?)?;
        let bytes = a.answer_identity(&vault, &request.to_bytes()?)?;
        let answer = IdentityAnswer::from_bytes(&bytes)?;
        let mut bundle: Bundle = serde_json::from_slice(&answer.open(&request, &secret)?)?;
        bundle.verify_database(a.database_context()?)?;
        assert!(bundle.verify_database(b.database_context()?).is_err());
        // Even relabeling the envelope cannot change the signed authority.
        bundle.mesh = b.mesh;
        bundle.domain = b.domain;
        assert!(bundle.verify_database(b.database_context()?).is_err());
        let (other_request, other_secret) =
            IdentityRequest::sign(&*person, &node, audience, now()?, b.database_context()?)?;
        assert!(answer.open(&other_request, &other_secret).is_err());
        assert!(
            a.answer_identity(&vault, &other_request.to_bytes()?)
                .is_err()
        );
        Ok(())
    }
}
