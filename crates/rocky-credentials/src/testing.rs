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
        .certify(identity, node.public_key())
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
                database: Some(database.unwrap_or(crate::DatabaseContext {
                    publisher: identity.public_key().expect("a publisher"),
                    scope: rocky_core::Scope {
                        mesh: rocky_core::MeshId([1; 16]),
                        app: rocky_core::AppId([2; 16]),
                        domain: rocky_core::DomainId([3; 16]),
                    },
                })),
                ..Extended::default()
            })
            .expect("an extension"),
        },
        identity,
    )
    .expect("a node certificate")
}

pub async fn claim_contract(
    coordinator: &dyn crate::Coordinator,
    scope: rocky_core::Scope,
    elapse: impl AsyncFn(u64),
) {
    use crate::counters::{CLAIM_WINDOW, Claim, Lookup, Name};
    use rocky_core::CoordinationError;
    let name = Name::new("rocky.claim-contract").expect("a counter name");
    let claim = |claim| coordinator.claim(scope, name.clone(), claim);
    let read = async |claim| {
        coordinator
            .counter(scope, name.clone(), Some(claim))
            .await
            .expect("a reading")
    };
    let first = Claim::fresh().expect("a claim");
    let taken = claim(first).await.expect("a first value");
    assert_eq!(
        claim(first).await,
        Ok(taken),
        "a retried claim returns its value"
    );
    let mut last = taken;
    for _ in 0..2 {
        let next = claim(Claim::fresh().expect("a claim"))
            .await
            .expect("a later value");
        assert!(next > last, "a new claim takes a greater value");
        last = next;
    }
    assert_eq!(
        claim(first).await,
        Ok(taken),
        "a retry after later claims returns its value"
    );
    let never_taken = Claim {
        id: rocky_core::commitment("rocky.claim-contract.expired", &[]),
        issued: 0,
    };
    assert_eq!(
        claim(never_taken).await,
        Err(CoordinationError::Expired),
        "a claim past its window is refused as expired"
    );
    let reading = read(never_taken).await;
    assert_eq!(reading.value, last, "an expired claim leaves the counter");
    assert_eq!(
        reading.claim,
        Some(Lookup::Expired),
        "a claim past its window reads as expired"
    );
    elapse(CLAIM_WINDOW + 1).await;
    assert_eq!(
        claim(first).await,
        Err(CoordinationError::Expired),
        "a claim that took a value is refused as expired once its window passes"
    );
    let reading = read(first).await;
    assert_eq!(reading.value, last, "an expired retry leaves the counter");
    assert_eq!(
        reading.claim,
        Some(Lookup::Expired),
        "a claim that took a value reads as expired once its window passes"
    );
}

#[derive(Clone, Copy)]
pub struct CoordinatorBehaviour {
    pub observes_calls: bool,
    pub denies_untrusted_claims: bool,
}
pub async fn coordinator_contract(
    coordinator: &dyn crate::Coordinator,
    trusted: &PrivateKey,
    database: crate::DatabaseContext,
    behaviour: CoordinatorBehaviour,
) {
    bindings_are_independent(coordinator, trusted, database, behaviour).await;
    denied_is_final(coordinator, trusted, database, behaviour).await;
}
fn bound(
    coordinator: &dyn crate::Coordinator,
    key: &PrivateKey,
    database: crate::DatabaseContext,
) -> std::sync::Arc<dyn crate::Coordinator> {
    coordinator
        .bind(key.clone(), certificate(key, key, 0, Some(database)))
        .expect("a binding")
}
fn counted(binding: &dyn crate::Coordinator) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    binding.observe(std::sync::Arc::new(move |_| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    calls
}
async fn bindings_are_independent(
    coordinator: &dyn crate::Coordinator,
    trusted: &PrivateKey,
    database: crate::DatabaseContext,
    behaviour: CoordinatorBehaviour,
) {
    use crate::counters::{Claim, Name};
    use std::sync::atomic::Ordering;
    let name = Name::new("rocky.coordinator-contract.bindings").expect("a counter name");
    let scope = database.scope;
    let unbound = coordinator.node();
    let stranger = PrivateKey::new();
    let held = bound(coordinator, trusted, database);
    let other = bound(coordinator, &stranger, database);
    assert_eq!(
        held.node(),
        *trusted.public_key().as_bytes(),
        "a binding acts as its own key"
    );
    assert_eq!(
        other.node(),
        *stranger.public_key().as_bytes(),
        "a second binding acts as its own key"
    );
    assert_eq!(
        coordinator.node(),
        unbound,
        "binding leaves the bound coordinator's identity"
    );
    let held_calls = counted(&*held);
    let other_calls = counted(&*other);
    held.claim(scope, name.clone(), Claim::fresh().expect("a claim"))
        .await
        .expect("a trusted binding claims");
    assert_eq!(
        other_calls.load(Ordering::SeqCst),
        0,
        "a binding observes only its own calls"
    );
    let observed = held_calls.load(Ordering::SeqCst);
    assert_eq!(
        observed > 0,
        behaviour.observes_calls,
        "a binding observes its own calls exactly when the coordinator reports calls"
    );
    let _ = other
        .claim(scope, name.clone(), Claim::fresh().expect("a claim"))
        .await;
    assert_eq!(
        held_calls.load(Ordering::SeqCst),
        observed,
        "a binding observes only its own calls"
    );
    let third = PrivateKey::new();
    other.certify(certificate(&third, &third, 0, Some(database)));
    other.present_claims(Some(CredentialChain::default()));
    held.claim(scope, name, Claim::fresh().expect("a claim"))
        .await
        .expect("another binding's certificate and claims leave this binding's");
    assert_eq!(
        held.node(),
        *trusted.public_key().as_bytes(),
        "another binding's certificate leaves this binding's key"
    );
}
async fn denied_is_final(
    coordinator: &dyn crate::Coordinator,
    trusted: &PrivateKey,
    database: crate::DatabaseContext,
    behaviour: CoordinatorBehaviour,
) {
    use crate::counters::{Claim, Name};
    use rocky_core::CoordinationError;
    let (key, other) = (PrivateKey::new(), PrivateKey::new());
    let foreign = certificate(&other, &other, 0, Some(database));
    for _ in 0..3 {
        assert!(
            matches!(
                coordinator.bind(key.clone(), foreign.clone()),
                Err(CoordinationError::Denied)
            ),
            "a binding to another key's certificate is denied on every retry"
        );
    }
    let name = Name::new("rocky.coordinator-contract.denied").expect("a counter name");
    let scope = database.scope;
    let stranger = bound(coordinator, &key, database);
    let claim = Claim::fresh().expect("a claim");
    let first = stranger.claim(scope, name.clone(), claim).await;
    if !behaviour.denies_untrusted_claims {
        assert!(
            first.is_ok(),
            "a coordinator that trusts every binding takes an untrusted binding's claim"
        );
        return;
    }
    assert_eq!(
        first,
        Err(CoordinationError::Denied),
        "an untrusted binding's claim is denied"
    );
    for _ in 0..2 {
        assert_eq!(
            stranger.claim(scope, name.clone(), claim).await,
            Err(CoordinationError::Denied),
            "a denied claim is denied on every retry"
        );
    }
    bound(coordinator, trusted, database)
        .claim(scope, name.clone(), claim)
        .await
        .expect("a trusted binding takes the claim");
    assert_eq!(
        stranger.claim(scope, name, claim).await,
        Err(CoordinationError::Denied),
        "a denied claim stays denied once another binding took it"
    );
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
