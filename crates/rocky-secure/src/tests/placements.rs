use super::compaction::{
    Replica, follow_main, import_checkpoint, insert, list_checkpoints, open, publish_seal, scalar,
    seal, sign_seal, sync,
};
use super::generations::{Lineage, identity_insert, session};
use super::*;
use crate::{PlacementPage, PlacementRequest};
use std::collections::BTreeSet;
mod conflicts;

const PAGE_BOUNDARY_TABLE_GRANTS: u8 = 4;

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn placement_pages_bound_renewed_chains_and_continue_in_author_order() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let alice_key = PrivateKey::new();
    let bob_key = PrivateKey::new();
    let alice = lineage
        .open(&dir.path().join("alice.db"), &alice_key, 3, None)
        .await
        .unwrap();
    let bob = lineage
        .open_scoped(&dir.path().join("bob.db"), &bob_key, false, &["items"])
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    let template = lineage.credentials(3, &alice_key);
    for index in 0..64 {
        let mut root = template.0[0].capability.clone();
        for table in 0..PAGE_BOUNDARY_TABLE_GRANTS {
            let mut grant = root.grants[0].clone();
            grant.scope.table = rocky_core::commitment("placement.fixture.table", &[table]);
            root.grants.push(grant);
        }
        root.expires -= index;
        alice
            .renew_credentials(CredentialChain(vec![
                SignedCapability::sign(root, lineage.publisher()).unwrap(),
                template.certificate().unwrap().clone(),
            ]))
            .await
            .unwrap();
        identity_insert(
            &session(&alice).await,
            &format!("INSERT INTO tags(key,label) VALUES('t{index}','tag')"),
            vec![],
        )
        .await
        .unwrap();
    }
    seal(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    let credentials = chain(&bob).await;
    let mut pages = 0;
    let mut stored = 0;
    while let Some(request) = bob.wanted_placements().await.unwrap().first().copied() {
        assert!(pages < 64, "placement pagination stopped making progress");
        let page = alice
            .placement_page(bob_key.public_key(), &credentials, request)
            .await
            .unwrap();
        assert!(crate::encode(&page).unwrap().len() < MAX_SECURE_FRAME - 512);
        let count = bob.receive_placements(request, page).await.unwrap();
        assert!(count > 0, "the page repeated positions already received");
        stored += count;
        pages += 1;
    }
    assert_eq!(stored, 64);
    assert!(pages > 1, "renewed chains must span native messages");
    bob.finalize_path(lineage.domain).await.unwrap();
    assert!(
        bob.compaction_status(lineage.domain)
            .await
            .unwrap()
            .finalized
            .is_some()
    );
}

struct Pair {
    lineage: Lineage,
    _dir: Scratch,
    alice: Replica,
    bob: (PrivateKey, Replica),
    seal: Commitment,
}

