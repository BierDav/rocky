//! features_13 phase 9 convergence scenarios on the todo app, over an
//! in-process link: devices at one seal hold the same rows and the same
//! drop map, offline edits continue in the successor tip, buffered
//! commits survive a process kill, and the open tip may grow while no
//! owner is online. Printed timings are diagnostic only.
mod common;

use common::{World, sync, titles};
use rocky_sdk::{DropReason, Failure, TableEvents, TransactionOutcome, TransactionToken, params};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use todo_app::{Role, TODOS, Todo, Todos, configuration, position, roles};

/// What replicas at one seal must agree on: every todo, and how each
/// transaction of `tokens` stands.
async fn state(
    device: &Todos,
    tokens: &[TransactionToken],
) -> Result<(Vec<Todo>, Vec<Option<TransactionOutcome>>), Failure> {
    let mut outcomes = Vec::new();
    for token in tokens {
        outcomes.push(device.database().transaction_outcome(*token).await?);
    }
    Ok((device.all_todos().await?, outcomes))
}

/// Asserts that every device holds the trusted device's state, and
/// returns it.
async fn converged(
    devices: &[&Todos],
    tokens: &[TransactionToken],
) -> Result<(Vec<Todo>, Vec<Option<TransactionOutcome>>), Failure> {
    let expected = state(devices[0], tokens).await?;
    for device in &devices[1..] {
        assert_eq!(state(device, tokens).await?, expected);
    }
    Ok(expected)
}

/// The trusted device seals, then everyone syncs; returns the new tip.
async fn seal(world: &World) -> Result<String, Failure> {
    let tip = world.trusted().database().session().seal(false).await?.tip;
    world.sync().await?;
    for device in &world.devices {
        assert_eq!(device.database().session().tip_status().await?.tip, tip);
    }
    Ok(tip)
}

/// Waits until `events` names `token` as dropped.
async fn notified(events: &TableEvents, token: TransactionToken) -> Result<(), Failure> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if events.next().await?.dropped.contains(&token) {
                return Ok::<_, Failure>(());
            }
        }
    })
    .await
    .map_err(|_| "no drop notification")?
}

fn dropped(outcome: &Option<TransactionOutcome>) -> Option<(DropReason, bool)> {
    match outcome {
        Some(TransactionOutcome::Dropped { reason, sealed }) => Some((*reason, *sealed)),
        _ => None,
    }
}

fn token(token: Option<TransactionToken>) -> Result<TransactionToken, Failure> {
    Ok(token.ok_or("a replayed write publishes a transaction")?)
}

