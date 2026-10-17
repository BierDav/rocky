use super::*;
use rusqlite::{Connection, types::ValueRef};
use std::collections::BTreeSet;

fn query(conn: &Connection, sql: &str) -> Result<Value, Failure> {
    let mut statement = conn.prepare(sql)?;
    let names = statement
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect::<Vec<_>>();
    let rows = statement
        .query_map([], |row| {
            let mut values = serde_json::Map::new();
            for (index, name) in names.iter().enumerate() {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => json!(v),
                    ValueRef::Real(v) => json!(v),
                    ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                    ValueRef::Blob(v) => {
                        if matches!(
                            name.as_str(),
                            "signed" | "sets" | "header" | "body" | "record"
                        ) {
                            digest(v)
                        } else {
                            json!({"hex":hex(v)})
                        }
                    }
                };
                values.insert(name.clone(), value);
            }
            Ok(Value::Object(values))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!(rows))
}
fn best_effort(conn: &Connection, output: &mut Value, name: &str, sql: &str) {
    output[name] = match query(conn, sql) {
        Ok(value) => value,
        Err(error) => json!({"diagnostic_error":error.to_string()}),
    };
}

pub(super) fn capture(
    directory: &Path,
    dependencies: &mut BTreeSet<Vec<u8>>,
) -> Result<Value, Failure> {
    let mut conn = inspect_file(directory)?;
    let transaction = conn.transaction()?;
    let mut output = json!({"snapshot_started_unix_ms":now(),"read_only":true,
        "atomicity":"one file read transaction, later than refusal; other workers may progress"});
    // Extract public configuration only; never print the settings object.
    let settings: Value = serde_json::from_slice(&std::fs::read(directory.join("identity.json"))?)?;
    let chain: rocky_sdk::credentials::CredentialChain =
        serde_json::from_value(settings["credentials"].clone())?;
    output["configured_node"] = json!(
        chain
            .0
            .last()
            .ok_or("node certificate")?
            .capability
            .subject
            .to_hex()
    );
    let publisher: rocky_sdk::credentials::PublicKey =
        serde_json::from_value(settings["publisher"].clone())?;
    let mut definition = settings["schema"].clone();
    let fields = definition.as_object_mut().ok_or("schema object")?;
    fields.remove("version");
    fields.remove("migrations");
    definition["publisher"] = json!(publisher.to_hex());
    let logical = serde_json::from_value::<rocky_core::SchemaDefinition>(definition)?.build()?;
    let counts = logical.table_id(logical.table("counts").ok_or("counts table")?);
    output["counts_table"] = json!(hex(&counts));
    let scopes = transaction
        .prepare("SELECT id,scope FROM secure_scope_state ORDER BY id")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    output["scope_ids"] = json!([]);
    for (id, bytes) in scopes {
        let scope: rocky_core::ResourceScope =
            rocky_sdk::credentials::decode_bounded(&bytes, bytes.len())?;
        let is_counts = scope.table == counts;
        output["scope_ids"].as_array_mut().ok_or("scope array")?.push(json!({
            "integer_scope_state_id":id,"encoded_scope_hex":hex(&bytes),"scope":scope,"is_counts":is_counts}));
    }
    for (name, sql) in [
        ("revision", "SELECT id,revision FROM secure_metadata"),
        ("followed_tip", "SELECT * FROM secure_followed_tip"),
        ("counts_keys", "SELECT key FROM counts ORDER BY key"),
        ("counts_count", "SELECT count(*) AS count FROM counts"),
        (
            "operation_groups",
            "SELECT scope,status,count(*) AS count FROM secure_operations GROUP BY scope,status ORDER BY scope,status",
        ),
        (
            "logs_with_encoded_scope",
            "SELECT l.*,s.scope AS encoded_scope FROM secure_logs l JOIN secure_scope_state s ON s.id=l.scope ORDER BY l.scope,l.author",
        ),
        (
            "applied_with_encoded_scope",
            "SELECT a.*,s.scope AS encoded_scope FROM secure_applied a JOIN secure_scope_state s ON s.id=a.scope ORDER BY a.position",
        ),
        (
            "row_keys",
            "SELECT scope,row FROM secure_rows ORDER BY scope,row",
        ),
    ] {
        best_effort(&transaction, &mut output, name, sql);
    }
    for table in [
        "secure_scope_state",
        "secure_evicted_logs",
        "secure_wanted_seals",
        "secure_compaction_lost",
        "secure_rebootstrap",
        "secure_bundle_handles",
    ] {
        best_effort(
            &transaction,
            &mut output,
            &format!("{table}_columns"),
            &format!("PRAGMA table_info({table})"),
        );
        best_effort(
            &transaction,
            &mut output,
            table,
            &format!("SELECT * FROM {table}"),
        );
    }
    let mut statement = transaction.prepare("SELECT hash,author,scope,sequence,header,body,status FROM secure_operations ORDER BY scope,author,sequence")?;
    let mut rows = statement.query([])?;
    let mut operations = Vec::new();
    while let Some(row) = rows.next()? {
        let hash: Vec<u8> = row.get(0)?;
        let scope: Vec<u8> = row.get(2)?;
        let status: String = row.get(6)?;
        let mut operation = json!({"hash":hex(&hash),"node":hex(&row.get::<_,Vec<u8>>(1)?),
            "encoded_scope_hex":hex(&scope),"sequence":row.get::<_,i64>(3)?,"status":status});
        let resource: rocky_core::ResourceScope =
            rocky_sdk::credentials::decode_bounded(&scope, scope.len())?;
        if status == "pending" && resource.table == counts {
            dependencies.insert(hash.clone());
            let bytes: Vec<u8> = row.get(4)?;
            operation["header_bytes"] = digest(&bytes);
            operation["body_bytes"] = digest(&row.get::<_, Vec<u8>>(5)?);
            match cbor::header(&bytes) {
                Ok((header, required)) => {
                    operation["header"] = header;
                    operation["exact_dependencies"] =
                        json!(required.iter().map(|h| hex(h)).collect::<Vec<_>>());
                    dependencies.extend(required);
                }
                Err(error) => operation["diagnostic_error"] = json!(error.to_string()),
            }
        }
        operations.push(operation);
    }
    output["retained_operations"] = json!(operations);
    best_effort(
        &transaction,
        &mut output,
        "authored_bundle_members",
        "SELECT own.hash AS trigger_hash,h.handle,h.member,h.hash AS member_hash,o.author,o.scope,o.sequence,o.status FROM secure_bundle_handles own JOIN secure_bundle_handles h ON h.handle=own.handle LEFT JOIN secure_operations o ON o.hash=h.hash ORDER BY own.hash,h.member",
    );
    output["bundle_mapping_note"] = json!(
        "Exact authored handle/member identities; encrypted memberships remain in original preserved bodies, no secret keys opened"
    );
    output["rebootstrap_signed_metadata"] = json!([]);
    let imports = transaction
        .prepare("SELECT scope,signed FROM secure_rebootstrap")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (scope, bytes) in imports {
        let metadata = match rocky_secure::SignedCheckpoint::from_bytes(&bytes) {
            Ok(signed) => {
                json!({"integer_scope_state_id":scope,"statement":signed.statement,"signed_bytes":digest(&bytes)})
            }
            Err(error) => {
                json!({"integer_scope_state_id":scope,"diagnostic_error":error.to_string(),"signed_bytes":digest(&bytes)})
            }
        };
        output["rebootstrap_signed_metadata"]
            .as_array_mut()
            .ok_or("import metadata")?
            .push(metadata);
    }
    // Compare every dependency, never a maximum position/count proxy.
    output["dependency_records"] = json!([]);
    for hash in dependencies.iter() {
        let hash_hex = hex(hash);
        let mut record = json!({"hash":hash_hex});
        for (name, sql) in [
            (
                "retained",
                format!(
                    "SELECT hash,scope,author,sequence,status FROM secure_operations WHERE hash=X'{hash_hex}'"
                ),
            ),
            (
                "applied",
                format!(
                    "SELECT a.*,s.scope AS encoded_scope FROM secure_applied a JOIN secure_scope_state s ON s.id=a.scope WHERE a.hash=X'{hash_hex}'"
                ),
            ),
            (
                "void",
                format!("SELECT * FROM secure_voids WHERE hash=X'{hash_hex}'"),
            ),
        ] {
            best_effort(&transaction, &mut record, name, &sql);
        }
        output["dependency_records"]
            .as_array_mut()
            .ok_or("dependencies array")?
            .push(record);
    }
    drop(rows);
    drop(statement);
    transaction.rollback()?;
    output["snapshot_finished_unix_ms"] = json!(now());
    let marker = directory.join("bootstrap.json");
    output["bootstrap_intent_later"] = match std::fs::read(marker) {
        Ok(bytes) => {
            let intent: Value = serde_json::from_slice(&bytes)?;
            json!({"bytes":digest(&bytes),"intent":intent.get("intent").or_else(||intent.as_bool().map(|_|&intent)),
                "pending":intent.get("pending"),"totals":intent.get("totals")})
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => json!({"diagnostic_error":error.to_string()}),
    };
    Ok(output)
}
