use super::*;
use rocky_credentials::{Authority, PersonalContext};

/// The owner of a personal database and the authority its devices share.
struct Owner {
    owner: PrivateKey,
    grant: CredentialChain,
    context: PersonalContext,
}
impl Owner {
    /// The owner and its personal database.
    fn new() -> (Self, Fixture) {
        let owner = PrivateKey::new();
        let namespace =
            rocky_credentials::PersonalNamespace::new("employees.example", "personal").unwrap();
        let f = Fixture::personal(owner.public_key(), namespace);
        let owner = Self::trusted(&f, owner);
        (owner, f)
    }
    /// `owner` with the root's trusted grant in `f`, whatever its mode.
    fn trusted(f: &Fixture, owner: PrivateKey) -> Self {
        let scope = f.public.domain;
        let grant = CredentialChain(vec![
            Authority::owner(&f.publisher)
                .issue_trusted(owner.public_key(), scope, 1)
                .unwrap(),
        ]);
        let context = PersonalContext {
            database: rocky_credentials::DatabaseContext {
                publisher: f.publisher.public_key(),
                scope,
            },
            owner: owner.public_key(),
            grant: grant.0[0].id().unwrap(),
            descriptor: f.manifest.descriptor().unwrap(),
        };
        Self {
            owner,
            grant,
            context,
        }
    }
    /// A device the owner certified on its own: the root's grant, what its
    /// node holds of the owner's authority and its independent certificate.
    fn device(&self, f: &Fixture, node: &PrivateKey) -> CredentialChain {
        let certified = self
            .grant
            .clone()
            .certify_personal(
                &self.owner,
                node,
                (self.context.owner, self.context.descriptor),
            )
            .unwrap();
        let authority = Authority::issuer(node, &certified).unwrap();
        let held = authority.hold(f.full(), Vec::new(), validity()).unwrap();
        let mut chain = self.grant.clone();
        chain.0.push(held);
        chain.0.push(certified.certificate().unwrap().clone());
        chain
    }
}

async fn replica(
    f: &Fixture,
    directory: &Scratch,
    name: &str,
    node: &PrivateKey,
    (chain, known): (CredentialChain, &RevocationSet),
) -> EncryptedReplica<Employees> {
    let mut config = f.config(node, f.full(), &[f.public]);
    config.credentials = chain;
    config.revocations = known.clone();
    EncryptedReplica::open(directory.path().join(name), f.app(), config)
        .await
        .unwrap()
}

async fn write(replica: &EncryptedReplica<Employees>, f: &Fixture, row: u8) -> EncryptedFrame {
    replica
        .author(
            [row; 32],
            vec![change(
                f.public,
                row,
                None,
                Some(f.app().name(&format!("row-{row}"))),
            )],
            vec![],
        )
        .await
        .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn devices_signed_in_against_one_snapshot_write_and_merge_in_both_orders() {
    let directory = Scratch::new().await;
    let (owner, f) = Owner::new();
    let first = PrivateKey::new();
    let first_chain = owner.device(&f, &first);
    let mut snapshot = f.policy.clone();
    snapshot
        .admit_personal(first_chain.certificate().unwrap().clone(), &owner.context)
        .unwrap();
    // Laptop and phone each certify themselves against the same snapshot
    // and write before they meet.
    let mut devices = Vec::new();
    for (index, name) in ["laptop", "phone"].into_iter().enumerate() {
        let node = PrivateKey::new();
        let chain = owner.device(&f, &node);
        let mut known = snapshot.clone();
        assert!(
            known
                .admit_personal(chain.certificate().unwrap().clone(), &owner.context)
                .unwrap()
        );
        let device = replica(&f, &directory, name, &node, (chain, &known)).await;
        let frame = write(&device, &f, index as u8 + 1).await;
        devices.push((device, known, frame));
    }
    for (order, name) in [([0, 1], "forward"), ([1, 0], "backward")] {
        let receiver = replica(
            &f,
            &directory,
            name,
            &first,
            (first_chain.clone(), &snapshot),
        )
        .await;
        for index in order {
            let (_, known, frame) = &devices[index];
            receiver.learn_revocations(known).await.unwrap();
            receiver.receive(frame.clone()).await.unwrap();
        }
        assert_eq!(receiver.snapshot(f.public).await.unwrap().rows.len(), 2);
        let held = receiver.revocations().await;
        assert!(!held.has_revocations());
        let membership = held.membership().unwrap();
        assert!(!membership.is_forked(owner.owner.public_key()));
        assert_eq!(membership.devices(owner.owner.public_key()).count(), 3);
        assert_eq!(
            held.personal_devices(owner.owner.public_key())
                .unwrap()
                .len(),
            3
        );
    }
    // Each sibling accepts the other's write under the merged authority.
    let (laptop, _, laptop_frame) = &devices[0];
    let (phone, phone_known, phone_frame) = &devices[1];
    laptop.learn_revocations(phone_known).await.unwrap();
    laptop.receive(phone_frame.clone()).await.unwrap();
    phone.receive(laptop_frame.clone()).await.unwrap();
    for device in [laptop, phone] {
        assert_eq!(device.snapshot(f.public).await.unwrap().rows.len(), 2);
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_cloned_device_key_with_conflicting_history_is_still_a_fork() {
    let directory = Scratch::new().await;
    let (owner, f) = Owner::new();
    let node = PrivateKey::new();
    let chain = owner.device(&f, &node);
    let mut known = f.policy.clone();
    known
        .admit_personal(chain.certificate().unwrap().clone(), &owner.context)
        .unwrap();
    let original = replica(&f, &directory, "original", &node, (chain.clone(), &known)).await;
    let clone = replica(&f, &directory, "clone", &node, (chain, &known)).await;
    let written = write(&original, &f, 1).await;
    let conflicting = write(&clone, &f, 2).await;
    let receiver_key = PrivateKey::new();
    let receiver_chain = owner.device(&f, &receiver_key);
    let receiver = replica(
        &f,
        &directory,
        "receiver",
        &receiver_key,
        (receiver_chain, &known),
    )
    .await;
    receiver.receive(written).await.unwrap();
    let error = receiver.receive(conflicting).await.unwrap_err().to_string();
    assert!(error.contains("author log fork"), "{error}");
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_shared_database_refuses_personal_certificates() {
    let f = Fixture::new();
    let directory = Scratch::new().await;
    // Any trusted identity of a shared database can hand-build one.
    let trusted = Owner::trusted(&f, PrivateKey::new());
    let node = PrivateKey::new();
    let chain = trusted.device(&f, &node);
    let mut known = f.policy.clone();
    known.link(chain.certificate().unwrap().clone()).unwrap();
    let member = PrivateKey::new();
    let receiver = EncryptedReplica::open(
        directory.path().join("receiver"),
        f.app(),
        f.config(&member, f.full(), &[f.public]),
    )
    .await
    .unwrap();
    let before = receiver.revocations().await;
    let error = receiver.learn_revocations(&known).await.unwrap_err();
    assert!(
        error.to_string().contains("another database mode"),
        "{error}"
    );
    assert_eq!(
        receiver.revocations().await.exchange_digest(0).unwrap(),
        before.exchange_digest(0).unwrap()
    );
    // Nor does such a device open a replica of it.
    let mut config = f.config(&node, f.full(), &[f.public]);
    config.credentials = chain;
    let error = EncryptedReplica::open(directory.path().join("device"), f.app(), config)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("another database mode"), "{error}");
}
