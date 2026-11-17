use super::*;
use crate::{Authority, Endorsement, RootKey, SignedManifest};
use rocky_core::{AuthorLogScope, DomainRule, Scope};

const NOW: u64 = 1_800_000_000;

struct Personal {
    root: RootKey,
    owner: crate::IdentityKey,
    namespace: PersonalNamespace,
    manifest: ApplicationManifest,
    genesis: SignedManifest,
    grant: CredentialChain,
    context: PersonalContext,
}

fn manifest(publisher: PublicKey, domain: DomainId) -> ApplicationManifest {
    let key = *publisher.as_bytes();
    ApplicationManifest {
        publisher: key,
        name: "notes".into(),
        app: ApplicationManifest::app_id(key, "notes"),
        schema: 1,
        projection: 1,
        engine: rocky_core::EngineIdentity::current(),
        entities: Vec::new(),
        fields: Vec::new(),
        operations: Vec::new(),
        domains: vec![DomainRule {
            domain,
            author_log: AuthorLogScope::Domain,
            rule: "independent".into(),
        }],
    }
}

impl Personal {
    fn new(secret: u8) -> Self {
        let root = RootKey::from_secret([secret; 32]);
        let owner = root.identity();
        let namespace = PersonalNamespace::new("notes.example", "account root").unwrap();
        let owner_key = owner.public_key();
        let manifest = manifest(root.public_key(), namespace.domain(owner_key).unwrap());
        let kind = DatabaseKind::Personal {
            owner: owner_key,
            namespace: namespace.clone(),
            creation: [secret; 16],
        };
        let genesis = SignedManifest::sign(&manifest, kind, &root).unwrap();
        let scope = Scope {
            mesh: namespace.mesh(owner_key).unwrap(),
            app: manifest.app,
            domain: namespace.domain(owner_key).unwrap(),
        };
        let grant = CredentialChain(vec![
            Authority::owner(&root)
                .issue_trusted(owner_key, scope, NOW)
                .unwrap(),
        ]);
        let context = PersonalContext {
            database: DatabaseContext {
                publisher: root.public_key(),
                scope,
            },
            owner: owner_key,
            grant: grant.0[0].id().unwrap(),
            descriptor: genesis.descriptor().unwrap(),
        };
        Self {
            root,
            owner,
            namespace,
            manifest,
            genesis,
            grant,
            context,
        }
    }
    /// A device signed in against `snapshot`: its own certificate only.
    fn device(&self) -> (PrivateKey, CredentialChain) {
        let node = PrivateKey::new();
        let chain = self
            .grant
            .clone()
            .certify_personal(
                &self.owner,
                &node,
                (self.context.owner, self.context.descriptor),
            )
            .unwrap();
        (node, chain)
    }
    fn sign_in(&self, snapshot: &RevocationSet) -> (RevocationSet, CredentialChain) {
        let (_, chain) = self.device();
        let mut set = snapshot.clone();
        assert!(
            set.admit_personal(chain.certificate().unwrap().clone(), &self.context)
                .unwrap()
        );
        (set, chain)
    }
    fn verifies(&self, chain: &CredentialChain, set: &RevocationSet) -> bool {
        chain.verified(self.root.public_key(), set, NOW).is_ok()
    }
    /// A second, differently dated certificate for the same node and binding.
    fn replay(&self, node: &PrivateKey) -> SignedCapability {
        let binding = PersonalBinding::sign(
            self.context.database,
            self.context.owner,
            node,
            (self.context.grant, self.context.descriptor),
        )
        .unwrap();
        let capability = crate::Capability {
            subject: node.public_key(),
            kind: rocky_core::PrincipalKind::Node,
            delegations: 0,
            grants: Vec::new(),
            not_before: NOW,
            expires: crate::NEVER,
            extension: crate::Extension::new(crate::Extended {
                node: Some(PeerId::of(node.public_key())),
                database: Some(self.context.database),
                personal: Some(binding),
                ..crate::Extended::default()
            })
            .unwrap(),
        };
        SignedCapability::sign(capability, &self.owner).unwrap()
    }
    fn unlink(&self, chain: &CredentialChain, node: PublicKey) -> RevocationSet {
        let statement = chain
            .unlink(&self.owner, self.manifest.app, NOW, node)
            .unwrap();
        RevocationSet::try_from(vec![statement]).unwrap()
    }
    fn merge(&self, set: &mut RevocationSet, other: &RevocationSet) {
        set.merge(other, self.root.public_key(), self.manifest.app, NOW)
            .unwrap();
    }
}

