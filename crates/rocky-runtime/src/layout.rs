//! The files of one `Runtime`. File 0 is the path given to `Runtime::open`;
//! file `k` adds `-k` to its stem: `laptop.db`, `laptop-1.db`, `laptop-2.db`.
//! Domain `i` in sorted scope order lives in file `i % files`, where `files`
//! is the requested count capped at the number of domains.
//!
//! Every file records the runtime's binding and the layout (requested count,
//! files used, its own index). The other files are installed first; file 0
//! is installed last, so its record marks the whole set as installed. An
//! interrupted install resumes; a missing, extra or foreign file, or another
//! file count, is refused. No transaction spans two files.
use crate::{
    DomainInstallation, Error, database::DatabaseOwner, runtime::MAX_FILES,
    scoped_connection::ScopedConnection, sqlite, storage,
};
use rocky_core::{SqlValue, StoredApplication};
use std::path::{Path, PathBuf};

/// Test hook: kill the process after the other files are installed, before
/// file 0 records the set as installed.
#[cfg(test)]
pub(crate) static ABORT_BEFORE_MARKER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn file_path(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(extension) => format!("{stem}-{index}.{}", extension.to_string_lossy()),
        None => format!("{stem}-{index}"),
    };
    path.with_file_name(name)
}
type Opened = (DatabaseOwner, sqlite::Connection);

/// Installs or verifies every file on a helper thread, then hands each open
/// file and the installations back for the file writers.
pub(crate) async fn install<A>(
    path: PathBuf,
    requested: usize,
    used: usize,
    binding: Vec<u8>,
    installations: Vec<DomainInstallation<A>>,
) -> Result<(Vec<DomainInstallation<A>>, Vec<Opened>), Error>
where
    A: StoredApplication,
    A::Policy: Send + 'static,
{
    let (done, result) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("rocky-install".into())
        .spawn(move || {
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(Error::Io)
                .and_then(|runtime| {
                    runtime.block_on(prepare(&path, requested, used, &binding, &installations))
                });
            let _ = done.send(outcome.map(|files| (installations, files)));
        })
        .map_err(Error::Io)?;
    result.await.map_err(|_| Error::Closed)?
}

enum State {
    Fresh,
    Installing(Vec<u8>),
    Installed(Vec<u8>),
    Foreign,
}
async fn state(conn: &ScopedConnection) -> Result<State, Error> {
    let tables = storage::rows(
        conn,
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
    )
    .await?;
    if tables.is_empty() {
        return Ok(State::Fresh);
    }
    if tables.contains(&vec![SqlValue::Text("rocky_bootstrap".into())]) {
        return match storage::rows(conn, "SELECT binding FROM rocky_bootstrap WHERE id=1")
            .await?
            .as_slice()
        {
            [row] => match row.as_slice() {
                [SqlValue::Blob(binding)] => Ok(State::Installing(binding.clone())),
                _ => Ok(State::Foreign),
            },
            _ => Ok(State::Foreign),
        };
    }
    if !tables.contains(&vec![SqlValue::Text("rocky_runtime".into())]) {
        return Ok(State::Foreign);
    }
    match storage::rows(conn, "SELECT binding FROM rocky_runtime WHERE id=1")
        .await?
        .as_slice()
    {
        [row] => match row.as_slice() {
            [SqlValue::Blob(binding)] => Ok(State::Installed(binding.clone())),
            _ => Ok(State::Foreign),
        },
        _ => Ok(State::Foreign),
    }
}
fn layout(binding: &[u8], requested: usize, used: usize, index: usize) -> Vec<u8> {
    let mut bytes = binding.to_vec();
    bytes.extend(b"rocky.layout.v1");
    for value in [requested, used, index] {
        bytes.extend((value as u32).to_le_bytes());
    }
    bytes
}
fn present(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|file| file.len() > 0)
}

