use crate::{
    Authority, Capability, CredentialChain, Extended, Extension, IdentityProvider, PrivateKey,
    SignedCapability, Signer,
};

pub fn node(
    identity: &dyn IdentityProvider,
    chain: &CredentialChain,
) -> (PrivateKey, CredentialChain) {
    let node = PrivateKey::new();
    let chain = chain
        .clone()
        .certify(identity, &node)
        .expect("a node certificate");
    (node, chain)
}
pub fn authority(identity: &dyn IdentityProvider, chain: &CredentialChain) -> Authority<'static> {
    let (node, chain) = node(identity, chain);
    let node: &'static PrivateKey = Box::leak(Box::new(node));
    Authority::issuer(node, &chain).expect("an issuer node")
}
pub fn signer<'a>(identity: &'a dyn IdentityProvider, chain: &CredentialChain) -> Signer<'a> {
    if chain.0.is_empty() {
        return Signer::Key(identity);
    }
    let node: &'static PrivateKey = Box::leak(Box::new(PrivateKey::new()));
    let certificate = certificate(
        identity,
        node,
        chain.0.last().map_or(0, |last| last.capability.not_before),
        chain.database_context(chain.0[0].issuer).ok(),
    );
    Signer::node(node, &CredentialChain(vec![certificate])).expect("a signing node")
}
std::thread_local! {
    static LANES: std::cell::RefCell<std::collections::BTreeMap<crate::PublicKey, (&'static PrivateKey, SignedCapability)>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}
pub fn lane(identity: &dyn IdentityProvider) -> Signer<'static> {
    let public = identity.public_key().expect("a key");
    let (key, certificate) = LANES.with(|lanes| {
        lanes
            .borrow_mut()
            .entry(public)
            .or_insert_with(|| {
                let key: &'static PrivateKey = Box::leak(Box::new(PrivateKey::new()));
                (key, certificate(identity, key, 0, None))
            })
            .clone()
    });
    Signer::node(key, &CredentialChain(vec![certificate])).expect("a lane")
}
pub fn lane_for(identity: &dyn IdentityProvider, chain: &CredentialChain) -> Signer<'static> {
    let public = identity.public_key().expect("an identity");
    let database = chain
        .database_context(chain.0.first().expect("authority").issuer)
        .expect("a database");
    let _ = lane(identity);
    let (key, linked) = LANES.with(|lanes| {
        let mut lanes = lanes.borrow_mut();
        let held = lanes.get_mut(&public).expect("a lane");
        if held.1.capability.extended().expect("extension").database != Some(database) {
            held.1 = certificate(identity, held.0, 0, Some(database));
        }
        held.clone()
    });
    Signer::node(key, &CredentialChain(vec![linked])).expect("a lane")
}
pub fn lane_of(identity: &dyn IdentityProvider) -> crate::PublicKey {
    lane(identity).signing_node().expect("a node")
}
pub fn lane_key(identity: crate::PublicKey) -> crate::PublicKey {
    LANES.with(|lanes| {
        lanes
            .borrow()
            .get(&identity)
            .map_or(identity, |(key, _)| key.public_key())
    })
}
fn certificate(
    identity: &dyn IdentityProvider,
    node: &PrivateKey,
    not_before: u64,
    database: Option<crate::DatabaseContext>,
) -> SignedCapability {
    let identity_key = identity.public_key().expect("an identity");
    let database = database.unwrap_or(crate::DatabaseContext {
        publisher: identity_key,
        scope: rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        },
    });
    SignedCapability::sign(
        Capability {
            subject: node.public_key(),
            kind: rocky_core::PrincipalKind::Node,
            grants: Vec::new(),
            not_before,
            expires: crate::NEVER,
            delegations: 0,
            extension: Extension::new(Extended {
                node: Some(crate::PeerId::of(node.public_key())),
                sequence: Some(0),
                database: Some(database),
                possession: Some(
                    crate::NodePossession::sign(node, identity_key, database)
                        .expect("a possession proof"),
                ),
                ..Extended::default()
            })
            .expect("an extension"),
        },
        identity,
    )
    .expect("a node certificate")
}

#[derive(Default)]
pub struct MemoryKeys(std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>);
impl MemoryKeys {
    pub fn secrets(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
impl crate::KeyProvider for MemoryKeys {
    fn load(&self, name: &str) -> Result<Option<Vec<u8>>, crate::ProviderError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| "poisoned keys")?
            .get(name)
            .cloned())
    }
    fn store(&self, name: &str, secret: &[u8]) -> Result<(), crate::ProviderError> {
        self.0
            .lock()
            .map_err(|_| "poisoned keys")?
            .insert(name.into(), secret.to_vec());
        Ok(())
    }
    fn delete(&self, name: &str) -> Result<(), crate::ProviderError> {
        self.0.lock().map_err(|_| "poisoned keys")?.remove(name);
        Ok(())
    }
}

pub fn key_provider_contract(provider: &dyn crate::KeyProvider) {
    let given = "rocky.key-provider-contract.given";
    let never_given = "rocky.key-provider-contract.never-given";
    let load = |name| provider.load(name).expect("a load");
    assert_eq!(
        load(never_given),
        None,
        "a provider never invents a secret for a name it was not given"
    );
    provider.store(given, &[7; 32]).expect("a stored secret");
    assert_eq!(load(given), Some(vec![7; 32]), "a stored secret loads");
    for name in [never_given, "rocky.key-provider-contract.given.other"] {
        assert_eq!(
            load(name),
            None,
            "a provider holding other secrets never invents one for {name}"
        );
    }
    provider.delete(given).expect("a deleted secret");
    assert_eq!(
        load(given),
        None,
        "a provider never invents a secret for a deleted name"
    );
    assert_eq!(
        load(never_given),
        None,
        "asking for a missing name never creates it"
    );
}
