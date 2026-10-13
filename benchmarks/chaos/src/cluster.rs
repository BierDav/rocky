//! The orchestrator's view of node processes: spawn, call, kill, reopen.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, oneshot},
};

pub type Reply = Result<Value, Value>;
type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

pub struct Node {
    pub name: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    pending: Pending,
    next: AtomicU64,
}

pub fn exe() -> PathBuf {
    std::env::current_exe().expect("current exe")
}

impl Node {
    /// Starts a node process. `mode` is `{"mode":"create"}`, `{"mode":"join","invitation":..}`
    /// or `{"mode":"resume"}`. Returns the open failure as `Err`.
    pub async fn spawn(
        name: &str,
        root: &Path,
        port: u16,
        mode: Value,
        open_timeout: Duration,
    ) -> Result<Arc<Node>, String> {
        let dir = root.join(name);
        let options = crate::node::options(dir.to_str().unwrap(), port, mode);
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join(format!("{name}.stderr.log")))
            .unwrap();
        let mut child = Command::new(exe())
            .arg("node")
            .arg(options.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = child.stdin.take().unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let first = tokio::time::timeout(open_timeout, lines.next_line()).await;
        let first: Value = match first {
            Err(_) => {
                let _ = child.start_kill();
                return Err(format!("{name}: open timed out after {open_timeout:?}"));
            }
            Ok(Ok(Some(line))) => serde_json::from_str(&line).map_err(|e| e.to_string())?,
            Ok(other) => {
                let status = child.wait().await;
                return Err(format!(
                    "{name}: process ended before opening: {other:?} {status:?}"
                ));
            }
        };
        if let Some(error) = first.get("open_failed") {
            let _ = child.wait().await;
            return Err(format!("{name}: open failed: {error}"));
        }
        let pending: Pending = Arc::default();
        let reader = pending.clone();
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value["id"].as_u64() else {
                    continue;
                };
                if let Some(tx) = reader.lock().unwrap().remove(&id) {
                    let _ = tx.send(value);
                }
            }
            // Process gone: fail everything still waiting.
            reader.lock().unwrap().clear();
        });
        Ok(Arc::new(Node {
            name: name.into(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            pending,
            next: AtomicU64::new(1),
        }))
    }

    async fn send(&self, message: Value, wait: Duration) -> Result<Value, Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut message = message;
        message["id"] = id.into();
        {
            let mut stdin = self.stdin.lock().await;
            if stdin
                .write_all(format!("{message}\n").as_bytes())
                .await
                .is_err()
                || stdin.flush().await.is_err()
            {
                return Err(json!({"harness":"process gone"}));
            }
        }
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(json!({"harness":"process gone"})),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(json!({"harness": format!("timeout after {wait:?}")}))
            }
        }
    }

    pub async fn call_for(&self, request: Value, wait: Duration) -> Reply {
        let value = self.send(json!({"call": request}), wait).await?;
        let reply = &value["reply"];
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(reply["error"].clone())
        }
    }
    pub async fn call(&self, request: Value) -> Reply {
        self.call_for(request, Duration::from_secs(60)).await
    }
    pub async fn ok(&self, request: Value) -> Value {
        match self.call(request.clone()).await {
            Ok(v) => v,
            Err(e) => panic!("{}: {request} failed: {e}", self.name),
        }
    }
    /// SIGKILL: no close, no flush, no drain.
    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    pub async fn close(&self) -> Result<(), String> {
        let r = self
            .send(json!({"close": true}), Duration::from_secs(30))
            .await;
        let mut child = self.child.lock().await;
        let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        let _ = child.start_kill();
        match r {
            Ok(v) if v["closed"].is_null() => Ok(()),
            Ok(v) => Err(v["closed"].to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    // ---- application helpers ----

    pub async fn node_id(&self) -> String {
        self.ok(json!({"type":"session"})).await["node"]
            .as_str()
            .unwrap_or_default()
            .into()
    }
    pub async fn contact(&self) -> String {
        self.ok(json!({"type":"contact"})).await["uri"]
            .as_str()
            .unwrap_or_default()
            .into()
    }
    pub async fn invite(&self, profile: Value) -> String {
        self.ok(json!({"type":"pairing","profile":profile})).await["uri"]
            .as_str()
            .unwrap_or_default()
            .into()
    }
    pub async fn sync(&self) -> Reply {
        self.call_for(json!({"type":"sync"}), Duration::from_secs(90))
            .await
    }
    pub async fn status(&self) -> Reply {
        self.call(json!({"type":"status"})).await
    }
    /// Every row of `sql` (which must not end in LIMIT), paged by the harness.
    pub async fn rows(&self, sql: &str) -> Result<Vec<Value>, Value> {
        let mut all = Vec::new();
        loop {
            let paged = format!("{sql} LIMIT 1000 OFFSET {}", all.len());
            let page = self
                .call(json!({"type":"read","query":{"sql":paged,"values":[],"max_rows":1000}}))
                .await?;
            // Pages are also bounded by bytes: stop only on an empty page.
            let rows = page["rows"].as_array().cloned().unwrap_or_default();
            let done = rows.is_empty();
            all.extend(rows);
            if done {
                return Ok(all);
            }
        }
    }
    /// Sorted text rendering of a query, for comparisons.
    pub async fn dump(&self, sql: &str) -> Result<Vec<String>, Value> {
        let rows = self.rows(sql).await?;
        let mut out: Vec<String> = rows
            .iter()
            .map(|row| {
                row.as_array()
                    .map(|cells| {
                        cells
                            .iter()
                            .map(|c| c["value"].to_string())
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .unwrap_or_default()
            })
            .collect();
        out.sort();
        Ok(out)
    }
    /// Installs this node's author pseudonym for `table`, assigned by `issuer`.
    pub async fn author(&self, issuer: &Node, table: &str) -> Reply {
        let asked = self
            .call(json!({"type":"pseudonym_request","collection":table}))
            .await?;
        let assigned = issuer
            .call(json!({"type":"pseudonym_assign","request":asked["request"]}))
            .await?;
        self.call(json!({"type":"pseudonym_install","pseudonym":assigned["pseudonym"]}))
            .await
    }
    pub async fn sql(&self, sql: &str, values: Value) -> Reply {
        self.call(json!({"type":"sql","sql":sql,"values":values}))
            .await
    }
    /// One transaction: next_id + INSERT; rolls back on failure.
    pub async fn insert(
        &self,
        table: &str,
        partition: Option<&str>,
        sql: &str,
        extra: Vec<Value>,
    ) -> Result<String, Value> {
        self.sql("BEGIN", json!([])).await?;
        let result = async {
            let mut next = json!({"type":"next_id","table":table});
            if let Some(list) = partition {
                next["partition_values"] = json!([{"type":"integer","value":list}]);
            }
            let id = self.call(next).await?["id"].clone();
            let mut values = vec![json!({"type":"integer","value":id})];
            values.extend(extra);
            self.sql(sql, Value::Array(values)).await?;
            self.sql("COMMIT", json!([])).await?;
            Ok(id.as_str().unwrap_or_default().to_owned())
        }
        .await;
        if result.is_err() {
            let _ = self.sql("ROLLBACK", json!([])).await;
        }
        result
    }
    pub async fn add_note(&self, key: &str, body: &str) -> Result<String, Value> {
        self.insert(
            "notes",
            None,
            "INSERT INTO notes(id,key,body) VALUES(?,?,?)",
            vec![
                json!({"type":"text","value":key}),
                json!({"type":"text","value":body}),
            ],
        )
        .await
    }
    /// `count` notes in one transaction.
    pub async fn add_notes_batch(
        &self,
        prefix: &str,
        start: usize,
        count: usize,
        len: usize,
    ) -> Result<(), Value> {
        self.sql("BEGIN", json!([])).await?;
        let result = async {
            for i in start..start + count {
                let id = self.call(json!({"type":"next_id","table":"notes"})).await?["id"].clone();
                self.sql(
                    "INSERT INTO notes(id,key,body) VALUES(?,?,?)",
                    json!([{"type":"integer","value":id},{"type":"text","value":format!("{prefix}{i}")},{"type":"text","value":format!("{i:06}:{}", "x".repeat(len))}]),
                )
                .await?;
            }
            self.sql("COMMIT", json!([])).await.map(|_| ())
        }
        .await;
        if result.is_err() {
            let _ = self.sql("ROLLBACK", json!([])).await;
        }
        result
    }
    /// Issuer: prepares the list's group secret, then inserts it.
    pub async fn add_list(&self, title: &str) -> Result<String, Value> {
        self.call(json!({"type":"partition_next","table":"lists"}))
            .await?;
        self.insert(
            "lists",
            None,
            "INSERT INTO lists(id,title) VALUES(?,?)",
            vec![json!({"type":"text","value":title})],
        )
        .await
    }
    pub async fn add_item(&self, list: &str, title: &str) -> Result<String, Value> {
        self.insert(
            "items",
            Some(list),
            "INSERT INTO items(id,list,title,done) VALUES(?,?,?,0)",
            vec![
                json!({"type":"integer","value":list}),
                json!({"type":"text","value":title}),
            ],
        )
        .await
    }
    pub async fn share(&self, list: &str, recipients: &[String]) -> Reply {
        self.call(json!({"type":"partition_share","group":"list","values":[{"type":"integer","value":list}],"recipients":recipients}))
            .await
    }
    pub async fn role(&self, partial: bool) -> Reply {
        self.call(json!({"type":"partition_role","role":{"role": if partial {"Partial"} else {"Full"},"full_nodes":[],"custody":0}}))
            .await
    }
    pub async fn add_peer(&self, other: &Node) -> Reply {
        let contact = other.contact().await;
        self.call(json!({"type":"peer_add","contact":contact}))
            .await
    }
}

/// Every node's configured peers include every other node.
pub async fn mesh(nodes: &[&Node]) {
    for a in nodes {
        for b in nodes {
            if a.name != b.name {
                let _ = a.add_peer(b).await;
            }
        }
    }
}
