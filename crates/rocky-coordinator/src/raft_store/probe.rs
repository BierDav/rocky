//! Bounded-state probe: drives the durable store as a single-voter Raft
//! would, with deterministic authority time, over weekly windows of
//! coordinated-log commits, control revisions and closures. The
//! ignored ten-window run and its report are in
//! `docs/runtime/coordinator-evidence.md`.
use super::*;
use crate::coordinated::{Action, Result as LogResult};
use crate::machine::Outcome;
use crate::{Limits, retention::DAY};
use openraft::storage::RaftLogStorageExt;
use p2panda_core::PrivateKey;
use rocky_core::*;
use std::time::{Duration, Instant};

const T0: u64 = 1_900_000_000;
const DOMAINS: usize = 4;
const SPARE: usize = 2;
/// Entries each open domain commits a day.
const ENTRIES: usize = 12;
const SNAPSHOT_EVERY: u64 = 1024;
const KEEP: u64 = 8;

struct Domain {
    scope: Scope,
    revision: u64,
    /// The trusted node's last join or append sequence.
    sequence: u64,
    open: bool,
}
struct Probe {
    store: Option<Store>,
    genesis: CertifiedGenesis,
    authority: PrivateKey,
    executor: PrivateKey,
    /// The voter that resolves entries with the nodes' signed acknowledgments.
    voter: PrivateKey,
    domains: Vec<Domain>,
    spare: Vec<Scope>,
    index: u64,
    snapshot: u64,
    apply: Duration,
    applied: u64,
    compaction: Duration,
    peak_disk: u64,
    latencies: Vec<Duration>,
    peak_swap_bytes: u64,
}
#[derive(Default)]
struct Window {
    state_bytes: usize,
    retry: usize,
    logs: usize,
    snapshots: usize,
    disk: u64,
    allocated: u64,
    snapshot_bytes: usize,
    committed: u64,
    refused: u64,
}
fn scope(n: u8) -> Scope {
    Scope {
        mesh: MeshId([1; 16]),
        app: AppId([2; 16]),
        domain: DomainId([n; 16]),
    }
}
impl Probe {
    fn store(&mut self) -> &mut Store {
        self.store.as_mut().expect("open store")
    }
    fn control(&self, scope: Scope, revision: u64, now: u64) -> ControlCertificate {
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [5; 16],
                revision,
                trusted: vec![self.executor.public_key()],
                not_before: now - DAY,
                expires: now + 30 * DAY,
            },
            &self.authority,
        )
        .unwrap()
    }
    async fn run(
        &mut self,
        now: u64,
        batch: Vec<(PrivateKey, Operation)>,
    ) -> Vec<Result<Outcome, CoordinationError>> {
        let entries: Vec<_> = batch
            .into_iter()
            .map(|(key, operation)| {
                self.index += 1;
                Entry::<Types> {
                    log_id: LogId::new(CommittedLeaderId::new(1, 1), self.index),
                    payload: EntryPayload::Normal(Proposal {
                        time: now,
                        request: crate::machine::self_certified(
                            self.genesis.id().unwrap(),
                            nonce(),
                            operation,
                            &key,
                            now,
                        )
                        .unwrap(),
                    }),
                }
            })
            .collect();
        let last = entries.last().map(|e| e.log_id);
        let count = entries.len() as u64;
        let durable_started = Instant::now();
        self.store().blocking_append(entries.clone()).await.unwrap();
        self.store().save_committed(last).await.unwrap();
        let started = Instant::now();
        let replies = self.store().apply(entries).await.unwrap();
        self.apply += started.elapsed();
        self.applied += count;
        self.latencies.push(durable_started.elapsed());
        if self.index - self.snapshot >= SNAPSHOT_EVERY {
            let before = self.store().footprint().await.unwrap().disk_bytes;
            let snapshot_size = encode(&self.store().0.image.lock().await.metadata)
                .unwrap()
                .len() as u64;
            // Conservative peak: two old snapshots plus the new file and its encoding overhead.
            self.peak_swap_bytes = self.peak_swap_bytes.max(before + snapshot_size + 4096);
            let started = Instant::now();
            self.store().build_snapshot().await.unwrap();
            self.peak_disk = self
                .peak_disk
                .max(self.store().footprint().await.unwrap().disk_bytes);
            let index = self.index;
            self.store()
                .purge(LogId::new(CommittedLeaderId::new(1, 1), index - KEEP))
                .await
                .unwrap();
            self.compaction += started.elapsed();
            self.snapshot = self.index;
        }
        self.peak_disk = self
            .peak_disk
            .max(self.store().footprint().await.unwrap().disk_bytes);
        replies
    }
    /// The trusted node's next call on domain `d`.
    fn next(&mut self, d: usize) -> u64 {
        self.domains[d].sequence += 1;
        self.domains[d].sequence
    }
    async fn log(
        &mut self,
        now: u64,
        scope: Scope,
        action: Action,
    ) -> Result<Outcome, CoordinationError> {
        let operation = Operation::Coordinated { scope, action };
        self.run(now, vec![(self.executor.clone(), operation)])
            .await
            .remove(0)
    }
    /// Registers `scope` under its first control and joins its log.
    async fn open_domain(&mut self, scope: Scope, revision: u64, now: u64) {
        let certificate = self.control(scope, revision, now);
        let replies = self
            .run(
                now,
                vec![(self.authority.clone(), Operation::Control(certificate))],
            )
            .await;
        assert!(replies.iter().all(Result::is_ok), "{replies:?}");
        self.domains.push(Domain {
            scope,
            revision,
            sequence: 0,
            open: true,
        });
        let d = self.domains.len() - 1;
        let sequence = self.next(d);
        let join = Action::Join {
            position: 0,
            running_hash: genesis(scope, [5; 16]),
            sequence,
        };
        self.log(now, scope, join).await.unwrap();
    }
    /// One simulated day: committed entries, a control revision each week
    /// and a closure every three.
    async fn day(&mut self, day: u64, window: &mut Window) {
        let now = T0 + day * DAY;
        for d in 0..self.domains.len() {
            if !self.domains[d].open {
                continue;
            }
            let scope = self.domains[d].scope;
            for n in 0..ENTRIES {
                let head = self.store().log_head(scope).await.unwrap();
                let sequence = self.next(d);
                let append = Action::Append {
                    expected: head.head,
                    epoch: head.epoch,
                    hash: commitment("probe", &[day, n as u64].map(u64::to_le_bytes).concat()),
                    deadline: now + 20,
                    sequence,
                };
                let pending = match self.log(now, scope, append).await {
                    Ok(Outcome::Coordinated(LogResult::Assigned(pending))) => *pending,
                    Err(CoordinationError::Backpressure) => {
                        window.refused += 1;
                        continue;
                    }
                    other => panic!("append: {other:?}"),
                };
                let acknowledgment = crate::machine::self_certified(
                    self.genesis.id().unwrap(),
                    nonce(),
                    Operation::Coordinated {
                        scope,
                        action: Action::Acknowledge {
                            pending: pending.clone(),
                        },
                    },
                    &self.executor,
                    now,
                )
                .unwrap();
                let resolve = Operation::Coordinated {
                    scope,
                    action: Action::Resolve {
                        pending,
                        acknowledgments: vec![acknowledgment],
                    },
                };
                match self
                    .run(now, vec![(self.voter.clone(), resolve)])
                    .await
                    .remove(0)
                {
                    Ok(Outcome::Coordinated(LogResult::Committed { .. })) => window.committed += 1,
                    other => panic!("resolve: {other:?}"),
                }
            }
        }
        // Weekly administration: a control revision per domain.
        if day % 7 == 6 {
            for d in 0..self.domains.len() {
                if !self.domains[d].open {
                    continue;
                }
                let revision = self.domains[d].revision + 1;
                let certificate = self.control(self.domains[d].scope, revision, now);
                let replies = self
                    .run(
                        now,
                        vec![(self.authority.clone(), Operation::Control(certificate))],
                    )
                    .await;
                assert!(replies.iter().all(Result::is_ok), "{replies:?}");
                self.domains[d].revision = revision;
            }
        }
        // Every three weeks the owner closes one domain and opens a spare one.
        if day % 21 == 20
            && let Some(next) = self.spare.pop()
        {
            let d = self.domains.iter().position(|d| d.open).unwrap();
            let scope = self.domains[d].scope;
            let replies = self
                .run(
                    now,
                    vec![(self.authority.clone(), Operation::Close { scope })],
                )
                .await;
            assert!(matches!(replies[0], Ok(Outcome::Closed)), "{replies:?}");
            self.domains[d].open = false;
            self.open_domain(next, 1, now).await;
        }
        let footprint = self.store().footprint().await.unwrap();
        let encoded = encode(&self.store().0.image.lock().await.metadata)
            .unwrap()
            .len();
        window.state_bytes = window.state_bytes.max(footprint.state.bytes);
        window.retry = window.retry.max(footprint.retry_records);
        window.logs = window.logs.max(footprint.logs);
        window.snapshots = window.snapshots.max(footprint.snapshots);
        window.disk = window.disk.max(footprint.disk_bytes);
        window.allocated = window.allocated.max(footprint.allocated_bytes);
        window.snapshot_bytes = window.snapshot_bytes.max(encoded);
    }
    /// Crash and reopen: the newest snapshot plus committed log replay.
    async fn reopen(&mut self, path: &Path) {
        let before = self.store().footprint().await.unwrap().state;
        drop(self.store.take());
        let mut store = Store::open(path, self.genesis.clone(), false, Limits::default()).unwrap();
        let (applied, _) = store.applied_state().await.unwrap();
        let start = applied.map_or(0, |id| id.index + 1);
        let entries = store.try_get_log_entries(start..=self.index).await.unwrap();
        assert_eq!(entries.len() as u64, self.index + 1 - start.max(1));
        store.apply(entries).await.unwrap();
        self.store = Some(store);
        assert_eq!(self.store().footprint().await.unwrap().state, before);
    }
}
fn nonce() -> Commitment {
    *PrivateKey::new().public_key().as_bytes()
}

