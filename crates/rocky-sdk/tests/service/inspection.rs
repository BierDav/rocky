use super::*;
use std::path::Path;

pub(super) fn inspect_file(directory: &Path) -> Result<rusqlite::Connection, Failure> {
    Ok(rusqlite::Connection::open_with_flags(
        directory.join("rocky.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

pub(super) fn stage(directory: &Path) -> Result<(rocky_secure::SchemaStage, String), Failure> {
    let (publication, bytes, state): (Vec<u8>, Vec<u8>, String) = inspect_file(directory)?
        .query_row(
            "SELECT publication,stage,state FROM secure_staged_schema",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let stage: rocky_secure::SchemaStage =
        rocky_sdk::credentials::decode_bounded(&bytes, bytes.len())?;
    assert_eq!(publication, stage.publication);
    assert_eq!(rocky_sdk::credentials::encode(&stage)?, bytes);
    Ok((stage, state))
}

pub(super) async fn head(
    bundle: &TrustBundle,
    directory: &Path,
    scope: &Value,
    tip: &Value,
) -> Result<rocky_coordinator::coordinated::Head, Failure> {
    use rocky_sdk::{
        credentials::{CredentialChain, Vault},
        transport::Connector,
    };
    let settings: Value = serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
    let vault = Vault::new(
        Arc::new(rocky_sdk::FileSecretStore::new(directory.join("secrets"))),
        settings["installation"].as_str().ok_or("installation")?,
    );
    let transport = starter()
        .start(false, *PrivateKey::new().as_bytes())
        .await?;
    let client = NetworkCoordinator::connect(
        bundle,
        PrivateKey::from_bytes(&vault.get("node")?),
        transport.clone(),
    )?;
    let credentials: CredentialChain = serde_json::from_value(settings["credentials"].clone())?;
    client.certify(credentials.0.last().ok_or("node certificate")?.clone());
    let tip: [u8; 32] = data_encoding::HEXLOWER
        .decode(tip.as_str().ok_or("tip")?.as_bytes())?
        .try_into()
        .map_err(|_| "tip length")?;
    let result = client
        .coordinated_head(rocky_core::TipLog {
            scope: serde_json::from_value(scope.clone())?,
            tip: rocky_core::TipId(tip),
        })
        .await;
    transport.close().await;
    Ok(result?)
}
