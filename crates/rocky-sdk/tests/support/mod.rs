//! Consumer fixtures use definition tables and the public SQL/pseudonym APIs.
#![allow(dead_code)]
use rocky_sdk::{Database, Engine};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, OnceLock, Weak};

enum Issuer {
    Database(Weak<Database>),
    Engine(Weak<Engine>),
}
static ISSUERS: OnceLock<Mutex<Vec<Issuer>>> = OnceLock::new();
pub fn database(database: &Arc<Database>) {
    ISSUERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .push(Issuer::Database(Arc::downgrade(database)));
}
pub fn engine(engine: &Arc<Engine>) {
    ISSUERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .push(Issuer::Engine(Arc::downgrade(engine)));
}
fn reply(reply: &str) -> Result<Value, Value> {
    let reply: Value = serde_json::from_str(reply).map_err(|error| json!(error.to_string()))?;
    if reply["status"] == "ok" {
        Ok(reply["value"].clone())
    } else {
        Err(reply["error"].clone())
    }
}
async fn assignment(scope: &Value, request: &Value) -> Result<Value, Value> {
    enum Live {
        Database(Arc<Database>),
        Engine(Arc<Engine>),
    }
    let issuers: Vec<_> = ISSUERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .iter()
        .filter_map(|issuer| match issuer {
            Issuer::Database(database) => database.upgrade().map(Live::Database),
            Issuer::Engine(engine) => engine.upgrade().map(Live::Engine),
        })
        .collect();
    for issuer in issuers {
        let frame = json!({"version":1,"scope":scope,"request":{"type":"pseudonym_assign","request":request}}).to_string();
        let response = match issuer {
            Live::Database(database) => {
                if serde_json::from_str::<Value>(&database.scope())
                    .ok()
                    .as_ref()
                    != Some(scope)
                {
                    continue;
                }
                let session = database.session();
                let response = session.call(frame).await;
                session.close().await;
                response
            }
            Live::Engine(engine) => {
                if serde_json::from_str::<Value>(&engine.scope()).ok().as_ref() != Some(scope) {
                    continue;
                }
                engine.call(frame).await
            }
        };
        if let Ok(response) = response
            && let Ok(response) = reply(&response)
        {
            return Ok(response["pseudonym"].clone());
        }
    }
    Err(json!({"message":"fixture has no live author assigner"}))
}
fn cell(value: &Value) -> Value {
    match value {
        Value::Null => json!({"type":"null"}),
        Value::Bool(value) => json!({"type":"integer","value":if *value { "1" } else { "0" }}),
        Value::Number(value) => json!({"type":"integer","value":value.to_string()}),
        Value::String(value)
            if value
                .parse::<i64>()
                .is_ok_and(|number| number.to_string() == *value) =>
        {
            json!({"type":"integer","value":value})
        }
        Value::String(value) => json!({"type":"text","value":value}),
        _ => json!({"type":"text","value":value.to_string()}),
    }
}
/// An application action expressed with modern SQL. `key` is a fixture's
/// business key; the actual incarnation is allocated and transported separately.
pub async fn call<F, Fut>(scope: &Value, request: Value, raw: F) -> Result<Value, Value>
where
    F: Fn(Value) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<Value, Value>> + Send,
{
    let mut request = request;
    if request["type"] == "mutate" || request["type"] == "mutation_progress" {
        let node = match request.as_object_mut().unwrap().remove("fixture_author") {
            Some(node) => node,
            None => raw(json!({"type":"session"})).await?["node"].clone(),
        };
        let mut bytes = node.as_str().unwrap().as_bytes().to_vec();
        bytes.extend(serde_json::to_vec(&request["handle"]).unwrap());
        request["handle"] = json!(rocky_core::commitment("fixture.sql.handle", &bytes));
    }
    if request["type"] != "mutate" {
        return raw(request).await;
    }
    let Some(payload) = request["payload"].as_array() else {
        return raw(request).await;
    };
    let bytes: Option<Vec<u8>> = payload
        .iter()
        .map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
        .collect();
    let Some(bytes) = bytes else {
        return raw(request).await;
    };
    let Ok(action) = serde_json::from_slice::<Value>(&bytes) else {
        return raw(request).await;
    };
    let Some(table) = action["collection"].as_str() else {
        return raw(request).await;
    };
    if request["baseline"] != Value::Null
        || !table
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return raw(request).await;
    }
    let Some(handle) = request["handle"]
        .as_array()
        .filter(|handle| handle.len() == 32)
    else {
        return raw(request).await;
    };
    let key = handle
        .iter()
        .map(|byte| format!("{:02x}", byte.as_u64().unwrap()))
        .collect::<String>();
    if raw(json!({"type":"mutation_progress","handle":handle})).await?["committed"] == true {
        return Ok(json!({"replayed":true,"handle":key}));
    }
    prepare(scope, table, &raw).await?;
    raw(json!({"type":"sql","sql":"BEGIN"})).await?;
    let result = async {
        let id = match action["type"].as_str() {
            Some("create") => {
                if request["entity"] != Value::Null { return raw(request.clone()).await; }
                let Some(values) = action["values"].as_object() else { return raw(request.clone()).await; };
                let id = raw(json!({"type":"next_id","table":table})).await?["id"].clone();
                let mut columns = vec!["key".to_string(), "id".to_string()];
                let mut bound = vec![json!({"type":"text","value":key}), json!({"type":"integer","value":id})];
                for (name, value) in values {
                    if !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') { return raw(request.clone()).await; }
                    columns.push(format!("\"{name}\"")); bound.push(cell(value));
                }
                let sql = format!("INSERT INTO \"{table}\"({}) VALUES({})", columns.join(","), vec!["?"; columns.len()].join(","));
                raw(json!({"type":"sql","sql":sql,"values":bound})).await?;
                key.clone()
            }
            Some("set") | Some("delete") => {
                let Some(id) = request["entity"]["id"].as_str() else { return raw(request.clone()).await; };
                let existing = raw(json!({"type":"sql","sql":format!("SELECT id FROM \"{table}\" WHERE key=?"),"values":[{"type":"text","value":id}]})).await?;
                if existing["rows"].as_array().is_none_or(Vec::is_empty) { return raw(request.clone()).await; }
                let numeric = existing["rows"][0][0].clone();
                let (sql, bound) = if action["type"] == "set" {
                    let Some(field) = action["field"].as_str() else { return raw(request.clone()).await; };
                    if !field.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') { return raw(request.clone()).await; }
                    (format!("UPDATE \"{table}\" SET \"{field}\"=? WHERE id=?"), vec![cell(&action["value"]), numeric])
                } else { (format!("DELETE FROM \"{table}\" WHERE id=?"), vec![numeric]) };
                raw(json!({"type":"sql","sql":sql,"values":bound})).await?;
                id.to_owned()
            }
            _ => return raw(request.clone()).await,
        };
        let committed = raw(json!({"type":"sql","sql":"COMMIT","handle":handle})).await?;
        Ok(json!({"id":id,"handle":key,"frames":committed["committed"]["frames"]}))
    }.await;
    if result.is_err() {
        let _ = raw(json!({"type":"sql","sql":"ROLLBACK"})).await;
    }
    result
}

