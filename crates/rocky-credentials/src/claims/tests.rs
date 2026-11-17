use super::*;
use crate::{Extension, PrivateKey, Revocation, SignedRevocation};
use rocky_core::{AppId, DomainId, MeshId};

const NOW: u64 = 1_000_000;

fn scope(domain: u8) -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([domain; 16]),
    }
}
fn only(names: &[&str]) -> ClaimRight {
    ClaimRight {
        scope: scope(3),
        counters: Some(names.iter().map(|name| name.as_bytes().to_vec()).collect()),
        grant: [7; 32],
    }
}
fn every() -> ClaimRight {
    ClaimRight {
        scope: scope(3),
        counters: None,
        grant: [7; 32],
    }
}
fn holder_chain(
    owner: &PrivateKey,
    holder: &PrivateKey,
    node: &PrivateKey,
    right: ClaimRight,
    delegations: u8,
) -> CredentialChain {
    let link = Authority::owner_in(
        owner,
        crate::DatabaseContext {
            publisher: owner.public_key(),
            scope: scope(3),
        },
    )
    .unwrap()
    .issue_claims(
        holder.public_key(),
        right,
        delegations,
        (NOW, CLAIM_LIFETIME),
    )
    .unwrap();
    CredentialChain(vec![link])
        .certify_claims(holder, node.public_key())
        .unwrap()
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn a_holder_claims_only_its_counters_of_its_domain() {
    let (owner, holder, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let none = RevocationSet::new();
    let chain = holder_chain(&owner, &holder, &node, only(&["invoices"]), 1);
    let claim = |counter: &str, domain: u8, key: PublicKey| {
        chain.claimant(
            owner.public_key(),
            key,
            (scope(domain), counter.as_bytes()),
            &none,
            NOW,
        )
    };
    assert_eq!(
        claim("invoices", 3, node.public_key()).unwrap(),
        holder.public_key()
    );
    assert!(claim("credits", 3, node.public_key()).is_err());
    assert!(claim("invoices", 4, node.public_key()).is_err());
    assert!(
        claim("invoices", 3, holder.public_key()).is_err(),
        "another node is refused as claimant"
    );
    assert!(
        chain
            .claimant(
                PrivateKey::new().public_key(),
                node.public_key(),
                (scope(3), b"invoices"),
                &none,
                NOW,
            )
            .is_err()
    );
    let all = holder_chain(&owner, &holder, &node, every(), 1);
    assert!(
        all.claimant(
            owner.public_key(),
            node.public_key(),
            (scope(3), b"anything"),
            &none,
            NOW
        )
        .is_ok()
    );
    assert!(
        chain
            .verify(owner.public_key(), node.public_key(), &none, NOW)
            .unwrap()
            .is_empty(),
        "a claim chain grants nothing in the domain itself"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn claim_links_expire_within_a_day_and_end_with_their_revocation() {
    let (owner, holder, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let chain = holder_chain(&owner, &holder, &node, every(), 1);
    let none = RevocationSet::new();
    let at = |time: u64, revocations: &RevocationSet| {
        chain.claimant(
            owner.public_key(),
            node.public_key(),
            (scope(3), b"invoices"),
            revocations,
            time,
        )
    };
    assert!(at(NOW + CLAIM_LIFETIME - 1, &none).is_ok());
    assert!(at(NOW + CLAIM_LIFETIME, &none).is_err());

    assert!(
        Authority::owner_in(
            &owner,
            crate::DatabaseContext {
                publisher: owner.public_key(),
                scope: scope(3)
            }
        )
        .unwrap()
        .issue_claims(holder.public_key(), every(), 1, (NOW, CLAIM_LIFETIME + 1),)
        .is_err(),
        "issuing refuses a longer link"
    );
    let mut long = chain.0[0].capability.clone();
    long.expires = long.not_before + CLAIM_LIFETIME + CLAIM_SKEW + 1;
    let long = CredentialChain(vec![SignedCapability::sign(long, &owner).unwrap()])
        .certify_claims(&holder, node.public_key())
        .unwrap();
    assert!(
        long.claimant(
            owner.public_key(),
            node.public_key(),
            (scope(3), b"invoices"),
            &none,
            NOW
        )
        .is_err()
    );
    let mut granting = chain.0[0].capability.clone();
    granting.grants = vec![rocky_core::ResourceGrant {
        scope: rocky_core::ResourceScope {
            domain: scope(3),
            encryption: [4; 32],
            table: [5; 32],
        },
        rows: rocky_core::RowSelection::All,
        rights: [rocky_core::Right::Read].into(),
        fields: rocky_core::FieldSelection::All,
    }];
    let granting = CredentialChain(vec![SignedCapability::sign(granting, &owner).unwrap()]);
    assert!(
        granting.links(owner.public_key(), None).is_err(),
        "a claim link carries no grants"
    );

    let statement = SignedRevocation::sign(
        Revocation {
            app: AppId([2; 16]),
            issuer: owner.public_key(),
            time: NOW,
            nodes: Default::default(),
            identities: [holder.public_key()].into(),
            capabilities: Default::default(),
            cut: Default::default(),
            mode: crate::RevocationMode::Lost,
            recovery: None,
        },
        &owner,
        &CredentialChain::default(),
    )
    .unwrap();
    let revocations = RevocationSet::try_from(vec![statement]).unwrap();
    assert!(at(NOW, &revocations).is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn delegation_never_exceeds_the_delegators_claim_right() {
    let (owner, holder, delegate, node) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let none = RevocationSet::new();
    let link = Authority::owner_in(
        &owner,
        crate::DatabaseContext {
            publisher: owner.public_key(),
            scope: scope(3),
        },
    )
    .unwrap()
    .issue_claims(
        holder.public_key(),
        only(&["invoices", "credits"]),
        2,
        (NOW, CLAIM_LIFETIME),
    )
    .unwrap();
    let held = CredentialChain(vec![link]);
    let delegate_with = |right: ClaimRight| {
        held.delegate_claims(
            &holder,
            delegate.public_key(),
            right,
            1,
            (NOW, CLAIM_LIFETIME),
        )
    };
    let narrowed = delegate_with(only(&["invoices"])).unwrap();
    let chain = narrowed
        .clone()
        .certify_claims(&delegate, node.public_key())
        .unwrap();
    let claim = |chain: &CredentialChain, counter: &str| {
        chain.claimant(
            owner.public_key(),
            node.public_key(),
            (scope(3), counter.as_bytes()),
            &none,
            NOW,
        )
    };
    assert_eq!(claim(&chain, "invoices").unwrap(), delegate.public_key());
    assert!(claim(&chain, "credits").is_err());
    assert!(
        delegate_with(only(&["invoices", "refunds"])).is_err(),
        "wider counters are refused"
    );
    assert!(delegate_with(every()).is_err());
    let mut elsewhere = only(&["invoices"]);
    elsewhere.scope = scope(4);
    assert!(delegate_with(elsewhere).is_err());
    assert!(
        narrowed
            .delegate_claims(
                &delegate,
                PrivateKey::new().public_key(),
                only(&["invoices"]),
                1,
                (NOW, CLAIM_LIFETIME),
            )
            .is_err(),
        "a delegate with no delegation left delegates nothing further"
    );
    let mut wide = narrowed.0[1].capability.clone();
    wide.extension = Extension::new(Extended {
        claims: Some(every()),
        ..Extended::default()
    })
    .unwrap();
    let wide = CredentialChain(vec![
        held.0[0].clone(),
        SignedCapability::sign(wide, &holder).unwrap(),
    ])
    .certify_claims(&delegate, node.public_key())
    .unwrap();
    assert!(
        claim(&wide, "refunds").is_err(),
        "a wider link signed by hand under the holder is refused by verifiers"
    );
    assert!(claim(&wide, "invoices").is_err());
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn an_issuer_grants_claims_and_a_node_certificate_carries_none() {
    let (owner, issuer, holder, node) = (
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
        PrivateKey::new(),
    );
    let issuer_link = Authority::owner_in(
        &owner,
        crate::DatabaseContext {
            publisher: owner.public_key(),
            scope: scope(3),
        },
    )
    .unwrap()
    .issue_issuer(
        issuer.public_key(),
        crate::Validity {
            issued_at: NOW,
            lifetime: std::time::Duration::from_secs(86_400 * 30),
            clock_skew: std::time::Duration::from_secs(60),
        },
    )
    .unwrap();
    let links = CredentialChain(vec![issuer_link]);
    let authority = crate::testing::authority(&issuer, &links);
    let link = authority
        .issue_claims(holder.public_key(), every(), 1, (NOW, CLAIM_LIFETIME))
        .unwrap();
    let chain = authority
        .chain_of(link)
        .certify_claims(&holder, node.public_key())
        .unwrap();
    assert_eq!(
        chain
            .claimant(
                owner.public_key(),
                node.public_key(),
                (scope(3), b"invoices"),
                &RevocationSet::new(),
                NOW,
            )
            .unwrap(),
        holder.public_key()
    );
    let mut leaf = chain.0[2].capability.clone();
    let mut extended = leaf.extended().unwrap();
    extended.claims = Some(every());
    leaf.extension = Extension::new(extended).unwrap();
    assert!(
        SignedCapability::sign(leaf, &holder).is_err(),
        "a node certificate that names a claim right itself is malformed"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
fn revoking_a_grant_ends_every_issuers_links_of_it_and_no_other_grant() {
    let (owner, holder, node) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let issuers = [PrivateKey::new(), PrivateKey::new()];
    let chain = |issuer: &PrivateKey, right: ClaimRight, time: u64| {
        let links = CredentialChain(vec![
            Authority::owner_in(
                &owner,
                crate::DatabaseContext {
                    publisher: owner.public_key(),
                    scope: scope(3),
                },
            )
            .unwrap()
            .issue_issuer(
                issuer.public_key(),
                crate::Validity {
                    issued_at: NOW,
                    lifetime: std::time::Duration::from_secs(86_400 * 30),
                    clock_skew: std::time::Duration::from_secs(60),
                },
            )
            .unwrap(),
        ]);
        let authority = crate::testing::authority(issuer, &links);
        let link = authority
            .issue_claims(holder.public_key(), right, 1, (time, CLAIM_LIFETIME))
            .unwrap();
        authority
            .chain_of(link)
            .certify_claims(&holder, node.public_key())
            .unwrap()
    };
    let first = chain(&issuers[0], every(), NOW);
    let later = NOW + CLAIM_LIFETIME + 60;
    let renewed = chain(&issuers[1], every(), later);
    let mut regranted = every();
    regranted.grant = [8; 32];
    let other = chain(&issuers[1], regranted, later);
    let claim = |chain: &CredentialChain, revocations: &RevocationSet, time: u64| {
        chain.claimant(
            owner.public_key(),
            node.public_key(),
            (scope(3), b"invoices"),
            revocations,
            time,
        )
    };
    let none = RevocationSet::new();
    assert!(claim(&first, &none, later).is_err());
    assert!(claim(&renewed, &none, later).is_ok());
    let statement = SignedRevocation::sign(
        Revocation {
            app: AppId([2; 16]),
            issuer: owner.public_key(),
            time: NOW,
            nodes: Default::default(),
            identities: Default::default(),
            capabilities: [every().grant].into(),
            cut: Default::default(),
            mode: crate::RevocationMode::Lost,
            recovery: None,
        },
        &owner,
        &CredentialChain::default(),
    )
    .unwrap();
    let revoked = RevocationSet::try_from(vec![statement]).unwrap();
    assert!(claim(&first, &revoked, NOW).is_err());
    assert!(claim(&renewed, &revoked, later).is_err());
    assert!(claim(&other, &revoked, later).is_ok());
}
