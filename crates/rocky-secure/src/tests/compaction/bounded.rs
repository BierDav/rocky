use super::*;
use crate::replica::CrashPoint;

async fn populate(replica: &Replica, count: usize) {
    let sql = session(replica).await;
    sql.run("BEGIN", vec![]).await.unwrap();
    for index in 0..count {
        identity_insert(
            &sql,
            "INSERT INTO items(key,title) VALUES(?,?)",
            vec![text(&format!("k{index:04}")), text("sealed")],
        )
        .await
        .unwrap();
    }
    sql.run("COMMIT", vec![]).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn base_preparation_keeps_reads_and_successful_writes_available() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "bounded", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    let count = Replica::BASE_TRANSITION_ROWS * 2 + 1;
    populate(&replica, count).await;
    let sealed = seal(&lineage, &replica).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    let measurements = replica.lock_measurements();
    measurements.start();
    let (reached, resume) = replica.pause_base_prepare().await;
    let compact = replica.compact(lineage.domain, 0);
    let interleave = async {
        reached.await.unwrap();
        assert_eq!(titles(&replica).await.len(), count);
        // More than STARVATION_YIELDS writes arrive during an unlocked step.
        // They succeed and do not extend the immutable source cut.
        for index in 0..12 {
            insert(&replica, &format!("live{index}"), "after seal").await;
        }
        update(&replica, "k0000", "after seal").await;
        assert_eq!(titles(&replica).await.len(), count + 12);
        resume.send(()).unwrap();
    };
    let (report, ()) = tokio::join!(compact, interleave);
    assert_eq!(report.unwrap().compacted, Some(sealed.id().unwrap()));
    let checkpoint = replica.checkpoint(lineage.scope("items")).await.unwrap();
    assert_eq!(checkpoint.statement.rows.count, count as u64);
    assert_eq!(titles(&replica).await.len(), count + 12);
    replica.audit(lineage.scope("items")).await.unwrap();
    let measured = measurements.stop();
    let mut hashed = 0;
    let mut publications = 0;
    for holder in measured.holders.values() {
        for (unit, samples) in &holder.units {
            match unit.as_str() {
                "transition_hash_row" => {
                    assert!(
                        samples
                            .iter()
                            .all(|(work, _)| *work <= Replica::BASE_TRANSITION_ROWS as u64)
                    );
                    hashed += samples.iter().map(|(work, _)| work).sum::<u64>();
                }
                "transition_cleanup_row" => assert!(
                    samples
                        .iter()
                        .all(|(work, _)| *work <= Replica::BASE_TRANSITION_ROWS as u64)
                ),
                "transition_publication" => {
                    publications += samples.iter().map(|(work, _)| work).sum::<u64>()
                }
                _ => {}
            }
        }
    }
    assert_eq!(hashed, count as u64);
    assert!(publications > 0);
    assert!(measured.interactive_steps.iter().all(|steps| *steps <= 1));
    assert_eq!(
        scalar(&replica, "SELECT count(*) FROM secure_base_transition").await,
        0
    );
    assert_eq!(
        scalar(&replica, "SELECT count(*) FROM secure_replayed_base_rows").await,
        0
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn base_metadata_preparation_is_bounded_per_acquisition() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "bounded-metadata", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    populate(&replica, Replica::BASE_TRANSITION_ROWS * 2 + 3).await;
    seal(&lineage, &replica).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    let measurements = replica.lock_measurements();
    measurements.start();
    replica.compact(lineage.domain, 0).await.unwrap();
    let measured = measurements.stop();
    assert!(
        measured
            .holders
            .values()
            .any(|holder| holder.units.contains_key("transition_history_record")),
        "base history was not prepared in steps"
    );
    for holder in measured.holders.values() {
        for (unit, samples) in &holder.units {
            if matches!(
                unit.as_str(),
                "transition_history_record"
                    | "transition_identity_record"
                    | "transition_ledger_record"
                    | "transition_outcome_record"
            ) {
                assert!(
                    samples
                        .iter()
                        .all(|(work, _)| *work <= Replica::BASE_TRANSITION_ROWS as u64),
                    "{unit} exceeded one bounded acquisition"
                );
            }
        }
    }
    replica.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn interrupted_base_steps_resume_without_mixed_or_double_applied_state() {
    for point in [
        CrashPoint::BasePrepare,
        CrashPoint::BasePrepared,
        CrashPoint::Compact,
        CrashPoint::BasePublished,
        CrashPoint::BaseCleanup,
        CrashPoint::BaseCleaned,
    ] {
        let lineage = Lineage::new();
        let dir = Scratch::new().await;
        let key = PrivateKey::new();
        let replica = open(&lineage, &dir, "crash", &key).await;
        follow_main(&lineage, &replica).await;
        populate(&replica, Replica::BASE_TRANSITION_ROWS + 1).await;
        seal(&lineage, &replica).await;
        replica.compact(lineage.domain, 0).await.unwrap();
        let old = replica.checkpoint(lineage.scope("items")).await.unwrap();
        session(&replica)
            .await
            .run("UPDATE items SET title='new'", vec![])
            .await
            .unwrap();
        let latest = seal(&lineage, &replica).await;
        replica.finalize_path(lineage.domain).await.unwrap();
        let expected = titles(&replica).await;
        replica.arm_crash(Some(point)).await;
        assert!(
            matches!(
                replica.compact(lineage.domain, 0).await,
                Err(Error::InjectedFailure)
            ),
            "{point:?}"
        );
        assert_eq!(replica.crashed().await, Some(point));
        // The selector and signed checkpoint always agree, including after
        // publication but before the first cleanup commit.
        let current = replica.checkpoint(lineage.scope("items")).await.unwrap();
        assert!(
            current.id().unwrap() == old.id().unwrap()
                || current.statement.seal == Some(latest.id().unwrap())
        );
        assert_eq!(titles(&replica).await, expected);
        replica.audit(lineage.scope("items")).await.unwrap();
        drop(replica);
        let replica = open(&lineage, &dir, "crash", &key).await;
        replica.compact(lineage.domain, 0).await.unwrap();
        assert_eq!(titles(&replica).await, expected);
        let current = replica.checkpoint(lineage.scope("items")).await.unwrap();
        assert_eq!(current.statement.seal, Some(latest.id().unwrap()));
        assert_eq!(
            current.statement.rows.count,
            (Replica::BASE_TRANSITION_ROWS + 1) as u64
        );
        replica.audit(lineage.scope("items")).await.unwrap();
        assert_eq!(
            scalar(&replica, "SELECT count(*) FROM secure_base_transition").await,
            0
        );
        assert_eq!(
            scalar(&replica, "SELECT count(*) FROM secure_base_cleanup").await,
            0
        );
    }
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_unchanged_base_never_uses_a_partially_retired_snapshot_after_reopen() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = open(&lineage, &dir, "unchanged", &key).await;
    follow_main(&lineage, &replica).await;
    populate(&replica, Replica::BASE_TRANSITION_ROWS + 1).await;
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let before = replica.checkpoint(lineage.scope("items")).await.unwrap();
    seal(&lineage, &replica).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    replica.arm_crash(Some(CrashPoint::BaseCleaned)).await;
    assert!(matches!(
        replica.compact(lineage.domain, 0).await,
        Err(Error::InjectedFailure)
    ));
    assert!(scalar(&replica, "SELECT count(*) FROM secure_replayed_seal_rows r WHERE NOT EXISTS(SELECT 1 FROM secure_replayed_seals s WHERE s.scope=r.scope AND s.seal=r.seal AND s.version=r.version)").await > 0);
    drop(replica);
    let replica = open(&lineage, &dir, "unchanged", &key).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let after = replica.checkpoint(lineage.scope("items")).await.unwrap();
    assert_eq!(after.id().unwrap(), before.id().unwrap());
    assert_eq!(
        after.statement.rows.count,
        (Replica::BASE_TRANSITION_ROWS + 1) as u64
    );
    replica.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn re_finalization_preserves_a_selected_generation_and_invalidates_preparation() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "versions", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    populate(&replica, Replica::BASE_TRANSITION_ROWS + 1).await;
    let sealed = seal(&lineage, &replica).await.id().unwrap();
    replica.compact(lineage.domain, 0).await.unwrap();
    let checkpoint = replica.checkpoint(lineage.scope("items")).await.unwrap();
    replica
        .retake_seal_rows(lineage.scope("items"), sealed)
        .await;
    assert!(scalar(&replica, "SELECT count(*) FROM secure_replayed_base_source b JOIN secure_replayed_seals s ON s.scope=b.scope AND s.seal=b.seal WHERE s.version<>b.version").await > 0);
    replica.audit(lineage.scope("items")).await.unwrap();
    let (reached, resume) = replica.pause_base_prepare().await;
    let (result, ()) = tokio::join!(replica.compact(lineage.domain, 0), async {
        reached.await.unwrap();
        replica
            .retake_seal_rows(lineage.scope("items"), sealed)
            .await;
        replica.audit(lineage.scope("items")).await.unwrap();
        resume.send(()).unwrap();
    });
    result.unwrap();
    assert_eq!(
        replica
            .checkpoint(lineage.scope("items"))
            .await
            .unwrap()
            .id()
            .unwrap(),
        checkpoint.id().unwrap()
    );
    replica.audit(lineage.scope("items")).await.unwrap();
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn linked_bases_publish_atomically_across_failure_and_reopen() {
    let mut lineage = Lineage::new();
    lineage.linked = true;
    let dir = Scratch::new().await;
    let key = PrivateKey::new();
    let replica = open(&lineage, &dir, "linked", &key).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "parent", "first").await;
    let id = select(&replica, "SELECT id FROM items").await[0][0].clone();
    identity_insert(
        &session(&replica).await,
        "INSERT INTO tags(key,label,item,also) VALUES('child','kept',?,?)",
        vec![id.clone(), id],
    )
    .await
    .unwrap();
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    let old_items = replica
        .checkpoint(lineage.scope("items"))
        .await
        .unwrap()
        .id()
        .unwrap();
    let old_tags = replica
        .checkpoint(lineage.scope("tags"))
        .await
        .unwrap()
        .id()
        .unwrap();
    delete(&replica, "parent").await;
    let latest = rivalled_seal(&lineage, &replica).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    replica.arm_crash(Some(CrashPoint::Compact)).await;
    assert!(matches!(
        replica.compact(lineage.domain, 0).await,
        Err(Error::InjectedFailure)
    ));
    assert_eq!(
        replica
            .checkpoint(lineage.scope("items"))
            .await
            .unwrap()
            .id()
            .unwrap(),
        old_items
    );
    assert_eq!(
        replica
            .checkpoint(lineage.scope("tags"))
            .await
            .unwrap()
            .id()
            .unwrap(),
        old_tags
    );
    replica.arm_crash(Some(CrashPoint::BasePublished)).await;
    assert!(matches!(
        replica.compact(lineage.domain, 0).await,
        Err(Error::InjectedFailure)
    ));
    drop(replica);
    let replica = open(&lineage, &dir, "linked", &key).await;
    assert_eq!(
        replica
            .compaction_status(lineage.domain)
            .await
            .unwrap()
            .compacted,
        Some(latest.id().unwrap())
    );
    for scope in [lineage.scope("items"), lineage.scope("tags")] {
        let checkpoint = replica.checkpoint(scope).await.unwrap();
        assert_eq!(checkpoint.statement.seal, Some(latest.id().unwrap()));
        assert_eq!(checkpoint.statement.rows.count, 0);
        replica.audit(scope).await.unwrap();
    }
    let (_, statements) = rival(&lineage, &latest);
    replica.receive_tip_statements(&statements).await.unwrap();
    assert_eq!(
        replica
            .compaction_status(lineage.domain)
            .await
            .unwrap()
            .rebootstrap,
        Some(latest.id().unwrap())
    );
}

#[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
async fn an_old_cleanup_depth_preserves_current_uncompacted_snapshots() {
    let lineage = Lineage::new();
    let dir = Scratch::new().await;
    let replica = open(&lineage, &dir, "retirement", &PrivateKey::new()).await;
    follow_main(&lineage, &replica).await;
    insert(&replica, "first", "base").await;
    seal(&lineage, &replica).await;
    replica.compact(lineage.domain, 0).await.unwrap();
    insert(&replica, "second", "new cut").await;
    let latest = seal(&lineage, &replica).await;
    replica.finalize_path(lineage.domain).await.unwrap();
    let pending = scalar(&replica, "SELECT count(*) FROM secure_replayed_seals s JOIN secure_finalized f ON f.seal=s.seal WHERE f.compacted=0").await;
    assert!(pending > 0);
    // A losing branch can leave a queue whose depth exceeds every seal on
    // the replacement path. Depth alone must not retire that path's input.
    replica
        .cleanup_bases_through(lineage.scope("items"), i64::MAX)
        .await;
    assert_eq!(scalar(&replica, "SELECT count(*) FROM secure_replayed_seals s JOIN secure_finalized f ON f.seal=s.seal WHERE f.compacted=0").await, pending);
    replica.compact(lineage.domain, 0).await.unwrap();
    let checkpoint = replica.checkpoint(lineage.scope("items")).await.unwrap();
    assert_eq!(checkpoint.statement.seal, Some(latest.id().unwrap()));
    assert_eq!(checkpoint.statement.rows.count, 2);
    replica.audit(lineage.scope("items")).await.unwrap();
}
