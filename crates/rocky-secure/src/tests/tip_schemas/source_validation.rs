//! Held originals use exact source semantics under current authority.
use super::super::generations::identity_insert;
use super::*;
use crate::wire::{Chain, RowPayload};
use rocky_core::{ApplicationManifest, RowApplication, RowIdentity, SqlValue, TypedRows};

pub(in crate::tests) fn reseal(
    frame: &EncryptedFrame,
    node: &PrivateKey,
    key: &[u8; 32],
    chain: Chain,
    writes: Vec<RowWrite>,
    original: &RowPayload,
) -> EncryptedFrame {
    let operation = frame.operation_ref().unwrap();
    let body = match original.membership() {
        Some(membership) => RowPayload::bundle(membership.clone(), writes).unwrap(),
        None => RowPayload::rows(
            &writes
                .into_iter()
                .map(|write| write.change)
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    };
    let proofs = original
        .writes()
        .iter()
        .filter_map(|write| {
            original
                .proof(&write.change.row)
                .map(|proof| (write.change.row, proof.clone()))
        })
        .collect();
    let carried = original
        .writes()
        .iter()
        .filter_map(|write| {
            original
                .carried(&write.change.row)
                .map(|proof| (write.change.row, proof.clone()))
        })
        .collect();
    let body = RowPayload::stamp_body(&body, proofs, carried).unwrap();
    let body = RowPayload::reauthor_body(body, original.reauthors()).unwrap();
    let plaintext = original
        .compact_test(
            &RowPayload::witness(body, original.creations().to_vec()).unwrap(),
            operation.header.extensions.scope,
        )
        .unwrap();
    let mut extension = operation.header.extensions.clone();
    extension.chain = chain.digest();
    extension.births = original.rebound_births_for_test(&chain).unwrap();
    crate::wire::author(
        node,
        extension,
        chain,
        key,
        (
            operation.header.seq_num,
            operation.header.backlink,
            operation.header.timestamp,
        ),
        operation.header.previous.clone(),
        &plaintext,
    )
    .unwrap()
}

async fn original(lineage: &Lineage, dir: &TempDir, node: &PrivateKey) -> EncryptedFrame {
    let source = lineage
        .open(&dir.path().join("source.db"), node, 3, None)
        .await
        .unwrap();
    let sql = session(&source).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    for key in ["first", "later"] {
        identity_insert(
            &sql,
            "INSERT INTO items(key,title,note) VALUES(?,?,?)",
            vec![
                SqlValue::Text(key.into()),
                SqlValue::Text("source".into()),
                SqlValue::Text("preserved note".into()),
            ],
        )
        .await
        .unwrap();
    }
    let mut frames = sql
        .commit([145; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(frames.len(), 1);
    frames.remove(0)
}

async fn target(
    lineage: &Lineage,
    path: &std::path::Path,
    node: &PrivateKey,
) -> super::super::compaction::Replica {
    let replica = lineage.open(path, node, 3, None).await.unwrap();
    let statements = closed_statements(
        lineage.domain,
        replica.followed_tip().await,
        lineage.definition(4).hash(),
        lineage.publisher(),
    );
    let mut config = lineage.config(4, node);
    config.credentials = lineage.credentials(3, node);
    lineage
        .follow(&replica, config, 4, &statements)
        .await
        .unwrap();
    replica
}

#[tokio::test]
async fn a_signed_later_foreign_source_field_cannot_hide_behind_removal() {
    let lineage = Lineage::pinned(170);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[171; 32]);
    let original = original(&lineage, &dir, &node).await;
    let payload = super::staging::payload(&lineage, 3, &node, &original);
    let principal = PrivateKey::from_bytes(&[172; 32]);
    let mut grant = grant(
        lineage.scope("items"),
        &[
            Right::Read,
            Right::Custody,
            Right::Insert,
            Right::Update,
            Right::Delete,
        ],
        &[],
    );
    grant.fields = rocky_core::FieldSelection::All;
    let link = rocky_credentials::Authority::owner(lineage.publisher())
        .issue_terms(
            principal.public_key(),
            vec![grant],
            (1, Some(rocky_credentials::AuthorRange::ALL)),
            validity(),
        )
        .unwrap();
    let credentials = CredentialChain(vec![link])
        .complete(
            &principal,
            node.public_key(),
            node_number(node.public_key()),
        )
        .unwrap();
    let chain = Chain::new(credentials).unwrap();
    let good = reseal(
        &original,
        &node,
        &[40; 32],
        chain.clone(),
        payload.writes().to_vec(),
        &payload,
    );
    let mut writes = payload.writes().to_vec();
    assert_eq!(writes.len(), 2);
    let foreign = lineage.definition(3).item_id("field", "tags.label");
    writes[1]
        .change
        .after
        .as_mut()
        .unwrap()
        .insert(foreign, vec![0]);
    let bad = reseal(&original, &node, &[40; 32], chain, writes, &payload);
    // Final signed bytes and source-epoch opening are valid; credentials grant that field.
    bad.operation_ref().unwrap();
    let operation = bad.operation_ref().unwrap();
    let malformed = crate::wire::decrypt(
        &[40; 32],
        &encode(&operation.header.extensions).unwrap(),
        bad.body_bytes(),
    )
    .unwrap();
    assert!(
        RowPayload::decode_source(
            &malformed,
            &lineage.app(3).manifest(),
            operation.header.extensions.scope
        )
        .is_err()
    );
    let receiver = target(
        &lineage,
        &dir.path().join("bad.db"),
        &PrivateKey::from_bytes(&[173; 32]),
    )
    .await;
    let error = receiver
        .receive_held_frames(vec![bad.clone()])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("source table schema"), "{error}");
    let before = super::super::compaction::audit::snapshot(&receiver).await;
    let error = receiver.settle().await.unwrap_err();
    assert!(error.to_string().contains("source table schema"), "{error}");
    assert_eq!(
        super::super::compaction::audit::snapshot(&receiver).await,
        before
    );
    assert!(receiver.held_changes().await.is_err());
    let inner = receiver.inner.lock().await;
    for table in [
        "secure_held",
        "secure_held_frames",
        "secure_held_resolved",
        "secure_voids",
        "secure_applied",
        "secure_admission_claims",
        "secure_admission_entries",
        "secure_identity_marks",
        "secure_identity_own",
        "secure_identity_summary",
        "secure_identity_ledger",
    ] {
        assert!(
            crate::storage::rows(&inner.conn, &format!("SELECT * FROM {table}"))
                .await
                .unwrap()
                .is_empty(),
            "{table}"
        );
    }
    drop(inner);
    let control = target(
        &lineage,
        &dir.path().join("good.db"),
        &PrivateKey::from_bytes(&[174; 32]),
    )
    .await;
    control.receive_held_frames(vec![good]).await.unwrap();
    assert_eq!(control.held_changes().await.unwrap()[0].rows.len(), 2);
    control.audit(lineage.scope("items")).await.unwrap();
}

struct Unsupported(TypedRows);
impl RowApplication for Unsupported {
    fn manifest(&self) -> ApplicationManifest {
        self.0.manifest()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        self.0.permission_fields()
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        self.0.validate_changes(changes)
    }
}

struct Adapter {
    rows: TypedRows,
    factory: u8,
}
impl RowApplication for Adapter {
    fn manifest(&self) -> ApplicationManifest {
        self.rows.manifest()
    }
    fn permission_fields(&self) -> BTreeSet<Commitment> {
        if self.factory == 3 {
            [self.rows.manifest().item_id("field", "items.title")].into()
        } else {
            self.rows.permission_fields()
        }
    }
    fn validate_changes(&self, changes: &[RowChange]) -> Result<(), String> {
        self.rows.validate_changes(changes)
    }
    fn validate_write(&self, write: &RowWrite) -> Result<(), String> {
        self.rows.validate_write(write)
    }
    fn row_identity(&self, change: &RowChange) -> Result<Option<RowIdentity>, String> {
        self.rows.row_identity(change)
    }
    fn row_partition(
        &self,
        change: &RowChange,
    ) -> Result<Option<rocky_core::PartitionTuple>, String> {
        self.rows.row_partition(change)
    }
    fn source_application(
        &self,
        definition: &ApplicationManifest,
    ) -> Result<Box<dyn RowApplication>, String> {
        match self.factory {
            0 => Err("source row validation is unavailable".into()),
            1 => Ok(Box::new(self.rows.clone())),
            _ => self.rows.source_application(definition),
        }
    }
}

#[tokio::test]
async fn unsupported_and_wrong_manifest_factories_refuse_originals_without_installing_them() {
    let lineage = Lineage::pinned(175);
    let dir = TempDir::new().unwrap();
    let source_key = PrivateKey::from_bytes(&[176; 32]);
    let original = original(&lineage, &dir, &source_key).await;
    let key = PrivateKey::from_bytes(&[177; 32]);
    let path = dir.path().join("unsupported.db");
    drop(target(&lineage, &path, &key).await);
    let receiver =
        EncryptedReplica::open(&path, Unsupported(lineage.app(4)), lineage.config(3, &key))
            .await
            .unwrap();
    let error = receiver
        .receive_held_frames(vec![original.clone()])
        .await
        .unwrap_err();
    assert!(
        matches!(error, Error::Incompatible(_))
            && error
                .to_string()
                .contains("source row validation is unavailable"),
        "{error}"
    );
    let before = super::super::compaction::audit::snapshot(&receiver).await;
    assert!(matches!(
        receiver.settle().await,
        Err(Error::Incompatible(_))
    ));
    assert_eq!(
        super::super::compaction::audit::snapshot(&receiver).await,
        before
    );
    for factory in 1..=3 {
        let key = PrivateKey::from_bytes(&[177 + factory; 32]);
        let path = dir.path().join(format!("target{factory}.db"));
        drop(target(&lineage, &path, &key).await);
        let mut config = lineage.config(3, &key);
        config.manifest = lineage.config(3, &key).manifest;
        let receiver = EncryptedReplica::open(
            &path,
            Adapter {
                rows: lineage.app(4),
                factory,
            },
            config,
        )
        .await
        .unwrap();
        if factory < 2 {
            let error = receiver
                .receive_held_frames(vec![original.clone()])
                .await
                .unwrap_err();
            assert!(matches!(error, Error::Incompatible(_)), "{error}");
            let before = super::super::compaction::audit::snapshot(&receiver).await;
            assert!(matches!(
                receiver.settle().await,
                Err(Error::Incompatible(_))
            ));
            assert_eq!(
                super::super::compaction::audit::snapshot(&receiver).await,
                before
            );
            assert!(receiver.held_changes().await.is_err());
        } else if factory == 3 {
            let error = receiver
                .receive_held_frames(vec![original.clone()])
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("held row/field transition denied"),
                "{error}"
            );
            let before = super::super::compaction::audit::snapshot(&receiver).await;
            assert!(receiver.settle().await.is_err());
            assert_eq!(
                super::super::compaction::audit::snapshot(&receiver).await,
                before
            );
        } else {
            receiver
                .receive_held_frames(vec![original.clone()])
                .await
                .unwrap();
            assert_eq!(receiver.held_changes().await.unwrap()[0].rows.len(), 2);
        }
        assert_eq!(
            receiver.inner.lock().await.app.manifest().hash(),
            lineage.definition(4).hash()
        );
    }
}

