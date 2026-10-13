use super::*;
use std::{path::PathBuf, sync::atomic::AtomicUsize, time::Instant};

struct Fixture {
    root: PathBuf,
    settings: [Settings; 2],
    vaults: [Vault; 2],
    databases: [PathBuf; 2],
    identities: [PathBuf; 2],
}

pub(super) async fn run(exhaustive: bool) -> Result<(), Failure> {
    tokio::task::LocalSet::new()
        .run_until(async {
            let root = if cfg!(target_os = "linux") {
                tempfile::Builder::new()
                    .prefix("rocky-cuts-")
                    .tempdir_in("/dev/shm")
                    .or_else(|_| tempfile::tempdir())?
            } else {
                tempfile::tempdir()?
            };
            let hub = Arc::new(Hub::default());
            let (a, b) = frozen(async {
                let a = enrolled(&root.path().join("a"), &hub, "a", None).await?;
                let b = enrolled(
                    &root.path().join("b"),
                    &hub,
                    "b",
                    Some((&a, Profile::Issuer)),
                )
                .await?;
                targets(&a, std::slice::from_ref(&b)).await?;
                // Establish enrollment metadata before the bidirectional row session.
                a.sync(&*transport(&a).await?).await?;
                add(&a, 1).await?;
                add(&b, 2).await?;
                Ok::<_, Failure>((a, b))
            })
            .await?;
            let (a_settings, b_settings) = (
                a.settings.lock().await.clone(),
                b.settings.lock().await.clone(),
            );
            let (a_vault, b_vault) = (a.vault.clone(), b.vault.clone());
            let (a_copy, b_copy) = (
                root.path().join("a-copy.sqlite"),
                root.path().join("b-copy.sqlite"),
            );
            frozen(a.replica.backup_into(&a_copy)).await?;
            frozen(b.replica.backup_into(&b_copy)).await?;
            let (a_identity, b_identity) = (
                root.path().join("a-copy.json"),
                root.path().join("b-copy.json"),
            );
            a_settings.save(&a_identity)?;
            b_settings.save(&b_identity)?;
            let before = *hub.written().borrow();
            frozen(a.sync(&*transport(&a).await?)).await?;
            let bytes = *hub.written().borrow() - before;
            assert!(bytes > 0);
            frozen(a.detach(None)).await;
            frozen(b.detach(None)).await;

            let fixture = Fixture {
                root: root.path().to_owned(),
                settings: [(*a_settings).clone(), (*b_settings).clone()],
                vaults: [a_vault, b_vault],
                databases: [a_copy, b_copy],
                identities: [a_identity, b_identity],
            };
            let offsets: Vec<_> = if exhaustive {
                (0..=bytes).collect()
            } else {
                [0, 1, 300, 3_000, 9_000, 58_030, bytes / 2, bytes - 1, bytes]
                    .into_iter()
                    .collect()
            };
            let count = offsets.len();
            let completed = AtomicUsize::new(0);
            let started = Instant::now();
            if exhaustive {
                let workers = std::thread::available_parallelism()?.get().min(8);
                eprintln!("exercising {count} session byte offsets on {workers} workers");
                // Each worker owns its paused clock and local tasks; byte cuts are independent.
                std::thread::scope(|scope| -> Result<(), Failure> {
                    let workers: Vec<_> = offsets
                        .chunks(count.div_ceil(workers))
                        .map(|offsets| {
                            let (fixture, completed) = (&fixture, &completed);
                            scope.spawn(move || -> Result<(), Failure> {
                                let runtime = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .start_paused(true)
                                    .build()?;
                                runtime.block_on(
                                    tokio::task::LocalSet::new()
                                        .run_until(trials(fixture, offsets, 1, completed, started)),
                                )
                            })
                        })
                        .collect();
                    for worker in workers {
                        worker.join().expect("cancellation worker panicked")?;
                    }
                    Ok(())
                })?;
            } else {
                trials(&fixture, &offsets, 8, &completed, started).await?;
            }
            assert_eq!(completed.load(Ordering::Relaxed), count);
            eprintln!(
                "cancelled and converged at {count} byte offsets in {:?}",
                started.elapsed()
            );
            Ok(())
        })
        .await
}

