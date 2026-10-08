//! Placement catalog: append-only assignments of domains to file slots and
//! the database bindings whose domains must share one. File ownership
//! remains independent of domain routing. A domain moves only through
//! `crate::relocation`.
use crate::{
    DomainInstallation, DomainPlacement, Error, database::DatabaseOwner, layout,
    scoped_connection::ScopedConnection, sqlite, storage, worker,
};
use rocky_core::{DatabaseBinding, Scope, SqlValue, StoredApplication};
use std::{collections::BTreeMap, path::PathBuf};

type Opened = (usize, (DatabaseOwner, sqlite::Connection));
pub(crate) fn key(scope: Scope) -> Vec<u8> {
    [scope.mesh.0, scope.app.0, scope.domain.0].concat()
}
pub(crate) const BINDINGS: &str = "CREATE TABLE IF NOT EXISTS rocky_bindings(name TEXT PRIMARY KEY,generation INTEGER NOT NULL,identity BLOB NOT NULL,body BLOB NOT NULL) WITHOUT ROWID";
pub(crate) const MIGRATIONS: &str = "CREATE TABLE IF NOT EXISTS rocky_migrations(scope BLOB PRIMARY KEY,source INTEGER NOT NULL,target INTEGER NOT NULL,phase INTEGER NOT NULL) WITHOUT ROWID";