/// Stable identities for deterministic strong-command fixture reducers.
pub fn identity_row(mut row: Vec<rocky_core::SqlValue>) -> Vec<rocky_core::SqlValue> {
    let sequence = format!("{:?}", row[0]).bytes().fold(1u32, |n, byte| {
        n.wrapping_mul(31).wrapping_add(u32::from(byte))
    }) % 1_000_000
        + 1;
    row.extend([
        rocky_core::SqlValue::Integer(4294967296 + i64::from(sequence)),
        rocky_core::SqlValue::Integer(0),
    ]);
    row
}

pub async fn prepare<F, Fut>(scope: &Value, table: &str, raw: &F) -> Result<(), Value>
where
    F: Fn(Value) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<Value, Value>> + Send,
{
    let asked = raw(json!({"type":"pseudonym_request","collection":table})).await?;
    let bytes = data_encoding::HEXLOWER
        .decode(asked["request"].as_str().unwrap().as_bytes())
        .unwrap();
    let asked_chain = rocky_credentials::PseudonymRequest::from_bytes(&bytes).unwrap();
    let pseudonym = match assignment(scope, &asked["request"]).await {
        Ok(chain) => chain,
        Err(_) => match asked_chain.previous {
            Some(previous) => serde_json::to_value(previous).unwrap(),
            None => assignment(scope, &asked["request"]).await?,
        },
    };
    raw(json!({"type":"pseudonym_install","pseudonym":pseudonym})).await?;
    Ok(())
}