/// The gate's run: at least ten windows, about a minute and a half of fsyncs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "bounded-state probe; run explicitly with --ignored --nocapture"]
async fn coordinator_state_plateaus_over_ten_retention_windows_with_churn() {
    let windows = std::env::var("ROCKY_PROBE_WINDOWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10);
    probe(windows.max(10)).await;
}
/// The same workload over two windows, for every workspace check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coordinator_state_settles_after_one_retention_window() {
    probe(2).await;
}
async fn probe(windows: u64) {
    let limits = Limits::default();
    let root = PrivateKey::new();
    let authority = PrivateKey::new();
    let executor = PrivateKey::new();
    let voter = PrivateKey::new();
    let scopes: Vec<_> = (1..=(DOMAINS + SPARE) as u8).map(scope).collect();
    let directory = ControlCertificate::sign(
        DomainControl {
            scope: scopes[0],
            configuration: [5; 16],
            revision: 1,
            trusted: vec![executor.public_key()],
            not_before: T0 - DAY,
            expires: T0 + 30 * DAY,
        },
        &authority,
    )
    .unwrap();
    let genesis = CertifiedGenesis::sign(
        Genesis {
            mesh: scopes[0].mesh,
            configuration: [5; 16],
            voters: (1..=3)
                .map(|id| Voter {
                    id,
                    address: format!("127.0.0.1:{}", 9000 + id),
                    key: if id == 1 {
                        voter.public_key()
                    } else {
                        PrivateKey::new().public_key()
                    },
                })
                .collect(),
            initial_voters: [1, 2, 3].into(),
            membership_authority: authority.public_key(),
            directory_definition: [1; 32],
            directory: Some(directory),
            authorities: scopes
                .iter()
                .map(|scope| crate::certified::Authority {
                    scope: *scope,
                    issuer: authority.public_key(),
                })
                .collect(),
            max_control_lifetime: 366 * DAY,
            public: None,
        },
        &root,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("voter");
    let mut probe = Probe {
        store: Some(Store::open(&path, genesis.clone(), true, limits).unwrap()),
        genesis,
        authority,
        executor,
        voter,
        domains: Vec::new(),
        spare: scopes[DOMAINS..].iter().rev().copied().collect(),
        index: 0,
        snapshot: 0,
        apply: Duration::ZERO,
        applied: 0,
        compaction: Duration::ZERO,
        peak_disk: 0,
        latencies: Vec::new(),
        peak_swap_bytes: 0,
    };
    for (n, scope) in scopes[..DOMAINS].iter().enumerate() {
        // The genesis already installed the first domain's first control.
        let revision = if n == 0 { 2 } else { 1 };
        probe.open_domain(*scope, revision, T0).await;
    }
    let mut report = Vec::new();
    let mut peak_rss = 0;
    let days = (windows + 1) * 7;
    let mut window = Window::default();
    for day in 0..days {
        probe.day(day, &mut window).await;
        if day == days / 2 {
            probe.reopen(&path).await;
        }
        if day % 7 == 6 {
            let output = std::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &std::process::id().to_string()])
                .output()
                .unwrap();
            let rss: u64 = String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            peak_rss = peak_rss.max(rss * 1024);
            report.push(std::mem::take(&mut window));
        }
    }
    println!(
        "window | committed refused | state_bytes retry | logs snapshots snapshot_bytes disk_bytes allocated_bytes"
    );
    for (n, w) in report.iter().enumerate() {
        println!(
            "{n:>6} | {:>9} {:>7} | {:>11} {:>5} | {:>4} {:>9} {:>14} {:>10} {:>15}",
            w.committed,
            w.refused,
            w.state_bytes,
            w.retry,
            w.logs,
            w.snapshots,
            w.snapshot_bytes,
            w.disk,
            w.allocated
        );
    }
    println!(
        "entries={} mean apply={:?} compaction total={:?} peak disk bytes={}",
        probe.applied,
        probe.apply / probe.applied as u32,
        probe.compaction,
        probe.peak_disk
    );
    probe.latencies.sort_unstable();
    let p99 = probe.latencies[probe.latencies.len() * 99 / 100];
    let mean = probe.latencies.iter().sum::<Duration>() / probe.latencies.len() as u32;
    println!(
        "durable batch mean={mean:?} p99={p99:?} conservative snapshot swap peak bytes={}",
        probe.peak_swap_bytes
    );
    assert!(mean < Duration::from_millis(50));
    println!("sampled peak process RSS bytes={peak_rss}");
    assert!(peak_rss <= 128 * 1024 * 1024);
    assert!(p99 < Duration::from_millis(250));
    // Settled after the first window: accounted state and retry records stay
    // within the second window's maxima, so they do not grow with lifetime.
    let settled = &report[1..];
    let ceiling = |f: fn(&Window) -> u64| settled.first().map(f).unwrap_or(0) * 5 / 4 + 1;
    for w in settled.iter().skip(1) {
        assert!(
            w.state_bytes as u64 <= ceiling(|w| w.state_bytes as u64),
            "state grew"
        );
        assert!(
            w.retry as u64 <= ceiling(|w| w.retry as u64),
            "retry records grew"
        );
        assert_eq!(w.refused, 0, "a bounded window refused work");
    }
    // Configured budgets: tenant bytes, two snapshots. Snapshots grow with
    // each log's hash window until it holds `HASH_WINDOW` hashes.
    for w in &report {
        assert!(w.disk <= 16 * 1024 * 1024);
        assert!(w.allocated <= 16 * 1024 * 1024);
        assert!(w.snapshot_bytes <= 256 * 1024);
        assert!(w.state_bytes <= limits.tenant_bytes);
        assert!(w.snapshots <= 2);
        assert!(w.logs as u64 <= SNAPSHOT_EVERY + KEEP + 64);
    }
}
