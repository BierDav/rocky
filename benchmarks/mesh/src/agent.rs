use crate::net::{BenchConnector, Path, Traffic};
use crate::retry::retrying;
use crate::schema::{ITEMS, Layout, PROBES, schema};
use rand::{Rng, distributions::Alphanumeric};
use rocky_sdk::{
    ChangeCause, Configuration, Database, Failure, GrantProfile, IdentityKey, Network,
    NetworkService, PartitionTarget, PublicKey, SqlValue, params,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Mutex, mpsc},
    task::JoinHandle,
};

#[derive(Deserialize)]
struct Request {
    id: u64,
    #[serde(flatten)]
    op: Op,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    Create {
        dir: PathBuf,
        path: String,
        #[serde(default)]
        layout: Layout,
    },
    Join {
        dir: PathBuf,
        path: String,
        invitation: String,
        #[serde(default)]
        linkable: bool,
        #[serde(default)]
        layout: Layout,
    },
    Open {
        dir: PathBuf,
        path: Option<String>,
        version: u32,
        #[serde(default)]
        layout: Layout,
    },
    Close,
    Invite {
        issuer: bool,
    },
    Contact,
    AddPeer {
        contact: String,
    },
    Address,
    Paths {
        address: String,
    },
    Seed {
        rows: u64,
        body: usize,
        batch: u64,
        grp: String,
        #[serde(default)]
        groups: u64,
    },
    Share {
        groups: Vec<String>,
        nodes: Vec<String>,
    },
    Touch {
        rows: u64,
        batch: u64,
        grp: String,
    },
    Probe {
        seq: i64,
    },
    Scalar {
        sql: String,
    },
    Wait {
        sql: String,
        at_least: i64,
        timeout_ms: u64,
    },
    Traffic,
    Stats,
    Networking {
        enabled: bool,
    },
    Automatic {
        enabled: bool,
    },
    Sync,
    Seal,
    GenesisMinimum {
        minimum: u32,
    },
    Clock,
    Status,
    Generation,
    Renew,
}

struct Running {
    database: Arc<Database>,
    network: Option<NetworkService>,
    connector: Option<Arc<BenchConnector>>,
    directory: PathBuf,
    watcher: JoinHandle<()>,
    node: String,
    layout: Layout,
}

struct Agent {
    running: Mutex<Option<Running>>,
    identities: Mutex<HashMap<PathBuf, Arc<IdentityKey>>>,
    out: mpsc::UnboundedSender<String>,
}

pub fn now_us() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_micros())
        .unwrap_or_default()
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn directory_bytes(directory: &std::path::Path) -> u64 {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(meta) if meta.is_dir() => directory_bytes(&entry.path()),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        })
        .sum()
}

fn process_stats() -> Value {
    json!({
        "cpu_ms": cpu_time::ProcessTime::try_now().map(|time| ms(time.as_duration())).unwrap_or(-1.0),
        "rss_bytes": memory_stats::memory_stats().map(|stats| stats.physical_mem).unwrap_or_default(),
    })
}