pub(in crate::tests) fn closed_statements(
    scope: rocky_core::Scope,
    source: rocky_core::TipId,
    manifest: Commitment,
    publisher: &PrivateKey,
) -> Vec<TipStatement> {
    let close = SignedSeal::sign(
        rocky_credentials::SealStatement {
            format: rocky_credentials::SEAL_FORMAT,
            scope,
            tip: source,
            frontier: vec![],
            action: SealAction::Close,
            recovery: None,
        },
        publisher,
    )
    .unwrap();
    let opening = rocky_credentials::SignedTipOpening::sign(
        rocky_credentials::TipOpeningStatement {
            format: rocky_credentials::TIP_OPENING_FORMAT,
            scope,
            seal: close.id().unwrap(),
            schema: manifest,
            minimum_in_sync: 1,
        },
        publisher,
    )
    .unwrap();
    let tip = opening.tip().unwrap();
    vec![
        TipStatement::Seal(close),
        TipStatement::Opening(opening),
        TipStatement::Bookmark(
            rocky_credentials::SignedBookmarkMove::sign(
                rocky_credentials::BookmarkMoveStatement {
                    format: rocky_credentials::BOOKMARK_MOVE_FORMAT,
                    scope,
                    name: "main".into(),
                    from: None,
                    to: Some(tip),
                },
                publisher,
            )
            .unwrap(),
        ),
    ]
}

