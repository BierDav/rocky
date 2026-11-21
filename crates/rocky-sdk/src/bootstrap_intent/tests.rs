use super::*;
use rocky_credentials::PrivateKey;

fn scope(table: u8) -> ResourceScope {
    ResourceScope {
        domain: rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        },
        encryption: [4; 32],
        table: [table; 32],
    }
}

fn key(seed: u8) -> PublicKey {
    PrivateKey::from_bytes(&[seed; 32]).public_key()
}

fn planned(table: u8) -> Planned {
    Planned {
        scope: scope(table),
        checkpoint: [table; 32],
        rows: u64::from(table),
    }
}

async fn loaded(file: &BootstrapIntentFile) -> Result<BootstrapIntent, Failure> {
    file.load()
        .await?
        .ok_or_else(|| "no bootstrap intent".into())
}

#[tokio::test]
async fn a_checkpoint_import_requested_after_a_join_keeps_its_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1)).await?;
    let started = loaded(&file).await?;
    file.record_progress(&started, &[scope(5)], &[planned(5)])
        .await?;
    file.request_checkpoint_import().await?;
    let intent = loaded(&file).await?;
    assert_eq!(
        intent.inviter(),
        Some(key(1)),
        "learning a revocation before meeting the inviter must keep waiting for it"
    );
    assert_eq!(intent.imports(), &Imports::Granted);
    assert!(intent.totals().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_checkpoint_import_without_an_intent_imports_every_granted_scope() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    assert!(file.load().await?.is_none());
    file.request_checkpoint_import().await?;
    let intent = loaded(&file).await?;
    assert_eq!(intent.imports(), &Imports::Granted);
    assert_eq!(intent.inviter(), None);
    Ok(())
}

#[tokio::test]
async fn a_join_imports_every_granted_scope_and_waits_for_its_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.add_scopes(&[scope(5)]).await?;
    file.join(key(2)).await?;
    let intent = loaded(&file).await?;
    assert_eq!(intent.imports(), &Imports::Granted);
    assert_eq!(intent.inviter(), Some(key(2)));
    Ok(())
}

#[tokio::test]
async fn added_scopes_join_the_listed_imports_and_keep_the_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1)).await?;
    let started = loaded(&file).await?;
    file.record_progress(&started, &[scope(5)], &[planned(5)])
        .await?;
    file.add_scopes(&[scope(5), scope(6)]).await?;
    let intent = loaded(&file).await?;
    assert_eq!(intent.imports(), &Imports::Scopes(vec![scope(5), scope(6)]));
    assert_eq!(intent.totals(), &[planned(5)]);
    assert_eq!(intent.inviter(), Some(key(1)));
    Ok(())
}

#[tokio::test]
async fn added_scopes_leave_an_import_of_every_granted_scope_unchanged() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.request_checkpoint_import().await?;
    let before = loaded(&file).await?;
    file.add_scopes(&[scope(5)]).await?;
    assert_eq!(loaded(&file).await?, before);
    Ok(())
}

#[tokio::test]
async fn added_scopes_without_an_intent_import_only_those_scopes() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.add_scopes(&[scope(5)]).await?;
    let intent = loaded(&file).await?;
    assert_eq!(intent.imports(), &Imports::Scopes(vec![scope(5)]));
    assert_eq!(intent.inviter(), None);
    Ok(())
}

#[tokio::test]
async fn recorded_progress_keeps_the_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1)).await?;
    let started = loaded(&file).await?;
    let recorded = file
        .record_progress(&started, &[scope(5)], &[planned(5), planned(6)])
        .await?;
    assert_eq!(loaded(&file).await?, recorded);
    assert_eq!(recorded.imports(), &Imports::Scopes(vec![scope(5)]));
    assert_eq!(recorded.totals(), &[planned(5), planned(6)]);
    assert_eq!(recorded.inviter(), Some(key(1)));
    Ok(())
}

#[tokio::test]
async fn progress_is_refused_once_a_newer_intent_replaced_the_expected_one() -> Result<(), Failure>
{
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.add_scopes(&[scope(5)]).await?;
    let started = loaded(&file).await?;
    file.request_checkpoint_import().await?;
    let newer = loaded(&file).await?;
    assert!(file.record_progress(&started, &[], &[]).await.is_err());
    assert!(
        file.finish(&started, key(1), &RevocationSet::new())
            .await
            .is_err()
    );
    assert_eq!(loaded(&file).await?, newer);
    Ok(())
}

#[tokio::test]
async fn a_finished_import_keeps_waiting_for_an_inviter_it_has_not_met() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1)).await?;
    let started = loaded(&file).await?;
    let completed = file.record_progress(&started, &[], &[]).await?;
    assert_eq!(
        file.finish(&completed, key(2), &RevocationSet::new())
            .await?,
        Finished::AwaitingInviter
    );
    assert_eq!(loaded(&file).await?, completed);
    Ok(())
}

#[tokio::test]
async fn a_finished_import_clears_once_it_meets_its_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1)).await?;
    let started = loaded(&file).await?;
    let completed = file.record_progress(&started, &[], &[]).await?;
    assert_eq!(
        file.finish(&completed, key(1), &RevocationSet::new())
            .await?,
        Finished::Cleared
    );
    assert!(!file.exists().await);
    Ok(())
}

#[tokio::test]
async fn a_finished_import_without_an_inviter_clears_on_any_peer() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.request_checkpoint_import().await?;
    let started = loaded(&file).await?;
    let completed = file.record_progress(&started, &[], &[]).await?;
    assert_eq!(
        file.finish(&completed, key(2), &RevocationSet::new())
            .await?,
        Finished::Cleared
    );
    assert!(!file.exists().await);
    Ok(())
}

#[tokio::test]
async fn an_intent_in_another_format_is_rejected() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    let database = directory.path().join(REPLICA_FILE);
    for other in [
        &b"true"[..],
        br#"{"intent":true,"inviter":"00"}"#,
        br#"{"version":2,"imports":"granted","totals":[],"inviter":null}"#,
        br#"{"version":1,"imports":"granted","totals":[],"inviter":null,"pending":[]}"#,
    ] {
        rocky_secure::write_authority_entry(&database, AUTHORITY_BOOTSTRAP_KEY, other, 1).await?;
        assert!(
            file.load().await.is_err(),
            "{}",
            String::from_utf8_lossy(other)
        );
        assert!(file.request_checkpoint_import().await.is_err());
        assert_eq!(
            rocky_secure::read_authority_entry(&database, AUTHORITY_BOOTSTRAP_KEY)?
                .map(|(bytes, _)| bytes),
            Some(other.to_vec())
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_persisted_intent_has_one_shape() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.request_checkpoint_import().await?;
    assert!(!directory.path().join("bootstrap.json").exists());
    let (bytes, _) = rocky_secure::read_authority_entry(
        &directory.path().join(REPLICA_FILE),
        AUTHORITY_BOOTSTRAP_KEY,
    )?
    .ok_or("missing bootstrap intent")?;
    let written: serde_json::Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        written,
        serde_json::json!({"version": 1, "imports": "granted", "totals": [], "inviter": null})
    );
    Ok(())
}
