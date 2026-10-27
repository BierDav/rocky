//! Release-only latency measurements; finite loads report their overlap explicitly.
use super::*;
use rocky_sdk::{
    Connection, ConnectionOptions, Durability, ReactorMeasurements, ReactorSnapshot, TableEvents,
};
use rocky_secure::{LockMeasurements, Measurements};
use std::time::{Duration, Instant};
use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};

fn smoke() -> bool {
    std::env::var_os("ROCKY_PROBE_SMOKE").is_some()
}
fn fixture_rows() -> usize {
    if smoke() { 100 } else { LARGE }
}

const UPDATE: &str = "UPDATE write_probe SET completed=1-completed WHERE key='probe'";

fn schema(base: &str) -> String {
    let mut schema: Value = serde_json::from_str(base).expect("fixture schema");
    let mut probe = schema["tables"][0].clone();
    probe["name"] = json!("write_probe");
    // The probe has the same three columns in every schema version.
    probe["columns"] =
        serde_json::from_str::<Value>(common::SCHEMA).unwrap()["tables"][0]["columns"].clone();
    schema["tables"].as_array_mut().unwrap().push(probe);
    schema.to_string()
}

async fn connection(database: &Database, durability: Durability) -> Result<Connection, Failure> {
    database
        .connect(ConnectionOptions {
            read_only: false,
            durability,
            profile: None,
            transaction_timeout: None,
            binding: None,
        })
        .await
}
async fn execute(connection: &Connection, sql: &str) -> Result<(), Failure> {
    let statement = connection.statement();
    connection.execute(statement, sql.into(), vec![]).await?;
    connection.finish(statement).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    Idle,
    Bulk,
    Small,
    Maintenance,
    Checkpoints,
}
impl Load {
    fn name(self) -> &'static str {
        match self {
            Self::Idle => "L0",
            Self::Bulk => "L1",
            Self::Small => "L2",
            Self::Maintenance => "L3",
            Self::Checkpoints => "L4",
        }
    }
}

struct Fixture {
    _root: common::identity::Directory,
    node: Arc<Device>,
    peer: Option<Arc<Device>>,
    hub: Arc<Hub>,
}
impl Fixture {
    async fn new(load: Load) -> Result<Self, Failure> {
        let root = common::identity::small_directory()?;
        let hub = Arc::new(Hub::default());
        let network = |name: &str| Some(Network::Owned(hub.connector(name)));
        let base = schema(common::SCHEMA);
        let seeded = schema(&SEEDED.replace("100000", &fixture_rows().to_string()));
        let a_dir = root.path().join("a");
        let b_dir = root.path().join("b");
        let a = Device::new(Database::create(open(&a_dir, &base, network("a")), None)?)?;
        a.follow_main_migrating().await;
        let mut tx = a.database.transaction().await?;
        let id = tx.next_id("write_probe", vec![]).await?;
        tx.execute(
            &format!(
                "INSERT INTO write_probe(key,title,completed,id) VALUES('probe','probe',0,{id})"
            ),
            vec![],
        )
        .await?;
        tx.commit().await?;
        let b = if load != Load::Idle && load != Load::Checkpoints {
            let b = Device::new(Database::join(
                open(&b_dir, &base, network("b")),
                &a.invite_as(json!({"kind":"writer","scopes":["items","write_probe"]}))
                    .await,
            )?)?;
            introduce(&a, &b).await;
            sync(&b).await;
            Some(b)
        } else {
            None
        };
        if load == Load::Bulk || load == Load::Maintenance {
            a.close().await;
            let b = b.ok_or("missing member")?;
            b.close().await;
            let a = Device::new(Database::open(open(&a_dir, &seeded, network("a")))?)?;
            // A publication peer makes the migration's closing base available,
            // exactly as in the member-pull migration fixture.
            let c = Device::new(Database::join(
                open(
                    &root.path().join("publication"),
                    &seeded,
                    network("publication"),
                ),
                &a.invite_as(json!({"kind":"writer","scopes":["items","write_probe"]}))
                    .await,
            )?)?;
            sync(&c).await;
            if load == Load::Bulk {
                sync(&a).await;
            }
            let b = Device::new(Database::open(open(&b_dir, &seeded, network("b")))?)?;
            if load == Load::Maintenance {
                c.close().await;
                // Probe the issuer while it performs closing compaction and
                // seal finalization; the peer already acknowledged publication.
                return Ok(Self {
                    _root: root,
                    node: Arc::new(a),
                    peer: Some(Arc::new(b)),
                    hub,
                });
            }
            let bootstrap_measurements = b.database.lock_measurements().await?;
            bootstrap_measurements.start();
            introduce(&b, &c).await;
            pull(&b, false).await?;
            ready_probe(&b).await?;
            let bootstrap = bootstrap_measurements.stop();
            eprintln!("P12 standalone=probe_bootstrap rows={}", fixture_rows());
            holders(&bootstrap);
            unit_costs(&bootstrap);
            c.close().await;
            hub.set_down("a", "b", true);
            // Owner-approved L1: an already writable member receives a queued
            // update of every row of the migration fixture after warm-up.
            let conn = connection(&a.database, Durability::Buffered).await?;
            let flushed = connection(&a.database, Durability::Flushed).await?;
            for first in (1..=fixture_rows()).step_by(32) {
                let last = (first + 31).min(fixture_rows());
                execute(if last == fixture_rows() { &flushed } else { &conn }, &format!("UPDATE items SET completed=1 WHERE key BETWEEN 'k{first:06}' AND 'k{last:06}'")).await?;
            }
            conn.close().await;
            flushed.close().await;
            assert_eq!(
                rows(&a, "SELECT count(*) FROM items WHERE completed=1").await[0][0]["value"],
                fixture_rows().to_string(),
                "bulk backlog was not authored"
            );
            return Ok(Self {
                _root: root,
                node: Arc::new(b),
                peer: Some(Arc::new(a)),
                hub,
            });
        }
        // Both writers update existing rows, so neither fixture grows with time.
        let mut tx = a.database.transaction().await?;
        for n in 0..32 {
            let id = tx.next_id("items", vec![]).await?;
            tx.execute(
                &format!(
                    "INSERT INTO items(key,title,completed,id) VALUES('pressure{n}','load',0,{id})"
                ),
                vec![],
            )
            .await?;
        }
        tx.commit().await?;
        if let Some(b) = &b {
            sync(b).await;
        }
        Ok(Self {
            _root: root,
            node: Arc::new(a),
            peer: b.map(Arc::new),
            hub,
        })
    }
}