#[tokio::test]
async fn a_later_invalid_source_member_leaves_the_removed_table_bundle_unstored() {
    let lineage = Lineage::pinned(200);
    let dir = TempDir::new().unwrap();
    let node = PrivateKey::from_bytes(&[201; 32]);
    let source = lineage
        .open(&dir.path().join("source.db"), &node, 3, None)
        .await
        .unwrap();
    let sql = session(&source).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    identity_insert(
        &sql,
        "INSERT INTO items(key,title,note) VALUES('kept','source','non-NULL')",
        vec![],
    )
    .await
    .unwrap();
    identity_insert(
        &sql,
        "INSERT INTO tags(key,label) VALUES('removed','source tag')",
        vec![],
    )
    .await
    .unwrap();
    let originals = sql
        .commit([148; 32])
        .await
        .unwrap()
        .committed
        .unwrap()
        .frames;
    assert_eq!(originals.len(), 2);
    let principal = PrivateKey::from_bytes(&[202; 32]);
    let grants = ["items", "tags"]
        .iter()
        .map(|table| {
            let mut grant = grant(
                lineage.scope(table),
                &[
                    Right::Read,
                    Right::Custody,
                    Right::Insert,
                    Right::Update,
                    Right::Delete,
                ],
                &[],
            );
            grant.fields = rocky_core::FieldSelection::All;
            grant
        })
        .collect();
    let root = rocky_credentials::Authority::owner(lineage.publisher())
        .issue_terms(
            principal.public_key(),
            grants,
            (1, Some(rocky_credentials::AuthorRange::ALL)),
            validity(),
        )
        .unwrap();
    let chain = Chain::new(
        CredentialChain(vec![root])
            .complete(
                &principal,
                node.public_key(),
                node_number(node.public_key()),
            )
            .unwrap(),
    )
    .unwrap();
    let mut frames = Vec::new();
    for frame in &originals {
        let payload = super::staging::payload(&lineage, 3, &node, frame);
        let mut writes = payload.writes().to_vec();
        if frame.scope().unwrap() == lineage.scope("items") {
            writes[0].change.after.as_mut().unwrap().insert(
                lineage.definition(3).item_id("field", "tags.label"),
                vec![0],
            );
        }
        let key = if frame.scope().unwrap() == lineage.scope("items") {
            [40; 32]
        } else {
            [41; 32]
        };
        frames.push(reseal(frame, &node, &key, chain.clone(), writes, &payload));
    }
    // Holding visits members in full scope order, independent of delivery order.
    frames.sort_by_key(|frame| frame.scope().unwrap());
    assert_eq!(frames[0].scope().unwrap(), lineage.scope("tags"));
    assert_eq!(frames[1].scope().unwrap(), lineage.scope("items"));
    let reader = target(
        &lineage,
        &dir.path().join("bad.db"),
        &PrivateKey::from_bytes(&[203; 32]),
    )
    .await;
    let error = reader.receive_held_frames(frames).await.unwrap_err();
    assert!(error.to_string().contains("source table schema"), "{error}");
    let before = super::super::compaction::audit::snapshot(&reader).await;
    assert!(
        reader
            .settle()
            .await
            .unwrap_err()
            .to_string()
            .contains("source table schema")
    );
    assert_eq!(
        super::super::compaction::audit::snapshot(&reader).await,
        before
    );
    let inner = reader.inner.lock().await;
    for table in [
        "secure_held",
        "secure_held_frames",
        "secure_held_resolved",
        "secure_voids",
        "secure_applied",
        "secure_admission_claims",
        "secure_admission_entries",
        "secure_identity_marks",
        "secure_identity_own",
        "secure_identity_summary",
        "secure_identity_ledger",
    ] {
        assert!(
            crate::storage::rows(&inner.conn, &format!("SELECT * FROM {table}"))
                .await
                .unwrap()
                .is_empty(),
            "{table}"
        );
    }
    drop(inner);
    let control = target(
        &lineage,
        &dir.path().join("good.db"),
        &PrivateKey::from_bytes(&[204; 32]),
    )
    .await;
    control.receive_held_frames(originals).await.unwrap();
    assert_eq!(control.held_changes().await.unwrap()[0].rows.len(), 2);
    control
        .audit_scopes(&[lineage.scope("items"), lineage.scope("tags")])
        .await
        .unwrap();
}
