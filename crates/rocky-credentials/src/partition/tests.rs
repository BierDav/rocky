use super::*;
use crate::{
    Authority, CredentialChain, Extended, Extension, Revocation, RevocationSet, SignedCapability,
    SignedRevocation,
};
use crate::{PrivateKey, decode_bounded, encode};
use rocky_core::{
    Affinity, AppId, Collation, DomainId, FieldSelection, KeyColumn, MeshId, PartitionSelection,
    PartitionValue, ResourceScope, RowChange, RowWrite, SqlValue,
};
const NOW: u64 = 100;
const VALIDITY: crate::Validity = crate::Validity {
    issued_at: NOW,
    lifetime: std::time::Duration::from_secs(1000),
    clock_skew: std::time::Duration::from_secs(0),
};
fn domain() -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([3; 16]),
    }
}
fn value(text: &str) -> PartitionValue {
    PartitionValue::new(
        KeyColumn {
            affinity: Affinity::Text,
            collation: Collation::Binary,
        },
        SqlValue::Text(text.into()),
    )
    .unwrap()
}
fn tuple(customer: &str, warehouse: &str) -> PartitionTuple {
    PartitionTuple::new([([1; 32], value(customer)), ([2; 32], value(warehouse))]).unwrap()
}
fn grant(
    customer: Option<&str>,
    warehouse: Option<&str>,
    rights: &[Right],
    fields: &[u8],
) -> PartitionGrant {
    let select = |text: Option<&str>| {
        text.map_or(PartitionSelection::Any, |text| {
            PartitionSelection::Only([value(text)].into())
        })
    };
    PartitionGrant {
        domain: domain(),
        table: [3; 32],
        selector: PartitionSelector(
            [([1; 32], select(customer)), ([2; 32], select(warehouse))].into(),
        ),
        rows: RowSelection::All,
        rights: rights.iter().copied().collect(),
        fields: FieldSelection::Only(fields.iter().map(|field| [*field; 32]).collect()),
    }
}
fn policy() -> RevocationSet {
    RevocationSet::new()
}
fn revoking(publisher: &PrivateKey, key: PublicKey) -> RevocationSet {
    let statement = Revocation {
        app: AppId([2; 16]),
        issuer: publisher.public_key(),
        time: NOW,
        nodes: BTreeSet::new(),
        identities: [key].into(),
        capabilities: BTreeSet::new(),
        cut: Default::default(),
        mode: crate::RevocationMode::Lost,
    };
    RevocationSet::try_from(vec![
        SignedRevocation::sign(statement, publisher, &CredentialChain::default()).unwrap(),
    ])
    .unwrap()
}
fn authority(key: &PrivateKey) -> Authority<'_> {
    Authority::owner(key)
}

struct SelectorFixture {
    publisher: PrivateKey,
    user: PrivateKey,
    node: PrivateKey,
    held: PartitionGrant,
    prefix: CredentialChain,
    chain: CredentialChain,
    t: PartitionTuple,
    scope: ResourceScope,
}