async fn trials(
    fixture: &Fixture,
    offsets: &[usize],
    concurrency: usize,
    completed: &AtomicUsize,
    started: Instant,
) -> Result<(), Failure> {
    let trials = stream::iter(offsets.iter().copied())
        .map(|offset| frozen(trial(fixture, offset)))
        .buffer_unordered(concurrency);
    tokio::pin!(trials);
    while let Some(result) = trials.next().await {
        result?;
        let finished = completed.fetch_add(1, Ordering::Relaxed) + 1;
        if finished.is_multiple_of(1000) {
            eprintln!("verified {finished} offsets in {:?}", started.elapsed());
        }
    }
    Ok(())
}

struct Progress {
    offset: usize,
    phase: std::cell::Cell<&'static str>,
}
impl Drop for Progress {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "byte cut {} stalled during {}",
                self.offset,
                self.phase.get()
            );
        }
    }
}

async fn trial(fixture: &Fixture, offset: usize) -> Result<(), Failure> {
    let progress = Progress {
        offset,
        phase: std::cell::Cell::new("fixture"),
    };
    let Fixture {
        root,
        settings,
        vaults,
        databases,
        identities,
    } = fixture;
    let [a_settings, b_settings] = settings;
    let [a_vault, b_vault] = vaults;
    let [a_copy, b_copy] = databases;
    let [a_identity, b_identity] = identities;
    let hub = Arc::new(Hub::default());
    let trial = root.join("trial").join(offset.to_string());
    let (a_dir, b_dir) = (trial.join("a"), trial.join("b"));
    for (directory, copy, identity) in [(&a_dir, a_copy, a_identity), (&b_dir, b_copy, b_identity)]
    {
        std::fs::create_dir_all(directory)?;
        std::fs::copy(copy, directory.join("rocky.sqlite"))?;
        std::fs::copy(identity, directory.join("identity.json"))?;
    }
    let a = node(
        &a_dir,
        a_settings.clone(),
        a_vault.clone(),
        hub.connector("a").start(true, [0; 32]).await?,
    )
    .await?;
    let b = node(
        &b_dir,
        b_settings.clone(),
        b_vault.clone(),
        hub.connector("b").start(true, [0; 32]).await?,
    )
    .await?;
    progress.phase.set("waiting for byte cut");
    let mut written = hub.written();
    let before = *written.borrow_and_update();
    let dials = hub.dials_to("b");
    hub.hold_cut.store(true, Ordering::SeqCst);
    hub.cut_after(Some(offset));
    start(&a, true).await?;
    hub.wait_dials("b", dials + 1).await;
    let mut running = a.worker_running.subscribe();
    while *written.borrow_and_update() < before + offset {
        tokio::select! {
            changed = written.changed() => changed?,
            changed = running.changed() => {
                changed?;
                if !*running.borrow() {
                    assert!(*written.borrow() < before + offset, "completion raced the requested byte cut");
                    break;
                }
            }
        }
    }
    progress.phase.set("stopping worker");
    let service = a.current().ok_or("service")?;
    // A cut may already have started a drain before the stop request.
    let mut closing = 0;
    {
        let stop = start(&a, false);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                stopped = &mut stop => { stopped?; break; }
                () = until(|| service.live.closing.load(Ordering::SeqCst) > closing) => {
                    closing = service.live.closing.load(Ordering::SeqCst);
                    tokio::time::advance(Duration::from_millis(100)).await;
                }
            }
        }
    }
    progress.phase.set("auditing partial session");
    for scope in a.scopes() {
        a.replica.audit(scope).await?;
    }
    for scope in b.scopes() {
        b.replica.audit(scope).await?;
    }
    hub.cut_after(None);
    assert!(
        rows(&a).await?.contains(&"1".to_string()),
        "lost local row at {offset}"
    );
    assert!(
        rows(&b).await?.contains(&"2".to_string()),
        "lost local row at {offset}"
    );
    progress.phase.set("reconciling after cut");
    if let Some(deadline) = a.retry_deadline() {
        tokio::time::advance(deadline.saturating_duration_since(tokio::time::Instant::now())).await;
    }
    let (report, _) = a.sync(&*transport(&a).await?).await?;
    assert_eq!(rows(&a).await?, ["1", "2"], "cut at {offset}: {report}");
    assert_eq!(rows(&b).await?, ["1", "2"], "cut at {offset}");
    progress.phase.set("detaching");
    a.detach(None).await;
    b.detach(None).await;
    drop(a);
    drop(b);
    std::fs::remove_dir_all(trial)?;
    Ok(())
}
