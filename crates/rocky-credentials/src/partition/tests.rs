use super::*;
use crate::{
    AuthorRange, Authority, CredentialChain, Extended, Extension, Revocation, RevocationSet,
    SignedCapability, SignedRevocation,
};
use crate::{decode_bounded, encode};
use rocky_core::{
    Affinity, AppId, Collation, DomainId, KeyColumn, MeshId, PartitionSelection, PartitionValue,
    PrincipalKind, ResourceScope, RowChange, RowWrite, SqlValue,
};
const NOW: u64 = 100;
const VALIDITY: (u64, u64, u64) = (NOW, 1000, 0);
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
        fields: fields.iter().map(|field| [*field; 32]).collect(),
    }
}
fn policy() -> RevocationSet {
    RevocationSet::new()
}
/// A set whose one statement revokes `key`.
fn revoking(publisher: &PrivateKey, key: PublicKey) -> RevocationSet {
    let issuer = PrivateKey::new();
    let statement = Revocation {
        app: AppId([2; 16]),
        issuer: issuer.public_key(),
        time: NOW,
        nodes: BTreeSet::new(),
        users: [key].into(),
        capabilities: BTreeSet::new(),
        pseudonyms: BTreeSet::new(),
    };
    RevocationSet::try_from(vec![
        SignedRevocation::sign(statement, publisher, &issuer).unwrap(),
    ])
    .unwrap()
}
fn authority(key: &PrivateKey) -> Authority<'_> {
    Authority {
        publisher: key,
        manifest: [9; 32],
    }
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
            (2, None),
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
            delegate.public_key(),
            PrincipalKind::Deployment,
            vec![],
            vec![narrower.clone()],
            (1, None),
            VALIDITY,
        )
        .unwrap();
    let chain = delegated
        .clone()
        .complete(&delegate, node.public_key())
        .unwrap();
    let t = tuple("customer-x", "warehouse-y");
    let scope = t.scope(domain(), [3; 32]).unwrap();
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