fn random_text(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

async fn insert_batch(
    database: &Database,
    first: u64,
    count: u64,
    body: usize,
    grp: &str,
    layout: Layout,
) -> Result<(), Failure> {
    let mut transaction = database.transaction().await?;
    for offset in 0..count {
        let n = i64::try_from(first + offset)?;
        let partition = match layout {
            Layout::Flat => params![],
            Layout::PartitionedByGroup => params![grp],
        };
        let id = transaction.next_id(ITEMS, partition).await?;
        transaction
            .execute(
                "INSERT INTO items(id,grp,title,body,n) VALUES(?,?,?,?,?)",
                params![id, grp, format!("{grp} item {n:08}"), random_text(body), n],
            )
            .await?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn update_batch(
    database: &Database,
    grp: &str,
    first: u64,
    count: u64,
) -> Result<(), Failure> {
    let mut transaction = database.transaction().await?;
    transaction
        .execute(
            "UPDATE items SET body = substr(body, 2) || substr(body, 1, 1) WHERE grp = ? AND n >= ? AND n < ?",
            params![grp, i64::try_from(first)?, i64::try_from(first + count)?],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn insert_probe(
    database: &Database,
    node: &str,
    seq: i64,
    start_us: u128,
) -> Result<(), Failure> {
    let mut transaction = database.transaction().await?;
    let id = transaction.next_id(PROBES, params![]).await?;
    transaction
        .execute(
            "INSERT INTO probes(id,origin,seq,sent) VALUES(?,?,?,?)",
            params![id, node, seq, i64::try_from(start_us)?],
        )
        .await?;
    transaction.commit().await?;
    Ok(())
}

async fn scalar(database: &Database, sql: &str) -> Result<i64, Failure> {
    let rows = database.query(sql, params![]).await?.collect().await?;
    let row = rows.first().ok_or("scalar query returned no row")?;
    Ok(match row.values().first() {
        Some(SqlValue::Integer(value)) => *value,
        Some(SqlValue::Null) | None => 0,
        Some(other) => return Err(format!("scalar query returned {other:?}").into()),
    })
}

fn cause_name(cause: &ChangeCause) -> &'static str {
    match cause {
        ChangeCause::Local => "local",
        ChangeCause::Sync => "sync",
        ChangeCause::Rebuild => "rebuild",
        ChangeCause::Authority => "authority",
        _ => "other",
    }
}

async fn watch(database: Arc<Database>, out: mpsc::UnboundedSender<String>) {
    let Ok(events) = database.observe_tables(Some(vec![PROBES.into()])).await else {
        return;
    };
    let mut seen = HashSet::new();
    let mut first = true;
    loop {
        let causes = if first {
            first = false;
            Vec::new()
        } else {
            match events.next().await {
                Ok(change) => change.causes,
                Err(_) => return,
            }
        };
        let at = now_us();
        let Ok(rows) = async {
            database
                .query("SELECT seq, origin FROM probes", params![])
                .await?
                .collect()
                .await
                .map_err(Failure::from)
        }
        .await
        else {
            continue;
        };
        for row in rows {
            let (Ok(seq), Ok(origin)) = (row.get_at::<i64>(0), row.get_at::<String>(1)) else {
                continue;
            };
            if seen.insert(seq) {
                let causes: Vec<_> = causes.iter().map(cause_name).collect();
                let line = json!({"event": "seen", "seq": seq, "origin": origin, "at_us": at, "causes": causes});
                if out.send(line.to_string()).is_err() {
                    return;
                }
            }
        }
    }
}

impl Agent {
    fn configuration(
        &self,
        directory: &std::path::Path,
        version: u32,
        layout: Layout,
        path: Option<&str>,
    ) -> Result<(Configuration, Option<Arc<BenchConnector>>), Failure> {
        let configuration = Configuration::new(directory, &schema(version, layout)?)?;
        let Some(path) = path else {
            return Ok((configuration, None));
        };
        let connector = BenchConnector::new(Path::parse(path)?);
        Ok((
            configuration.network(Network::Owned(connector.clone())),
            Some(connector),
        ))
    }

    async fn install(
        &self,
        opened: rocky_sdk::Opened,
        connector: Option<Arc<BenchConnector>>,
        directory: PathBuf,
        layout: Layout,
    ) -> Result<Value, Failure> {
        let database = opened.database;
        if self.running.lock().await.is_some() {
            let _ = database.close().await;
            return Err("this agent already holds an open database".into());
        }
        if connector.is_some() {
            database.session().set_automatic_sync(true).await?;
        }
        let node = database.session().status().await?.authority.node;
        let watcher = tokio::spawn(watch(database.clone(), self.out.clone()));
        let address = connector.as_ref().and_then(|connector| connector.address());
        *self.running.lock().await = Some(Running {
            database,
            network: opened.network,
            connector,
            directory,
            watcher,
            node: node.clone(),
            layout,
        });
        Ok(json!({"node": node, "address": address}))
    }

    async fn database(&self) -> Result<Arc<Database>, Failure> {
        Ok(self
            .running
            .lock()
            .await
            .as_ref()
            .ok_or("no database is open")?
            .database
            .clone())
    }

    async fn connector(&self) -> Option<Arc<BenchConnector>> {
        self.running
            .lock()
            .await
            .as_ref()
            .and_then(|running| running.connector.clone())
    }

    async fn traffic(&self) -> Traffic {
        self.connector()
            .await
            .map(|connector| connector.traffic())
            .unwrap_or_default()
    }

    async fn close(&self) -> Result<Value, Failure> {
        let Some(running) = self.running.lock().await.take() else {
            return Ok(json!({"closed": false}));
        };
        let clock = Instant::now();
        running.watcher.abort();
        if let Some(network) = &running.network {
            network.close().await;
        }
        let result = running.database.close().await;
        if let Some(connector) = &running.connector {
            connector.release();
        }
        result?;
        Ok(json!({"closed": true, "close_ms": ms(clock.elapsed())}))
    }

    async fn handle(&self, op: Op) -> Result<Value, Failure> {
        match op {
            Op::Create { dir, path, layout } => {
                let clock = Instant::now();
                let (configuration, connector) =
                    self.configuration(&dir, 1, layout, Some(&path))?;
                let opened = configuration.create().await?;
                let elapsed = ms(clock.elapsed());
                if let Some(root) = &opened.root {
                    self.identities
                        .lock()
                        .await
                        .insert(dir.clone(), Arc::new(root.identity()));
                }
                let mut reply = self.install(opened, connector, dir, layout).await?;
                reply["create_ms"] = json!(elapsed);
                Ok(reply)
            }
            Op::Join {
                dir,
                path,
                invitation,
                linkable,
                layout,
            } => {
                let clock = Instant::now();
                let (configuration, connector) =
                    self.configuration(&dir, 1, layout, Some(&path))?;
                let opened = if linkable {
                    configuration.join_linkable(&invitation).await?
                } else {
                    configuration.join(&invitation).await?
                };
                let elapsed = ms(clock.elapsed());
                if let Some(phrase) = &opened.identity {
                    self.identities
                        .lock()
                        .await
                        .insert(dir.clone(), Arc::new(IdentityKey::from_phrase(phrase)?));
                }
                let mut reply = self.install(opened, connector, dir, layout).await?;
                reply["join_ms"] = json!(elapsed);
                Ok(reply)
            }
            Op::Open {
                dir,
                path,
                version,
                layout,
            } => {
                let clock = Instant::now();
                let (mut configuration, connector) =
                    self.configuration(&dir, version, layout, path.as_deref())?;
                if let Some(identity) = self.identities.lock().await.get(&dir) {
                    configuration = configuration.identity(identity.clone());
                }
                let opened = configuration.open().await?;
                let elapsed = ms(clock.elapsed());
                let mut reply = self.install(opened, connector, dir, layout).await?;
                reply["open_ms"] = json!(elapsed);
                Ok(reply)
            }
            Op::Close => self.close().await,
            Op::Invite { issuer } => {
                let profile = if issuer {
                    GrantProfile::Issuer
                } else {
                    GrantProfile::Writer {
                        scopes: [ITEMS, PROBES].map(String::from).into(),
                    }
                };
                let invitation = self
                    .database()
                    .await?
                    .session()
                    .invitation_with(profile)
                    .await?;
                Ok(json!({"invitation": invitation.uri}))
            }
            Op::Contact => Ok(json!({
                "contact": self.database().await?.session().contact().await?
            })),
            Op::AddPeer { contact } => {
                let node = self.database().await?.session().add_peer(&contact).await?;
                Ok(json!({"node": node.to_hex()}))
            }
            Op::Address => Ok(json!({
                "address": self.connector().await.and_then(|connector| connector.address())
            })),
            Op::Paths { address } => {
                let paths = match self.connector().await {
                    Some(connector) => connector.paths(&address).await?,
                    None => Vec::new(),
                };
                Ok(json!({"paths": paths}))
            }
            Op::Seed {
                rows,
                body,
                batch,
                grp,
                groups,
            } => self.seed(rows, body, batch, &grp, groups).await,
            Op::Share { groups, nodes } => {
                let session = self.database().await?.session();
                let recipients = nodes
                    .iter()
                    .map(|node| node.parse())
                    .collect::<Result<BTreeSet<PublicKey>, _>>()?;
                let clock = Instant::now();
                for group in &groups {
                    let target = PartitionTarget::Table {
                        table: ITEMS.into(),
                        values: vec![SqlValue::Text(group.clone())],
                    };
                    session.create_partition(target.clone()).await?;
                    session
                        .share_partition(target, recipients.clone(), None)
                        .await?;
                }
                Ok(json!({"ms": ms(clock.elapsed()), "groups": groups.len()}))
            }
            Op::Touch { rows, batch, grp } => self.touch(rows, batch, &grp).await,
            Op::Probe { seq } => {
                let database = self.database().await?;
                let node = self
                    .running
                    .lock()
                    .await
                    .as_ref()
                    .map(|running| running.node.clone())
                    .unwrap_or_default();
                let start_us = now_us();
                let clock = Instant::now();
                retrying!(insert_probe(&database, &node, seq, start_us).await)?;
                Ok(json!({
                    "start_us": start_us,
                    "commit_us": now_us(),
                    "commit_ms": ms(clock.elapsed()),
                }))
            }
            Op::Scalar { sql } => {
                let clock = Instant::now();
                let database = self.database().await?;
                let value = retrying!(scalar(&database, &sql).await)?;
                Ok(json!({"value": value, "ms": ms(clock.elapsed())}))
            }
            Op::Wait {
                sql,
                at_least,
                timeout_ms,
            } => self.wait(&sql, at_least, timeout_ms).await,
            Op::Traffic => Ok(serde_json::to_value(self.traffic().await)?),
            Op::Stats => {
                let directory = self
                    .running
                    .lock()
                    .await
                    .as_ref()
                    .map(|running| running.directory.clone());
                let mut stats = process_stats();
                stats["dir_bytes"] = json!(directory.as_deref().map(directory_bytes));
                stats["retries"] = json!(crate::retry::count());
                Ok(stats)
            }
            Op::Networking { enabled } => {
                self.database()
                    .await?
                    .session()
                    .set_networking_enabled(enabled)
                    .await?;
                Ok(json!({"enabled": enabled}))
            }
            Op::Automatic { enabled } => {
                self.database()
                    .await?
                    .session()
                    .set_automatic_sync(enabled)
                    .await?;
                Ok(json!({"automatic": enabled}))
            }
            Op::Sync => {
                let clock = Instant::now();
                let report = self.database().await?.session().sync_once().await?;
                Ok(json!({"ms": ms(clock.elapsed()), "report": report}))
            }
            Op::Seal => {
                let clock = Instant::now();
                let status = self.database().await?.session().seal(false).await?;
                Ok(json!({"ms": ms(clock.elapsed()), "tip": status}))
            }
            Op::GenesisMinimum { minimum } => {
                let clock = Instant::now();
                self.database()
                    .await?
                    .session()
                    .set_genesis_minimum(minimum)
                    .await?;
                Ok(json!({"ms": ms(clock.elapsed())}))
            }
            Op::Generation => Ok(serde_json::to_value(
                self.database().await?.session().generation().await?,
            )?),
            Op::Renew => {
                let clock = Instant::now();
                self.database().await?.session().renew().await?;
                Ok(json!({"ms": ms(clock.elapsed())}))
            }
            Op::Clock => Ok(json!({"now_us": now_us()})),
            Op::Status => Ok(serde_json::to_value(
                self.database().await?.session().status().await?,
            )?),
        }
    }

    async fn seed(
        &self,
        rows: u64,
        body: usize,
        batch: u64,
        grp: &str,
        groups: u64,
    ) -> Result<Value, Failure> {
        let database = self.database().await?;
        let layout = self
            .running
            .lock()
            .await
            .as_ref()
            .map(|running| running.layout)
            .unwrap_or_default();
        let clock = Instant::now();
        let mut commits = Vec::new();
        let mut written = 0;
        while written < rows {
            let count = batch.min(rows - written);
            let started = Instant::now();
            let group = if groups > 0 {
                format!("{grp}-{}", (written / batch) % groups)
            } else {
                grp.to_owned()
            };
            retrying!(insert_batch(&database, written, count, body, &group, layout).await)?;
            commits.push(ms(started.elapsed()));
            written += count;
        }
        let elapsed = clock.elapsed();
        Ok(json!({
            "rows": rows,
            "ms": ms(elapsed),
            "rows_per_s": rows as f64 / elapsed.as_secs_f64(),
            "commit_ms": crate::stats::summary(&mut commits),
        }))
    }

    async fn touch(&self, rows: u64, batch: u64, grp: &str) -> Result<Value, Failure> {
        let database = self.database().await?;
        let clock = Instant::now();
        let mut written = 0;
        while written < rows {
            let count = batch.min(rows - written);
            retrying!(update_batch(&database, grp, written, count).await)?;
            written += count;
        }
        let elapsed = clock.elapsed();
        Ok(
            json!({"rows": rows, "ms": ms(elapsed), "rows_per_s": rows as f64 / elapsed.as_secs_f64()}),
        )
    }

    async fn wait(&self, sql: &str, at_least: i64, timeout_ms: u64) -> Result<Value, Failure> {
        let database = self.database().await?;
        let clock = Instant::now();
        let start_us = now_us();
        let traffic = self.traffic().await;
        let mut samples = Vec::new();
        let mut next_sample = Duration::ZERO;
        let mut polls = 0u64;
        let mut first_answer = None;
        let mut last_error = None;
        loop {
            let value = match retrying!(scalar(&database, sql).await) {
                Ok(value) => {
                    if first_answer.is_none() {
                        first_answer = Some(ms(clock.elapsed()));
                    }
                    value
                }
                Err(failure) => {
                    last_error = Some(failure.to_string());
                    -1
                }
            };
            polls += 1;
            let elapsed = clock.elapsed();
            if elapsed >= next_sample || value >= at_least {
                let now = self.traffic().await;
                samples.push(json!({
                    "ms": ms(elapsed),
                    "value": value,
                    "direct_received": now.direct_received - traffic.direct_received,
                    "relay_received": now.relay_received - traffic.relay_received,
                }));
                next_sample = elapsed + Duration::from_secs(1);
            }
            if value >= at_least {
                return Ok(json!({
                    "reached": true,
                    "value": value,
                    "start_us": start_us,
                    "ms": ms(elapsed),
                    "polls": polls,
                    "first_answer_ms": first_answer,
                    "samples": samples,
                }));
            }
            if elapsed >= Duration::from_millis(timeout_ms) {
                return Ok(json!({
                    "reached": false,
                    "value": value,
                    "last_error": last_error,
                    "start_us": start_us,
                    "ms": ms(elapsed),
                    "samples": samples,
                }));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

pub async fn run() -> Result<(), Failure> {
    let (out, mut lines) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = lines.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                return;
            }
        }
    });
    let agent = Arc::new(Agent {
        running: Mutex::new(None),
        identities: Mutex::new(HashMap::new()),
        out: out.clone(),
    });
    out.send(json!({"event": "ready", "pid": std::process::id(), "now_us": now_us()}).to_string())?;
    let mut input = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = input.next_line().await? {
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                out.send(json!({"event": "bad_request", "error": error.to_string()}).to_string())?;
                continue;
            }
        };
        let agent = agent.clone();
        let out = out.clone();
        tokio::spawn(async move {
            let line = match agent.handle(request.op).await {
                Ok(value) => json!({"id": request.id, "ok": value}),
                Err(error) => json!({"id": request.id, "err": error.to_string()}),
            };
            let _ = out.send(line.to_string());
        });
    }
    let _ = agent.close().await;
    drop(agent);
    drop(out);
    let _ = writer.await;
    Ok(())
}
