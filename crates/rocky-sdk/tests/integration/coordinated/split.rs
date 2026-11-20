use super::*;
use reach::Reach;
use rocky_sdk::{AlertKind, CoordinatorSlot};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

async fn owner(
    directory: &Path,
    network: Option<Network>,
    log: &MemoryLog,
) -> Result<(Device, Arc<AtomicBool>), Failure> {
    let owner = Device::new(Database::create(open(directory, network), None)?)?
        .with_identity(common::external_identity(directory));
    owner.follow_main_migrating().await;
    configure(&owner, log)?;
    let down = compose(&owner, log).await?;
    Ok((owner, down))
}
fn configure(owner: &Device, log: &MemoryLog) -> Result<(), Failure> {
    log.configure(domain(owner), 1)?;
    Ok(())
}
async fn trusted(
    owner: &Device,
    directory: &Path,
    network: Option<Network>,
    coordination: Option<CoordinatorSlot>,
) -> Result<Device, Failure> {
    let device = Device::new(Database::join(
        Open {
            coordination,
            ..open(directory, network)
        },
        &owner.invite_as(json!({"kind":"issuer"})).await,
    )?)?
    .with_identity(common::external_identity(directory));
    introduce(owner, &device).await;
    let principal = snap::ok(device.session.info()).await["principal"].clone();
    let grant = snap::ok(
        owner
            .session
            .trust(snap::public_key(&principal), owner.root()),
    )
    .await;
    snap::ok(device.session.install_trust(&snap::text(&grant["grant"]))).await;
    device.sync().await.expect("trusted sync");
    Ok(device)
}
async fn compose(device: &Device, log: &MemoryLog) -> Result<Arc<AtomicBool>, Failure> {
    let (client, down) = Reach::new(Arc::new(log.client(node_key(device).await)));
    device.database.coordinate(Some(client)).await?;
    Ok(down)
}
async fn tip(device: &Device) -> String {
    device.session.tip_status().await.expect("tip status").tip
}
async fn seal_skipped(device: &Device) -> Vec<rocky_sdk::SealSkip> {
    device
        .session
        .status()
        .await
        .expect("status")
        .alerts
        .into_iter()
        .filter_map(|alert| match alert.kind {
            AlertKind::SealSkipped { reason } => Some(reason),
            _ => None,
        })
        .collect()
}
fn code(error: &Failure) -> String {
    rocky_sdk::CallFailure::from_failure(error)
        .code()
        .to_owned()
}
async fn named(device: &Device) -> Option<String> {
    device
        .session
        .inspect_tips()
        .await
        .expect("inspection")
        .coordinator
        .map(|coordinator| coordinator.genesis)
}
fn genesis_of(log: &MemoryLog) -> String {
    data_encoding::HEXLOWER.encode(&log.bundle().identity.genesis)
}
async fn import(
    device: &Device,
    handles: std::ops::RangeInclusive<u8>,
    label: &str,
) -> Vec<String> {
    let mut titles = Vec::new();
    for handle in handles {
        let title = format!("{label} {handle:03}");
        device.add(handle, &title).await;
        titles.push(title);
    }
    titles
}
fn missing(device: &[String], expected: &[String]) -> usize {
    expected
        .iter()
        .filter(|title| !device.contains(title))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_without_its_coordinated_log_skips_routine_seals_and_its_rows_keep_flowing()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let log = MemoryLog::new();
    let (a, a_down) = owner(&root.path().join("a"), network("a"), &log).await?;
    snap::ok(a.session.declare_coordinated(a.root())).await;
    let importer = trusted(&a, &root.path().join("i"), network("i"), None).await?;
    assert_eq!(named(&importer).await, Some(genesis_of(&log)));
    let genesis = tip(&a).await;

    a_down.store(true, Ordering::SeqCst);
    let imported = import(&importer, 0..=127, "imported").await;
    importer.sync().await.expect("importer sync");
    assert_eq!(
        tip(&importer).await,
        genesis,
        "the importer leaves the coordinated line unsealed rather than splitting it"
    );
    let skipped = seal_skipped(&importer).await;
    assert!(
        matches!(
            skipped.as_slice(),
            [rocky_sdk::SealSkip::CoordinatorRequired(_)]
        ),
        "the skipped routine seal is an alert naming the missing log: {skipped:?}"
    );
    let refused = importer.session.seal(false).await.unwrap_err();
    assert_eq!(code(&refused), "coordinator_required");
    a.sync().await.expect("owner sync");
    importer.sync().await.expect("importer sync");
    assert_eq!(
        missing(&a.titles().await, &imported),
        0,
        "both follow genesis, so every imported row reaches the coordinated peer"
    );

    compose(&importer, &log).await?;
    importer.sync().await.expect("importer sync");
    assert_ne!(
        tip(&importer).await,
        genesis,
        "composed, the importer seals"
    );
    assert!(seal_skipped(&importer).await.is_empty());
    importer.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_line_split_by_a_device_without_its_log_converges_once_it_composes_one()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let log = MemoryLog::new();
    let (a, a_down) = owner(&root.path().join("a"), network("a"), &log).await?;
    let browser = trusted(&a, &root.path().join("b"), network("b"), None).await?;
    let browser_down = compose(&browser, &log).await?;
    let importer = trusted(&a, &root.path().join("i"), network("i"), None).await?;
    let genesis = tip(&a).await;
    let mut expected = import(&browser, 0..=2, "browser").await;
    browser.sync().await.expect("browser sync");

    for down in [&a_down, &browser_down] {
        down.store(true, Ordering::SeqCst);
    }
    expected.extend(import(&browser, 10..=12, "browser late").await);
    let waiting = browser.session.seal(false).await.unwrap_err();
    assert_eq!(
        code(&waiting),
        "coordinator_unavailable",
        "the browser's own genesis seal waits in its journal"
    );
    let mut imported = import(&importer, 0..=127, "import").await;
    importer.sync().await.expect("first routine seal");
    imported.extend(import(&importer, 128..=255, "import").await);
    importer.sync().await.expect("second routine seal");
    a.sync().await.expect("owner sync");
    let sealed = importer.session.inspect_tips().await?.seals;
    assert_eq!(sealed.len(), 2, "the importer sealed twice without a log");
    assert!(sealed.iter().all(|seal| !seal.finalized));
    assert_eq!(tip(&a).await, genesis, "the coordinated peer defers them");
    assert_eq!(
        missing(&a.titles().await, &imported),
        imported.len(),
        "the split: no imported row reaches the coordinated peer"
    );

    snap::ok(a.session.declare_coordinated(a.root())).await;
    importer
        .sync()
        .await
        .expect("importer learns the declaration");
    let refused = importer.session.seal(false).await.unwrap_err();
    assert_eq!(code(&refused), "coordinator_required");
    let other = MemoryLog::new();
    let mismatched = compose(&importer, &other).await.unwrap_err();
    assert_eq!(
        code(&mismatched),
        "coordinator_mismatch",
        "a coordinator the settings do not name is refused, never chosen"
    );

    compose(&importer, &log).await?;
    importer
        .sync()
        .await
        .expect("importer ends its seals' logs");
    let sealed = importer.session.inspect_tips().await?.seals;
    assert!(
        sealed.iter().all(|seal| seal.finalized && seal.canonical),
        "the importer ended its seals' logs and acknowledged them: {sealed:?}"
    );

    for _ in 0..3 {
        a.sync().await.expect("owner sync");
        importer.sync().await.expect("importer sync");
        browser.sync().await.expect("browser sync");
    }
    let converged = tip(&importer).await;
    assert_ne!(converged, genesis);
    expected.extend(imported);
    for device in [&a, &browser, &importer] {
        assert_eq!(
            tip(device).await,
            converged,
            "every device follows the importer's final seals"
        );
        assert_eq!(
            missing(&device.titles().await, &expected),
            0,
            "every row of every device is kept"
        );
    }
    for down in [&a_down, &browser_down] {
        down.store(false, Ordering::SeqCst);
    }
    expected.extend(import(&browser, 20..=21, "browser after").await);
    for _ in 0..2 {
        browser.sync().await.expect("browser sync");
        a.sync().await.expect("owner sync");
        importer.sync().await.expect("importer sync");
    }
    for device in [&a, &browser, &importer] {
        assert_eq!(
            tip(device).await,
            converged,
            "the coordinator's return changes nothing"
        );
        assert_eq!(missing(&device.titles().await, &expected), 0);
    }
    browser.close().await;
    importer.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_reaches_the_coordinator_its_database_settings_name() -> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let log = MemoryLog::new();
    let a_dir = root.path().join("a");
    let placeholder = rocky_sdk::credentials::PrivateKey::new().public_key();
    let a = Device::new(Database::create(
        Open {
            coordination: Some(CoordinatorSlot::Client(Arc::new(log.client(placeholder)))),
            ..open(&a_dir, network("a"))
        },
        None,
    )?)?
    .with_identity(common::external_identity(&a_dir));
    assert_eq!(
        named(&a).await,
        Some(genesis_of(&log)),
        "creating with a coordinator names it in the database's settings"
    );
    a.follow_main_migrating().await;
    configure(&a, &log)?;
    let genesis = tip(&a).await;

    let importer = trusted(
        &a,
        &root.path().join("i"),
        network("i"),
        Some(CoordinatorSlot::Connector(Arc::new(log.clone()))),
    )
    .await?;
    let imported = import(&importer, 0..=127, "imported").await;
    importer.sync().await.expect("importer sync");
    let sealed = tip(&importer).await;
    assert_ne!(
        sealed, genesis,
        "the importer seals through the named coordinator"
    );
    assert!(seal_skipped(&importer).await.is_empty());
    let terminal = log
        .head(rocky_core::TipLog {
            scope: domain(&a),
            tip: rocky_core::TipId(
                data_encoding::HEXLOWER
                    .decode(genesis.as_bytes())?
                    .try_into()
                    .map_err(|_| "tip")?,
            ),
        })?
        .terminal;
    assert!(terminal.is_some(), "its seal ended the genesis log first");
    for _ in 0..2 {
        a.sync().await.expect("owner sync");
        importer.sync().await.expect("importer sync");
    }
    assert_eq!(tip(&a).await, sealed);
    assert_eq!(missing(&a.titles().await, &imported), 0);

    let other = MemoryLog::new();
    let stranger = trusted(
        &a,
        &root.path().join("s"),
        network("s"),
        Some(CoordinatorSlot::Connector(Arc::new(other))),
    )
    .await?;
    stranger.sync().await.expect("stranger sync");
    let refused = stranger.session.seal(false).await.unwrap_err();
    assert_eq!(
        code(&refused),
        "coordinator_required",
        "a connector that cannot verify the named coordinator composes nothing"
    );
    stranger.close().await;
    importer.close().await;
    a.close().await;
    Ok(())
}