async fn pull(device: &Device, updated: bool) -> Result<(), Failure> {
    let sql = if updated {
        "SELECT count(*) FROM items WHERE completed=1"
    } else {
        "SELECT count(*) FROM items"
    };
    let expected = fixture_rows().to_string();
    let mut last_report = Value::Null;
    let mut last_page = Value::Null;
    for _ in 0..64 {
        last_report = device.sync().await.map_err(|error| error.to_string())?;
        if let Ok(page) = device
            .call(json!({"type":"read","query":{"sql":sql,"values":[],"max_rows":1}}))
            .await
        {
            if page["rows"][0][0]["value"].as_str() == Some(expected.as_str()) {
                return Ok(());
            }
            last_page = page;
        }
    }
    Err(format!(
        "bulk member did not receive every fixture row: page={last_page} report={last_report}"
    )
    .into())
}

async fn ready_probe(device: &Device) -> Result<(), Failure> {
    // Bootstrap can refuse BEGIN, the write, or COMMIT until every scope is installed.
    let mut last_report = Value::Null;
    for attempt in 1..=64 {
        let result = async {
            let mut tx = device.database.transaction().await?;
            tx.execute(UPDATE, vec![]).await?;
            tx.commit().await?;
            Ok::<_, Failure>(())
        }
        .await;
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if error
                    .downcast_ref::<rocky_sdk::SqlFailure>()
                    .is_some_and(|error| error.retryable) =>
            {
                eprintln!("P12 fixture=probe_not_ready attempt={attempt} error={error}");
                // A failed bootstrap attempt can leave every peer in backoff.
                device
                    .ok(json!({"type":"networking_control","enabled":true}))
                    .await;
                last_report = device.sync().await.map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error),
        }
    }
    Err(format!("member bootstrap did not make the probe writable: {last_report}").into())
}

