use p2panda_core::PrivateKey;
use std::process::Command;

#[test]
fn bare_relative_key_is_private_and_never_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let voter = crate::support::public::voter();
    let generate = || {
        Command::new(&voter)
            .current_dir(directory.path())
            .args(["key", "tenant.key"])
            .output()
    };
    let output = generate()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = directory.path().join("tenant.key");
    let secret = std::fs::read_to_string(&path)?;
    let key: PrivateKey = serde_json::from_value(serde_json::Value::String(secret.clone()))?;
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        key.public_key().to_hex()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
    }
    let retry = generate()?;
    assert!(!retry.status.success());
    assert!(String::from_utf8(retry.stderr)?.contains("secrets are never replaced"));
    assert_eq!(std::fs::read_to_string(path)?, secret);
    Ok(())
}