fn partitions(partitions: Vec<PartitionGrant>) -> crate::Granted {
    crate::Granted {
        partitions,
        ..crate::Granted::default()
    }
}
fn selector_fixture() -> SelectorFixture {
    let publisher = PrivateKey::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let held = grant(
        Some("customer-x"),
        Some("warehouse-y"),
        &[Right::Custody],
        &[],
    );
    let root = authority(&publisher)
        .issue_person(user.public_key(), partitions(vec![held.clone()]), VALIDITY)
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let chain = prefix.clone().complete(&user, &node).unwrap();
    let t = tuple("customer-x", "warehouse-y");
    let scope = t.scope(domain(), [3; 32], &[4; 32]).unwrap();
    SelectorFixture {
        publisher,
        user,
        node,
        held,
        prefix,
        chain,
        t,
        scope,
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn selector_chain_checks_attenuation_and_concrete_scope_without_a_key() {
    let SelectorFixture {
        node,
        publisher,
        chain,
        t,
        scope,
        ..
    } = selector_fixture();
    assert!(
        chain
            .historic_provenance(publisher.public_key(), node.public_key())
            .is_ok()
    );
    let grants = chain
        .verify_partition(
            publisher.public_key(),
            node.public_key(),
            scope,
            &t,
            &policy(),
            NOW,
        )
        .unwrap();
    assert!(grants[0].permits(scope, [8; 32], Right::Custody));
    assert!(!grants[0].permits(scope, [8; 32], Right::Read));
    assert!(
        chain
            .verify(publisher.public_key(), node.public_key(), &policy(), NOW)
            .is_err(),
        "the ordinary table-scope admission path cannot discard the selector"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn selector_chain_binds_each_slot_to_its_concrete_scope() {
    let SelectorFixture {
        node,
        publisher,
        chain,
        scope,
        ..
    } = selector_fixture();
    for t in [
        tuple("customer-other", "warehouse-y"),
        tuple("customer-x", "warehouse-other"),
    ] {
        assert!(
            chain
                .verify_partition(
                    publisher.public_key(),
                    node.public_key(),
                    t.scope(domain(), [3; 32], &[4; 32]).unwrap(),
                    &t,
                    &policy(),
                    NOW
                )
                .is_err()
        );
    }
    let changed = tuple("customer-x", "warehouse-other");
    assert!(
        chain
            .verify_partition(
                publisher.public_key(),
                node.public_key(),
                scope,
                &changed,
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_person_signs_no_link_for_another_person() {
    let SelectorFixture {
        publisher,
        user,
        prefix,
        t,
        scope,
        held,
        ..
    } = selector_fixture();
    let (other, node) = (PrivateKey::new(), PrivateKey::new());
    for shared in [
        held,
        grant(None, None, &[Right::Custody], &[]),
        grant(
            Some("customer-x"),
            None,
            &[Right::Custody, Right::Read],
            &[],
        ),
    ] {
        let mut leaf = prefix.0[0].capability.clone();
        leaf.subject = other.public_key();
        leaf.extension = Extension::new(Extended {
            partitions: vec![shared],
            ..Extended::default()
        })
        .unwrap();
        let forged = CredentialChain(vec![
            prefix.0[0].clone(),
            SignedCapability::sign(leaf, &user).unwrap(),
        ])
        .complete(&other, &node)
        .unwrap();
        assert!(
            forged
                .verify_partition(
                    publisher.public_key(),
                    node.public_key(),
                    scope,
                    &t,
                    &policy(),
                    NOW
                )
                .is_err(),
            "only an issuer signs a person's link, never another person"
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn selector_chain_refuses_expired_credentials() {
    let SelectorFixture {
        node,
        publisher,
        chain,
        t,
        scope,
        ..
    } = selector_fixture();
    let p = policy();
    let time = 1100;

    assert!(
        chain
            .verify_partition(
                publisher.public_key(),
                node.public_key(),
                scope,
                &t,
                &p,
                time
            )
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn selector_chain_refuses_a_revoked_person() {
    let SelectorFixture {
        node,
        publisher,
        user,
        chain,
        t,
        scope,
        ..
    } = selector_fixture();
    let p = revoking(&publisher, user.public_key());
    let time = NOW;

    assert!(
        chain
            .verify_partition(
                publisher.public_key(),
                node.public_key(),
                scope,
                &t,
                &p,
                time
            )
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn selector_chain_renewal_preserves_its_partition_terms() {
    let SelectorFixture {
        publisher,
        prefix,
        held,
        ..
    } = selector_fixture();
    let renewed = authority(&publisher)
        .renew(
            &prefix.0[0].capability,
            &revoking(&publisher, PrivateKey::new().public_key()),
            VALIDITY,
        )
        .unwrap();
    assert_eq!(
        renewed.capability.extended().unwrap().partitions,
        vec![held]
    );
}

struct NodeFixture {
    publisher: PrivateKey,
    holder: PrivateKey,
    node_signer: PrivateKey,
    prefix: CredentialChain,
    a: PartitionGrant,
    chain: CredentialChain,
    t: PartitionTuple,
    scope: ResourceScope,
}

fn node_fixture() -> NodeFixture {
    let publisher = PrivateKey::new();
    let holder = PrivateKey::new();
    let node_signer = PrivateKey::new();
    let a = grant(Some("x"), None, &[Right::Insert], &[1]);
    let b = grant(Some("x"), None, &[Right::Update], &[2]);
    let root = authority(&publisher)
        .issue_person(
            holder.public_key(),
            partitions(vec![a.clone(), b.clone()]),
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let chain = prefix.clone().complete(&holder, &node_signer).unwrap();
    let t = tuple("x", "w");
    let scope = t.scope(domain(), [3; 32], &[4; 32]).unwrap();
    NodeFixture {
        publisher,
        holder,
        node_signer,
        prefix,
        a,
        chain,
        t,
        scope,
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn paired_grants_and_partition_nodes_never_expand_authority() {
    let NodeFixture {
        prefix,
        holder,
        node_signer,
        a,
        ..
    } = node_fixture();
    let cross = grant(Some("x"), None, &[Right::Insert, Right::Update], &[1, 2]);
    let mut chain = prefix.clone().complete(&holder, &node_signer).unwrap();
    let mut leaf = chain.0.pop().unwrap().capability;
    let certified = leaf.extended().unwrap();
    leaf.extension = Extension::new(Extended {
        node: Some(crate::PeerId::of(node_signer.public_key())),
        partitions: vec![cross],
        whole: None,
        role: None,
        issues: false,
        trusted: None,
        origin: None,
        sequence: Some(0),
        previous: None,
        database: certified.database,
        possession: certified.possession,
    })
    .unwrap();
    assert!(
        SignedCapability::sign(leaf, &holder).is_err(),
        "a node certificate holds no partitions of its own"
    );
    assert!(
        prefix
            .certify_member(&holder, &node_signer, 1, None)
            .is_err(),
        "a later membership sequence names its predecessor"
    );
    assert!(!a.rights.is_empty());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn partition_node_signer_rows_keep_their_granted_fields() {
    let NodeFixture {
        publisher,
        node_signer,
        chain,
        t,
        scope,
        ..
    } = node_fixture();
    let grants = chain
        .verify_partition_author(
            publisher.public_key(),
            node_signer.public_key(),
            crate::PartitionContext { scope, tuple: &t },
            &policy(),
            NOW,
        )
        .unwrap();
    let insert = RowWrite::changed(RowChange {
        scope,
        row: [8; 32],
        before: None,
        after: Some([([1; 32], vec![1])].into()),
    });
    assert!(insert.permitted_by(&grants));
    let wrong_field = RowWrite::changed(RowChange {
        after: Some([([2; 32], vec![1])].into()),
        ..insert.change.clone()
    });
    assert!(!wrong_field.permitted_by(&grants));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn partition_nodes_bind_their_signing_key() {
    let NodeFixture {
        publisher,
        chain,
        t,
        scope,
        ..
    } = node_fixture();
    assert!(
        chain
            .verify_partition_author(
                publisher.public_key(),
                PrivateKey::new().public_key(),
                crate::PartitionContext { scope, tuple: &t },
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn partition_nodes_cannot_discard_their_selector() {
    let NodeFixture {
        publisher,
        node_signer,
        chain,
        ..
    } = node_fixture();
    let other = tuple("other", "w");
    assert!(
        chain
            .verify_partition_author(
                publisher.public_key(),
                node_signer.public_key(),
                crate::PartitionContext {
                    scope: other.scope(domain(), [3; 32], &[4; 32]).unwrap(),
                    tuple: &other
                },
                &policy(),
                NOW
            )
            .is_err()
    );
    assert!(
        chain
            .verify(
                publisher.public_key(),
                node_signer.public_key(),
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn partition_node_signer_credentials_verify_after_round_trip() {
    let NodeFixture {
        publisher,
        node_signer,
        chain,
        t,
        scope,
        ..
    } = node_fixture();
    let grants = chain
        .verify_partition_author(
            publisher.public_key(),
            node_signer.public_key(),
            crate::PartitionContext { scope, tuple: &t },
            &policy(),
            NOW,
        )
        .unwrap();
    let roundtrip: CredentialChain =
        decode_bounded(&encode(&chain).unwrap(), crate::MAX_CREDENTIAL_BYTES).unwrap();
    assert_eq!(
        roundtrip
            .verify_partition_author(
                publisher.public_key(),
                node_signer.public_key(),
                crate::PartitionContext { scope, tuple: &t },
                &policy(),
                NOW
            )
            .unwrap(),
        grants
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn partition_selectors_cannot_be_dropped() {
    let publisher = PrivateKey::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let root = authority(&publisher)
        .issue_person(
            user.public_key(),
            partitions(vec![grant(Some("x"), None, &[Right::Read], &[1])]),
            VALIDITY,
        )
        .unwrap();
    let chain = CredentialChain(vec![root]).complete(&user, &node).unwrap();
    let mut leaf = chain.0[1].capability.clone();
    leaf.extension = crate::Extension::new(crate::Extended::default()).unwrap();
    assert!(
        SignedCapability::sign(leaf, &user).is_err(),
        "a certificate names its node alone, no selectors"
    );
    let t = tuple("x", "w");
    let scope = t.scope(domain(), [3; 32], &[4; 32]).unwrap();
    let mut parent = chain.0[0].capability.clone();
    parent.extension = crate::Extension::new(crate::Extended::default()).unwrap();
    let dropped = CredentialChain(vec![
        SignedCapability::sign(parent, &publisher).unwrap(),
        chain.0[1].clone(),
    ]);
    assert!(
        dropped
            .verify_partition(
                publisher.public_key(),
                node.public_key(),
                scope,
                &t,
                &policy(),
                NOW
            )
            .is_err()
    );
}
