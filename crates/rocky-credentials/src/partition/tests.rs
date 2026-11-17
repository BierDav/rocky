use super::*;
use crate::{
    Authority, CredentialChain, Extended, Extension, Revocation, RevocationSet, SignedCapability,
    SignedRevocation,
};
use crate::{PrivateKey, decode_bounded, encode};
use rocky_core::{
    Affinity, AppId, Collation, DomainId, FieldSelection, KeyColumn, MeshId, PartitionSelection,
    PartitionValue, PrincipalKind, ResourceScope, RowChange, RowWrite, SqlValue,
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
/// A set whose one statement revokes `key`.
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
        recovery: None,
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
    delegate: PrivateKey,
    node: PrivateKey,
    held: PartitionGrant,
    prefix: CredentialChain,
    delegated: CredentialChain,
    chain: CredentialChain,
    t: PartitionTuple,
    scope: ResourceScope,
}

fn selector_fixture() -> SelectorFixture {
    let publisher = PrivateKey::new();
    let user = PrivateKey::new();
    let delegate = PrivateKey::new();
    let node = PrivateKey::new();
    let held = grant(Some("customer-x"), None, &[Right::Custody], &[]);
    let root = authority(&publisher)
        .issue_partition_terms(
            user.public_key(),
            vec![],
            vec![held.clone()],
            crate::DelegationTerms { delegations: 2 },
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let narrower = grant(
        Some("customer-x"),
        Some("warehouse-y"),
        &[Right::Custody],
        &[],
    );
    let delegated = prefix
        .delegate_partitions(
            &user,
            crate::Delegation {
                subject: delegate.public_key(),
                kind: PrincipalKind::Deployment,
                grants: vec![],
                partitions: vec![narrower.clone()],
                terms: crate::DelegationTerms { delegations: 1 },
                validity: VALIDITY,
            },
        )
        .unwrap();
    let chain = delegated
        .clone()
        .complete(&delegate, node.public_key())
        .unwrap();
    let t = tuple("customer-x", "warehouse-y");
    let scope = t.scope(domain(), [3; 32], &[4; 32]).unwrap();
    SelectorFixture {
        publisher,
        user,
        delegate,
        node,
        held,
        prefix,
        delegated,
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
            .provenance(publisher.public_key(), node.public_key())
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

fn refuses_wider_partition_grant(wide: PartitionGrant) {
    let SelectorFixture {
        publisher,
        user,
        delegate,
        node,
        prefix,
        delegated,
        t,
        scope,
        ..
    } = selector_fixture();

    assert!(
        prefix
            .delegate_partitions(
                &user,
                crate::Delegation {
                    subject: delegate.public_key(),
                    kind: PrincipalKind::User,
                    grants: vec![],
                    partitions: vec![wide.clone()],
                    terms: crate::DelegationTerms { delegations: 1 },
                    validity: VALIDITY
                }
            )
            .is_err()
    );
    let mut leaf = delegated.0[1].capability.clone();
    leaf.extension = Extension::new(Extended {
        partitions: vec![wide],
        ..Extended::default()
    })
    .unwrap();
    let forged = CredentialChain(vec![
        prefix.0[0].clone(),
        SignedCapability::sign(leaf, &user).unwrap(),
    ])
    .complete(&delegate, node.public_key())
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
        "an expanded grant signed without the issuance APIs is refused"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegated_selectors_cannot_widen_to_every_partition() {
    refuses_wider_partition_grant(grant(None, None, &[Right::Custody], &[]));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegated_selectors_cannot_change_the_customer() {
    refuses_wider_partition_grant(grant(Some("other"), None, &[Right::Custody], &[]));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegated_partition_rights_cannot_expand() {
    refuses_wider_partition_grant(grant(
        Some("customer-x"),
        None,
        &[Right::Custody, Right::Read],
        &[],
    ));
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegated_partition_fields_cannot_expand() {
    refuses_wider_partition_grant(grant(Some("customer-x"), None, &[Right::Custody], &[1]));
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
fn selector_chain_refuses_a_revoked_delegate() {
    let SelectorFixture {
        node,
        publisher,
        delegate,
        chain,
        t,
        scope,
        ..
    } = selector_fixture();
    let p = revoking(&publisher, delegate.public_key());
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
        delegate,
        prefix,
        held,
        ..
    } = selector_fixture();
    let renewed = authority(&publisher)
        .renew(
            &prefix.0[0].capability,
            &revoking(&publisher, delegate.public_key()),
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
        .issue_partition_terms(
            holder.public_key(),
            vec![],
            vec![a.clone(), b.clone()],
            crate::DelegationTerms { delegations: 2 },
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let chain = prefix
        .clone()
        .complete(&holder, node_signer.public_key())
        .unwrap();
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
    let mut chain = prefix
        .clone()
        .complete(&holder, node_signer.public_key())
        .unwrap();
    let mut leaf = chain.0.pop().unwrap().capability;
    leaf.extension = Extension::new(Extended {
        node: Some(crate::PeerId::of(node_signer.public_key())),
        partitions: vec![cross],
        families: false,
        issues: false,
        trusted: None,
        claims: None,
        origin: None,
        sequence: Some(0),
        previous: None,
        database: None,
        personal: None,
    })
    .unwrap();
    assert!(
        SignedCapability::sign(leaf, &holder).is_err(),
        "a node certificate holds no partitions of its own"
    );
    assert!(
        prefix
            .certify_member(&holder, node_signer.public_key(), 1, None)
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
    let partitions = vec![grant(Some("x"), None, &[Right::Read], &[1])];
    let root = authority(&publisher)
        .issue_partition_terms(
            user.public_key(),
            vec![],
            partitions,
            crate::DelegationTerms { delegations: 1 },
            VALIDITY,
        )
        .unwrap();
    let chain = CredentialChain(vec![root])
        .complete(&user, node.public_key())
        .unwrap();
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

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn partition_delegation_backdates_and_expires_within_its_parent() {
    use std::time::Duration;
    let fixture = selector_fixture();
    for (lifetime, clock_skew, not_before, expires) in
        [(200, 20, NOW + 30, NOW + 250), (2000, 200, NOW, NOW + 1000)]
    {
        let chain = fixture
            .prefix
            .delegate_partitions(
                &fixture.user,
                crate::Delegation {
                    subject: fixture.delegate.public_key(),
                    kind: PrincipalKind::Deployment,
                    grants: vec![],
                    partitions: vec![fixture.held.clone()],
                    terms: crate::DelegationTerms { delegations: 1 },
                    validity: crate::Validity {
                        issued_at: NOW + 50,
                        lifetime: Duration::from_secs(lifetime),
                        clock_skew: Duration::from_secs(clock_skew),
                    },
                },
            )
            .unwrap();
        let leaf = &chain.0.last().unwrap().capability;
        assert_eq!((leaf.not_before, leaf.expires), (not_before, expires));
        let node = chain
            .certify(&fixture.delegate, fixture.node.public_key())
            .unwrap();
        assert!(
            node.verified(fixture.publisher.public_key(), &policy(), not_before - 1)
                .is_err()
        );
        assert!(
            node.verified(fixture.publisher.public_key(), &policy(), not_before)
                .is_ok()
        );
        assert!(
            node.verified(fixture.publisher.public_key(), &policy(), expires)
                .is_err()
        );
    }
}