fn percentiles(values: &[u64]) -> [u64; 4] {
    if values.is_empty() {
        return [0; 4];
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let quantile = |p: usize| sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)];
    [
        quantile(50),
        quantile(95),
        quantile(99),
        sorted[sorted.len() - 1],
    ]
}
fn report(name: &str, values: &[u64]) {
    let [p50, p95, p99, max] = percentiles(values);
    eprintln!(
        "P12 metric={name} samples={} p50_us={p50} p95_us={p95} p99_us={p99} max_us={max}",
        values.len()
    );
}
fn merge(into: &mut Measurements, interval: Measurements) {
    for (site, mut values) in interval.holders {
        let holder = into.holders.entry(site).or_default();
        holder.wait_us.append(&mut values.wait_us);
        holder.hold_us.append(&mut values.hold_us);
        for (unit, mut samples) in values.units {
            holder.units.entry(unit).or_default().append(&mut samples);
        }
    }
    into.flush_us.extend(interval.flush_us);
    for (mode, mut samples) in interval.checkpoints {
        into.checkpoints
            .entry(mode)
            .or_default()
            .append(&mut samples);
    }
}
pub(super) fn holders(measurements: &Measurements) {
    for (order, percentile) in [("p99", 2), ("max", 3)] {
        let mut sites: Vec<_> = measurements.holders.iter().collect();
        sites
            .sort_by_key(|(_, values)| std::cmp::Reverse(percentiles(&values.hold_us)[percentile]));
        for ((site, class), values) in sites.into_iter().take(10) {
            let [p50, p95, p99, max] = percentiles(&values.hold_us);
            eprintln!(
                "P12 holder_order={order} site={site} class={class} acquisitions={} hold_p50_us={p50} hold_p95_us={p95} hold_p99_us={p99} hold_max_us={max} unit=acquisition",
                values.hold_us.len()
            );
        }
    }
}

fn merge_polls(into: &mut ReactorSnapshot, interval: ReactorSnapshot) {
    for (name, mut values) in interval.polls {
        into.polls.entry(name).or_default().append(&mut values);
    }
}

fn poll_table(snapshot: &ReactorSnapshot) {
    for (order, percentile) in [("p99", 2), ("max", 3)] {
        let mut tasks: Vec<_> = snapshot.polls.iter().collect();
        tasks.sort_by_key(|(_, values)| std::cmp::Reverse(percentiles(values)[percentile]));
        for (name, values) in tasks {
            let [p50, p95, p99, max] = percentiles(values);
            eprintln!(
                "P12 poll_order={order} task={name} polls={} poll_p50_us={p50} poll_p95_us={p95} poll_p99_us={p99} poll_max_us={max}",
                values.len()
            );
        }
    }
}

pub(super) fn unit_costs(measurements: &Measurements) {
    for (mode, samples) in &measurements.checkpoints {
        let elapsed: Vec<_> = samples.iter().map(|(_, us)| *us).collect();
        report(&format!("checkpoint_{mode}"), &elapsed);
        let costs: Vec<_> = samples
            .iter()
            .filter(|(pages, _)| *pages > 0)
            .map(|(pages, us)| us * 1000 / pages)
            .collect();
        report(&format!("checkpoint_{mode}_ns_per_page"), &costs);
    }
    for ((site, class), holder) in &measurements.holders {
        for (unit, samples) in &holder.units {
            let costs: Vec<_> = samples
                .iter()
                .map(|(units, hold)| hold * 1000 / units)
                .collect();
            let [p50, p95, p99, max] = percentiles(&costs);
            eprintln!(
                "P12 unit_site={site} class={class} unit={unit} acquisitions={} work={} p50_ns={p50} p95_ns={p95} p99_ns={p99} max_ns={max}",
                samples.len(),
                samples.iter().map(|(units, _)| units).sum::<u64>()
            );
        }
    }
}

