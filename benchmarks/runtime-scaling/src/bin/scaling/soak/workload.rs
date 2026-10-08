use super::*;

pub(super) fn scope(domain: u16) -> Scope {
    Scope {
        mesh: MeshId([11; 16]),
        app: WideSchema::app_id(),
        domain: domain_id(domain),
    }
}
pub(super) fn table(index: usize, tables: u16) -> u16 {
    // Seed each cold table once, then put all other rows in the hot set.
    if index < usize::from(tables) {
        index as u16
    } else {
        (index % usize::from(tables.min(50))) as u16
    }
}
pub(super) async fn open(
    path: &std::path::Path,
    app: WideSchema,
    durability: Durability,
    instrument: bool,
) -> Result<Runtime<WideSchema>, Error> {
    Runtime::open_with_options(
        path,
        (1..=3)
            .map(|domain| DomainInstallation {
                app,
                policy: Policy {
                    scope: scope(domain),
                    caller: CALLER,
                },
                identity: Identity {
                    scope: scope(domain),
                    node: NODE,
                    caller: CALLER,
                },
                config: RuntimeConfig {
                    authors: vec![(NODE, CALLER)],
                    delivery_targets: vec![],
                    full_nodes: vec![],
                },
            })
            .collect(),
        RuntimeOptions {
            durability,
            instrument,
            ..RuntimeOptions::default()
        },
    )
    .await
}
pub(super) async fn commit(
    replica: &Replica<WideSchema>,
    entity: Option<OperationId>,
    payload: Vec<u8>,
    counters: &Counters,
    latency: &Mutex<Latency>,
) -> Result<Envelope, Error> {
    let started = Instant::now();
    let op = replica
        .submit(Mutation { entity, payload })
        .await?
        .committed()
        .await?;
    if let Ok(mut histogram) = latency.lock() {
        histogram.record(started.elapsed());
    }
    counters.writes.fetch_add(1, Relaxed);
    Ok(op)
}
pub(super) struct Writer {
    pub(super) replica: Replica<WideSchema>,
    pub(super) counters: Arc<Counters>,
    pub(super) latency: Arc<Mutex<Latency>>,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) tables: u16,
    pub(super) rows: usize,
    pub(super) client: usize,
    pub(super) clients: usize,
}
impl Writer {
    pub(super) async fn run(self) -> Result<(), Error> {
        let mut live = Vec::with_capacity(self.rows.div_ceil(self.clients));
        for index in (self.client..self.rows).step_by(self.clients) {
            let table = table(index, self.tables);
            let op = commit(
                &self.replica,
                None,
                WideSchema::create(table, index as i64, 0, "seed"),
                &self.counters,
                &self.latency,
            )
            .await?;
            live.push((table, op.id));
            self.counters.creates.fetch_add(1, Relaxed);
        }
        let mut step = 0usize;
        while !self.stop.load(Relaxed) {
            let slot = step % live.len();
            let (table, entity) = live[slot];
            // Replacement keeps live rows fixed and exercises permanent deletion.
            if step % 128 == 127 {
                commit(
                    &self.replica,
                    Some(entity),
                    WideSchema::delete(table),
                    &self.counters,
                    &self.latency,
                )
                .await?;
                self.counters.deletes.fetch_add(1, Relaxed);
                commit(
                    &self.replica,
                    Some(entity),
                    WideSchema::set_a(table, -1),
                    &self.counters,
                    &self.latency,
                )
                .await?;
                let op = commit(
                    &self.replica,
                    None,
                    WideSchema::create(table, step as i64, 0, "replacement"),
                    &self.counters,
                    &self.latency,
                )
                .await?;
                self.counters.creates.fetch_add(1, Relaxed);
                live[slot].1 = op.id;
            } else {
                commit(
                    &self.replica,
                    Some(entity),
                    WideSchema::set_a(table, step as i64),
                    &self.counters,
                    &self.latency,
                )
                .await?;
            }
            step += 1;
        }
        Ok(())
    }
}
pub(super) fn range(table: u16) -> BoundQuery {
    BoundQuery {
        sql: format!("SELECT node,sequence,a FROM t{table} WHERE a>=? ORDER BY a"),
        values: vec![SqlValue::Integer(0)],
    }
}
pub(super) async fn reads(
    replica: Replica<WideSchema>,
    stop: Arc<AtomicBool>,
    latency: Arc<Mutex<Latency>>,
    ranges: Arc<Mutex<Latency>>,
) -> Result<(), Error> {
    while !stop.load(Relaxed) {
        let started = Instant::now();
        let point = replica
            .read(
                BoundQuery {
                    sql: "SELECT a FROM t0 WHERE node=? AND sequence=?".into(),
                    values: vec![SqlValue::Blob(NODE.0.to_vec()), SqlValue::Integer(1)],
                },
                ReadOptions::new(scope(1), 1),
            )
            .await?;
        if point.rows != vec![vec![SqlValue::Integer(0)]] {
            return Err(Error::Corrupt("soak sentinel disappeared".into()));
        }
        if let Ok(mut histogram) = latency.lock() {
            histogram.record(started.elapsed());
        }
        let started = Instant::now();
        let page = replica
            .read(range(0), ReadOptions::new(scope(1), 32))
            .await?;
        if let Some(cursor) = page.next {
            replica
                .read(range(0), ReadOptions::new(scope(1), 32).after(cursor))
                .await?;
        }
        if let Ok(mut histogram) = ranges.lock() {
            histogram.record(started.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}
