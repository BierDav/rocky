//! Self-enrollment: a new node of an identity that already holds grants
//! certifies itself with its identity provider. A node of the same identity
//! hands it what the identity holds; no issuer and no invitation is
//! involved. See `docs/runtime/sdk-enrollment.md#another-node-of-an-identity`.
use crate::{
    Failure,
    enrollment::{IDENTITY, Intent, Settings, exchange, now},
    groups::GroupRecord,
    schema::Schema,
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
    mesh: MeshId,
    domain: DomainId,
    schema: Schema,
    manifest: String,
    lineage: Vec<crate::generation::Step>,
    revocations: RevocationSet,
    /// The identity's links: issuer links and its own capability.
    links: CredentialChain,
    profile: Profile,
    ttl: u64,
    /// Every held epoch of each collection the identity reads.
    keys: BTreeMap<String, Vec<(rocky_core::KeyEpoch, String)>>,
    epoch_statements: BTreeMap<String, Vec<Commitment>>,
    groups: Vec<(GroupRecord, Vec<Vec<u8>>)>,
    /// Contacts of the issuers the answering node reaches.
    issuers: Vec<String>,
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
        let request = IdentityRequest::from_bytes(request)?;
        let node = PrivateKey::from_bytes(&vault.get("node")?);
        request.verify(node.public_key(), self.identity, (now()?, SKEW))?;
        if self.revocations.revokes(&request.node) || self.revocations.revokes(&self.identity) {
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
        let leaf = &self
            .credentials
            .0
            .last()
            .ok_or("missing node credential")?
            .capability;
        let bundle = Bundle {
            mesh: self.mesh,
            domain: self.domain,
            schema: self.schema.clone(),
            manifest: self.manifest.clone(),
            lineage: self.lineage.clone(),
            revocations: self.revocations.clone(),
            links: self.identity_links(),
            profile: self.profile.clone(),
            ttl: self.ttl,
            keys,
            epoch_statements: self.epoch_statements.clone(),
            groups: self.groups_for(vault, request.node, leaf, true)?,
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
        let (request, secret) = IdentityRequest::sign(&*provider, &node, record.node, now()?)?;
        let reply = exchange(
            transport,
            &addresses,
            Protocol::Renew,
            &[IDENTITY, &request.to_bytes()?].concat(),
        )
        .await?;
        let bundle: Bundle =
            serde_json::from_slice(&IdentityAnswer::from_bytes(&reply)?.open(&request, &secret)?)?;
        let identity = provider.public_key()?;
        if bundle
            .links
            .0
            .last()
            .is_none_or(|link| link.capability.subject != identity)
        {
            return Err("the answer names another identity".into());
        }
        let mut settings = Settings::installed(
            (intent.installation.clone(), intent.stored_identity),
            (record.publisher, identity, provider),
            (bundle.mesh, bundle.domain, bundle.schema.clone()),
            bundle.manifest.clone(),
            bundle.ttl,
        );
        settings.lineage = bundle.lineage;
        // The schema and lineage are the publisher's.
        settings.verify_lineage()?;
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
        settings.credentials = settings.certify(&vault, bundle.links, &domain)?;
        let grants = settings.credentials.grants().to_vec();
        settings.epoch_statements = bundle.epoch_statements;
        for (name, scope) in domain.scopes() {
            if !crate::enrollment::reads(&grants, scope) {
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
        }
        settings.adopt_groups(&vault, &bundle.groups)?;
        settings.add_peer(&vault, contact)?;
        settings.learn_issuers(&vault, &bundle.issuers);
        settings.save(path)?;
        std::fs::remove_file(Intent::path(path, false))?;
        Ok(settings)
    }
}
