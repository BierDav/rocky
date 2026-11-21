use crate::retry::retrying;
use crate::schema::{ITEMS, Layout, schema};
use crate::stats::summary;
use rand::{Rng, distributions::Alphanumeric, seq::SliceRandom};
use rocky_sdk::{Configuration, Database, Durability, Failure, RowIdentity, params};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub struct Plan {
    pub directory: PathBuf,
    pub single: usize,
    pub batch_rows: usize,
    pub batches: usize,
    pub body: usize,
}

enum Store {
    Sqlite(rusqlite::Connection),
    Rocky(Arc<Database>, Durability),
}

#[derive(Clone)]
enum Key {
    Integer(i64),
    Row(RowIdentity),
}

fn text(length: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length)
        .map(char::from)
        .collect()
}

fn us(clock: Instant) -> f64 {
    clock.elapsed().as_secs_f64() * 1_000_000.0
}

fn directory_bytes(directory: &Path) -> u64 {
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

impl Store {
    fn sqlite(directory: &Path, full: bool) -> Result<Self, Failure> {
        std::fs::create_dir_all(directory)?;
        let connection = rusqlite::Connection::open(directory.join("plain.sqlite"))?;
        connection.query_row("PRAGMA journal_mode=wal", [], |_| Ok(()))?;
        connection.execute_batch(if full {
            "PRAGMA synchronous=FULL; PRAGMA fullfsync=1; PRAGMA checkpoint_fullfsync=1;"
        } else {
            "PRAGMA synchronous=NORMAL;"
        })?;
        connection.execute_batch(
            "CREATE TABLE items(id INTEGER PRIMARY KEY, grp TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, n INTEGER NOT NULL DEFAULT 0)",
        )?;
        Ok(Self::Sqlite(connection))
    }

    async fn rocky(directory: &Path, durability: Durability) -> Result<Self, Failure> {
        let opened = Configuration::new(directory, &schema(1, Layout::Flat)?)?
            .create()
            .await?;
        Ok(Self::Rocky(opened.database, durability))
    }

    async fn insert(&mut self, rows: usize, n: i64, body: usize) -> Result<Vec<Key>, Failure> {
        match self {
            Self::Sqlite(connection) => {
                let transaction = connection.transaction()?;
                let mut keys = Vec::with_capacity(rows);
                {
                    let mut statement = transaction
                        .prepare_cached("INSERT INTO items(grp,title,body,n) VALUES(?,?,?,?)")?;
                    for offset in 0..rows {
                        let n = n + i64::try_from(offset)?;
                        statement.execute(rusqlite::params![
                            "local",
                            format!("local item {n:08}"),
                            text(body),
                            n
                        ])?;
                        keys.push(Key::Integer(transaction.last_insert_rowid()));
                    }
                }
                transaction.commit()?;
                Ok(keys)
            }
            Self::Rocky(database, durability) => {
                let mut transaction = database.transaction_with(*durability).await?;
                let mut keys = Vec::with_capacity(rows);
                for offset in 0..rows {
                    let n = n + i64::try_from(offset)?;
                    let id = transaction.next_id(ITEMS, params![]).await?;
                    transaction
                        .execute(
                            "INSERT INTO items(id,grp,title,body,n) VALUES(?,?,?,?,?)",
                            params![id, "local", format!("local item {n:08}"), text(body), n],
                        )
                        .await?;
                    keys.push(Key::Row(id));
                }
                transaction.commit().await?;
                Ok(keys)
            }
        }
    }

    async fn update(&mut self, key: &Key) -> Result<(), Failure> {
        match (self, key) {
            (Self::Sqlite(connection), Key::Integer(id)) => {
                let transaction = connection.transaction()?;
                transaction
                    .prepare_cached("UPDATE items SET n = n + 1 WHERE id = ?")?
                    .execute([id])?;
                transaction.commit()?;
                Ok(())
            }
            (Self::Rocky(database, durability), Key::Row(id)) => {
                let mut transaction = database.transaction_with(*durability).await?;
                transaction
                    .execute("UPDATE items SET n = n + 1 WHERE id = ?", params![*id])
                    .await?;
                transaction.commit().await?;
                Ok(())
            }
            _ => Err("key does not belong to this store".into()),
        }
    }

    async fn read(&mut self, key: &Key) -> Result<(), Failure> {
        match (self, key) {
            (Self::Sqlite(connection), Key::Integer(id)) => {
                let length: i64 = connection
                    .prepare_cached("SELECT title, body, n FROM items WHERE id = ?")?
                    .query_row([id], |row| Ok(row.get::<_, String>(1)?.len() as i64))?;
                if length == 0 {
                    return Err("empty row".into());
                }
                Ok(())
            }
            (Self::Rocky(database, _), Key::Row(id)) => {
                let rows = database
                    .query(
                        "SELECT title, body, n FROM items WHERE id = ?",
                        params![*id],
                    )
                    .await?
                    .collect()
                    .await?;
                if rows.is_empty() {
                    return Err("missing row".into());
                }
                Ok(())
            }
            _ => Err("key does not belong to this store".into()),
        }
    }

    async fn scan(&mut self) -> Result<i64, Failure> {
        const SCAN: &str = "SELECT count(*) + sum(n) FROM items";
        match self {
            Self::Sqlite(connection) => Ok(connection.query_row(SCAN, [], |row| row.get(0))?),
            Self::Rocky(database, _) => {
                let rows = database.query(SCAN, params![]).await?.collect().await?;
                Ok(rows.first().ok_or("no row")?.get_at(0)?)
            }
        }
    }

    async fn phases(&self, samples: usize, body: usize) -> Result<Option<Value>, Failure> {
        let Self::Rocky(database, durability) = self else {
            return Ok(None);
        };
        let (mut begin, mut id, mut execute, mut commit) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for index in 0..samples {
            let clock = Instant::now();
            let mut transaction = database.transaction_with(*durability).await?;
            begin.push(us(clock));
            let clock = Instant::now();
            let row = transaction.next_id(ITEMS, params![]).await?;
            id.push(us(clock));
            let clock = Instant::now();
            transaction
                .execute(
                    "INSERT INTO items(id,grp,title,body,n) VALUES(?,?,?,?,?)",
                    params![
                        row,
                        "phase",
                        format!("phase {index}"),
                        text(body),
                        i64::try_from(index)?
                    ],
                )
                .await?;
            execute.push(us(clock));
            let clock = Instant::now();
            transaction.commit().await?;
            commit.push(us(clock));
        }
        Ok(Some(json!({
            "begin_us": summary(&mut begin),
            "next_id_us": summary(&mut id),
            "execute_us": summary(&mut execute),
            "commit_us": summary(&mut commit),
        })))
    }

    async fn close(self) -> Result<(), Failure> {
        match self {
            Self::Sqlite(connection) => connection.close().map_err(|(_, error)| error.into()),
            Self::Rocky(database, _) => database.close().await,
        }
    }
}

async fn measure(
    variant: &str,
    mut store: Store,
    plan: &Plan,
    directory: &Path,
) -> Result<Value, Failure> {
    let retries_before = crate::retry::count();
    let mut keys = Vec::new();
    let mut inserts = Vec::with_capacity(plan.single);
    for index in 0..plan.single {
        let clock = Instant::now();
        keys.extend(retrying!(
            store.insert(1, i64::try_from(index)?, plan.body).await
        )?);
        inserts.push(us(clock));
    }
    let mut batches = Vec::with_capacity(plan.batches);
    let batch_clock = Instant::now();
    for index in 0..plan.batches {
        let clock = Instant::now();
        let first = i64::try_from(plan.single + index * plan.batch_rows)?;
        keys.extend(retrying!(
            store.insert(plan.batch_rows, first, plan.body).await
        )?);
        batches.push(us(clock) / 1000.0);
    }
    let batch_rows_per_s =
        (plan.batches * plan.batch_rows) as f64 / batch_clock.elapsed().as_secs_f64();
    let mut sample: Vec<Key> = keys.clone();
    sample.shuffle(&mut rand::thread_rng());
    sample.truncate(plan.single);
    let mut updates = Vec::with_capacity(sample.len());
    for key in &sample {
        let clock = Instant::now();
        retrying!(store.update(key).await)?;
        updates.push(us(clock));
    }
    sample.shuffle(&mut rand::thread_rng());
    let mut reads = Vec::with_capacity(sample.len());
    for key in &sample {
        let clock = Instant::now();
        retrying!(store.read(key).await)?;
        reads.push(us(clock));
    }
    let mut scans = Vec::new();
    for _ in 0..10 {
        let clock = Instant::now();
        retrying!(store.scan().await)?;
        scans.push(us(clock) / 1000.0);
    }
    let phases = retrying!(store.phases(plan.single.min(200), plan.body).await)?;
    store.close().await?;
    let retries = crate::retry::count() - retries_before;
    let rows = keys.len();
    let bytes = directory_bytes(directory);
    Ok(json!({
        "variant": variant,
        "rows": rows,
        "body": plan.body,
        "insert_us": summary(&mut inserts),
        "update_us": summary(&mut updates),
        "read_us": summary(&mut reads),
        "batch_ms": summary(&mut batches),
        "batch_rows": plan.batch_rows,
        "batch_rows_per_s": batch_rows_per_s,
        "scan_ms": summary(&mut scans),
        "disk_bytes": bytes,
        "disk_bytes_per_row": bytes as f64 / rows as f64,
        "phases": phases,
        "retries": retries,
    }))
}

pub async fn run(plan: Plan) -> Result<(), Failure> {
    for variant in [
        "sqlite-normal",
        "rocky-buffered",
        "sqlite-full",
        "rocky-flushed",
    ] {
        let directory = plan.directory.join(variant);
        if directory.exists() {
            std::fs::remove_dir_all(&directory)?;
        }
        let store = match variant {
            "sqlite-normal" => Store::sqlite(&directory, false)?,
            "sqlite-full" => Store::sqlite(&directory, true)?,
            "rocky-buffered" => Store::rocky(&directory, Durability::Buffered).await?,
            _ => Store::rocky(&directory, Durability::Flushed).await?,
        };
        let result = measure(variant, store, &plan, &directory).await?;
        println!("{result}");
        std::fs::remove_dir_all(&directory)?;
    }
    Ok(())
}