/// The slot holding every domain of `binding`. A binding promises one local
/// transaction over its tables, which exists only inside one file.
pub(crate) fn colocated(
    binding: &DatabaseBinding,
    assignments: &BTreeMap<Vec<u8>, usize>,
) -> Result<usize, Error> {
    let mut slot = None;
    for scope in binding.scopes() {
        let file = *assignments.get(&key(scope)).ok_or_else(|| {
            Error::Incompatible(format!(
                "database binding {} names an unassigned domain",
                binding.name
            ))
        })?;
        match slot {
            None => slot = Some(file),
            Some(first) if first != file => {
                return Err(Error::Incompatible(format!(
                    "database binding {} spans file slots {first} and {file}: transactions and \
                     queries across files are a blocked capability; co-locate its domains with \
                     migrate_placement",
                    binding.name
                )));
            }
            Some(_) => {}
        }
    }
    slot.ok_or_else(|| Error::Incompatible("database binding without tables".into()))
}
/// Checks `bindings` against the recorded ones and returns the rows to store.
/// A changed binding must carry the next generation.
async fn admit_bindings(
    catalog: &ScopedConnection,
    recorded: bool,
    bindings: &[DatabaseBinding],
    assignments: &BTreeMap<Vec<u8>, usize>,
) -> Result<Vec<Vec<sqlite::Value>>, Error> {
    let invalid = |error: rocky_core::BindingError| Error::Incompatible(error.to_string());
    let mut known = BTreeMap::new();
    if recorded {
        for row in storage::rows(
            catalog,
            "SELECT name,generation,identity FROM rocky_bindings",
        )
        .await?
        {
            match row.as_slice() {
                [
                    SqlValue::Text(name),
                    SqlValue::Integer(generation),
                    SqlValue::Blob(identity),
                ] => known.insert(name.clone(), (*generation as u64, identity.clone())),
                _ => return Err(Error::Corrupt("recorded database binding".into())),
            };
        }
    }
    let mut rows = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for binding in bindings {
        let body = binding.canonical().map_err(invalid)?;
        let identity = binding.identity().map_err(invalid)?.to_vec();
        if !names.insert(&binding.name) {
            return Err(Error::Conflict);
        }
        colocated(binding, assignments)?;
        match known.get(&binding.name) {
            Some((_, saved)) if *saved == identity => continue,
            Some((generation, _)) if generation.checked_add(1) != Some(binding.generation) => {
                return Err(Error::Incompatible(format!(
                    "database binding {} changed without the next generation {}",
                    binding.name,
                    generation.saturating_add(1)
                )));
            }
            _ => {}
        }
        rows.push(sqlite::params![
            binding.name.clone(),
            binding.generation as i64,
            identity,
            body
        ]);
    }
    Ok(rows)
}
pub(crate) async fn install<A>(
    path: PathBuf,
    files: usize,
    placement: Vec<DomainPlacement>,
    bindings: Vec<DatabaseBinding>,
    installations: Vec<DomainInstallation<A>>,
) -> Result<(Vec<DomainInstallation<A>>, Vec<Opened>), Error>
where
    A: StoredApplication,
    A::Policy: Send + 'static,
{
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("rocky-placement".into())
        .spawn(move || {
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(Error::Io)
                .and_then(|runtime| {
                    runtime.block_on(prepare(path, files, placement, bindings, &installations))
                });
            let _ = done.send(outcome.map(|files| (installations, files)));
        })
        .map_err(Error::Io)?;
    result.await.map_err(|_| Error::Closed)?
}
async fn prepare<A: StoredApplication>(
    path: PathBuf,
    files: usize,
    placement: Vec<DomainPlacement>,
    bindings: Vec<DatabaseBinding>,
    installations: &[DomainInstallation<A>],
) -> Result<Vec<Opened>, Error> {
    let mut catalog_path = path.as_os_str().to_owned();
    catalog_path.push("-placement.sqlite");
    let (_catalog_owner, raw) = DatabaseOwner::open(&PathBuf::from(catalog_path)).await?;
    let catalog = ScopedConnection::new(raw, None, &[""; 0]);
    let tables = storage::rows(
        &catalog,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .await?;
    let fresh = tables.is_empty();
    if !fresh && !tables.contains(&vec![SqlValue::Text("rocky_placement".into())]) {
        return Err(Error::Incompatible("foreign placement catalog".into()));
    }
    let assignments: BTreeMap<_, _> = placement
        .iter()
        .map(|item| (key(item.scope), item.file))
        .collect();
    let mut recorded = BTreeMap::new();
    let id = if fresh {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Error::Incompatible(error.to_string()))?
            .as_nanos();
        rocky_core::commitment(
            "rocky.placement.catalog.v1",
            &[path.to_string_lossy().as_bytes(), &nonce.to_le_bytes()].concat(),
        )
        .to_vec()
    } else {
        let metadata = storage::rows(
            &catalog,
            "SELECT identity,files FROM rocky_placement WHERE id=1",
        )
        .await?;
        let id = match metadata.as_slice() {
            [row] => match row.as_slice() {
                [SqlValue::Blob(id), SqlValue::Integer(count)] if *count == files as i64 => {
                    id.clone()
                }
                _ => return Err(Error::Incompatible("placement file count differs".into())),
            },
            _ => return Err(Error::Corrupt("placement metadata".into())),
        };
        for row in storage::rows(
            &catalog,
            "SELECT scope,file,identity,ready FROM rocky_assignments",
        )
        .await?
        {
            match row.as_slice() {
                [
                    SqlValue::Blob(scope),
                    SqlValue::Integer(file),
                    identity,
                    SqlValue::Integer(ready),
                ] => {
                    if assignments.get(scope) != Some(&(*file as usize)) {
                        return Err(Error::Incompatible(
                            "recorded placement cannot be removed or moved".into(),
                        ));
                    }
                    recorded.insert(
                        scope.clone(),
                        (*file as usize, identity.clone(), *ready != 0),
                    );
                }
                _ => return Err(Error::Corrupt("placement assignment".into())),
            }
        }
        id
    };
    let has = |table: &str| tables.contains(&vec![SqlValue::Text(table.into())]);
    if has("rocky_migrations")
        && !storage::rows(&catalog, "SELECT 1 FROM rocky_migrations LIMIT 1")
            .await?
            .is_empty()
    {
        return Err(Error::Incompatible(
            "a placement migration is pending; finish it with migrate_placement".into(),
        ));
    }
    let bindings = admit_bindings(&catalog, has("rocky_bindings"), &bindings, &assignments).await?;
    // Validate every selected identity and acquire every required file before writes.
    let mut selected: BTreeMap<usize, Vec<&DomainInstallation<A>>> = BTreeMap::new();
    for domain in installations {
        let scope = key(domain.identity.scope);
        let file = *assignments
            .get(&scope)
            .ok_or_else(|| Error::Incompatible("unassigned scope".into()))?;
        let identity = worker::identity_bytes(domain.identity, &domain.app, &domain.config);
        if let Some((_, saved, _)) = recorded.get(&scope)
            && *saved != SqlValue::Null
            && *saved != SqlValue::Blob(identity)
        {
            return Err(Error::Incompatible("placed domain identity differs".into()));
        }
        selected.entry(file).or_default().push(domain);
    }
    let mut opened = Vec::new();
    for (index, domains) in &selected {
        let file = layout::file_path(&path, *index);
        if domains.iter().any(|domain| {
            recorded
                .get(&key(domain.identity.scope))
                .is_some_and(|(_, _, ready)| *ready)
        }) && !file.exists()
        {
            return Err(Error::Incompatible(
                "assigned runtime file is missing".into(),
            ));
        }
        let (owner, raw) = DatabaseOwner::open(&file).await?;
        let conn = ScopedConnection::new(raw, None, &[""; 0]);
        let mut binding = id.clone();
        binding.extend((*index as u32).to_le_bytes());
        let tables = storage::rows(
            &conn,
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )
        .await?;
        if !tables.is_empty() {
            let marker = if tables.contains(&vec![SqlValue::Text("rocky_bootstrap".into())]) {
                "rocky_bootstrap"
            } else if tables.contains(&vec![SqlValue::Text("rocky_runtime".into())]) {
                "rocky_runtime"
            } else {
                return Err(Error::Incompatible(
                    "file belongs to another runtime or layout".into(),
                ));
            };
            if storage::rows(&conn, &format!("SELECT binding FROM {marker} WHERE id=1")).await?
                != vec![vec![SqlValue::Blob(binding.clone())]]
            {
                return Err(Error::Incompatible(
                    "file belongs to another placement catalog".into(),
                ));
            }
        }
        for domain in domains {
            let mapping = conn.rescoped(
                Some(&crate::runtime::namespace(domain.identity.scope)),
                &domain.app.installed_schema().statements,
            );
            let physical = mapping
                .names()
                .physical("rocky_replica")
                .ok_or_else(|| Error::Corrupt("identity table mapping".into()))?;
            let exists = tables.contains(&vec![SqlValue::Text(physical.to_string())]);
            if recorded
                .get(&key(domain.identity.scope))
                .is_some_and(|(_, _, ready)| *ready)
            {
                if !exists {
                    return Err(Error::Corrupt("assigned domain is missing".into()));
                }
                if storage::rows(&mapping, "SELECT identity FROM rocky_replica WHERE id=1").await?
                    != vec![vec![SqlValue::Blob(worker::identity_bytes(
                        domain.identity,
                        &domain.app,
                        &domain.config,
                    ))]]
                {
                    return Err(Error::Incompatible("placed domain identity differs".into()));
                }
            }
        }
        opened.push((*index, owner, conn, binding));
    }
    storage::rows(&catalog, "PRAGMA journal_mode=wal").await?;
    catalog.execute("PRAGMA synchronous=FULL", ()).await?;
    catalog.execute("PRAGMA fullfsync=ON", ()).await?;
    catalog.execute("BEGIN IMMEDIATE", ()).await?;
    let result = async {
        catalog.execute("CREATE TABLE IF NOT EXISTS rocky_placement(id INTEGER PRIMARY KEY CHECK(id=1),identity BLOB NOT NULL,files INTEGER NOT NULL)", ()).await?;
        catalog.execute("CREATE TABLE IF NOT EXISTS rocky_assignments(scope BLOB PRIMARY KEY,file INTEGER NOT NULL,identity BLOB,ready INTEGER NOT NULL)", ()).await?;
        catalog.execute("INSERT OR IGNORE INTO rocky_placement VALUES(1,?,?)", sqlite::params![id, files as i64]).await?;
        catalog.execute(BINDINGS, ()).await?;
        catalog.execute(MIGRATIONS, ()).await?;
        for item in &placement { catalog.execute("INSERT OR IGNORE INTO rocky_assignments VALUES(?,?,NULL,0)", sqlite::params![key(item.scope), item.file as i64]).await?; }
        for binding in bindings {
            catalog.execute("INSERT OR REPLACE INTO rocky_bindings VALUES(?,?,?,?)", binding).await?;
        }
        for domain in installations {
            catalog.execute("UPDATE rocky_assignments SET identity=? WHERE scope=?", sqlite::params![worker::identity_bytes(domain.identity, &domain.app, &domain.config), key(domain.identity.scope)]).await?;
        }
        Ok(())
    }.await;
    storage::finish(&catalog, result).await?;
    let mut jobs = Vec::new();
    for (index, owner, conn, binding) in opened {
        // Pending domains retain the same ordered plan across interrupted opens.
        let pending: Vec<_> = selected[&index]
            .iter()
            .copied()
            .filter(|domain| {
                !recorded
                    .get(&key(domain.identity.scope))
                    .is_some_and(|(_, _, ready)| *ready)
            })
            .collect();
        let mut statements = Vec::new();
        if !pending.is_empty() {
            let tables =
                storage::rows(&conn, "SELECT name FROM sqlite_schema WHERE type='table'").await?;
            let interrupted = tables.contains(&vec![SqlValue::Text("rocky_bootstrap".into())]);
            let mut unfinished = Vec::new();
            for domain in &pending {
                let mapping = conn.rescoped(
                    Some(&crate::runtime::namespace(domain.identity.scope)),
                    &domain.app.installed_schema().statements,
                );
                let physical = mapping
                    .names()
                    .physical("rocky_replica")
                    .ok_or_else(|| Error::Corrupt("identity table mapping".into()))?;
                if !interrupted && tables.contains(&vec![SqlValue::Text(physical.to_string())]) {
                    if storage::rows(&mapping, "SELECT identity FROM rocky_replica WHERE id=1")
                        .await?
                        != vec![vec![SqlValue::Blob(worker::identity_bytes(
                            domain.identity,
                            &domain.app,
                            &domain.config,
                        ))]]
                    {
                        return Err(Error::Incompatible("placed domain identity differs".into()));
                    }
                } else {
                    unfinished.push(*domain);
                }
            }
            for domain in unfinished {
                let mapping = conn.rescoped(
                    Some(&crate::runtime::namespace(domain.identity.scope)),
                    &domain.app.installed_schema().statements,
                );
                statements.extend(crate::bootstrap::domain_statements(
                    &mapping,
                    domain.identity,
                    &domain.app,
                    &domain.config,
                ));
            }
        }
        let pending = pending
            .into_iter()
            .map(|domain| key(domain.identity.scope))
            .collect::<Vec<_>>();
        jobs.push((index, owner, conn, binding, statements, pending));
    }
    let mut result = Vec::new();
    let mut jobs = jobs.into_iter();
    loop {
        // Independent files install concurrently, with bounded disk pressure.
        let batch: Vec<_> = jobs.by_ref().take(4).collect();
        if batch.is_empty() {
            break;
        }
        let outcomes = std::thread::scope(|threads| {
            let handles: Vec<_> = batch
                .into_iter()
                .map(|(index, owner, conn, binding, statements, pending)| {
                    threads.spawn(move || {
                        if !statements.is_empty() {
                            let runtime = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .map_err(Error::Io)?;
                            runtime
                                .block_on(crate::bootstrap::install(&conn, &binding, statements))?;
                        }
                        Ok::<_, Error>((index, owner, conn, pending))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap_or(Err(Error::Closed)))
                .collect::<Vec<_>>()
        });
        for outcome in outcomes {
            let (index, owner, conn, pending) = outcome?;
            if !pending.is_empty() {
                #[cfg(test)]
                if ABORT_BEFORE_READY.load(std::sync::atomic::Ordering::Relaxed) {
                    std::process::abort();
                }
                catalog.execute("BEGIN IMMEDIATE", ()).await?;
                let marked = async {
                    for scope in pending {
                        catalog
                            .execute(
                                "UPDATE rocky_assignments SET ready=1 WHERE scope=?",
                                [scope],
                            )
                            .await?;
                    }
                    Ok(())
                }
                .await;
                storage::finish(&catalog, marked).await?;
            }
            result.push((index, (owner, conn.into_connection()?)));
        }
    }
    Ok(result)
}

#[cfg(test)]
pub(crate) static ABORT_BEFORE_READY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