async fn chain(replica: &Replica) -> CredentialChain {
    replica
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .credentials
        .clone()
}
async fn place(source: &Replica, target: &(PrivateKey, Replica)) -> usize {
    let credentials = chain(&target.1).await;
    let mut stored = 0;
    for request in target.1.wanted_placements().await.unwrap() {
        let page = source
            .placement_page(target.0.public_key(), &credentials, request)
            .await
            .unwrap();
        stored += target.1.receive_placements(request, page).await.unwrap();
    }
    stored
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_joiner_places_a_frame_its_newer_base_covers_to_finalize_an_older_base() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let keys = [PrivateKey::new(), PrivateKey::new()];
    let a = open(&lineage, &dir, "a", &keys[0]).await;
    follow_main(&lineage, &a).await;
    insert(&a, "one", "one").await;
    let first = seal(&lineage, &a).await.id().unwrap();
    a.compact(lineage.domain, 0).await.unwrap();
    identity_insert(
        &session(&a).await,
        "INSERT INTO tags(key,label) VALUES('t1','tag')",
        vec![],
    )
    .await
    .unwrap();
    let second = seal(&lineage, &a).await.id().unwrap();
    a.compact(lineage.domain, 0).await.unwrap();
    list_checkpoints(&lineage, &a).await;
    let joiner = (keys[1].clone(), open(&lineage, &dir, "b", &keys[1]).await);
    let b = &joiner.1;
    crate::tests::exchange_graphs(&a, b, lineage.domain).await;
    b.follow_bookmark(lineage.domain, "main").await.unwrap();
    let credentials = lineage.credentials(3, &keys[1]);
    a.admit_peer(keys[1].public_key(), &credentials, b.followed_tip().await)
        .await
        .unwrap();
    let mut bases = Vec::new();
    for scope in [lineage.scope("items"), lineage.scope("tags")] {
        let offered = a
            .checkpoint_offer(keys[1].public_key(), &credentials, scope)
            .await
            .unwrap()
            .unwrap();
        bases.push(offered.statement.seal);
    }
    assert_eq!(
        bases,
        [Some(first), Some(second)],
        "items keeps its base at the seal before the tags frame"
    );
    import_checkpoint(&lineage, &a, b, &keys[1], lineage.scope("items")).await;
    import_checkpoint(&lineage, &a, b, &keys[1], lineage.scope("tags")).await;
    b.finalize_path(lineage.domain).await.unwrap();
    let waiting = b
        .compaction_status(lineage.domain)
        .await
        .unwrap()
        .waiting
        .unwrap_or_default();
    assert!(waiting.contains("missing frames"), "{waiting}");
    assert_eq!(
        place(&a, &joiner).await,
        1,
        "the tags frame between the seals is placed although tags is held, since its base covers it"
    );
    b.finalize_path(lineage.domain).await.unwrap();
    let status = b.compaction_status(lineage.domain).await.unwrap();
    assert_eq!((status.finalized, status.waiting), (Some(second), None));
    b.audit(lineage.scope("items")).await.unwrap();
    b.audit(lineage.scope("tags")).await.unwrap();
}

