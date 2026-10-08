//! Engine-only probes for the SQLite runtime switch: synchronous switching on
//! one connection, and one batched writer per file with clients waiting for
//! acknowledgements. See the README for the commands.
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use harness::{NODE, OP_BYTES, Result, Row, mix, row};
use rusqlite::{Connection, params};

const USAGE: &str = "usage: sync-switch <dir> | batched <dir> <files> <domains> <tables-per-domain> \
                     <rows-per-table> <seconds> <full|normal>; results append to $ENGINE_SCALE_OUT";

struct Out {
    prefix: String,
    file: Option<std::fs::File>,
}

impl Out {
    fn new(mode: &str, extra: &str) -> Result<Self> {
        let file = match std::env::var_os("ENGINE_SCALE_OUT") {
            Some(path) => Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            ),
            None => None,
        };
        Ok(Self {
            prefix: format!(
                "\"engine\":\"sqlite\",\"variant\":{:?},\"version\":\"{} wal\",\"mode\":\"{mode}\"{extra}",
                std::env::var("ENGINE_SCALE_VARIANT").unwrap_or_default(),
                rusqlite::version()
            ),
            file,
        })
    }
    fn line(&mut self, metric: &str, fields: String) {
        let line = format!("{{{},\"metric\":\"{metric}\",{fields}}}", self.prefix);
        println!("{line}");
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{line}");
        }
    }
    fn value(&mut self, metric: &str, value: u64, unit: &str) {
        self.line(metric, format!("\"value\":{value},\"unit\":\"{unit}\""));
    }
    fn latency(&mut self, metric: &str, mut micros: Vec<u64>) {
        micros.sort_unstable();
        let at = |q: usize| {
            micros
                .get((micros.len() * q / 100).min(micros.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        self.line(
            metric,
            format!(
                "\"n\":{},\"p50_us\":{},\"p99_us\":{},\"max_us\":{}",
                micros.len(),
                at(50),
                at(99),
                micros.last().copied().unwrap_or(0)
            ),
        );
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn pragma_i64(conn: &Connection, name: &str) -> rusqlite::Result<i64> {
    conn.pragma_query_value(None, name, |row| row.get(0))
}

fn wal_bytes(db: &Path) -> u64 {
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(wal).map_or(0, |meta| meta.len())
}

fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if mode != "wal" {
        return Err(format!("journal mode is {mode}").into());
    }
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    conn.pragma_update(None, "fullfsync", 1)?;
    conn.pragma_update(None, "checkpoint_fullfsync", 1)?;
    Ok(conn)
}

/// Sets and reads back `synchronous` (1 NORMAL, 2 FULL) with fullfsync on.
fn set_sync(conn: &Connection, level: i64) -> Result<()> {
    conn.pragma_update(None, "synchronous", level)?;
    let (actual, full) = (
        pragma_i64(conn, "synchronous")?,
        pragma_i64(conn, "fullfsync")?,
    );
    if (actual, full) != (level, 1) {
        return Err(format!("synchronous/fullfsync read {actual}/{full}, wanted {level}/1").into());
    }
    Ok(())
}

// ---------------------------------------------------------------- sync-switch

const NORMAL: i64 = 1;
const FULL: i64 = 2;

fn commit_one(conn: &Connection, id: &mut i64, payload: &[u8]) -> Result<u64> {
    *id += 1;
    let started = Instant::now();
    conn.prepare_cached("INSERT INTO kv VALUES(?,?)")?
        .execute(params![*id, payload])?;
    Ok(started.elapsed().as_micros() as u64)
}

fn sync_switch(dir: &Path) -> Result<()> {
    let mut out = Out::new("sync-switch", "")?;
    let path = dir.join("switch.sqlite");
    let conn = open(&path)?;
    // The WAL is truncated at each reset, so its length is the bytes written since.
    conn.pragma_update(None, "journal_size_limit", 0)?;
    conn.execute_batch("CREATE TABLE kv(id INTEGER PRIMARY KEY, v BLOB)")?;
    let payload = vec![7u8; OP_BYTES];
    let mut id = 0i64;

    // (a) Changes between transactions read back.
    let mut readback = Vec::new();
    for level in [NORMAL, FULL, NORMAL, FULL, NORMAL] {
        conn.pragma_update(None, "synchronous", level)?;
        readback.push(pragma_i64(&conn, "synchronous")?);
        commit_one(&conn, &mut id, &payload)?;
    }
    out.line(
        "readback_between_tx",
        format!("\"set\":[1,2,1,2,1],\"read\":{readback:?}"),
    );

    // Inside an open transaction or savepoint.
    for (label, begin, end) in [
        ("inside_begin", "BEGIN IMMEDIATE", "COMMIT"),
        ("inside_savepoint", "SAVEPOINT s", "RELEASE s"),
    ] {
        set_sync(&conn, NORMAL)?;
        conn.execute_batch(begin)?;
        let attempt = conn.pragma_update(None, "synchronous", FULL);
        let inside = pragma_i64(&conn, "synchronous")?;
        conn.prepare_cached("INSERT INTO kv VALUES(?,?)")?
            .execute(params![id + 1, &payload])?;
        id += 1;
        conn.execute_batch(end)?;
        let after = pragma_i64(&conn, "synchronous")?;
        let error = attempt.err().map(|e| e.to_string()).unwrap_or_default();
        out.line(
            &format!("set_full_{label}"),
            format!("\"error\":{error:?},\"read_inside\":{inside},\"read_after\":{after}"),
        );
    }
    // fullfsync is accepted inside a transaction; reported as read back.
    set_sync(&conn, FULL)?;
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let attempt = conn.pragma_update(None, "fullfsync", 0);
    let inside = pragma_i64(&conn, "fullfsync")?;
    conn.execute_batch("COMMIT")?;
    let error = attempt.err().map(|e| e.to_string()).unwrap_or_default();
    out.line(
        "set_fullfsync_off_inside_begin",
        format!(
            "\"error\":{error:?},\"read_inside\":{inside},\"read_after\":{}",
            pragma_i64(&conn, "fullfsync")?
        ),
    );
    conn.pragma_update(None, "fullfsync", 1)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;

    // (b) Alternating commits on one connection: per-commit latency per setting.
    let commits = env_u64("SWITCH_COMMITS", 1000);
    let (mut normal, mut full) = (Vec::new(), Vec::new());
    for i in 0..commits {
        let level = if i % 2 == 0 { NORMAL } else { FULL };
        set_sync(&conn, level)?;
        let micros = commit_one(&conn, &mut id, &payload)?;
        if level == NORMAL {
            normal.push(micros)
        } else {
            full.push(micros)
        }
    }
    out.latency("alternating_normal_commit", normal);
    out.latency("alternating_full_commit", full);

    // Blocks of one setting, plus FULL with plain fsync for reference.
    for (label, level, fullfsync) in [
        ("normal", NORMAL, 1),
        ("full", FULL, 1),
        ("full_plain_fsync", FULL, 0),
    ] {
        conn.pragma_update(None, "synchronous", level)?;
        conn.pragma_update(None, "fullfsync", fullfsync)?;
        let mut micros = Vec::new();
        for _ in 0..commits / 2 {
            micros.push(commit_one(&conn, &mut id, &payload)?);
        }
        out.latency(&format!("block_{label}_commit"), micros);
    }
    conn.pragma_update(None, "fullfsync", 1)?;

    // A FULL commit after k unflushed NORMAL commits. The checkpoint first
    // flushes everything and resets the WAL; the first commit after a reset
    // syncs the WAL header even at NORMAL, so it is reported separately.
    let reps = env_u64("SWITCH_REPS", 20);
    for k in [0u64, 1, 10, 100, 1000] {
        let (mut first, mut rest, mut flush, mut pending) =
            (Vec::new(), Vec::new(), Vec::new(), 0u64);
        for _ in 0..reps {
            set_sync(&conn, FULL)?;
            let busy: i64 =
                conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| row.get(0))?;
            if busy != 0 {
                return Err("checkpoint busy".into());
            }
            set_sync(&conn, NORMAL)?;
            for j in 0..k {
                let micros = commit_one(&conn, &mut id, &payload)?;
                if j == 0 {
                    first.push(micros)
                } else {
                    rest.push(micros)
                }
            }
            pending = wal_bytes(&path);
            set_sync(&conn, FULL)?;
            flush.push(commit_one(&conn, &mut id, &payload)?);
        }
        out.latency(&format!("after_{k}_normal_full_commit"), flush);
        out.value(&format!("after_{k}_normal_wal_kib"), pending / 1024, "KiB");
        if k > 0 {
            out.latency(&format!("after_{k}_normal_first_normal_after_reset"), first);
        }
        if k > 1 {
            out.latency(&format!("after_{k}_normal_other_normal"), rest);
        }
    }
    let rows: i64 = conn.query_row("SELECT count(*) FROM kv", [], |row| row.get(0))?;
    if rows != id {
        return Err(format!("{rows} rows, {id} committed").into());
    }
    out.value("rows_committed", rows as u64, "rows");
    Ok(())
}

// ---------------------------------------------------------------- batched writer

struct Request {
    domain: u64,
    table: u64,
    op: i64,
    bytes: Vec<u8>,
    row: Row,
    reply: SyncSender<std::result::Result<(), String>>,
}

impl Request {
    fn size(&self) -> usize {
        self.bytes.len() + self.row.c.len() + 2 * NODE.len() + 64
    }
}

#[derive(Default)]
struct WriterStats {
    groups: u64,
    mutations: u64,
    commit_micros: Vec<u64>,
    checkpoint_micros: Vec<u64>,
    max_wal: u64,
}

fn create_domain(conn: &Connection, domain: u64, tables: u64, rows: u64) -> rusqlite::Result<()> {
    for t in 0..tables {
        conn.execute_batch(&format!(
            "CREATE TABLE d{domain}_t{t}(node BLOB NOT NULL,seq INTEGER NOT NULL,a INTEGER,b INTEGER,c TEXT,\
             lamport INTEGER,stamp_node BLOB,stamp_seq INTEGER,PRIMARY KEY(node,seq));\
             CREATE INDEX d{domain}_t{t}_a ON d{domain}_t{t}(a);\
             CREATE INDEX d{domain}_t{t}_b ON d{domain}_t{t}(b);"
        ))?;
        let mut insert = conn.prepare(&format!(
            "INSERT INTO d{domain}_t{t} VALUES(?,?,?,?,?,?,?,?)"
        ))?;
        for seq in 0..rows as i64 {
            let r = row(t, seq, rows);
            insert.execute(params![
                &NODE[..],
                r.seq,
                r.a,
                r.b,
                r.c,
                r.lamport,
                &NODE[..],
                r.seq
            ])?;
        }
    }
    conn.execute_batch(&format!(
        "CREATE TABLE d{domain}_ops(node BLOB NOT NULL,seq INTEGER NOT NULL,bytes BLOB,lamport INTEGER,\
         status INTEGER,origin INTEGER,PRIMARY KEY(node,seq));\
         CREATE TABLE d{domain}_outbox(node BLOB NOT NULL,seq INTEGER NOT NULL,PRIMARY KEY(node,seq));\
         CREATE TABLE d{domain}_replica(id INTEGER PRIMARY KEY,seq INTEGER,lamport INTEGER,revision INTEGER);\
         INSERT INTO d{domain}_replica VALUES(0,0,0,0);"
    ))
}

/// The same four statements as the sqlite-engine `mutation`, without its transaction.
fn apply(conn: &Connection, r: &Request) -> rusqlite::Result<()> {
    let (d, t, row) = (r.domain, r.table, &r.row);
    conn.prepare_cached(&format!(
        "INSERT INTO d{d}_t{t} VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(node,seq) DO UPDATE SET \
         a=excluded.a,b=excluded.b,c=excluded.c,lamport=excluded.lamport,\
         stamp_node=excluded.stamp_node,stamp_seq=excluded.stamp_seq"
    ))?
    .execute(params![
        &NODE[..],
        row.seq,
        row.a,
        row.b,
        row.c,
        row.lamport,
        &NODE[..],
        r.op
    ])?;
    conn.prepare_cached(&format!("INSERT INTO d{d}_ops VALUES(?,?,?,?,0,0)"))?
        .execute(params![&NODE[..], r.op, r.bytes, row.lamport])?;
    conn.prepare_cached(&format!("INSERT INTO d{d}_outbox VALUES(?,?)"))?
        .execute(params![&NODE[..], r.op])?;
    let changed = conn
        .prepare_cached(&format!(
            "UPDATE d{d}_replica SET seq=?,lamport=?,revision=revision+1 WHERE id=0"
        ))?
        .execute(params![r.op, row.lamport])?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// One group: each mutation in its own savepoint, one commit, then acknowledgements.
fn commit_group(conn: &Connection, group: &mut Vec<Request>) -> rusqlite::Result<()> {
    conn.prepare_cached("BEGIN IMMEDIATE")?.execute([])?;
    let mut results = Vec::with_capacity(group.len());
    for request in group.iter() {
        conn.prepare_cached("SAVEPOINT m")?.execute([])?;
        match apply(conn, request) {
            Ok(()) => {
                conn.prepare_cached("RELEASE m")?.execute([])?;
                results.push(Ok(()));
            }
            Err(error) => {
                conn.execute_batch("ROLLBACK TO m; RELEASE m")?;
                results.push(Err(error.to_string()));
            }
        }
    }
    if let Err(error) = conn.prepare_cached("COMMIT")?.execute([]) {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(error);
    }
    for (request, result) in group.drain(..).zip(results) {
        let _ = request.reply.send(result);
    }
    Ok(())
}

fn writer(
    path: PathBuf,
    level: i64,
    requests: Receiver<Request>,
    ready: Arc<Barrier>,
) -> Result<WriterStats> {
    let conn = open(&path);
    ready.wait();
    let conn = conn?;
    set_sync(&conn, level)?;
    // Truncate the WAL at each reset so its length measures content, not history.
    conn.pragma_update(None, "journal_size_limit", 0)?;
    conn.set_prepared_statement_cache_capacity(4096);
    let max_count = env_u64("WRITER_MAX_GROUP", 64) as usize;
    let max_bytes = env_u64("WRITER_MAX_GROUP_BYTES", 256 * 1024) as usize;
    let wal_limit = env_u64("WRITER_WAL_CHECKPOINT_BYTES", 4 * 1024 * 1024);
    let mut stats = WriterStats::default();
    let mut group = Vec::with_capacity(max_count);
    loop {
        // Block for the first; take whatever else is already waiting, no delay.
        match requests.recv() {
            Ok(request) => group.push(request),
            Err(_) => break,
        }
        let mut bytes = group[0].size();
        while group.len() < max_count && bytes < max_bytes {
            match requests.try_recv() {
                Ok(request) => {
                    bytes += request.size();
                    group.push(request);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let size = group.len() as u64;
        let started = Instant::now();
        commit_group(&conn, &mut group)?;
        stats
            .commit_micros
            .push(started.elapsed().as_micros() as u64);
        stats.groups += 1;
        stats.mutations += size;
        let wal = wal_bytes(&path);
        stats.max_wal = stats.max_wal.max(wal);
        if wal > wal_limit {
            let started = Instant::now();
            let busy: i64 =
                conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| row.get(0))?;
            if busy != 0 {
                return Err("passive checkpoint busy".into());
            }
            stats
                .checkpoint_micros
                .push(started.elapsed().as_micros() as u64);
        }
    }
    Ok(stats)
}

struct Shape {
    files: u64,
    domains: u64,
    tables: u64,
    rows: u64,
    seconds: u64,
    level: i64,
}

fn batched(dir: &Path, shape: Shape) -> Result<()> {
    let Shape {
        files,
        domains,
        tables,
        rows,
        seconds,
        level,
    } = shape;
    if files == 0 || domains % files != 0 {
        return Err("domains must divide evenly over files".into());
    }
    let per_file = domains / files;
    let mut out = Out::new(
        "batched",
        &format!(
            ",\"files\":{files},\"domains\":{domains},\"clients\":{domains},\"tables_per_domain\":{tables},\
             \"rows_per_table\":{rows},\"sync\":\"{}\"",
            if level == FULL { "full" } else { "normal" }
        ),
    )?;
    let started = Instant::now();
    let paths: Vec<PathBuf> = (0..files)
        .map(|f| dir.join(format!("f{f}.sqlite")))
        .collect();
    for (f, path) in paths.iter().enumerate() {
        let conn = open(path)?;
        conn.pragma_update(None, "synchronous", "OFF")?;
        conn.execute_batch("BEGIN")?;
        for domain in f as u64 * per_file..(f as u64 + 1) * per_file {
            create_domain(&conn, domain, tables, rows)?;
        }
        conn.execute_batch("COMMIT; PRAGMA wal_checkpoint(TRUNCATE)")?;
    }
    out.value(
        "install_load_ms",
        started.elapsed().as_millis() as u64,
        "ms",
    );

    // Writers and clients open first; the clock starts when all are ready.
    let ready = Arc::new(Barrier::new((files + domains + 1) as usize));
    let run = Duration::from_secs(seconds);
    let mut senders: Vec<Sender<Request>> = Vec::new();
    let mut writers = Vec::new();
    for path in &paths {
        let (send, receive) = mpsc::channel();
        senders.push(send);
        let (path, ready) = (path.clone(), ready.clone());
        writers.push(std::thread::spawn(move || {
            writer(path, level, receive, ready)
        }));
    }
    let mut clients = Vec::new();
    for domain in 0..domains {
        let (send, ready) = (senders[(domain / per_file) as usize].clone(), ready.clone());
        clients.push(std::thread::spawn(move || -> Result<Vec<u64>> {
            let (reply, acks) = mpsc::sync_channel(1);
            let mut random = mix(domain + 1);
            let mut op = (domain as i64) << 40;
            let mut micros = Vec::new();
            ready.wait();
            let deadline = Instant::now() + run;
            while Instant::now() < deadline {
                op += 1;
                random = mix(random);
                let table = random % tables;
                random = mix(random);
                let request = Request {
                    domain,
                    table,
                    op,
                    bytes: vec![5u8; OP_BYTES],
                    row: row(table, (random % rows.max(1)) as i64, rows),
                    reply: reply.clone(),
                };
                let sent = Instant::now();
                send.send(request).map_err(|_| "writer stopped")?;
                acks.recv().map_err(|_| "writer stopped")??;
                micros.push(sent.elapsed().as_micros() as u64);
            }
            Ok(micros)
        }));
    }
    drop(senders);
    ready.wait();
    let started = Instant::now();
    let mut all = Vec::new();
    for client in clients {
        all.extend(client.join().map_err(|_| "client panicked")??);
    }
    let elapsed = started.elapsed().as_micros().max(1) as u64;
    let mut stats = WriterStats::default();
    for handle in writers {
        let s = handle.join().map_err(|_| "writer panicked")??;
        stats.groups += s.groups;
        stats.mutations += s.mutations;
        stats.commit_micros.extend(s.commit_micros);
        stats.checkpoint_micros.extend(s.checkpoint_micros);
        stats.max_wal = stats.max_wal.max(s.max_wal);
    }
    if stats.mutations != all.len() as u64 {
        return Err(format!("{} acknowledged, {} committed", all.len(), stats.mutations).into());
    }
    out.value(
        "writes_per_s",
        all.len() as u64 * 1_000_000 / elapsed,
        "writes/s",
    );
    out.value(
        "commits_per_s",
        stats.groups * 1_000_000 / elapsed,
        "commits/s",
    );
    out.value(
        "mean_group_x100",
        stats.mutations * 100 / stats.groups.max(1),
        "mutations",
    );
    out.latency("client_ack", all);
    out.latency("group_commit", stats.commit_micros);
    out.latency("passive_checkpoint", stats.checkpoint_micros);
    out.value("max_wal_kib", stats.max_wal / 1024, "KiB");
    // Committed rows match acknowledgements in every file.
    let mut ops = 0u64;
    for (f, path) in paths.iter().enumerate() {
        let conn = Connection::open(path)?;
        for domain in f as u64 * per_file..(f as u64 + 1) * per_file {
            ops += conn.query_row(&format!("SELECT count(*) FROM d{domain}_ops"), [], |row| {
                row.get::<_, i64>(0)
            })? as u64;
        }
    }
    if ops != stats.mutations {
        return Err(format!("{ops} operations stored, {} committed", stats.mutations).into());
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).ok_or(USAGE);
    let dir = PathBuf::from(arg(1)?);
    std::fs::create_dir_all(&dir)?;
    if std::fs::read_dir(&dir)?.next().is_some() {
        return Err("needs an empty directory".into());
    }
    match arg(0)?.as_str() {
        "sync-switch" => sync_switch(&dir),
        "batched" => {
            let num = |i: usize| -> Result<u64> { Ok(arg(i)?.parse::<u64>()?) };
            let level = match arg(7)?.as_str() {
                "full" => FULL,
                "normal" => NORMAL,
                _ => return Err(USAGE.into()),
            };
            batched(
                &dir,
                Shape {
                    files: num(2)?,
                    domains: num(3)?,
                    tables: num(4)?.max(1),
                    rows: num(5)?.max(1),
                    seconds: num(6)?,
                    level,
                },
            )
        }
        _ => Err(USAGE.into()),
    }
}
