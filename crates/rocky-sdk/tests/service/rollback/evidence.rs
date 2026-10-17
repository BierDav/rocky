use super::*;
use rusqlite::OptionalExtension;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Endpoint {
    sequence: i64,
    hash: Vec<u8>,
    scope: Vec<u8>,
    author: Vec<u8>,
    header: Vec<u8>,
    body: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Original {
    endpoint: Endpoint,
    held_id: Vec<u8>,
    held_frame: Vec<u8>,
}

pub(super) fn endpoint(directory: &Path) -> Result<Endpoint, Failure> {
    let settings: Value = serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
    let chain: rocky_sdk::credentials::CredentialChain =
        serde_json::from_value(settings["credentials"].clone())?;
    let node = chain.0.last().ok_or("node certificate")?.capability.subject;
    let conn = inspect_file(directory)?;
    let endpoint = conn.query_row(
        "SELECT o.sequence,o.hash,o.scope,o.author,o.header,o.body FROM secure_logs l JOIN secure_operations o ON o.hash=l.hash WHERE l.author=?1 AND o.author=?1 AND o.sequence=l.sequence",
        [node.as_bytes().as_slice()],
        |row| Ok(Endpoint { sequence: row.get(0)?, hash: row.get(1)?, scope: row.get(2)?, author: row.get(3)?, header: row.get(4)?, body: row.get(5)? }),
    )?;
    assert_eq!(endpoint.sequence, 0);
    assert_eq!(endpoint.author, node.as_bytes());
    Ok(endpoint)
}

pub(super) fn guarded(directory: &Path, expected: &Endpoint) -> Result<Original, Failure> {
    let conn = inspect_file(directory)?;
    let guard: Option<(i64, Vec<u8>)> = conn
        .query_row(
            "SELECT sequence,hash FROM secure_evicted_logs WHERE scope=?",
            [&expected.scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    // Prefix recovery may finish while the separate wanted-seal gate still waits.
    if let Some((sequence, hash)) = guard {
        assert_eq!(sequence, expected.sequence);
        assert_eq!(hash, expected.hash);
    }
    let original = original(&conn, &expected.hash)?;
    assert_eq!(&original.endpoint, expected);
    let frame = rocky_secure::EncryptedFrame::from_bytes(&original.held_frame)?;
    assert_eq!(frame.id()?.as_bytes().as_slice(), expected.hash);
    Ok(original)
}

fn original(conn: &rusqlite::Connection, hash: &[u8]) -> Result<Original, Failure> {
    Ok(conn.query_row("SELECT o.sequence,o.hash,o.scope,o.author,o.header,o.body,h.id,h.frame,o.status FROM secure_operations o JOIN secure_held_frames h ON h.hash=o.hash WHERE o.hash=?", [hash], |row| {
        let status: String = row.get(8)?;
        assert_eq!(status, "held");
        Ok(Original { endpoint: Endpoint { sequence: row.get(0)?, hash: row.get(1)?, scope: row.get(2)?, author: row.get(3)?, header: row.get(4)?, body: row.get(5)? }, held_id: row.get(6)?, held_frame: row.get(7)? })
    })?)
}

pub(super) fn restored(directory: &Path, expected: &Original) -> Result<(), Failure> {
    let conn = inspect_file(directory)?;
    assert_eq!(&original(&conn, &expected.endpoint.hash)?, expected);
    for table in [
        "secure_evicted_logs",
        "secure_wanted_seals",
        "secure_compaction_lost",
        "secure_rebootstrap",
    ] {
        let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 0, "{table}");
    }
    for table in ["secure_applied", "secure_voids"] {
        let count: i64 = conn.query_row(
            &format!("SELECT count(*) FROM {table} WHERE hash=?"),
            [&expected.endpoint.hash],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0, "{table}");
    }
    let resolved: i64 = conn.query_row(
        "SELECT count(*) FROM secure_held_resolved WHERE id=?",
        [&expected.held_id],
        |row| row.get(0),
    )?;
    assert_eq!(resolved, 0);
    let authored: i64 = conn.query_row(
        "SELECT count(*) FROM secure_operations WHERE author=?",
        [&expected.endpoint.author],
        |row| row.get(0),
    )?;
    assert_eq!(
        authored, 1,
        "original node never re-authored the held write"
    );
    Ok(())
}