async fn prepare<A: StoredApplication>(
    path: &Path,
    requested: usize,
    used: usize,
    binding: &[u8],
    installations: &[DomainInstallation<A>],
) -> Result<Vec<Opened>, Error> {
    let name = |index: usize| file_path(path, index).display().to_string();
    let (owner, raw) = DatabaseOwner::open(path).await?;
    let first = ScopedConnection::new(raw, None, &[""; 0]);
    let complete = match state(&first).await? {
        State::Fresh => false,
        State::Installing(recorded) if recorded == layout(binding, requested, used, 0) => false,
        State::Installed(recorded) if recorded == layout(binding, requested, used, 0) => true,
        State::Installed(recorded) if recorded.starts_with(binding) => {
            return Err(Error::Incompatible(format!(
                "{} was installed with another file count; open it with the recorded count",
                name(0)
            )));
        }
        State::Installed(_) | State::Installing(_) => {
            return Err(Error::Incompatible(
                "installed domains, manifests or node configuration differ".into(),
            ));
        }
        State::Foreign => {
            return Err(Error::Incompatible(
                "legacy single-domain file; export into a new shared file".into(),
            ));
        }
    };
    for index in used..MAX_FILES {
        if file_path(path, index).exists() {
            return Err(Error::Incompatible(format!(
                "{} is not part of this runtime's {used}-file layout",
                name(index)
            )));
        }
    }
    let mut siblings = Vec::new();
    for index in 1..used {
        let sibling = file_path(path, index);
        if complete && !present(&sibling) {
            return Err(Error::Incompatible(format!(
                "{} of this runtime is missing",
                name(index)
            )));
        }
        let (owner, raw) = DatabaseOwner::open(&sibling).await?;
        let conn = ScopedConnection::new(raw, None, &[""; 0]);
        let expected = layout(binding, requested, used, index);
        match state(&conn).await? {
            State::Fresh if !complete => {
                bootstrap(&conn, &expected, assigned(installations, used, index)).await?;
            }
            State::Installing(recorded) if !complete && recorded == expected => {
                bootstrap(&conn, &expected, assigned(installations, used, index)).await?;
            }
            State::Installed(recorded) if recorded == expected => {}
            State::Fresh => {
                return Err(Error::Incompatible(format!(
                    "{} of this runtime is empty",
                    name(index)
                )));
            }
            State::Installed(_) | State::Installing(_) | State::Foreign => {
                return Err(Error::Incompatible(format!(
                    "{} belongs to another runtime or layout",
                    name(index)
                )));
            }
        }
        siblings.push((owner, conn.into_connection()?));
    }
    if !complete {
        #[cfg(test)]
        if ABORT_BEFORE_MARKER.load(std::sync::atomic::Ordering::Relaxed) {
            std::process::abort();
        }
        // File 0 last: its record marks the set as installed.
        bootstrap(
            &first,
            &layout(binding, requested, used, 0),
            assigned(installations, used, 0),
        )
        .await?;
    }
    let raw = first.into_connection()?;
    Ok(std::iter::once((owner, raw)).chain(siblings).collect())
}
fn assigned<A: StoredApplication>(
    installations: &[DomainInstallation<A>],
    used: usize,
    index: usize,
) -> impl Iterator<Item = &DomainInstallation<A>> {
    installations
        .iter()
        .enumerate()
        .filter(move |(position, _)| position % used == index)
        .map(|(_, domain)| domain)
}
/// Every bounded step and its cursor commit together. Admission starts only
/// after the final binding record replaces the bootstrap marker.
pub(crate) async fn bootstrap<'a, A: StoredApplication + 'a>(
    root: &ScopedConnection,
    binding: &[u8],
    domains: impl Iterator<Item = &'a DomainInstallation<A>>,
) -> Result<(), Error> {
    let mut statements = Vec::new();
    for domain in domains {
        let namespace = crate::runtime::namespace(domain.identity.scope);
        let mapping = root.rescoped(Some(&namespace), &domain.app.installed_schema().statements);
        statements.extend(crate::bootstrap::domain_statements(
            &mapping,
            domain.identity,
            &domain.app,
            &domain.config,
        ));
    }
    crate::bootstrap::install(root, binding, statements).await
}