/// Slice 3: three devices edit one list at once: two moves into one gap,
/// three raises of one priority and two adds of one title.
#[tokio::test(flavor = "multi_thread")]
async fn three_devices_editing_one_list_converge_at_the_next_seal() -> Result<(), Failure> {
    let mut world = World::new().await?;
    let phone = world.join("phone", Role::Full).await?;
    let tablet = world.join("tablet", Role::Full).await?;
    world.sync().await?;
    let laptop = world.trusted().clone();
    let list = laptop.create_list("Errands").await?.id;
    let mut ids = Vec::new();
    for title in ["Post", "Bank", "Bakery", "Florist"] {
        ids.push(laptop.add(list, title).await?.id);
    }
    world.sync().await?;
    for device in [&phone, &tablet] {
        assert_eq!(titles(device, list).await?.len(), 4);
    }
    let laptop_events = laptop.database().observe_tables(None).await?;
    let phone_events = phone.database().observe_tables(None).await?;

    // Each device's edits run at the same time as the others'.
    let (on_laptop, on_phone, on_tablet) = tokio::join!(
        async {
            let moved = token(laptop.move_to(list, ids[2], 1).await?)?;
            let raised = token(laptop.raise_priority(list, ids[0]).await?)?;
            let added = token(laptop.add(list, "Pharmacy").await?.token)?;
            Ok::<_, Failure>([moved, raised, added])
        },
        async {
            let moved = token(phone.move_to(list, ids[3], 1).await?)?;
            let raised = token(phone.raise_priority(list, ids[0]).await?)?;
            let added = token(phone.add(list, "Pharmacy").await?.token)?;
            Ok::<_, Failure>([moved, raised, added])
        },
        async {
            let raised = token(tablet.raise_priority(list, ids[0]).await?)?;
            let renamed = token(tablet.rename(list, ids[1], "Bank and cash").await?)?;
            Ok::<_, Failure>([raised, renamed])
        },
    );
    let (on_laptop, on_phone, on_tablet) = (on_laptop?, on_phone?, on_tablet?);
    let tokens: Vec<_> = on_laptop
        .iter()
        .chain(&on_phone)
        .chain(&on_tablet)
        .copied()
        .collect();
    let devices = [&laptop, &phone, &tablet];
    world.sync().await?;
    seal(&world).await?;
    let (todos, outcomes) = converged(&devices, &tokens).await?;

    // Both moves into the gap after "Post" keep their place, in tag order.
    let order: Vec<_> = todos.iter().map(|todo| todo.title.as_str()).collect();
    assert_eq!(order[0], "Post");
    let mut gap = order[1..3].to_vec();
    gap.sort_unstable();
    assert_eq!(gap, ["Bakery", "Florist"]);
    assert_eq!(&order[3..], ["Bank and cash", "Pharmacy"]);
    assert_eq!(todos[0].priority, 3);
    // Only one of the two "Pharmacy" adds stands; the seal made it final.
    let drops: Vec<_> = outcomes.iter().map(dropped).collect();
    assert_eq!(
        drops.iter().flatten().collect::<Vec<_>>(),
        [&(DropReason::Constraint, true)]
    );
    let lost = if drops[2].is_some() {
        on_laptop[2]
    } else {
        on_phone[2]
    };
    assert!(drops[2].is_some() != drops[5].is_some());
    // The device whose add was dropped is told so.
    let author = if lost == on_laptop[2] {
        &laptop_events
    } else {
        &phone_events
    };
    notified(author, lost).await?;

    // Compacting at the seal hands bases to the members and to a device
    // joining later; every device still holds the same rows.
    laptop.database().session().compact(0).await?;
    world.sync().await?;
    let watch = world.join("watch", Role::Full).await?;
    world.sync().await?;
    laptop.share_lists_with_members().await?;
    world.sync().await?;
    let rows = laptop.all_todos().await?;
    for device in [&phone, &tablet, &watch] {
        assert_eq!(device.all_todos().await?, rows);
    }
    world.close().await
}

/// Slice 4: a device edits offline across a routine seal of another,
/// then reconnects; its late transactions replay in the successor tip.
#[tokio::test(flavor = "multi_thread")]
async fn offline_edits_across_a_routine_seal_replay_in_the_successor_tip() -> Result<(), Failure> {
    let mut world = World::new().await?;
    let phone = world.join("phone", Role::Full).await?;
    world.sync().await?;
    let laptop = world.trusted().clone();
    let list = laptop.create_list("Errands").await?.id;
    let post = laptop.add(list, "Post").await?.id;
    world.sync().await?;
    let opened = laptop.database().session().tip_status().await?.tip;

    phone
        .database()
        .session()
        .set_networking_enabled(false)
        .await?;
    let late = [
        token(phone.add(list, "Milk").await?.token)?,
        token(phone.raise_priority(list, post).await?)?,
        token(phone.add(list, "Bread").await?.token)?,
    ];
    // Enough writes for a routine seal while the phone is away.
    let mut tokens = Vec::new();
    for index in 0..rocky_secure_routine() {
        tokens.push(token(
            laptop.add(list, &format!("Item {index}")).await?.token,
        )?);
    }
    tokens.push(token(laptop.add(list, "Bread").await?.token)?);
    let sealed = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            laptop.database().session().sync_once().await?;
            let tip = laptop.database().session().tip_status().await?.tip;
            if tip != opened {
                return Ok::<_, Failure>(tip);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "the trusted device sealed no routine seal")??;
    assert_ne!(sealed, opened);

    phone
        .database()
        .session()
        .set_networking_enabled(true)
        .await?;
    world.sync().await?;
    tokens.extend(late);
    let devices = [&laptop, &phone];
    let (todos, outcomes) = converged(&devices, &tokens).await?;
    // The phone's offline "Milk" and raise stand in the successor tip; its
    // "Bread" came later than the laptop's sealed one, so it alone drops.
    assert!(todos.iter().any(|todo| todo.title == "Milk"));
    assert_eq!(
        todos
            .iter()
            .find(|todo| todo.id == post)
            .map(|t| t.priority),
        Some(1)
    );
    let drops: Vec<_> = tokens
        .iter()
        .zip(outcomes.iter().map(dropped))
        .filter_map(|(token, drop)| drop.map(|drop| (*token, drop)))
        .collect();
    assert_eq!(drops, [(late[2], (DropReason::Constraint, false))]);
    seal(&world).await?;
    let (_, outcomes) = converged(&devices, &late).await?;
    assert_eq!(dropped(&outcomes[2]), Some((DropReason::Constraint, true)));
    eprintln!("slice 4: drops {drops:?}");
    world.close().await
}