async fn pair() -> Pair {
    pair_sealed_by(None).await
}
async fn pair_sealed_by(sealer: Option<&PrivateKey>) -> Pair {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let alice = lineage
        .open(&dir.path().join("alice.db"), &PrivateKey::new(), 3, None)
        .await
        .unwrap();
    let bob_key = PrivateKey::new();
    let bob = lineage
        .open_scoped(&dir.path().join("bob.db"), &bob_key, false, &["items"])
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    bob.follow_bookmark(lineage.domain, "main").await.unwrap();
    insert(&alice, "a", "one").await;
    for key in ["t1", "t2"] {
        identity_insert(
            &session(&alice).await,
            &format!("INSERT INTO tags(key,label) VALUES('{key}','tag')"),
            vec![],
        )
        .await
        .unwrap();
    }
    let seal = match sealer {
        None => seal(&lineage, &alice).await,
        Some(sealer) => {
            publish_seal(
                &lineage,
                &alice,
                trusted_seal(&lineage, &alice, sealer).await,
            )
            .await
        }
    }
    .id()
    .unwrap();
    sync(&lineage, &alice, &bob, &bob_key).await;
    Pair {
        lineage,
        _dir: dir,
        alice,
        bob: (bob_key, bob),
        seal,
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn full_role_and_an_ungranted_key_do_not_turn_placements_into_payload_custody() {
    let pair = pair().await;
    let scope = pair.lineage.scope("tags");
    let key = pair
        .alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .keys
        .iter()
        .find(|key| key.scope == scope)
        .unwrap()
        .clone();
    {
        let mut inner = pair
            .bob
            .1
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        inner.config.keys.push(key);
        assert!(inner.holds(scope).await.unwrap());
        inner.conn.execute(
            "INSERT OR IGNORE INTO secure_scope_state(scope,rows,tombstones,digest,applied) SELECT ?,0,0,zeroblob(length(digest)),0 FROM secure_scope_state LIMIT 1",
            [encode(&scope).unwrap()],
        ).await.unwrap();
    }
    assert_eq!(place(&pair.alice, &pair.bob).await, 2);
    assert_eq!(
        pair.bob
            .1
            .compact(pair.lineage.domain, 0)
            .await
            .unwrap()
            .compacted,
        Some(pair.seal)
    );
    assert_eq!(
        scalar(&pair.bob.1, "SELECT count(*) FROM tags").await,
        0,
        "retained crypto state is not a current custody grant"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_held_scope_requires_its_payload_even_when_its_key_is_missing() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let alice = lineage
        .open(&dir.path().join("a.db"), &PrivateKey::new(), 3, None)
        .await
        .unwrap();
    let key = PrivateKey::new();
    let bob = lineage
        .open_scoped(&dir.path().join("b.db"), &key, false, &["items"])
        .await
        .unwrap();
    follow_main(&lineage, &alice).await;
    insert(&alice, "a", "one").await;
    let seal = seal(&lineage, &alice).await.id().unwrap();
    crate::tests::exchange_graphs(&alice, &bob, lineage.domain).await;
    let keys = std::mem::take(
        &mut bob
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .config
            .keys,
    );
    let mark = alice.seal_frontier(lineage.domain, false).await.unwrap()[0];
    let request = PlacementRequest {
        seal,
        author: mark.author,
        from: 0,
        to: mark.sequence,
    };
    let credentials = chain(&bob).await;
    let page = alice
        .placement_page(key.public_key(), &credentials, request)
        .await
        .unwrap();
    assert_eq!(bob.receive_placements(request, page).await.unwrap(), 0);
    assert_eq!(
        scalar(&bob, "SELECT count(*) FROM secure_placements").await,
        0,
        "the grant still requires real payloads when no opening key is available"
    );
    bob.inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .keys = keys;
    assert_eq!(
        bob.compact(lineage.domain, 0).await.unwrap().compacted,
        None
    );
    sync(&lineage, &alice, &bob, &key).await;
    assert_eq!(
        bob.compact(lineage.domain, 0).await.unwrap().compacted,
        Some(seal)
    );
    bob.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_member_finalizes_and_compacts_a_seal_whose_tail_lies_in_a_scope_it_cannot_hold() {
    let pair = pair().await;
    let (lineage, bob) = (&pair.lineage, &pair.bob);
    let waiting = bob
        .1
        .compaction_status(lineage.domain)
        .await
        .unwrap()
        .waiting
        .unwrap_or_default();
    assert!(waiting.contains("missing frames"), "{waiting}");
    assert_eq!(
        bob.1.compact(lineage.domain, 0).await.unwrap().compacted,
        None
    );
    assert_eq!(
        place(&pair.alice, bob).await,
        2,
        "two `tags` positions are placed by their headers, without bodies"
    );
    assert!(bob.1.wanted_placements().await.unwrap().is_empty());
    assert_eq!(
        bob.1.compact(lineage.domain, 0).await.unwrap().compacted,
        Some(pair.seal)
    );
    assert_eq!(
        scalar(&bob.1, "SELECT count(*) FROM secure_placements").await,
        0,
        "the base folded the placements, so nothing consults them again"
    );
    bob.1.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_later_held_frame_places_gaps_before_an_older_seals_frontier() {
    let pair = pair().await;
    insert(&pair.alice, "b", "two").await;
    sync(&pair.lineage, &pair.alice, &pair.bob.1, &pair.bob.0).await;
    assert!(pair.bob.1.wanted_placements().await.unwrap().is_empty());
    assert_eq!(
        pair.bob
            .1
            .compact(pair.lineage.domain, 0)
            .await
            .unwrap()
            .compacted,
        Some(pair.seal)
    );
    assert_eq!(scalar(&pair.bob.1, "SELECT count(*) FROM items").await, 2);
    assert_eq!(scalar(&pair.bob.1, "SELECT count(*) FROM tags").await, 0);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn forged_out_of_range_held_scope_and_revoked_placements_are_refused() {
    let pair = pair().await;
    let (alice, bob) = (&pair.alice, &pair.bob);
    let credentials = chain(&bob.1).await;
    let requests = bob.1.wanted_placements().await.unwrap();
    let [request] = requests.as_slice() else {
        panic!("{requests:?}")
    };
    let credentials = &credentials;
    let answer = |request: PlacementRequest| async move {
        alice
            .placement_page(bob.0.public_key(), credentials, request)
            .await
    };
    let PlacementPage { headers, chains } = answer(*request).await.unwrap();
    assert_eq!(headers.len(), 2);
    let mut forged = headers.clone();
    let last = forged[0].0.len() - 1;
    forged[0].0[last] ^= 1;
    assert!(
        bob.1
            .receive_placements(
                *request,
                PlacementPage {
                    headers: forged,
                    chains: chains.clone(),
                },
            )
            .await
            .is_err(),
        "a forged signature is refused"
    );
    let narrow = PlacementRequest {
        to: request.from,
        ..*request
    };
    assert!(
        bob.1
            .receive_placements(
                narrow,
                PlacementPage {
                    headers: headers.clone(),
                    chains: chains.clone(),
                },
            )
            .await
            .is_err(),
        "headers outside the request are refused"
    );
    let past = PlacementRequest {
        to: request.to + 1,
        ..*request
    };
    assert!(
        answer(past).await.is_err(),
        "a request past the seal's frontier is refused"
    );
    assert!(
        bob.1
            .receive_placements(
                *request,
                PlacementPage {
                    headers: headers.clone(),
                    chains: Vec::new(),
                },
            )
            .await
            .is_err(),
        "a header without its chain is incomplete"
    );
    let signer = alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .node
        .clone();
    for wrong_domain in [true, false] {
        let mut altered: p2panda_core::Header<crate::wire::Extensions> =
            decode(&headers[0].0).unwrap();
        assert_eq!(altered.public_key, signer.public_key());
        if wrong_domain {
            altered.extensions.scope.domain.domain = rocky_core::DomainId([255; 16]);
        } else {
            altered.extensions.row_format = 0;
        }
        altered.sign(&signer);
        assert!(
            bob.1
                .receive_placements(
                    *request,
                    PlacementPage {
                        headers: vec![crate::Placement(altered.to_bytes())],
                        chains: chains.clone(),
                    }
                )
                .await
                .is_err(),
            "a genuine signature does not authorize a different domain or row format"
        );
    }
    let items = PlacementRequest {
        from: 0,
        to: 0,
        ..*request
    };
    let page = answer(items).await.unwrap();
    assert_eq!(
        bob.1.receive_placements(items, page).await.unwrap(),
        0,
        "a position of a scope bob holds needs the frame itself"
    );
    assert_eq!(
        scalar(&bob.1, "SELECT count(*) FROM secure_placements").await,
        0
    );
    let user = PrivateKey::from_bytes(&rocky_core::commitment(
        "lineage.user",
        bob.0.public_key().as_bytes(),
    ));
    let revocations = crate::tests::revoking(
        pair.lineage.publisher(),
        pair.lineage.domain.app,
        &[user.public_key()],
        &[],
    );
    alice.learn_revocations(&revocations).await.unwrap();
    assert!(
        answer(*request).await.is_err(),
        "a revoked requester is refused"
    );
    assert_eq!(
        scalar(&bob.1, "SELECT count(*) FROM secure_placements").await,
        0,
        "no refused page stored anything"
    );
    assert_eq!(
        bob.1
            .receive_placements(*request, PlacementPage { headers, chains })
            .await
            .unwrap(),
        2,
        "the genuine page still places both positions"
    );
}

async fn trusted_seal(
    lineage: &Lineage,
    replica: &Replica,
    sealer: &PrivateKey,
) -> rocky_credentials::SignedSeal {
    let chain = CredentialChain(vec![
        rocky_credentials::Authority::owner_in(
            lineage.publisher(),
            rocky_credentials::DatabaseContext {
                publisher: lineage.publisher().public_key(),
                scope: lineage.domain,
            },
        )
        .unwrap()
        .issue_trusted(sealer.public_key(), lineage.domain, now().unwrap())
        .unwrap(),
    ]);
    let node_chain = chain.clone().certify(sealer, sealer).unwrap();
    let tip = replica.followed_tip().await;
    let frontier = replica.seal_frontier(lineage.domain, false).await.unwrap();
    rocky_credentials::SignedSeal::endorse_linked(
        sign_seal(lineage, tip, frontier).statement,
        rocky_credentials::Signer::node(sealer, &node_chain).unwrap(),
        &chain,
        Some(rocky_credentials::ChainLink::first(sealer.public_key())),
    )
    .unwrap()
}

async fn revoke_alice_and_receive(
    pair: &Pair,
    mode: rocky_credentials::RevocationMode,
    also: &[PublicKey],
) -> Result<usize, crate::Error> {
    let request = pair.bob.1.wanted_placements().await.unwrap()[0];
    assert_eq!(request.seal, pair.seal);
    let credentials = chain(&pair.bob.1).await;
    let page = pair
        .alice
        .placement_page(pair.bob.0.public_key(), &credentials, request)
        .await
        .unwrap();
    let node = pair
        .alice
        .inner
        .lock(crate::replica::Class::Background)
        .await
        .config
        .node
        .public_key();
    let user = PrivateKey::from_bytes(&rocky_core::commitment("lineage.user", node.as_bytes()));
    let revoked: Vec<_> = [user.public_key()]
        .into_iter()
        .chain(also.iter().copied())
        .collect();
    pair.bob
        .1
        .learn_revocations(&crate::tests::revoking_as(
            pair.lineage.publisher(),
            pair.lineage.domain.app,
            &revoked,
            &[],
            mode,
            now().unwrap(),
        ))
        .await
        .unwrap();
    pair.bob.1.receive_placements(request, page).await
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_placement_from_a_compromised_author_is_refused() {
    let pair = pair().await;
    assert!(
        revoke_alice_and_receive(&pair, rocky_credentials::RevocationMode::Compromised, &[])
            .await
            .is_err()
    );
    assert_eq!(
        scalar(&pair.bob.1, "SELECT count(*) FROM secure_placements").await,
        0
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_departed_authors_placement_under_a_seal_past_the_revocation_cut_is_refused() {
    let sealer = PrivateKey::new();
    let pair = pair_sealed_by(Some(&sealer)).await;
    let refused = revoke_alice_and_receive(
        &pair,
        rocky_credentials::RevocationMode::Lost,
        &[sealer.public_key()],
    )
    .await;
    assert!(
        pair.bob
            .1
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .graph_for(pair.lineage.domain)
            .unwrap()
            .revoked
            .contains(&pair.seal),
        "a revocation without a cut puts every statement of its sealer past the cut"
    );
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("after its author's revocation"),
        "a seal past the cut no longer vouches for a departed author's positions"
    );
    assert_eq!(
        scalar(&pair.bob.1, "SELECT count(*) FROM secure_placements").await,
        0
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_frame_at_a_placed_position_replaces_it_and_another_header_is_equivocation() {
    let pair = pair().await;
    let (lineage, alice, bob) = (&pair.lineage, &pair.alice, &pair.bob);
    assert_eq!(place(alice, bob).await, 2);
    let placed: Vec<u8> = {
        let inner = bob.1.inner.lock(crate::replica::Class::Background).await;
        match crate::storage::rows(&inner.conn, "SELECT header FROM secure_placements LIMIT 1")
            .await
            .unwrap()
            .first()
            .map(Vec::as_slice)
        {
            Some([rocky_core::SqlValue::Blob(header)]) => header.clone(),
            other => panic!("{other:?}"),
        }
    };
    let header: p2panda_core::Header<crate::wire::Extensions> = decode(&placed).unwrap();
    let author = header.extensions.author(&header.public_key).unwrap();
    let position_of_alices_next_items_frame = {
        let inner = bob.1.inner.lock(crate::replica::Class::Background).await;
        let mut highest = 0;
        for record in crate::storage::rows(&inner.conn, "SELECT header FROM secure_placements")
            .await
            .unwrap()
        {
            let [rocky_core::SqlValue::Blob(bytes)] = record.as_slice() else {
                panic!()
            };
            let header: p2panda_core::Header<crate::wire::Extensions> = decode(bytes).unwrap();
            highest = highest.max(header.extensions.author_sequence);
        }
        highest + 1
    };
    {
        let inner = bob.1.inner.lock(crate::replica::Class::Background).await;
        inner
            .conn
            .execute(
                "INSERT INTO secure_placements VALUES(?,?,?,?,?,?)",
                crate::storage::bindings(vec![
                    rocky_core::SqlValue::Blob(encode(&lineage.domain).unwrap()),
                    crate::writer::writer_value(author),
                    rocky_core::SqlValue::Integer(position_of_alices_next_items_frame as i64),
                    rocky_core::SqlValue::Blob(encode(&header.extensions.scope).unwrap()),
                    rocky_core::SqlValue::Blob(header.hash().as_bytes().to_vec()),
                    rocky_core::SqlValue::Blob(placed.clone()),
                ]),
            )
            .await
            .unwrap();
    }
    insert(alice, "b", "two").await;
    sync(lineage, alice, &bob.1, &bob.0).await;
    assert_eq!(
        bob.1.equivocations().await.unwrap(),
        1,
        "a placement whose header is another frame's conflicts with the frame at its position"
    );
    assert_eq!(
        scalar(
            &bob.1,
            &format!(
                "SELECT count(*) FROM secure_placements WHERE sequence={position_of_alices_next_items_frame}"
            )
        )
        .await,
        0
    );
}

async fn sync_scope(
    lineage: &Lineage,
    source: &Replica,
    target: &Replica,
    key: &PrivateKey,
    scope: ResourceScope,
) {
    let credentials = chain(target).await;
    source
        .admit_peer(key.public_key(), &credentials, target.followed_tip().await)
        .await
        .unwrap();
    let mut after = None;
    loop {
        let frontier = target.frontier(scope).await.unwrap();
        let page = source
            .range_page(
                key.public_key(),
                &credentials,
                &frontier,
                after,
                crate::MAX_SECURE_FRAME,
            )
            .await
            .unwrap();
        for status in target.receive_voids(&page.voids).await {
            status.unwrap();
        }
        for status in target.receive_all(page.frames).await {
            status.unwrap();
        }
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    let _ = lineage;
}
fn revoking_node(
    lineage: &Lineage,
    node: PublicKey,
    mode: rocky_credentials::RevocationMode,
    time: u64,
) -> RevocationSet {
    let user = PrivateKey::from_bytes(&rocky_core::commitment("lineage.user", node.as_bytes()));
    crate::tests::revoking_as(
        lineage.publisher(),
        lineage.domain.app,
        &[user.public_key()],
        &[],
        mode,
        time,
    )
}

struct Relayed {
    lineage: Lineage,
    _dir: Scratch,
    relay_path: std::path::PathBuf,
    relay_key: PrivateKey,
    relay: Replica,
    bob: (PrivateKey, Replica),
    alice_node: PublicKey,
    alice_request: PlacementRequest,
    relay_request: PlacementRequest,
    items: PlacementRequest,
    alice_page: PlacementPage,
    items_page: PlacementPage,
    alice_headers: String,
    retained: i64,
    before_alice_wrote: u64,
}

impl Relayed {
    async fn new() -> Self {
        let before_alice_wrote = now().unwrap() - 1;
        let lineage = Lineage::new();
        let dir = Scratch::new().await;
        let keys = [PrivateKey::new(), PrivateKey::new(), PrivateKey::new()];
        let relay_path = dir.path().join("relay.db");
        let alice = lineage
            .open(&dir.path().join("alice.db"), &keys[0], 3, None)
            .await
            .unwrap();
        let relay = lineage.open(&relay_path, &keys[1], 3, None).await.unwrap();
        let bob = lineage
            .open_scoped(&dir.path().join("bob.db"), &keys[2], false, &["items"])
            .await
            .unwrap();
        follow_main(&lineage, &alice).await;
        for replica in [&relay, &bob] {
            crate::tests::exchange_graphs(&alice, replica, lineage.domain).await;
            replica
                .follow_bookmark(lineage.domain, "main")
                .await
                .unwrap();
        }
        let tag = |replica, key: &'static str| async move {
            identity_insert(
                &session(replica).await,
                &format!("INSERT INTO tags(key,label) VALUES('{key}','tag')"),
                vec![],
            )
            .await
            .unwrap();
        };
        insert(&alice, "a", "one").await;
        tag(&alice, "t1").await;
        tag(&alice, "t2").await;
        for name in ["items", "tags"] {
            sync_scope(&lineage, &alice, &relay, &keys[1], lineage.scope(name)).await;
        }
        tag(&alice, "t3").await;
        let third_tag_reaching_the_relay_only_as_a_void = {
            let inner = alice.inner.lock(crate::replica::Class::Background).await;
            match crate::storage::rows(
                &inner.conn,
                "SELECT header FROM secure_operations ORDER BY rowid DESC LIMIT 1",
            )
            .await
            .unwrap()
            .first()
            .map(Vec::as_slice)
            {
                Some([rocky_core::SqlValue::Blob(header)]) => header.clone(),
                other => panic!("{other:?}"),
            }
        };
        let void =
            crate::SignedVoid::sign(&keys[0], vec![third_tag_reaching_the_relay_only_as_a_void])
                .unwrap();
        for status in relay.receive_voids(&[void]).await {
            status.unwrap();
        }
        tag(&relay, "r1").await;
        let sealed = seal(&lineage, &relay).await.id().unwrap();
        crate::tests::exchange_graphs(&relay, &bob, lineage.domain).await;
        sync(&lineage, &relay, &bob, &keys[2]).await;
        let node = |replica: &Replica| {
            let replica = replica.clone();
            async move {
                replica
                    .inner
                    .lock(crate::replica::Class::Background)
                    .await
                    .config
                    .node
                    .public_key()
            }
        };
        let (alice_node, relay_node) = (node(&alice).await, node(&relay).await);
        let requests = bob.wanted_placements().await.unwrap();
        let (alice_author, relay_author) = (
            crate::writer::writer(&alice_node),
            crate::writer::writer(&relay_node),
        );
        let alice_request = *requests
            .iter()
            .find(|request| request.author == alice_author)
            .expect("alice's positions");
        let relay_request = *requests
            .iter()
            .find(|request| request.author == relay_author)
            .expect("the relay's positions");
        assert_eq!(alice_request.seal, sealed);
        let items = PlacementRequest {
            from: 0,
            to: 0,
            ..alice_request
        };
        let mut relayed = Self {
            lineage,
            _dir: dir,
            relay_path,
            relay_key: keys[1].clone(),
            relay,
            bob: (keys[2].clone(), bob),
            alice_node,
            alice_request,
            relay_request,
            items,
            alice_page: PlacementPage {
                headers: Vec::new(),
                chains: Vec::new(),
            },
            items_page: PlacementPage {
                headers: Vec::new(),
                chains: Vec::new(),
            },
            alice_headers: format!(
                "SELECT (SELECT count(*) FROM secure_operations WHERE author=x'{0}')+(SELECT count(*) FROM secure_voids WHERE author=x'{0}')",
                alice_node
                    .as_bytes()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            retained: 0,
            before_alice_wrote,
        };
        relayed.alice_page = relayed.answer(alice_request).await;
        assert_eq!(
            Self::headers_of(&relayed.alice_page),
            (3, 1),
            "before the revocation the relay places Alice's two tags and her void"
        );
        relayed.items_page = relayed.answer(items).await;
        assert_eq!(
            Self::headers_of(&relayed.items_page).0,
            1,
            "before the revocation the relay places Alice's `items` position bob holds"
        );
        relayed.retained = scalar(&relayed.relay, &relayed.alice_headers).await;
        assert_eq!(relayed.retained, 4);
        relayed
    }
    async fn answer(&self, request: PlacementRequest) -> PlacementPage {
        let credentials = chain(&self.bob.1).await;
        self.relay
            .placement_page(self.bob.0.public_key(), &credentials, request)
            .await
            .unwrap()
    }
    fn headers_of(page: &PlacementPage) -> (usize, usize) {
        (page.headers.len(), page.chains.len())
    }
    fn revoking_alice(&self, mode: rocky_credentials::RevocationMode, time: u64) -> RevocationSet {
        revoking_node(&self.lineage, self.alice_node, mode, time)
    }
    async fn placed(&self) -> i64 {
        scalar(&self.bob.1, "SELECT count(*) FROM secure_placements").await
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_relay_omits_a_compromised_authors_placements_and_still_places_others() {
    let relayed = Relayed::new().await;
    let (relay, bob) = (&relayed.relay, &relayed.bob.1);
    let compromised = relayed.revoking_alice(
        rocky_credentials::RevocationMode::Compromised,
        now().unwrap(),
    );
    relay.learn_revocations(&compromised).await.unwrap();
    assert_eq!(
        scalar(relay, &relayed.alice_headers).await,
        relayed.retained,
        "revoked headers stay physically held"
    );
    let omitted = relayed.answer(relayed.alice_request).await;
    assert_eq!(
        Relayed::headers_of(&omitted),
        (0, 0),
        "a compromised author's headers are no longer offered as evidence"
    );
    assert_eq!(
        Relayed::headers_of(&relayed.answer(relayed.items).await),
        (0, 0)
    );
    assert_eq!(
        bob.receive_placements(relayed.alice_request, omitted)
            .await
            .unwrap(),
        0
    );
    assert!(
        bob.wanted_placements()
            .await
            .unwrap()
            .contains(&relayed.alice_request)
    );
    let relay_page = relayed.answer(relayed.relay_request).await;
    assert_eq!(Relayed::headers_of(&relay_page), (1, 1));
    assert_eq!(
        bob.receive_placements(relayed.relay_request, relay_page)
            .await
            .unwrap(),
        1,
        "an unrevoked author's position still transfers"
    );
    bob.learn_revocations(&compromised).await.unwrap();
    let revision = bob.stored_and_memory_revision().await;
    let placed = relayed.placed().await;
    assert!(
        bob.receive_placements(relayed.alice_request, relayed.alice_page.clone())
            .await
            .is_err(),
        "a page captured before the compromise is refused whole once bob learns it"
    );
    assert!(
        bob.receive_placements(relayed.items, relayed.items_page.clone())
            .await
            .is_err(),
        "a captured page of a held scope is refused too"
    );
    assert_eq!(relayed.placed().await, placed);
    assert_eq!(
        bob.stored_and_memory_revision().await,
        revision,
        "a refused captured page changes nothing"
    );
    let Relayed {
        lineage,
        relay,
        relay_path,
        relay_key,
        bob,
        alice_request,
        alice_headers,
        retained,
        ..
    } = relayed;
    drop(relay);
    let relay = lineage.reopen(&relay_path, &relay_key, 3, 3).await.unwrap();
    relay.audit(lineage.scope("items")).await.unwrap();
    relay.audit(lineage.scope("tags")).await.unwrap();
    let page = relay
        .placement_page(bob.0.public_key(), &chain(&bob.1).await, alice_request)
        .await
        .unwrap();
    assert_eq!(
        Relayed::headers_of(&page),
        (0, 0),
        "reopened with its keys, the relay still omits a compromised author's headers"
    );
    assert_eq!(scalar(&relay, &alice_headers).await, retained);
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_departed_authors_placements_inside_the_seal_are_served_and_accepted() {
    let relayed = Relayed::new().await;
    let (relay, bob) = (&relayed.relay, &relayed.bob.1);
    let departed = relayed.revoking_alice(rocky_credentials::RevocationMode::Lost, now().unwrap());
    relay.learn_revocations(&departed).await.unwrap();
    bob.learn_revocations(&departed).await.unwrap();
    let served = relayed.answer(relayed.alice_request).await;
    assert_eq!(
        (&served.headers, &served.chains),
        (&relayed.alice_page.headers, &relayed.alice_page.chains),
        "what a departed author wrote before leaving is still offered as evidence"
    );
    assert_eq!(
        bob.receive_placements(relayed.alice_request, served)
            .await
            .unwrap(),
        3,
        "a departed author's sealed positions are accepted"
    );
    let relay_page = relayed.answer(relayed.relay_request).await;
    bob.receive_placements(relayed.relay_request, relay_page)
        .await
        .unwrap();
    assert!(
        bob.wanted_placements().await.unwrap().is_empty(),
        "no position of the departed author is left waiting"
    );
    bob.finalize_path(relayed.lineage.domain).await.unwrap();
    assert!(
        bob.compaction_status(relayed.lineage.domain)
            .await
            .unwrap()
            .finalized
            .is_some(),
        "the seal finalizes after its author departed"
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn a_departed_authors_placements_written_after_the_revocation_are_refused() {
    let relayed = Relayed::new().await;
    let (relay, bob) = (&relayed.relay, &relayed.bob.1);
    let departed_earlier = relayed.revoking_alice(
        rocky_credentials::RevocationMode::Lost,
        relayed.before_alice_wrote,
    );
    relay.learn_revocations(&departed_earlier).await.unwrap();
    assert_eq!(
        Relayed::headers_of(&relayed.answer(relayed.alice_request).await),
        (0, 0),
        "frames stamped after the revocation are not offered"
    );
    bob.learn_revocations(&departed_earlier).await.unwrap();
    let refused = bob
        .receive_placements(relayed.alice_request, relayed.alice_page.clone())
        .await;
    assert!(
        refused
            .unwrap_err()
            .to_string()
            .contains("after its author's revocation")
    );
    assert_eq!(relayed.placed().await, 0, "a refused page stores nothing");
}
