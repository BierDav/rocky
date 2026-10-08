use super::*;
use crate::{
    AuthorRange, Authority, CredentialChain, Extended, Extension, PolicyStatement, SignedCapability,
};
use rocky_core::{
    Affinity, AppId, Collation, DomainId, KeyColumn, MeshId, PartitionSelection, PartitionValue,
    PrincipalKind, RowChange, RowWrite, SqlValue,
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
fn policy() -> PolicyStatement {
    PolicyStatement {
        manifest: [9; 32],
        epoch: 1,
        revoked: BTreeSet::new(),
    }
}
fn authority(key: &PrivateKey) -> Authority<'_> {
    Authority {
        publisher: key,
        manifest: [9; 32],
    }
}

#[test]
fn selector_chain_checks_attenuation_and_concrete_scope_without_a_key() {
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
            1,
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
    for wide in [
        grant(None, None, &[Right::Custody], &[]),
        grant(
            Some("customer-x"),
            None,
            &[Right::Custody, Right::Read],
            &[],
        ),
        grant(Some("customer-x"), None, &[Right::Custody], &[1]),
        grant(Some("other"), None, &[Right::Custody], &[]),
    ] {
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
        leaf.extension = Some(
            Extension::new(Extended {
                partitions: vec![wide],
                ..Extended::default()
            })
            .unwrap(),
        );
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
    for (p, time) in [
        (policy(), 1100),
        (
            PolicyStatement {
                epoch: 2,
                ..policy()
            },
            NOW,
        ),
        (
            PolicyStatement {
                revoked: [crate::revoked_subject(&delegate.public_key())].into(),
                ..policy()
            },
            NOW,
        ),
    ] {
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
    let renewed = authority(&publisher)
        .renew(
            &prefix.0[0].capability,
            &PolicyStatement {
                epoch: 2,
                ..policy()
            },
            VALIDITY,
        )
        .unwrap();
    assert_eq!(
        renewed.capability.extended().unwrap().partitions,
        vec![held]
    );
}

#[test]
fn paired_grants_and_partition_pseudonyms_never_expand_authority() {
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
            1,
            VALIDITY,
        )
        .unwrap();
    let prefix = CredentialChain(vec![root]);
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
    let chain = prefix
        .assign_partitions(
            &holder,
            pseudonym.public_key(),
            11,
            node.public_key(),
            vec![a, b],
            VALIDITY,
        )
        .unwrap();
    let t = tuple("x", "w");
    let scope = t.scope(domain(), [3; 32]).unwrap();
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
    assert_eq!(
        chain.pseudonym_device(&publisher, [9; 32]).unwrap(),
        node.public_key()
    );
    assert!(chain.pseudonym_device(&holder, [9; 32]).is_err());
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
fn partition_keys_open_only_for_the_pinned_recipient_context_and_epoch() {
    let issuer = PrivateKey::new();
    let alice = PrivateKey::new();
    let bob = PrivateKey::new();
    let outsider = PrivateKey::new();
    let t = tuple("list-a", "warehouse");
    let scope = t.scope(domain(), [3; 32]).unwrap();
    let recipients = [alice.public_key(), bob.public_key()].into();
    let envelopes = PartitionKeyEnvelope::seal_many(
        &issuer,
        [9; 32],
        scope,
        t.clone(),
        1,
        [7; 32],
        &recipients,
    )
    .unwrap();
    for recipient in [&alice, &bob] {
        let envelope = envelopes
            .iter()
            .find(|envelope| envelope.recipient() == recipient.public_key())
            .unwrap();
        let decoded = PartitionKeyEnvelope::from_bytes(&envelope.to_bytes().unwrap()).unwrap();
        assert_eq!(
            decoded
                .open(recipient, issuer.public_key(), [9; 32], scope, 1)
                .unwrap(),
            [7; 32]
        );
        assert!(
            decoded
                .open(&outsider, issuer.public_key(), [9; 32], scope, 1)
                .is_err()
        );
        assert!(
            decoded
                .open(recipient, outsider.public_key(), [9; 32], scope, 1)
                .is_err()
        );
        assert!(
            decoded
                .open(recipient, issuer.public_key(), [8; 32], scope, 1)
                .is_err()
        );
        assert!(
            decoded
                .open(recipient, issuer.public_key(), [9; 32], scope, 2)
                .is_err()
        );
        let other = tuple("list-b", "warehouse")
            .scope(domain(), [3; 32])
            .unwrap();
        assert!(
            decoded
                .open(recipient, issuer.public_key(), [9; 32], other, 1)
                .is_err()
        );
        let mut tampered = decoded.clone();
        tampered.body.sealed[20] ^= 1;
        assert!(
            tampered
                .verify(issuer.public_key(), [9; 32], scope, 1)
                .is_err()
        );
        // Even a valid issuer signature cannot transplant existing ciphertext.
        tampered = decoded.clone();
        tampered.body.epoch = 2;
        tampered.signature =
            issuer.sign(&signing_bytes("rocky.partition-key-envelope.v1", &tampered.body).unwrap());
        assert!(
            tampered
                .open(recipient, issuer.public_key(), [9; 32], scope, 2)
                .is_err()
        );
        tampered = decoded.clone();
        tampered.body.ephemeral = [0; 32];
        tampered.signature =
            issuer.sign(&signing_bytes("rocky.partition-key-envelope.v1", &tampered.body).unwrap());
        assert!(
            tampered
                .open(recipient, issuer.public_key(), [9; 32], scope, 1)
                .is_err()
        );
        tampered = decoded.clone();
        tampered.body.tuple = tuple("list-b", "warehouse");
        tampered.signature =
            issuer.sign(&signing_bytes("rocky.partition-key-envelope.v1", &tampered.body).unwrap());
        assert!(
            tampered
                .verify(issuer.public_key(), [9; 32], scope, 1)
                .is_err()
        );
    }
    assert_ne!(envelopes[0].body.ephemeral, envelopes[1].body.ephemeral);
    assert!(
        PartitionKeyEnvelope::seal_many(
            &issuer,
            [9; 32],
            scope,
            t.clone(),
            1,
            [7; 32],
            &BTreeSet::new()
        )
        .is_err()
    );
    assert!(
        PartitionKeyEnvelope::seal(
            &issuer,
            [9; 32],
            scope,
            t.clone(),
            0,
            [7; 32],
            alice.public_key()
        )
        .is_err()
    );
    let wrong_scope = tuple("list-b", "warehouse")
        .scope(domain(), [3; 32])
        .unwrap();
    assert!(
        PartitionKeyEnvelope::seal(
            &issuer,
            [9; 32],
            wrong_scope,
            t,
            1,
            [7; 32],
            alice.public_key()
        )
        .is_err()
    );
    assert!(PartitionKeyEnvelope::from_bytes(&[0; 5000]).is_err());
}

#[test]
fn legacy_verifiers_refuse_partition_extensions_and_selectors_cannot_be_dropped() {
    #[derive(serde::Deserialize)]
    struct Version {
        version: u32,
    }
    let publisher = PrivateKey::new();
    let user = PrivateKey::new();
    let node = PrivateKey::new();
    let partitions = vec![grant(Some("x"), None, &[Right::Read], &[1])];
    let root = authority(&publisher)
        .issue_partition_terms(
            user.public_key(),
            vec![],
            partitions,
            (1, None),
            1,
            VALIDITY,
        )
        .unwrap();
    let extension = root.capability.extension.clone().unwrap();
    let opaque: Vec<u8> =
        decode_bounded(&encode(&extension).unwrap(), crate::MAX_CREDENTIAL_BYTES).unwrap();
    let old: Version = p2panda_core::cbor::decode_cbor(opaque.as_slice()).unwrap();
    assert_eq!(old.version, 2);
    // `docs/runtime/credential-primitives.md`'s verifier accepts version 1 only.
    assert_ne!(old.version, 1);
    let chain = CredentialChain(vec![root])
        .complete(&user, node.public_key())
        .unwrap();
    let mut leaf = chain.0[1].capability.clone();
    leaf.extension = None;
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
    parent.extension = None;
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