/// Slice 6: with the only owner offline, two members grow the open tip to
/// four routine seals' worth of interleaved transactions; once the owner
/// returns and seals, all three converge.
#[tokio::test(flavor = "multi_thread")]
async fn the_open_tip_grows_while_no_owner_is_online_and_converges() -> Result<(), Failure> {
    let mut world = World::new().await?;
    let phone = world.join("phone", Role::Full).await?;
    let tablet = world.join("tablet", Role::Full).await?;
    world.sync().await?;
    let laptop = world.trusted().clone();
    let list = laptop.create_list("Errands").await?.id;
    let shared = laptop.add(list, "Shared").await?.id;
    world.sync().await?;
    laptop
        .database()
        .session()
        .set_networking_enabled(false)
        .await?;

    let target = 4 * rocky_secure_routine();
    let rounds = target / 32;
    let mut tokens = Vec::new();
    let mut syncs = Vec::new();
    for round in 0..rounds {
        // Each round, both members write 16 transactions apart, then sync:
        // half of each side's arrivals order before the other's writes.
        for (name, device) in [("phone", &phone), ("tablet", &tablet)] {
            for index in 0..16 {
                tokens.push(if index % 4 == 0 {
                    token(device.raise_priority(list, shared).await?)?
                } else {
                    token(
                        device
                            .add(list, &format!("{name} {round}.{index}"))
                            .await?
                            .token,
                    )?
                });
            }
        }
        let started = Instant::now();
        sync(&[&phone, &tablet]).await?;
        syncs.push(started.elapsed());
    }
    assert!(tokens.len() >= target);
    syncs.sort_unstable();
    eprintln!(
        "slice 6: {} transactions, {} member sync rounds, median {:?}, max {:?}",
        tokens.len(),
        syncs.len(),
        syncs[syncs.len() / 2],
        syncs[syncs.len() - 1]
    );
    let (members_rows, members_outcomes) = converged(&[&phone, &tablet], &tokens).await?;
    assert!(
        members_outcomes
            .iter()
            .all(|outcome| dropped(outcome).is_none())
    );

    laptop
        .database()
        .session()
        .set_networking_enabled(true)
        .await?;
    let started = Instant::now();
    world.sync().await?;
    let catch_up = started.elapsed();
    seal(&world).await?;
    let (todos, _) = converged(&[&laptop, &phone, &tablet], &tokens).await?;
    assert_eq!(todos, members_rows);
    assert_eq!(
        todos
            .iter()
            .find(|todo| todo.id == shared)
            .map(|t| t.priority),
        Some(i64::try_from(tokens.len() / 4)?)
    );
    eprintln!("slice 6: owner catch-up {catch_up:?}");
    world.close().await
}

