//! Bounded-state probe: drives the durable store as a single-voter Raft
//! would, with deterministic authority time, over retention windows. The
//! ignored ten-window run and its report are in
//! `docs/runtime/coordinator-evidence.md`.
use super::*;
use crate::machine::Outcome;
use crate::{Limits, retention::DAY};
use openraft::storage::RaftLogStorageExt;
use p2panda_core::PrivateKey;
use rocky_core::*;
use std::time::{Duration, Instant};

const T0: u64 = 1_900_000_000;
const DOMAINS: usize = 4;
const SPARE: usize = 2;
const DEVICES: usize = 8;
const SNAPSHOT_EVERY: u64 = 1024;
const KEEP: u64 = 8;

struct Domain {
    scope: Scope,
    revision: u64,
    devices: Vec<PrivateKey>,
    head: StrongHead,
    open: bool,
}
struct Probe {
    store: Option<Store>,
    genesis: CertifiedGenesis,
    authority: PrivateKey,
    executor: PrivateKey,
    owner: PrincipalId,
    domains: Vec<Domain>,
    spare: Vec<Scope>,
    index: u64,
    snapshot: u64,
    counter: u64,
    apply: Duration,
    applied: u64,
    compaction: Duration,
    peak_disk: u64,
    latencies: Vec<Duration>,
    peak_swap_bytes: u64,
}
#[derive(Default)]
struct Window {
    entries: usize,
    sessions: usize,
    state_bytes: usize,
    retry: usize,
    logs: usize,
    snapshots: usize,
    disk: u64,
    allocated: u64,
    snapshot_bytes: usize,
    confirmed: u64,
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
    fn control(
        &self,
        scope: Scope,
        revision: u64,
        devices: &[PrivateKey],
        now: u64,
    ) -> ControlCertificate {
        let mut invokers = vec![(self.executor.public_key(), self.owner)];
        invokers.extend(devices.iter().map(|d| (d.public_key(), self.owner)));
        ControlCertificate::sign(
            DomainControl {
                scope,
                configuration: [5; 16],
                revision,
                owner: self.owner,
                executors: vec![self.executor.public_key()],
                invokers,
                schema: 1,
                handler: 1,
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
                        request: Request::sign_at(
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
    fn sequence(&mut self, now: u64) -> u64 {
        self.counter += 1;
        (now << ISSUE_SHIFT) + self.counter % (1 << ISSUE_SHIFT)
    }
    fn proposal(&self, domain: usize, device: &PrivateKey, sequence: u64) -> Confirmation {
        let head = &self.domains[domain].head;
        let request_hash = commitment("probe", &sequence.to_le_bytes());
        let mut p = Confirmation {
            caller: self.owner,
            scope: head.scope,
            configuration: head.configuration,
            executor: head.executor,
            epoch: head.epoch,
            request: RequestId {
                node: node_id(device.public_key()),
                sequence: AuthorSequence::new(sequence).unwrap(),
            },
            request_hash,
            expected_sequence: head.sequence,
            expected_head: head.head,
            schema: 1,
            handler: 1,
            result: [3; 32],
            new_head: [0; 32],
        };
        p.new_head = successor(p.expected_head, head.sequence + 1, p.request_hash, p.result);
        p
    }
    /// One simulated day: commands, acknowledgments, reads, a takeover.
    async fn day(&mut self, day: u64, window: &mut Window) {
        let now = T0 + day * DAY;
        for d in 0..self.domains.len() {
            if !self.domains[d].open {
                continue;
            }
            let scope = self.domains[d].scope;
            let devices = self.domains[d].devices.clone();
            let mut issued = Vec::new();
            for (n, device) in devices.iter().enumerate() {
                // Every device issues one command a day; even ones issue two.
                for _ in 0..1 + usize::from(n % 2 == 0) {
                    let sequence = self.sequence(now);
                    let p = self.proposal(d, device, sequence);
                    match self
                        .run(now, vec![(self.executor.clone(), Operation::Confirm(p))])
                        .await
                        .remove(0)
                    {
                        Ok(Outcome::Receipt(Some(receipt))) => {
                            let head = &mut self.domains[d].head;
                            head.sequence = receipt.sequence;
                            head.head = receipt.confirmation.new_head;
                            window.confirmed += 1;
                            issued.push((device.clone(), sequence));
                        }
                        Err(CoordinationError::Backpressure) => window.refused += 1,
                        other => panic!("confirm: {other:?}"),
                    }
                }
            }
            // Half the devices acknowledge; the others rely on the horizon.
            let mut batch = Vec::new();
            for (device, sequence) in issued.iter().filter(|(device, _)| {
                devices
                    .iter()
                    .position(|d| d.public_key() == device.public_key())
                    .unwrap()
                    < DEVICES / 2
            }) {
                batch.push((
                    device.clone(),
                    Operation::Retire {
                        scope,
                        through: *sequence,
                    },
                ));
            }
            batch.push((
                self.executor.clone(),
                Operation::Takeover {
                    scope,
                    expected_epoch: self.domains[d].head.epoch,
                },
            ));
            let replies = self.run(now, batch).await;
            match replies.last() {
                Some(Ok(Outcome::Head(head))) => self.domains[d].head = head.clone(),
                other => panic!("takeover: {other:?}"),
            }
            assert!(replies.iter().all(Result::is_ok), "{replies:?}");
        }
        // Weekly administration: rotate two devices per domain.
        if day % 7 == 6 {
            for d in 0..self.domains.len() {
                if !self.domains[d].open {
                    continue;
                }
                let mut devices = self.domains[d].devices.clone();
                devices.drain(..2);
                devices.extend([PrivateKey::new(), PrivateKey::new()]);
                let revision = self.domains[d].revision + 1;
                let certificate = self.control(self.domains[d].scope, revision, &devices, now);
                let replies = self
                    .run(
                        now,
                        vec![
                            (self.authority.clone(), Operation::Control(certificate)),
                            (
                                self.executor.clone(),
                                Operation::Acquire {
                                    scope: self.domains[d].scope,
                                },
                            ),
                        ],
                    )
                    .await;
                let Ok(Outcome::Head(head)) = &replies[1] else {
                    panic!("reacquire: {replies:?}")
                };
                let domain = &mut self.domains[d];
                domain.revision = revision;
                domain.devices = devices;
                domain.head = head.clone();
            }
        }
        // Every three weeks the owner closes one domain and opens a spare one.
        if day % 21 == 20
            && let Some(next) = self.spare.pop()
        {
            let d = self.domains.iter().position(|d| d.open).unwrap();
            let head = self.domains[d].head.clone();
            let replies = self
                .run(
                    now,
                    vec![(self.authority.clone(), Operation::Close { head })],
                )
                .await;
            assert!(matches!(replies[0], Ok(Outcome::Head(_))), "{replies:?}");
            self.domains[d].open = false;
            let devices: Vec<_> = (0..DEVICES).map(|_| PrivateKey::new()).collect();
            let certificate = self.control(next, 1, &devices, now);
            let replies = self
                .run(
                    now,
                    vec![
                        (self.authority.clone(), Operation::Control(certificate)),
                        (self.executor.clone(), Operation::Acquire { scope: next }),
                    ],
                )
                .await;
            let Ok(Outcome::Head(head)) = &replies[1] else {
                panic!("open: {replies:?}")
            };
            self.domains.push(Domain {
                scope: next,
                revision: 1,
                devices,
                head: head.clone(),
                open: true,
            });
        }
        let footprint = self.store().footprint().await.unwrap();
        let encoded = encode(&self.store().0.image.lock().await.metadata)
            .unwrap()
            .len();
        window.entries = window.entries.max(footprint.state.entries);
        window.sessions = window.sessions.max(footprint.state.sessions);
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
    let owner = PrincipalId([9; 16]);
    let scopes: Vec<_> = (1..=(DOMAINS + SPARE) as u8).map(scope).collect();
    let directory = ControlCertificate::sign(
        DomainControl {
            scope: scopes[0],
            configuration: [5; 16],
            revision: 1,
            owner,
            executors: vec![executor.public_key()],
            invokers: vec![(executor.public_key(), owner)],
            schema: 1,
            handler: 1,
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
                    key: PrivateKey::new().public_key(),
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
        owner,
        domains: Vec::new(),
        spare: scopes[DOMAINS..].iter().rev().copied().collect(),
        index: 0,
        snapshot: 0,
        counter: 0,
        apply: Duration::ZERO,
        applied: 0,
        compaction: Duration::ZERO,
        peak_disk: 0,
        latencies: Vec::new(),
        peak_swap_bytes: 0,
    };
    for (n, scope) in scopes[..DOMAINS].iter().enumerate() {
        let devices: Vec<_> = (0..DEVICES).map(|_| PrivateKey::new()).collect();
        let revision = if n == 0 { 2 } else { 1 };
        let certificate = probe.control(*scope, revision, &devices, T0);
        let replies = probe
            .run(
                T0,
                vec![
                    (probe.authority.clone(), Operation::Control(certificate)),
                    (probe.executor.clone(), Operation::Acquire { scope: *scope }),
                ],
            )
            .await;
        let Ok(Outcome::Head(head)) = &replies[1] else {
            panic!("setup: {replies:?}")
        };
        probe.domains.push(Domain {
            scope: *scope,
            revision,
            devices,
            head: head.clone(),
            open: true,
        });
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
        "window | confirmed refused | sessions responses state_bytes retry | logs snapshots snapshot_bytes disk_bytes allocated_bytes"
    );
    for (n, w) in report.iter().enumerate() {
        println!(
            "{n:>6} | {:>9} {:>7} | {:>8} {:>9} {:>11} {:>5} | {:>4} {:>9} {:>14} {:>10} {:>15}",
            w.confirmed,
            w.refused,
            w.sessions,
            w.entries,
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
    // Settled after the first horizon: later windows stay within the
    // second window's maxima, so nothing grows with lifetime.
    let settled = &report[1..];
    let ceiling = |f: fn(&Window) -> u64| settled.first().map(f).unwrap_or(0) * 5 / 4 + 1;
    for w in settled.iter().skip(1) {
        assert!(
            w.sessions as u64 <= ceiling(|w| w.sessions as u64),
            "sessions grew"
        );
        assert!(
            w.entries as u64 <= ceiling(|w| w.entries as u64),
            "responses grew"
        );
        assert!(
            w.state_bytes as u64 <= ceiling(|w| w.state_bytes as u64),
            "state grew"
        );
        assert!(
            w.snapshot_bytes as u64 <= ceiling(|w| w.snapshot_bytes as u64),
            "snapshot grew"
        );
        assert!(
            w.retry as u64 <= ceiling(|w| w.retry as u64),
            "retry records grew"
        );
        assert_eq!(w.refused, 0, "a bounded window refused work");
    }
    // Configured budgets: sessions per open domain, tenant bytes, two snapshots.
    for w in &report {
        assert!(w.disk <= 16 * 1024 * 1024);
        assert!(w.allocated <= 16 * 1024 * 1024);
        assert!(w.snapshot_bytes <= 256 * 1024);
        assert!(w.sessions <= (DOMAINS + SPARE) * limits.sessions);
        assert!(w.state_bytes <= limits.tenant_bytes);
        assert!(w.snapshots <= 2);
        assert!(w.logs as u64 <= SNAPSHOT_EVERY + KEEP + 64);
    }
}