#[test]
fn selector_chain_checks_attenuation_and_concrete_scope_without_a_key() {
    let SelectorFixture {
        publisher,
        node,
        chain,
        t,
        scope,
        ..
    } = selector_fixture();
    assert!(
        chain
            .provenance(publisher.public_key(), [9; 32], node.public_key())
            .is_ok()
    );
    let grants = chain
        .verify_partition(
            publisher.public_key(),
            [9; 32],
            node.public_key(),
            scope,
            &t,
            &policy(),
            NOW,
        )
        .unwrap();
    assert!(grants[0].permits(scope, [8; 32], Right::Custody));
    assert!(!grants[0].permits(scope, [8; 32], Right::Read));
    // The ordinary table-scope admission path cannot discard the selector.
    assert!(
        chain
            .verify(
                publisher.public_key(),
                [9; 32],
                node.public_key(),
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[test]
fn selector_chain_binds_each_slot_to_its_concrete_scope() {
    let SelectorFixture {
        publisher,
        node,
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
                    [9; 32],
                    node.public_key(),
                    t.scope(domain(), [3; 32]).unwrap(),
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
                [9; 32],
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
                delegate.public_key(),
                PrincipalKind::User,
                vec![],
                vec![wide.clone()],
                (1, None),
                VALIDITY
            )
            .is_err()
    );
    // A malicious holder signs the expanded grant without using issuance APIs.
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
                [9; 32],
                node.public_key(),
                scope,
                &t,
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[test]
fn delegated_selectors_cannot_widen_to_every_partition() {
    refuses_wider_partition_grant(grant(None, None, &[Right::Custody], &[]));
}

#[test]
fn delegated_selectors_cannot_change_the_customer() {
    refuses_wider_partition_grant(grant(Some("other"), None, &[Right::Custody], &[]));
}

#[test]
fn delegated_partition_rights_cannot_expand() {
    refuses_wider_partition_grant(grant(
        Some("customer-x"),
        None,
        &[Right::Custody, Right::Read],
        &[],
    ));
}

#[test]
fn delegated_partition_fields_cannot_expand() {
    refuses_wider_partition_grant(grant(Some("customer-x"), None, &[Right::Custody], &[1]));
}

#[test]
fn selector_chain_refuses_expired_credentials() {
    let SelectorFixture {
        publisher,
        node,
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
                [9; 32],
                node.public_key(),
                scope,
                &t,
                &p,
                time
            )
            .is_err()
    );
}

#[test]
fn selector_chain_refuses_a_revoked_delegate() {
    let SelectorFixture {
        publisher,
        delegate,
        node,
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
                [9; 32],
                node.public_key(),
                scope,
                &t,
                &p,
                time
            )
            .is_err()
    );
}

#[test]
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

struct PseudonymFixture {
    publisher: PrivateKey,
    holder: PrivateKey,
    pseudonym: PrivateKey,
    node: PrivateKey,
    prefix: CredentialChain,
    a: PartitionGrant,
    chain: CredentialChain,
    t: PartitionTuple,
    scope: ResourceScope,
}

fn pseudonym_fixture() -> PseudonymFixture {
    let publisher = PrivateKey::new();
    let holder = PrivateKey::new();
    let pseudonym = PrivateKey::new();
    let node = PrivateKey::new();
    let a = grant(Some("x"), None, &[Right::Insert], &[1]);
    let b = grant(Some("x"), None, &[Right::Update], &[2]);
    let root = authority(&publisher)
        .issue_partition_terms(
            holder.public_key(),
            vec![],
            vec![a.clone(), b.clone()],
            (2, Some(AuthorRange::new(10, 20).unwrap())),
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
    let chain = prefix
        .assign_partitions(
            &holder,
            pseudonym.public_key(),
            11,
            node.public_key(),
            vec![a.clone(), b],
            VALIDITY,
        )
        .unwrap();
    let t = tuple("x", "w");
    let scope = t.scope(domain(), [3; 32]).unwrap();
    PseudonymFixture {
        publisher,
        holder,
        pseudonym,
        node,
        prefix,
        a,
        chain,
        t,
        scope,
    }
}

#[test]
fn paired_grants_and_partition_pseudonyms_never_expand_authority() {
    let PseudonymFixture {
        prefix,
        holder,
        pseudonym,
        node,
        a,
        ..
    } = pseudonym_fixture();
    let cross = grant(Some("x"), None, &[Right::Insert, Right::Update], &[1, 2]);
    assert!(
        prefix
            .assign_partitions(
                &holder,
                pseudonym.public_key(),
                11,
                node.public_key(),
                vec![cross],
                VALIDITY
            )
            .is_err()
    );
    assert!(
        prefix
            .assign_partitions(
                &holder,
                pseudonym.public_key(),
                21,
                node.public_key(),
                vec![a.clone()],
                VALIDITY
            )
            .is_err()
    );
}

#[test]
fn partition_pseudonym_rows_keep_their_granted_fields() {
    let PseudonymFixture {
        publisher,
        pseudonym,
        chain,
        t,
        scope,
        ..
    } = pseudonym_fixture();
    let grants = chain
        .verify_partition_author(
            publisher.public_key(),
            [9; 32],
            pseudonym.public_key(),
            11,
            scope,
            &t,
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

#[test]
fn partition_pseudonyms_bind_the_author_and_sequence() {
    let PseudonymFixture {
        publisher,
        pseudonym,
        node,
        chain,
        t,
        scope,
        ..
    } = pseudonym_fixture();
    assert!(
        chain
            .verify_partition_author(
                publisher.public_key(),
                [9; 32],
                pseudonym.public_key(),
                12,
                scope,
                &t,
                &policy(),
                NOW
            )
            .is_err()
    );
    assert!(
        chain
            .verify_partition_author(
                publisher.public_key(),
                [9; 32],
                node.public_key(),
                11,
                scope,
                &t,
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[test]
fn partition_pseudonyms_cannot_discard_their_selector() {
    let PseudonymFixture {
        publisher,
        pseudonym,
        chain,
        ..
    } = pseudonym_fixture();
    let other = tuple("other", "w");
    assert!(
        chain
            .verify_partition_author(
                publisher.public_key(),
                [9; 32],
                pseudonym.public_key(),
                11,
                other.scope(domain(), [3; 32]).unwrap(),
                &other,
                &policy(),
                NOW
            )
            .is_err()
    );
    assert!(
        chain
            .verify_pseudonym(
                publisher.public_key(),
                [9; 32],
                pseudonym.public_key(),
                &policy(),
                NOW
            )
            .is_err()
    );
}

#[test]
fn only_the_publisher_resolves_a_partition_pseudonyms_device() {
    let PseudonymFixture {
        publisher,
        holder,
        node,
        chain,
        ..
    } = pseudonym_fixture();
    assert_eq!(
        chain.pseudonym_device(&publisher, [9; 32]).unwrap(),
        node.public_key()
    );
    assert!(chain.pseudonym_device(&holder, [9; 32]).is_err());
}

#[test]
fn partition_pseudonym_credentials_verify_after_round_trip() {
    let PseudonymFixture {
        publisher,
        pseudonym,
        chain,
        t,
        scope,
        ..
    } = pseudonym_fixture();
    let grants = chain
        .verify_partition_author(
            publisher.public_key(),
            [9; 32],
            pseudonym.public_key(),
            11,
            scope,
            &t,
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
                [9; 32],
                pseudonym.public_key(),
                11,
                scope,
                &t,
                &policy(),
                NOW
            )
            .unwrap(),
        grants
    );
}

#[test]
fn partition_selectors_cannot_be_dropped() {
    let publisher = PrivateKey::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let partitions = vec![grant(Some("x"), None, &[Right::Read], &[1])];
    let root = authority(&publisher)
        .issue_partition_terms(user.public_key(), vec![], partitions, (1, None), VALIDITY)
        .unwrap();
    let chain = CredentialChain(vec![root])
        .complete(&user, node.public_key())
        .unwrap();
    let mut leaf = chain.0[1].capability.clone();
    leaf.extension = crate::Extension::new(crate::Extended::default()).unwrap();
    let dropped = CredentialChain(vec![
        chain.0[0].clone(),
        SignedCapability::sign(leaf, &user).unwrap(),
    ]);
    let t = tuple("x", "w");
    let scope = t.scope(domain(), [3; 32]).unwrap();
    assert!(
        dropped
            .verify_partition(
                publisher.public_key(),
                [9; 32],
                node.public_key(),
                scope,
                &t,
                &policy(),
                NOW
            )
            .is_err()
    );
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
                [9; 32],
                node.public_key(),
                scope,
                &t,
                &policy(),
                NOW
            )
            .is_err()
    );
}