/// Unsealed replayed transactions at which an owner seals routinely.
fn rocky_secure_routine() -> usize {
    rocky_secure::ROUTINE_SEAL_TRANSACTIONS as usize
}

const CHILD: &str = "TODO_QUALIFICATION_CHILD";

/// Slice 5, child side: writes into the installation `CHILD` names and is
/// killed by the parent.
#[tokio::test(flavor = "multi_thread")]
async fn qualification_child() -> Result<(), Failure> {
    let Ok(spec) = std::env::var(CHILD) else {
        return Ok(());
    };
    let (mode, directory) = spec.split_once(':').ok_or("mode:directory")?;
    let todos = Todos::new(configuration(directory)?.open().await?.database);
    let list = todos.lists().await?[0].id;
    let mut out = std::io::stdout();
    match mode {
        "one" => {
            let id = todos.add(list, "Buffered").await?.id;
            writeln!(out, "{REPORT}{id}")?;
            out.flush()?;
            // Ended at once, as a killed app is: nothing closes or flushes.
            std::process::abort();
        }
        _ => {
            for pair in 0.. {
                let mut transaction = todos.database().transaction().await?;
                for side in ["a", "b"] {
                    let id = transaction.next_id(TODOS, params![list]).await?;
                    let position = position::between(None, None, id)?;
                    transaction
                        .execute(
                            "INSERT INTO todos(id,list,title,completed,position,priority) VALUES(?,?,?,0,?,0)",
                            params![id, list, format!("{side} {pair}"), position],
                        )
                        .await?;
                }
                transaction.commit().await?;
                writeln!(out, "{REPORT}{pair}")?;
                out.flush()?;
            }
            Ok(())
        }
    }
}

fn child(mode: &str, directory: &Path) -> Result<std::process::Child, Failure> {
    Ok(Command::new(std::env::current_exe()?)
        .args(["--exact", "qualification_child", "--nocapture", "--quiet"])
        .env(CHILD, format!("{mode}:{}", directory.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?)
}

/// The child's reports, skipping the test harness's own output.
fn reports(child: &mut std::process::Child) -> Result<impl Iterator<Item = String>, Failure> {
    let stdout = child.stdout.take().ok_or("the child's stdout")?;
    Ok(BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| line.strip_prefix(REPORT).map(str::to_owned)))
}
const REPORT: &str = "qualification: ";

/// Slice 5: a write whose buffered commit returned survives a kill; one
/// killed before `commit()` returns is absent or complete.
#[tokio::test(flavor = "multi_thread")]
async fn a_killed_app_reopens_offline_with_whole_transactions() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("phone");
    let config = configuration(&directory)?;
    let todos = roles::create(&config, "Phone").await?;
    todos.create_list("Errands").await?;
    todos.database().close().await?;

    let mut killed = child("one", &directory)?;
    let id: i64 = reports(&mut killed)?
        .next()
        .ok_or("the child committed nothing")?
        .parse()?;
    killed.wait()?;
    let todos = Todos::new(config.open().await?.database);
    let list = todos.lists().await?[0].id;
    assert!(todos.todos(list).await?.iter().any(|todo| todo.id == id));
    todos.database().close().await?;

    // Killed while committing: every pair is whole or absent, and each
    // pair the child reported committed is present.
    let mut writer = child("pairs", &directory)?;
    let committed = reports(&mut writer)?.take(40).count();
    assert_eq!(committed, 40, "the child stopped");
    writer.kill()?;
    writer.wait()?;
    let todos = Todos::new(config.open().await?.database);
    let mut sides = std::collections::BTreeMap::<String, usize>::new();
    for todo in todos.todos(list).await? {
        if let Some((side, pair)) = todo.title.split_once(' ')
            && matches!(side, "a" | "b")
        {
            *sides.entry(pair.to_owned()).or_default() += 1;
        }
    }
    assert!(sides.values().all(|count| *count == 2), "{sides:?}");
    assert!(sides.len() >= committed);
    todos.database().close().await
}
