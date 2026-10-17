use super::{fixture::*, inspection::*, *};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_voters_finalize_an_owner_seal_after_a_trusted_write() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let (a, t) = household(root.path(), &bundle).await?;
    assert_eq!(
        t.write("main", 11, true).await?["committed"]["position"],
        "1"
    );
    // Committed-entry requests carry metadata; range sync supplies their frames.
    a.sync().await?;
    assert_eq!(a.keys().await?, t.keys().await?);
    let tip = a.tips("status", json!({})).await?["tip"].clone();
    let before = head(&bundle, &root.path().join("t"), &t.scope, &tip).await?;
    let owner = a.call(json!({"type":"session"})).await?["node"].clone();
    assert!(
        !before
            .members
            .iter()
            .any(|member| member.key.to_hex() == owner)
    );
    let sealed = a.tips("seal", json!({})).await?["tip"].clone();
    final_on_both(&a, &t, &sealed).await?;
    assert_eq!(a.keys().await?, t.keys().await?);
    t.close().await?;
    a.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_voters_keep_branch_positions_separate_from_main() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let (a, t) = household(root.path(), &bundle).await?;
    assert_eq!(
        t.write("before", 21, true).await?["committed"]["position"],
        "1"
    );
    a.sync().await?;
    let latest = a.tips("seal", json!({})).await?["tip"].clone();
    final_on_both(&a, &t, &latest).await?;
    assert_eq!(
        t.write("main only", 22, true).await?["committed"]["position"],
        "1"
    );
    let main = t.call(json!({"type":"coordinated"})).await?;
    let main_rows = t.keys().await?;
    let main_head = head(&bundle, &root.path().join("t"), &t.scope, &main["tip"]).await?;
    let dev = a
        .tips("open", json!({"seal":latest,"minimum_in_sync":1}))
        .await?["tip"]
        .clone();
    a.tips("create", json!({"name":"dev","tip":dev})).await?;
    t.sync().await?;
    t.tips("pull", json!({"seal":latest})).await?;
    t.sync().await?;
    t.tips("follow", json!({"name":"dev"})).await?;
    assert_eq!(
        t.write("dev only", 23, true).await?["committed"]["position"],
        "1"
    );
    let branch = t.call(json!({"type":"coordinated"})).await?;
    assert_eq!(branch["tip"], dev);
    assert_eq!(branch["position"], "1");
    assert_ne!(branch["tip"], main["tip"]);
    t.tips("follow", json!({"name":"main"})).await?;
    assert_eq!(t.call(json!({"type":"coordinated"})).await?, main);
    assert_eq!(t.keys().await?, main_rows);
    let after = head(&bundle, &root.path().join("t"), &t.scope, &main["tip"]).await?;
    assert_eq!(
        (after.head, after.commit, after.running_hash, after.terminal),
        (
            main_head.head,
            main_head.commit,
            main_head.running_hash,
            main_head.terminal
        )
    );
    t.close().await?;
    a.close().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_coordinator_commits_while_peer_networking_is_disabled() -> Result<(), Failure> {
    let root = tempfile::tempdir()?;
    let (bundle, _admin) = service(root.path()).await?;
    let (a, t) = household(root.path(), &bundle).await?;
    t.session.set_networking_enabled(false).await?;
    assert_eq!(t.sync().await?["skip"], "networking_disabled");
    assert_eq!(
        t.write("peer offline", 31, true).await?["committed"]["position"],
        "1"
    );
    assert_eq!(
        t.call(json!({"type":"coordinated"})).await?["position"],
        "1"
    );
    assert_eq!(t.sync().await?["skip"], "networking_disabled");
    assert_eq!(a.keys().await?, json!([]));
    t.close().await?;
    a.close().await?;
    Ok(())
}