async fn load(
    fixture: &Fixture,
    kind: Load,
    mut stop: watch::Receiver<bool>,
) -> Result<u64, Failure> {
    let mut units = 0;
    match kind {
        Load::Idle => {
            let _ = stop.changed().await;
        }
        Load::Bulk => {
            fixture.hub.set_down("a", "b", false);
            fixture.hub.network_changed();
            fixture
                .node
                .ok(json!({"type":"networking_control","enabled":true}))
                .await;
            pull(&fixture.node, true).await?;
            units = fixture_rows() as u64;
        }
        Load::Maintenance => {
            sync(&fixture.node).await;
            fixture.node.seal_and_compact().await;
            units = fixture_rows() as u64;
        }
        Load::Small => {
            let peer = fixture.peer.as_ref().ok_or("missing author")?;
            let mut author_stop = stop.clone();
            let author = async {
                let conn = connection(&peer.database, Durability::Flushed).await?;
                let mut ticks = interval(Duration::from_millis(20));
                ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
                let mut authored = 0;
                while !*author_stop.borrow() {
                    tokio::select! { _ = ticks.tick() => {}, _ = author_stop.changed() => break }
                    execute(
                        &conn,
                        "UPDATE items SET completed=1-completed WHERE key='pressure0'",
                    )
                    .await?;
                    authored += 1;
                }
                conn.close().await;
                Ok::<_, Failure>(authored)
            };
            let receiver = async {
                while !*stop.borrow() {
                    fixture.node.sync().await.map_err(|e| e.to_string())?;
                    tokio::task::yield_now().await;
                }
                Ok::<_, Failure>(())
            };
            let (authored, received) = tokio::join!(author, receiver);
            received?;
            units = authored?;
        }
        Load::Checkpoints => {
            let conn = connection(&fixture.node.database, Durability::Flushed).await?;
            let reader = fixture
                .node
                .database
                .connect(ConnectionOptions {
                    read_only: true,
                    durability: Durability::Buffered,
                    profile: None,
                    transaction_timeout: None,
                    binding: None,
                })
                .await?;
            execute(&reader, "BEGIN").await?;
            execute(&reader, "SELECT key FROM items LIMIT 1").await?;
            while !*stop.borrow() {
                execute(&conn, "BEGIN").await?;
                execute(
                    &conn,
                    "UPDATE items SET completed=1-completed WHERE key LIKE 'pressure%'",
                )
                .await?;
                execute(&conn, "COMMIT").await?;
                units += 32;
            }
            reader.close().await;
            conn.close().await;
        }
    }
    Ok(units)
}

async fn probe(
    database: &Database,
    count: usize,
    measurements: Option<&LockMeasurements>,
    reactor: Option<&ReactorMeasurements>,
    events: &TableEvents,
    buffered: Option<&Connection>,
) -> Result<(Measurements, ReactorSnapshot), Failure> {
    let mut write = Vec::with_capacity(count);
    let mut observed = Vec::with_capacity(count);
    let mut waits = Vec::with_capacity(count);
    let mut holds = Vec::with_capacity(count);
    let mut flushes = Vec::with_capacity(count);
    let mut delays = Vec::with_capacity(count);
    let mut in_calls = Vec::with_capacity(count);
    let mut unattributed = Vec::with_capacity(count);
    let mut overlap = Vec::with_capacity(count);
    let mut polls = ReactorSnapshot::default();
    let mut all = Measurements::default();
    let mut ticks = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_millis(100),
        Duration::from_millis(100),
    );
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    for _ in 0..count {
        ticks.tick().await;
        let start = Instant::now();
        let connection_id;
        if let Some(conn) = buffered {
            connection_id = conn.measurement_id();
            execute(conn, "BEGIN").await?;
            execute(conn, UPDATE).await?;
            execute(conn, "COMMIT").await?;
        } else {
            let mut transaction = database.transaction().await?;
            connection_id = transaction.measurement_id();
            transaction.execute(UPDATE, vec![]).await?;
            transaction.commit().await?;
        }
        let committed = Instant::now();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let change = events.next().await?;
                if change.tables.iter().any(|table| table == "write_probe")
                    && change.causes.iter().any(|cause| cause == "local")
                {
                    return Ok::<_, Failure>(());
                }
            }
        })
        .await??;
        write.push(committed.duration_since(start).as_micros() as u64);
        observed.push(committed.elapsed().as_micros() as u64);
        let interval = measurements.map(LockMeasurements::take).unwrap_or_default();
        let work = interval.connections.get(&connection_id);
        if measurements.is_some() && work.is_none() {
            return Err("probe SQL work was not attributed to its connection".into());
        }
        waits.push(work.map_or(0, |work| work.wait_us));
        holds.push(work.map_or(0, |work| work.hold_us));
        flushes.push(work.map_or(0, |work| work.flush_us));
        let timing = reactor.map(ReactorMeasurements::take).unwrap_or_default();
        let calls = timing.calls.get(&connection_id);
        if reactor.is_some() && calls.is_none() {
            return Err("probe calls were not attributed to their reactor connection".into());
        }
        let delay = calls.map_or(0, |calls| {
            calls.iter().map(|call| call.queue_us).sum::<u64>()
        });
        delays.push(delay);
        in_calls.push(calls.map_or(0, |calls| {
            calls.iter().map(|call| call.in_call_us).sum::<u64>()
        }));
        let accounted = waits.last().copied().unwrap_or(0)
            + holds.last().copied().unwrap_or(0)
            + flushes.last().copied().unwrap_or(0)
            + delay;
        let elapsed = *write.last().unwrap_or(&0);
        unattributed.push(elapsed.saturating_sub(accounted));
        overlap.push(accounted.saturating_sub(elapsed));
        merge_polls(&mut polls, timing);
        merge(&mut all, interval);
    }
    report("write", &write);
    report("observed", &observed);
    report("write_lock_wait", &waits);
    report("write_lock_hold", &holds);
    report("write_flush_wait", &flushes);
    report("write_reactor_delay", &delays);
    report("write_sql_in_call", &in_calls);
    report("write_unattributed", &unattributed);
    report("write_attribution_overlap", &overlap);
    report("replica_flush_wait", &all.flush_us);
    Ok((all, polls))
}