fn tip_id(hex: &str) -> Result<rocky_core::TipId, Failure> {
    Ok(rocky_core::TipId(
        data_encoding::HEXLOWER
            .decode(hex.as_bytes())?
            .try_into()
            .map_err(|_| "tip")?,
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_line_split_converges_with_every_row_when_the_waiting_seal_wins_genesis()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let log = MemoryLog::new();
    let (a, a_down) = owner(&root.path().join("a"), network("a"), &log).await?;
    let browser = trusted(&a, &root.path().join("b"), network("b"), None).await?;
    let browser_down = compose(&browser, &log).await?;
    let importer = trusted(&a, &root.path().join("i"), network("i"), None).await?;
    hub.set_down("b", "i", true);
    let genesis = tip(&a).await;
    let mut expected = import(&browser, 0..=2, "browser").await;
    let contested = [
        browser.add(3, "contested edit").await,
        browser.add(4, "contested delete").await,
        browser.add(5, "deleted on the winning line").await,
    ];
    browser.sync().await.expect("browser sync");
    a.sync().await.expect("owner sync");
    importer.sync().await.expect("importer sync");
    assert_eq!(
        importer.items().await,
        browser.items().await,
        "both lines start from the same rows"
    );
    for down in [&a_down, &browser_down] {
        down.store(true, Ordering::SeqCst);
    }
    expected.extend(import(&browser, 10..=12, "browser late").await);
    let waiting = browser.session.seal(false).await.unwrap_err();
    assert_eq!(code(&waiting), "coordinator_unavailable");
    let mut imported = import(&importer, 0..=39, "on genesis").await;
    snap::ok(importer.session.seal(false)).await;
    importer
        .set(
            200,
            &contested[0],
            "title",
            json!("contested edit by the importer"),
        )
        .await;
    importer.remove(201, &contested[1]).await;
    importer
        .set(202, &contested[2], "title", json!("kept by the importer"))
        .await;
    imported.extend(import(&importer, 40..=79, "after the first seal").await);
    snap::ok(importer.session.seal(false)).await;
    imported.extend(import(&importer, 80..=99, "after the second seal").await);
    importer.sync().await.expect("importer sync");
    let sealed = importer.session.inspect_tips().await?.seals;
    assert_eq!(sealed.len(), 2, "the importer sealed twice without a log");
    a.sync().await.expect("owner sync");

    snap::ok(a.session.declare_coordinated(a.root())).await;
    browser_down.store(false, Ordering::SeqCst);
    browser.sync().await.expect("browser sync");
    let winning = tip(&browser).await;
    let genesis_log = rocky_core::TipLog {
        scope: domain(&a),
        tip: tip_id(&genesis)?,
    };
    assert_eq!(
        log.head(genesis_log)?
            .terminal
            .map(|terminal| data_encoding::HEXLOWER.encode(&terminal.digest)),
        Some(winning),
        "the browser's waiting seal ended genesis first"
    );
    browser
        .set(
            200,
            &contested[0],
            "title",
            json!("contested edit by the browser"),
        )
        .await;
    browser
        .set(201, &contested[1], "completed", json!(true))
        .await;
    browser.remove(202, &contested[2]).await;
    a_down.store(false, Ordering::SeqCst);
    for _ in 0..2 {
        a.sync().await.expect("owner sync");
        browser.sync().await.expect("browser sync");
    }
    compose(&importer, &log).await?;
    for _ in 0..3 {
        importer.sync().await.expect("importer sync");
        a.sync().await.expect("owner sync");
        browser.sync().await.expect("browser sync");
    }
    let second = sealed
        .iter()
        .find(|seal| seal.tip != genesis)
        .ok_or("the importer's second seal")?;
    let losing = log.head(rocky_core::TipLog {
        scope: domain(&a),
        tip: tip_id(&second.tip)?,
    })?;
    assert!(
        losing.terminal.is_none(),
        "the importer never ends the log of a tip on its losing branch"
    );
    let converged = tip(&a).await;
    expected.extend(imported);
    let rows = a.items().await;
    for device in [&a, &browser, &importer] {
        assert_eq!(
            tip(device).await,
            converged,
            "every device follows one line"
        );
        assert_eq!(
            missing(&device.titles().await, &expected),
            0,
            "the importer's rows written after its losing seals walk on as late writes"
        );
        assert_eq!(
            device.items().await,
            rows,
            "every device holds the same rows, contested edits and deletes replayed in one order"
        );
    }
    let titles = a.titles().await;
    let distinct: std::collections::BTreeSet<_> = titles.iter().collect();
    assert_eq!(distinct.len(), titles.len(), "no row is duplicated");
    let written: std::collections::BTreeSet<_> = expected
        .iter()
        .map(String::as_str)
        .chain([
            "contested edit",
            "contested delete",
            "deleted on the winning line",
            "contested edit by the importer",
            "contested edit by the browser",
            "kept by the importer",
        ])
        .collect();
    assert!(
        titles.iter().all(|title| written.contains(title.as_str())),
        "no row appears that no device wrote: {titles:?}"
    );
    assert!(
        (expected.len() + 1..=expected.len() + 3).contains(&titles.len()),
        "every written row once, the contested ones at most once each: {titles:?}"
    );
    browser.close().await;
    importer.close().await;
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_coordinator_the_settings_do_not_name_is_refused_for_counters_and_at_open()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let log = MemoryLog::new();
    let (a, _a_down) = owner(&root.path().join("a"), network("a"), &log).await?;
    let other = MemoryLog::new();
    let placeholder = rocky_sdk::credentials::PrivateKey::new().public_key();
    let explicit = || Some(CoordinatorSlot::Client(Arc::new(other.client(placeholder))));
    let directory = root.path().join("d");
    let device = trusted(&a, &directory, network("d"), explicit()).await?;
    snap::ok(a.session.declare_coordinated(a.root())).await;
    a.sync().await.expect("owner sync");
    device.sync().await.expect("device sync");
    assert_eq!(named(&device).await, Some(genesis_of(&log)));
    let counter = rocky_sdk::counters::Name::new("invoices")?;
    let claimed = device
        .database
        .claim(counter.clone(), rocky_sdk::counters::Claim::fresh()?)
        .await
        .unwrap_err();
    assert_eq!(
        code(&claimed),
        "coordinator_mismatch",
        "another service's counters are never the database's"
    );
    let read = device.database.counter(counter, None).await.unwrap_err();
    assert_eq!(code(&read), "coordinator_mismatch");
    let sealed = device.session.seal(false).await.unwrap_err();
    assert_eq!(code(&sealed), "coordinator_mismatch");
    device.close().await;
    let reopened = Database::open(Open {
        coordination: explicit(),
        ..open(&directory, network("d"))
    })
    .err()
    .ok_or("a mismatched coordinator was accepted at open")?;
    assert_eq!(code(&reopened), "coordinator_mismatch");
    let joined = trusted(&a, &root.path().join("e"), network("e"), explicit())
        .await
        .err()
        .ok_or("a device joined with a coordinator the settings do not name")?;
    assert_eq!(code(&joined), "coordinator_mismatch");
    a.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_skipped_members_seal_raises_the_alert_and_leaves_the_revocation_complete()
-> Result<(), Failure> {
    let root = crate::support::small_directory()?;
    let hub = Arc::new(Hub::default());
    let network = |name: &str| Some(Network::Owned(hub.connector(name)));
    let log = MemoryLog::new();
    let (a, _a_down) = owner(&root.path().join("a"), network("a"), &log).await?;
    snap::ok(a.session.declare_coordinated(a.root())).await;
    let t = trusted(&a, &root.path().join("t"), network("t"), None).await?;
    let invitation = a
        .session
        .invitation_with(rocky_sdk::GrantProfile::Reader {
            scopes: ["items".into()].into(),
        })
        .await?
        .uri;
    let reader = Device::new(Database::join(
        open(&root.path().join("x"), network("x")),
        &invitation,
    )?)?;
    introduce(&t, &reader).await;
    import(&t, 0..=2, "before the rotation").await;
    for device in [&t, &a, &reader] {
        device.sync().await.expect("sync");
    }
    let revoked = a
        .session
        .revoke_identity(reader.identity().await.parse()?)
        .await?;
    assert_eq!(revoked.revocations.statements, 1);
    t.sync().await.expect("trusted sync");
    assert_eq!(
        t.session.info().await?.revocations.statements,
        1,
        "the revocation is complete on the trusted device"
    );
    assert!(
        matches!(
            seal_skipped(&t).await.as_slice(),
            [rocky_sdk::SealSkip::CoordinatorRequired(_)]
        ),
        "the members seal it owes is skipped with the alert: {:?}",
        seal_skipped(&t).await
    );
    compose(&t, &log).await?;
    t.sync().await.expect("trusted sync");
    assert!(
        seal_skipped(&t).await.is_empty(),
        "composed, nothing it owes members is skipped"
    );
    reader.close().await;
    t.close().await;
    a.close().await;
    Ok(())
}
