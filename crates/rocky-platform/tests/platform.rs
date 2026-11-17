use rocky_platform::{fs, task, time};
use std::time::Duration;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn timers_wait_and_time_out() {
    let start = time::Instant::now();
    time::sleep(Duration::from_millis(20)).await;
    assert!(start.elapsed() >= Duration::from_millis(20));

    let slow = time::timeout(
        Duration::from_millis(5),
        time::sleep(Duration::from_secs(60)),
    );
    assert!(slow.await.is_err());
    let fast = time::timeout(Duration::from_secs(60), async { 7 });
    assert_eq!(fast.await.unwrap(), 7);

    let mut ticks = time::interval(Duration::from_millis(5));
    ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let first = ticks.tick().await;
    let second = ticks.tick().await;
    assert!(second >= first + Duration::from_millis(5));
}

async fn local<F: std::future::Future>(future: F) -> F::Output {
    #[cfg(not(target_family = "wasm"))]
    return tokio::task::LocalSet::new().run_until(future).await;
    #[cfg(target_family = "wasm")]
    return future.await;
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn tasks_join_abort_and_yield() {
    local(async {
        let quick = task::spawn(async { 1 });
        while !quick.is_finished() {
            task::yield_now().await;
        }
        assert_eq!(quick.await.unwrap(), 1);
        assert_eq!(task::spawn_blocking(|| 2).await.unwrap(), 2);
        let stuck = task::spawn_local(time::sleep(Duration::from_secs(60)));
        stuck.abort();
        assert!(stuck.await.unwrap_err().is_cancelled());

        let mut tasks = task::JoinSet::new();
        for value in 0..3 {
            tasks.spawn_local(async move {
                task::yield_now().await;
                value
            });
        }
        let mut values = Vec::new();
        while let Some(value) = tasks.join_next().await {
            values.push(value.unwrap());
        }
        values.sort_unstable();
        assert_eq!(values, [0, 1, 2]);

        let dedicated = task::dedicated("test", || async { 3 }).unwrap();
        assert_eq!(dedicated.join().await.unwrap(), 3);
    })
    .await;
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn files_replace_rename_and_remove() {
    let scratch = fs::Scratch::new().await;
    let directory = scratch.path().join("install");
    let secret = directory.join("secrets/key");
    fs::prepare(&directory.join("rocky.sqlite")).await.unwrap();
    fs::create_private_dir_all(directory.join("secrets")).unwrap();

    assert!(!fs::exists(&secret));
    assert_eq!(
        fs::read(&secret).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    fs::write_private(&secret, b"one").unwrap();
    fs::write_private(&secret, b"two").unwrap();
    assert_eq!(fs::read(&secret).unwrap(), b"two");
    assert_eq!(fs::len(&secret).unwrap(), 3);
    assert!(fs::exists(&secret) && fs::exists(directory.join("secrets")));

    let moved = directory.join("identity.json");
    fs::write(directory.join("staged"), "{}").unwrap();
    fs::rename(directory.join("staged"), &moved).unwrap();
    assert_eq!(fs::read_to_string(&moved).unwrap(), "{}");
    assert_eq!(fs::copy(&moved, directory.join("copy")).unwrap(), 2);
    assert!(fs::sync(&moved).unwrap());
    assert!(!fs::sync(directory.join("absent")).unwrap());

    fs::remove_file(&moved).unwrap();
    assert!(!fs::exists(&moved));
    fs::remove_dir_all(&directory).unwrap();
    assert!(!fs::exists(&secret) && !fs::exists(directory.join("copy")));
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn one_owner_per_file() {
    let scratch = fs::Scratch::new().await;
    let path = scratch.path().join("owned.sqlite");
    fs::prepare(&path).await.unwrap();
    let owner = fs::Owner::acquire(&path).await.unwrap();
    assert!(
        fs::Owner::acquire(&path).await.is_err(),
        "a second owner is refused, natively after briefly retrying"
    );
    drop(owner);
    fs::Owner::acquire(&path).await.unwrap();
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn only_the_named_replacement_moves_a_database_where_a_browser_cannot_rename_atomically() {
    let scratch = fs::Scratch::new().await;
    let database = scratch.path().join("live.sqlite");
    let staged = scratch.path().join("staged.sqlite");
    fs::prepare(&database).await.unwrap();
    fs::write_database(&staged, b"staged").unwrap();
    let renamed = fs::rename(&staged, &database);
    if cfg!(target_family = "wasm") {
        assert_eq!(
            renamed.unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput,
            "an atomic rename refuses a database in the browser"
        );
        fs::replace_database_not_atomic_in_browser(&staged, &database).unwrap();
    } else {
        renamed.unwrap();
    }
    assert!(fs::exists(&database) && !fs::exists(&staged));
    assert_eq!(fs::len(&database).unwrap(), 6);
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn files_differing_only_in_extension_have_separate_owners() {
    let scratch = fs::Scratch::new().await;
    let database = scratch.path().join("owned.db");
    let sqlite = scratch.path().join("owned.sqlite");
    fs::prepare(&database).await.unwrap();
    let _database = fs::Owner::acquire(&database).await.unwrap();
    fs::Owner::acquire(&sqlite)
        .await
        .expect("a lock beside each file, not one per stem");
}

#[wasm_bindgen_test(unsupported = tokio::test)]
async fn create_new_refuses_an_existing_file_so_the_first_writer_wins() {
    let scratch = fs::Scratch::new().await;
    let path = scratch.path().join("claim");
    fs::prepare(&path).await.unwrap();
    fs::create_new(&path, b"first").unwrap();
    assert_eq!(
        fs::create_new(&path, b"second").unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&path).unwrap(), b"first");
}