async fn scenario(kind: Load) -> Result<(), Failure> {
    if cfg!(debug_assertions) {
        return Err("run measurement tests with --release".into());
    }
    let modes: &[Durability] = match std::env::var("ROCKY_PROBE_DURABILITY").as_deref() {
        Ok("Flushed") => &[Durability::Flushed],
        Ok("Buffered") => &[Durability::Buffered],
        Err(std::env::VarError::NotPresent) => &[Durability::Flushed, Durability::Buffered],
        _ => return Err("ROCKY_PROBE_DURABILITY must be Flushed or Buffered".into()),
    };
    for &durability in modes {
        let fixture = Fixture::new(kind).await?;
        let measurements = fixture.node.database.lock_measurements().await?;
        let reactor = fixture.node.database.reactor_measurements().await?;
        let events = fixture
            .node
            .database
            .observe_tables(Some(vec!["write_probe".into()]))
            .await?;
        let buffered = if durability == Durability::Buffered {
            Some(connection(&fixture.node.database, durability).await?)
        } else {
            None
        };
        eprintln!(
            "P12 scenario={} durability={durability:?} profile=release rows=100000 samples=600 warmup_samples=50 period_ms=100",
            kind.name()
        );
        let (warmup, samples) = if smoke() { (1, 3) } else { (50, 600) };
        eprintln!(
            "P12 mode={} rows={} warmup_samples={warmup} samples={samples}",
            if smoke() { "smoke" } else { "baseline" },
            fixture_rows()
        );
        // Warm-up precedes the one-shot load so its beginning is measured.
        probe(
            &fixture.node.database,
            warmup,
            None,
            None,
            &events,
            buffered.as_ref(),
        )
        .await?;
        measurements.start();
        reactor.start();
        let (stop, stopping) = watch::channel(false);
        let started = Instant::now();
        let workload = async {
            let result = load(&fixture, kind, stopping).await;
            eprintln!(
                "P12 scenario={} load_elapsed_us={} units={result:?}",
                kind.name(),
                started.elapsed().as_micros()
            );
            result
        };
        let measurement = async {
            let result = probe(
                &fixture.node.database,
                samples,
                Some(&measurements),
                Some(&reactor),
                &events,
                buffered.as_ref(),
            )
            .await;
            let _ = stop.send(true);
            result
        };
        let (result, loaded) = tokio::join!(measurement, workload);
        let (mut result, mut reactor_result) = result?;
        loaded?;
        merge(&mut result, measurements.stop());
        merge_polls(&mut reactor_result, reactor.stop());
        poll_table(&reactor_result);
        if kind == Load::Checkpoints && !smoke() {
            for mode in ["passive", "truncate"] {
                assert!(
                    result
                        .checkpoints
                        .get(mode)
                        .is_some_and(|samples| !samples.is_empty()),
                    "missing {mode} checkpoint"
                );
            }
        }
        holders(&result);
        unit_costs(&result);
        if let Some(conn) = buffered {
            conn.close().await;
        }
        fixture.node.close().await;
        if let Some(peer) = &fixture.peer {
            peer.close().await;
        }
    }
    Ok(())
}

macro_rules! scenario_test {
    ($name:ident, $load:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "60-second release qualification per durability; run alone"]
        async fn $name() -> Result<(), Failure> {
            scenario(Load::$load).await
        }
    };
}
scenario_test!(l0_idle, Idle);
scenario_test!(l1_bulk_receive, Bulk);
scenario_test!(l2_small_receive, Small);
scenario_test!(l3_local_maintenance, Maintenance);
scenario_test!(l4_checkpoint_pressure, Checkpoints);
