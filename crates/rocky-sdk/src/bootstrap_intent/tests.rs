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

fn loaded(file: &BootstrapIntentFile) -> Result<BootstrapIntent, Failure> {
    file.load()?.ok_or_else(|| "no bootstrap intent".into())
}

#[test]
fn a_checkpoint_import_requested_after_a_join_keeps_its_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1))?;
    let started = loaded(&file)?;
    file.record_progress(&started, &[scope(5)], &[planned(5)])?;
    file.request_checkpoint_import()?;
    let intent = loaded(&file)?;
    assert_eq!(
        intent.inviter(),
        Some(key(1)),
        "learning a revocation before meeting the inviter must keep waiting for it"
    );
    assert_eq!(intent.imports(), &Imports::Granted);
    assert!(intent.totals().is_empty());
    Ok(())
}

#[test]
fn a_checkpoint_import_without_an_intent_imports_every_granted_scope() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    assert!(file.load()?.is_none());
    file.request_checkpoint_import()?;
    let intent = loaded(&file)?;
    assert_eq!(intent.imports(), &Imports::Granted);
    assert_eq!(intent.inviter(), None);
    Ok(())
}

#[test]
fn a_join_imports_every_granted_scope_and_waits_for_its_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.add_scopes(&[scope(5)])?;
    file.join(key(2))?;
    let intent = loaded(&file)?;
    assert_eq!(intent.imports(), &Imports::Granted);
    assert_eq!(intent.inviter(), Some(key(2)));
    Ok(())
}

#[test]
fn added_scopes_join_the_listed_imports_and_keep_the_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1))?;
    let started = loaded(&file)?;
    file.record_progress(&started, &[scope(5)], &[planned(5)])?;
    file.add_scopes(&[scope(5), scope(6)])?;
    let intent = loaded(&file)?;
    assert_eq!(intent.imports(), &Imports::Scopes(vec![scope(5), scope(6)]));
    assert_eq!(intent.totals(), &[planned(5)]);
    assert_eq!(intent.inviter(), Some(key(1)));
    Ok(())
}

#[test]
fn added_scopes_leave_an_import_of_every_granted_scope_unchanged() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.request_checkpoint_import()?;
    let before = loaded(&file)?;
    file.add_scopes(&[scope(5)])?;
    assert_eq!(loaded(&file)?, before);
    Ok(())
}

#[test]
fn added_scopes_without_an_intent_import_only_those_scopes() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.add_scopes(&[scope(5)])?;
    let intent = loaded(&file)?;
    assert_eq!(intent.imports(), &Imports::Scopes(vec![scope(5)]));
    assert_eq!(intent.inviter(), None);
    Ok(())
}

#[test]
fn recorded_progress_keeps_the_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1))?;
    let started = loaded(&file)?;
    let recorded = file.record_progress(&started, &[scope(5)], &[planned(5), planned(6)])?;
    assert_eq!(loaded(&file)?, recorded);
    assert_eq!(recorded.imports(), &Imports::Scopes(vec![scope(5)]));
    assert_eq!(recorded.totals(), &[planned(5), planned(6)]);
    assert_eq!(recorded.inviter(), Some(key(1)));
    Ok(())
}

#[test]
fn progress_is_refused_once_a_newer_intent_replaced_the_expected_one() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.add_scopes(&[scope(5)])?;
    let started = loaded(&file)?;
    file.request_checkpoint_import()?;
    let newer = loaded(&file)?;
    assert!(file.record_progress(&started, &[], &[]).is_err());
    assert!(
        file.finish(&started, key(1), &RevocationSet::new())
            .is_err()
    );
    assert_eq!(loaded(&file)?, newer);
    Ok(())
}

#[test]
fn a_finished_import_keeps_waiting_for_an_inviter_it_has_not_met() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1))?;
    let started = loaded(&file)?;
    let completed = file.record_progress(&started, &[], &[])?;
    assert_eq!(
        file.finish(&completed, key(2), &RevocationSet::new())?,
        Finished::AwaitingInviter
    );
    assert_eq!(loaded(&file)?, completed);
    Ok(())
}

#[test]
fn a_finished_import_clears_once_it_meets_its_inviter() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.join(key(1))?;
    let started = loaded(&file)?;
    let completed = file.record_progress(&started, &[], &[])?;
    assert_eq!(
        file.finish(&completed, key(1), &RevocationSet::new())?,
        Finished::Cleared
    );
    assert!(!file.exists());
    Ok(())
}

#[test]
fn a_finished_import_without_an_inviter_clears_on_any_peer() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.request_checkpoint_import()?;
    let started = loaded(&file)?;
    let completed = file.record_progress(&started, &[], &[])?;
    assert_eq!(
        file.finish(&completed, key(2), &RevocationSet::new())?,
        Finished::Cleared
    );
    assert!(!file.exists());
    Ok(())
}

#[test]
fn an_intent_in_another_format_is_rejected() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    let path = directory.path().join(BOOTSTRAP_INTENT_FILE);
    for other in [
        &b"true"[..],
        br#"{"intent":true,"inviter":"00"}"#,
        br#"{"version":2,"imports":"granted","totals":[],"inviter":null}"#,
        br#"{"version":1,"imports":"granted","totals":[],"inviter":null,"pending":[]}"#,
    ] {
        rocky_credentials::write_private(&path, other)?;
        assert!(file.load().is_err(), "{}", String::from_utf8_lossy(other));
        assert!(file.request_checkpoint_import().is_err());
        assert_eq!(std::fs::read(&path)?, other);
    }
    Ok(())
}

#[test]
fn the_persisted_intent_has_one_shape() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let file = BootstrapIntentFile::in_directory(directory.path());
    file.request_checkpoint_import()?;
    let written: serde_json::Value = serde_json::from_slice(&std::fs::read(
        directory.path().join(BOOTSTRAP_INTENT_FILE),
    )?)?;
    assert_eq!(
        written,
        serde_json::json!({"version": 1, "imports": "granted", "totals": [], "inviter": null})
    );
    Ok(())
}
