use super::*;
use rocky_credentials::{SignedTipOpening, TIP_OPENING_FORMAT, TipOpeningStatement};

pub(super) struct HeldPrefix {
    pub lineage: Lineage,
    pub directory: TempDir,
    pub key: PrivateKey,
    pub replica: Replica,
    pub frames: Vec<EncryptedFrame>,
    pub keeper: Option<(PrivateKey, Replica)>,
}
impl HeldPrefix {
    pub async fn new(count: usize) -> Self {
        Self::with_anchor(0, count).await
    }
    pub async fn with_anchor(prefix: usize, count: usize) -> Self {
        let lineage = Lineage::pinned(181);
        let directory = TempDir::new().unwrap();
        let key = PrivateKey::from_bytes(&[182; 32]);
        let peer = PrivateKey::from_bytes(&[183; 32]);
        let replica = open(&lineage, &directory, "a", &key).await;
        let keeper = open(&lineage, &directory, "b", &peer).await;
        follow_main(&lineage, &replica).await;
        for index in 0..prefix {
            insert(&replica, &format!("base{index}"), "installed prefix").await;
        }
        let earlier = seal(&lineage, &replica).await;
        if prefix > 0 {
            crate::tests::exchange_graphs(&replica, &keeper, lineage.domain).await;
            keeper
                .follow_bookmark(lineage.domain, "main")
                .await
                .unwrap();
            sync(&lineage, &replica, &keeper, &peer).await;
            assert_eq!(
                keeper.compact(lineage.domain, 0).await.unwrap().compacted,
                Some(earlier.id().unwrap())
            );
        }
        for index in 0..count {
            insert(&replica, &format!("old{index}"), "old line").await;
        }
        let frames = crate::tests::stored_frames(
            &replica,
            "SELECT header,body FROM secure_operations ORDER BY sequence",
        )
        .await;
        crate::tests::exchange_graphs(&replica, &keeper, lineage.domain).await;
        keeper
            .follow_bookmark(lineage.domain, "main")
            .await
            .unwrap();
        sync(&lineage, &replica, &keeper, &peer).await;
        let close = SignedSeal::sign(
            SealStatement {
                format: SEAL_FORMAT,
                scope: lineage.domain,
                tip: replica.followed_tip().await,
                frontier: replica.seal_frontier(lineage.domain, true).await.unwrap(),
                action: SealAction::Close,
                recovery: None,
            },
            lineage.publisher(),
        )
        .unwrap();
        let scope = lineage.scope("items");
        replica
            .set_role(crate::RoleConfig {
                role: crate::ReplicaRole::Partial,
                full_nodes: [rocky_credentials::PeerId::of(peer.public_key())].into(),
                custody: 1,
            })
            .await
            .unwrap();
        replica.subscribe(scope).await.unwrap();
        replica
            .acknowledge_frontier(
                peer.public_key(),
                &lineage.credentials(3, &peer),
                &keeper.frontier(scope).await.unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            replica.evict(scope).await.unwrap().operations,
            (prefix + count) as u64
        );
        replica.set_role(Default::default()).await.unwrap();
        let opening = SignedTipOpening::sign(
            TipOpeningStatement {
                format: TIP_OPENING_FORMAT,
                scope: lineage.domain,
                seal: earlier.id().unwrap(),
                schema: lineage.definition(3).hash(),
                minimum_in_sync: 1,
            },
            lineage.publisher(),
        )
        .unwrap();
        let target = opening.tip().unwrap();
        let from = replica.followed_tip().await;
        replica
            .publish_tip_statements(&[
                TipStatement::Seal(close),
                TipStatement::Opening(opening),
                bookmark(&lineage, Some(from), target),
            ])
            .await
            .unwrap();
        // Register the target's genuine line node as ordinary consumers do.
        let identity = PrivateKey::from_bytes(&rocky_core::commitment(
            "lineage.user",
            key.public_key().as_bytes(),
        ));
        let line = PrivateKey::from_bytes(&[185; 32]);
        replica
            .register_line_node(crate::LineNode {
                credentials: lineage.certified(3, &identity, &line),
                key: line,
            })
            .await
            .unwrap();
        assert_eq!(
            scalar(&replica, "SELECT count(*) FROM secure_logs").await,
            0
        );
        assert_eq!(
            scalar(&replica, "SELECT count(*) FROM secure_evicted_logs").await,
            1
        );
        if prefix > 0 {
            keeper
                .receive_tip_statements(&crate::tests::all_tip_statements(&replica).await)
                .await
                .unwrap();
        }
        Self {
            lineage,
            directory,
            key,
            replica,
            frames,
            keeper: Some((peer, keeper)),
        }
    }
    pub async fn waits(&self) {
        let scope = self.lineage.scope("items");
        self.replica.restored(scope).await.unwrap();
        assert_eq!(
            scalar(&self.replica, "SELECT count(*) FROM secure_evicted_logs").await,
            1
        );
        assert!(
            self.replica
                .session([scope].into())
                .await
                .unwrap()
                .sql(Default::default())
                .await
                .unwrap()
                .run("SELECT * FROM items", vec![])
                .await
                .is_err()
        );
        assert!(
            identity_insert(
                &session(&self.replica).await,
                "INSERT INTO items(key,title) VALUES('blocked','blocked')",
                vec![]
            )
            .await
            .is_err()
        );
    }
    pub async fn reopen(self) -> Self {
        let Self {
            lineage,
            directory,
            key,
            replica,
            frames,
            keeper,
        } = self;
        drop(replica);
        let replica = lineage
            .reopen(&directory.path().join("a.db"), &key, 3, 3)
            .await
            .unwrap();
        let identity = PrivateKey::from_bytes(&rocky_core::commitment(
            "lineage.user",
            key.public_key().as_bytes(),
        ));
        let line = PrivateKey::from_bytes(&[185; 32]);
        replica
            .register_line_node(crate::LineNode {
                credentials: lineage.certified(3, &identity, &line),
                key: line,
            })
            .await
            .unwrap();
        Self {
            lineage,
            directory,
            key,
            replica,
            frames,
            keeper,
        }
    }
}
pub(super) async fn held(replica: &Replica) -> Vec<Vec<SqlValue>> {
    rows(&replica.inner.lock().await.conn,"SELECT hash,header,body,status,(SELECT count(*) FROM secure_applied a WHERE a.hash=o.hash),(SELECT count(*) FROM secure_voids v WHERE v.hash=o.hash) FROM secure_operations o ORDER BY sequence").await.unwrap()
}