fn personal(error: Error) -> PersonalError {
    match error {
        Error::Personal(error) => error,
        other => panic!("expected a personal refusal, got {other}"),
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_namespace_locates_one_database_per_owner() {
    let a = Personal::new(1);
    let again = Personal::new(1);
    assert_eq!(a.context.database, again.context.database);
    let other = PersonalNamespace::new("notes.example", "chats").unwrap();
    let owner = a.owner.public_key();
    assert_ne!(other.mesh(owner).unwrap(), a.namespace.mesh(owner).unwrap());
    assert_ne!(
        other.domain(owner).unwrap(),
        a.namespace.domain(owner).unwrap()
    );
    let stranger = Personal::new(2).owner.public_key();
    assert_ne!(
        a.namespace.mesh(stranger).unwrap(),
        a.namespace.mesh(owner).unwrap()
    );
    for (rp, purpose) in [("", "x"), ("Notes.example", "x"), ("notes.example", "")] {
        assert!(PersonalNamespace::new(rp, purpose).is_err());
    }
    let encoded = encode(&a.namespace).unwrap();
    assert_eq!(
        crate::decode_bounded::<PersonalNamespace>(&encoded, 1024).unwrap(),
        a.namespace
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_genesis_pins_root_owner_namespace_and_mode() {
    let p = Personal::new(1);
    let (root, owner) = (p.root.public_key(), p.owner.public_key());
    p.genesis
        .verify_personal(root, owner, &p.namespace)
        .unwrap();
    assert!(p.genesis.verify_kind(root).unwrap().1.is_personal());
    let stranger = Personal::new(2);
    let checks = [
        (
            root,
            stranger.owner.public_key(),
            &p.namespace,
            PersonalError::WrongOwner,
        ),
        (
            stranger.root.public_key(),
            owner,
            &p.namespace,
            PersonalError::WrongPublisher,
        ),
    ];
    for (publisher, owner, namespace, expected) in checks {
        let error = p
            .genesis
            .verify_personal(publisher, owner, namespace)
            .unwrap_err();
        assert_eq!(personal(error), expected);
    }
    let other = PersonalNamespace::new("notes.example", "chats").unwrap();
    let error = p.genesis.verify_personal(root, owner, &other).unwrap_err();
    assert_eq!(personal(error), PersonalError::NamespaceMismatch);

    let shared = SignedManifest::sign(&p.manifest, DatabaseKind::Shared, &p.root).unwrap();
    let error = shared
        .verify_personal(root, owner, &p.namespace)
        .unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::ModeMismatch,
        "the same definition signed as shared is a downgrade"
    );
    assert_ne!(
        shared.descriptor().unwrap(),
        p.genesis.descriptor().unwrap()
    );

    #[derive(Serialize, Deserialize)]
    struct Mirror {
        body: Vec<u8>,
        kind: DatabaseKind,
        endorsement: Endorsement,
    }
    let mut mirror: Mirror =
        crate::decode_bounded(&p.genesis.to_bytes().unwrap(), 1 << 20).unwrap();
    mirror.kind = DatabaseKind::Shared;
    let swapped = SignedManifest::from_bytes(&encode(&mirror).unwrap()).unwrap();
    assert!(
        swapped.verify(root).is_err(),
        "swapping the signed mode breaks the root's signature"
    );

    let kind = |owner| DatabaseKind::Personal {
        owner,
        namespace: p.namespace.clone(),
        creation: [0; 16],
    };
    assert!(
        SignedManifest::sign(&p.manifest, kind(root), &p.root).is_err(),
        "the root cannot pin itself as owner"
    );
    let elsewhere = manifest(root, DomainId([9; 16]));
    assert!(SignedManifest::sign(&elsewhere, kind(owner), &p.root).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn devices_signed_in_against_one_snapshot_merge_in_every_order_without_forks() {
    let p = Personal::new(1);
    let (snapshot, first) = p.sign_in(&RevocationSet::new());
    let (mut laptop, laptop_chain) = p.sign_in(&snapshot);
    let (mut phone, phone_chain) = p.sign_in(&snapshot);
    let (laptop_alone, phone_alone) = (laptop.clone(), phone.clone());
    p.merge(&mut laptop, &phone_alone);
    p.merge(&mut phone, &laptop_alone);
    let owner = p.owner.public_key();
    for set in [&laptop, &phone] {
        let membership = set.membership().unwrap();
        assert_eq!(membership.devices(owner).count(), 3);
        assert!(!membership.is_forked(owner));
        assert!(!set.has_revocations());
        assert_eq!(
            set.digest(NOW).unwrap(),
            RevocationSet::new().digest(NOW).unwrap()
        );
        assert_eq!(set.personal_devices(owner).unwrap().len(), 3);
        for chain in [&first, &laptop_chain, &phone_chain] {
            assert!(p.verifies(chain, set));
            assert!(membership.accepts(chain.certificate().unwrap().id().unwrap()));
        }
    }
    assert_eq!(
        laptop.exchange_digest(NOW).unwrap(),
        phone.exchange_digest(NOW).unwrap()
    );
    let before = laptop.clone();
    p.merge(&mut laptop, &phone);
    assert_eq!(
        laptop, before,
        "merging again deduplicates by certificate ID"
    );
    assert!(
        !laptop
            .admit_personal(phone_chain.certificate().unwrap().clone(), &p.context)
            .unwrap()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_tombstone_dominates_every_replay_in_every_order() {
    let p = Personal::new(1);
    let (node, chain) = p.device();
    let mut held = RevocationSet::new();
    held.admit_personal(chain.certificate().unwrap().clone(), &p.context)
        .unwrap();
    let tombstone = p.unlink(&chain, node.public_key());
    let replay = p.replay(&node);
    assert_ne!(
        replay.id().unwrap(),
        chain.certificate().unwrap().id().unwrap()
    );

    let mut early = held.clone();
    early.admit_personal(replay.clone(), &p.context).unwrap();
    p.merge(&mut early, &tombstone);
    let mut late = held.clone();
    p.merge(&mut late, &tombstone);
    let before = late.clone();
    let error = late.admit_personal(replay.clone(), &p.context).unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::Tombstoned {
            node: PeerId::of(node.public_key())
        },
        "a holder that learned the replay before the tombstone drops it"
    );
    assert_eq!(late, before);
    assert!(!late.link(replay.clone()).unwrap());
    let mut carrier = RevocationSet::new();
    carrier.link(replay.clone()).unwrap();
    p.merge(&mut late, &carrier);
    assert_eq!(late, before);
    assert_eq!(
        early.exchange_digest(NOW).unwrap(),
        late.exchange_digest(NOW).unwrap()
    );
    for set in [&early, &late] {
        assert!(!p.verifies(&chain, set));
        assert!(!p.verifies(&chain.with_certificate(replay.clone()), set));
        assert!(
            set.personal_devices(p.owner.public_key())
                .unwrap()
                .is_empty()
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn the_device_limit_refuses_without_removing_a_sibling() {
    let p = Personal::new(1);
    let mut set = RevocationSet::new();
    let mut chains = Vec::new();
    for _ in 0..MAX_PERSONAL_DEVICES {
        let (next, chain) = p.sign_in(&set);
        set = next;
        chains.push(chain);
    }
    let before = set.clone();
    let (_, extra) = p.device();
    let error = set
        .admit_personal(extra.certificate().unwrap().clone(), &p.context)
        .unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::DeviceLimit {
            limit: MAX_PERSONAL_DEVICES
        }
    );
    assert_eq!(set, before);
    for chain in &chains {
        assert!(p.verifies(chain, &set));
    }
    let mut elsewhere = RevocationSet::new();
    elsewhere
        .admit_personal(extra.certificate().unwrap().clone(), &p.context)
        .unwrap();
    p.merge(&mut set, &elsewhere);
    for chain in chains.iter().chain([&extra]) {
        assert!(
            p.verifies(chain, &set),
            "independent additions beyond the limit still merge; nothing is evicted"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn wrong_bindings_and_forged_possession_change_nothing() {
    let p = Personal::new(1);
    let (snapshot, _) = p.sign_in(&RevocationSet::new());
    let stranger = Personal::new(2);
    let (_, foreign) = stranger.device();
    let mut set = snapshot.clone();
    let error = set
        .admit_personal(foreign.certificate().unwrap().clone(), &p.context)
        .unwrap_err();
    assert_eq!(personal(error), PersonalError::WrongOwner);
    let node = PrivateKey::new();
    let wrong_descriptor = p
        .grant
        .clone()
        .certify_personal(&p.owner, &node, (p.context.owner, [7; 32]))
        .unwrap();
    let error = set
        .admit_personal(wrong_descriptor.certificate().unwrap().clone(), &p.context)
        .unwrap_err();
    assert_eq!(personal(error), PersonalError::WrongBinding);
    assert_eq!(set, snapshot);

    let mut forged = p.replay(&node);
    let other = p.replay(&PrivateKey::new());
    let mut extended = forged.capability.extended().unwrap();
    extended.personal = other.capability.extended().unwrap().personal;
    forged.capability.extension = crate::Extension::new(extended).unwrap();
    let resigned = SignedCapability::sign(forged.capability.clone(), &p.owner).unwrap();
    let error = set.link(resigned.clone()).unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::Possession,
        "another key's possession proof never certifies the node"
    );
    let error = set.admit_personal(resigned, &p.context).unwrap_err();
    assert_eq!(personal(error), PersonalError::Possession);
    assert_eq!(set, snapshot);

    let regranted = CredentialChain(vec![
        Authority::owner(&p.root)
            .issue_trusted(p.owner.public_key(), p.context.database.scope, NOW + 1)
            .unwrap(),
    ]);
    let (_, chain) = p.device();
    let crossed = regranted.with_certificate(chain.certificate().unwrap().clone());
    assert!(
        p.verifies(&chain, &set),
        "a leaf binding another grant does not verify under this chain's grant"
    );
    assert!(!p.verifies(&crossed, &set));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_shared_genesis_refuses_personal_certificates() {
    let p = Personal::new(1);
    let root = p.root.public_key();
    let (_, device) = p.device();
    let certificate = device.certificate().unwrap().clone();
    let shared = SignedManifest::sign(&p.manifest, DatabaseKind::Shared, &p.root).unwrap();
    let mut held = RevocationSet::new();
    held.link(certificate.clone()).unwrap();
    let mut pinning = held.clone();
    let error = pinning.pin(&shared, root).unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::ModeMismatch,
        "pinning judges what a set already holds"
    );
    assert!(pinning.genesis().is_none());

    let mut pinned = RevocationSet::new();
    pinned.pin(&shared, root).unwrap();
    let error = pinned.link(certificate).unwrap_err();
    assert_eq!(personal(error), PersonalError::ModeMismatch);
    assert!(pinned.merge(&held, root, p.manifest.app, NOW).is_err());
    assert_eq!(pinned.certificates().count(), 0);
    assert!(!p.verifies(&device, &pinned));
    pinned
        .merge(&RevocationSet::new(), root, p.manifest.app, NOW)
        .unwrap();
    assert!(
        pinned.genesis().is_some(),
        "a merge keeps this holder's pin"
    );
    assert!(pinned.pin(&p.genesis, root).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_personal_genesis_admits_only_its_owners_bound_devices() {
    let p = Personal::new(1);
    let root = p.root.public_key();
    let owner = p.owner.public_key();
    let mut set = RevocationSet::new();
    set.pin(&p.genesis, root).unwrap();
    let (_, device) = p.device();
    set.link(device.certificate().unwrap().clone()).unwrap();
    assert!(p.verifies(&device, &set));
    let before = set.clone();

    let sequenced = p
        .grant
        .clone()
        .certify(&p.owner, PrivateKey::new().public_key())
        .unwrap();
    let error = set
        .link(sequenced.certificate().unwrap().clone())
        .unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::WrongBinding,
        "the owner's node never takes a membership sequence here"
    );
    assert!(!p.verifies(&sequenced, &set));

    let node = PrivateKey::new();
    let unbound = p
        .grant
        .clone()
        .certify_personal(&p.owner, &node, (owner, [7; 32]))
        .unwrap();
    let error = set
        .link(unbound.certificate().unwrap().clone())
        .unwrap_err();
    assert_eq!(
        personal(error),
        PersonalError::WrongBinding,
        "a personal certificate bound to another genesis is refused"
    );
    assert!(!p.verifies(&unbound, &set));

    let stranger = Personal::new(2).owner;
    let trusted = CredentialChain(vec![
        Authority::owner(&p.root)
            .issue_trusted(stranger.public_key(), p.context.database.scope, NOW)
            .unwrap(),
    ]);
    let strangers = trusted
        .clone()
        .certify(&stranger, PrivateKey::new().public_key())
        .unwrap();
    assert!(
        !p.verifies(&strangers, &set),
        "the root's trusted grant to anyone else carries no authority here"
    );
    let error = trusted
        .clone()
        .certify_personal(&stranger, &node, (owner, p.context.descriptor))
        .unwrap_err();
    assert_eq!(personal(error), PersonalError::WrongOwner);
    let forged = trusted
        .certify_personal(
            &stranger,
            &node,
            (stranger.public_key(), p.context.descriptor),
        )
        .unwrap();
    let error = set.link(forged.certificate().unwrap().clone()).unwrap_err();
    assert_eq!(personal(error), PersonalError::WrongOwner);
    assert!(!p.verifies(&forged, &set));
    assert_eq!(set, before);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn devices_a_reset_signed_out_leave_the_device_limit() {
    let p = Personal::new(1);
    let owner = p.owner.public_key();
    let mut set = RevocationSet::new();
    for _ in 0..MAX_PERSONAL_DEVICES {
        set = p.sign_in(&set).0;
    }
    let reset = crate::SignedRevocation::sign(
        crate::Revocation {
            app: p.manifest.app,
            issuer: p.root.public_key(),
            time: NOW,
            mode: crate::RevocationMode::Lost,
            nodes: Default::default(),
            identities: Default::default(),
            capabilities: Default::default(),
            cut: Vec::new(),
            recovery: Some(crate::RootDecision::Reset(Box::new(crate::Reset {
                follows: Default::default(),
                keep: [PeerId::of(PrivateKey::new().public_key())].into(),
                fence: Vec::new(),
            }))),
        },
        &p.root,
        &CredentialChain::default(),
    )
    .unwrap();
    p.merge(&mut set, &RevocationSet::try_from(vec![reset]).unwrap());
    assert!(set.personal_devices(owner).unwrap().is_empty());
    p.sign_in(&set);
}
