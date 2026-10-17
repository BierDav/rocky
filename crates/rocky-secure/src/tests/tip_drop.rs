use super::*;
use rocky_core::TipId;
use rocky_credentials::{
    Authority, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT, TipOpeningStatement,
};

fn bookmark(f: &Fixture, name: &str, from: Option<TipId>, to: TipId) -> TipStatement {
    TipStatement::Bookmark(
        SignedBookmarkMove::sign(
            BookmarkMoveStatement {
                format: BOOKMARK_MOVE_FORMAT,
                scope: f.public.domain,
                name: name.into(),
                from,
                to: Some(to),
            },
            &f.publisher,
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn dropping_branch_frames_preserves_main_and_requires_catchup_on_return() {
    let f = Fixture::new();
    let dir = TempDir::new().unwrap();
    let identity = PrivateKey::new();
    let node = PrivateKey::new();
    let chain = CredentialChain(vec![
        Authority::owner(&f.publisher)
            .issue(
                identity.public_key(),
                f.full(),
                rocky_credentials::AuthorRange::ALL,
                validity(),
            )
            .unwrap(),
    ]);
    let mut config = f.config(&node, f.full(), &[f.public]);
    config.credentials = chain
        .clone()
        .complete(&identity, node.public_key(), 1)
        .unwrap();
    let replica = EncryptedReplica::open(&dir.path().join("drop.db"), f.app(), config)
        .await
        .unwrap();
    let genesis = replica.followed_tip().await;
    replica
        .receive_tip_statements(&[bookmark(&f, "main", None, genesis)])
        .await
        .unwrap();
    replica
        .follow_bookmark(f.public.domain, "main")
        .await
        .unwrap();
    let seal = SignedSeal::sign(
        SealStatement {
            format: SEAL_FORMAT,
            scope: f.public.domain,
            tip: genesis,
            frontier: vec![],
            action: SealAction::Continue {
                schema: f.app().definition().hash(),
            },
            recovery: None,
        },
        &f.publisher,
    )
    .unwrap();
    let main = seal.successor().unwrap().unwrap();
    replica
        .receive_tip_statements(&[
            TipStatement::Seal(seal.clone()),
            bookmark(&f, "main", Some(genesis), main),
        ])
        .await
        .unwrap();
    let opening = SignedTipOpening::sign(
        TipOpeningStatement {
            format: TIP_OPENING_FORMAT,
            schema: f.manifest.verify(f.publisher.public_key()).unwrap().hash(),
            scope: f.public.domain,
            seal: seal.id().unwrap(),
            minimum_in_sync: 1,
        },
        &f.publisher,
    )
    .unwrap();
    let dev = opening.tip().unwrap();
    replica
        .receive_tip_statements(&[
            TipStatement::Opening(opening),
            bookmark(&f, "dev", None, dev),
        ])
        .await
        .unwrap();
    replica
        .follow_bookmark(f.public.domain, "dev")
        .await
        .unwrap();
    let branch_node = PrivateKey::new();
    replica
        .register_line_node(LineNode {
            credentials: chain
                .complete(&identity, branch_node.public_key(), 2)
                .unwrap(),
            key: branch_node.clone(),
        })
        .await
        .unwrap();
    let frame = replica
        .author(
            [1; 32],
            vec![change(f.public, 1, None, Some(f.app().name("Dev")))],
            vec![],
        )
        .await
        .unwrap();
    assert!(replica.drop_tip(dev).await.is_err());
    let peer = PrivateKey::new();
    let peer_chain = f.config(&peer, f.full(), &[f.public]).credentials;
    replica
        .admit_peer(peer.public_key(), &peer_chain, dev)
        .await
        .unwrap();
    replica
        .transfer_page(
            peer.public_key(),
            &peer_chain,
            &[f.public],
            None,
            MAX_SECURE_FRAME,
        )
        .await
        .unwrap();
    replica
        .follow_bookmark(f.public.domain, "main")
        .await
        .unwrap();
    assert!(replica.drop_tip(genesis).await.is_err());
    assert!(replica.drop_tip(dev).await.is_err());
    // The reader acknowledges the original frame offered before switching.
    replica
        .acknowledge(peer.public_key(), &peer_chain, frame.id().unwrap(), true)
        .await
        .unwrap();
    assert_eq!(replica.drop_tip(dev).await.unwrap(), 1);
    assert_eq!(replica.drop_tip(dev).await.unwrap(), 0);
    assert!(replica.snapshot(f.public).await.unwrap().rows.is_empty());
    let main_frame = replica
        .author(
            [2; 32],
            vec![change(f.public, 2, None, Some(f.app().name("Main")))],
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        main_frame.operation().unwrap().header.public_key,
        node.public_key()
    );
    replica
        .follow_bookmark(f.public.domain, "dev")
        .await
        .unwrap();
    assert!(
        replica
            .author(
                [3; 32],
                vec![change(f.public, 3, None, Some(f.app().name("Too early")))],
                vec![]
            )
            .await
            .is_err()
    );
    replica.receive(frame.clone()).await.unwrap();
    let next = replica
        .author(
            [4; 32],
            vec![change(f.public, 4, None, Some(f.app().name("Caught up")))],
            vec![],
        )
        .await
        .unwrap();
    let old = frame.operation().unwrap();
    let new = next.operation().unwrap();
    assert_eq!(new.header.public_key, branch_node.public_key());
    assert!(new.header.seq_num > old.header.seq_num);
    assert!(new.header.extensions.author_sequence > old.header.extensions.author_sequence);
}
