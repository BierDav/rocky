//! Root recovery of a domain from a backup archive: the root opens the
//! escrow records the archived store holds and installs a fresh trusted
//! node holding those keys. It resumes after a crash at any step: the
//! marker (`backup::Marker::recovery`) records how far it got, and only
//! `Configuration::recover` with the same archive finishes it.
use crate::{
    Failure, SecretStore,
    backup::{MARKER, Marker, Recovery},
    enrollment::Settings,
};
use rocky_secure::EncryptedReplica;
use std::{path::Path, sync::Arc};

/// The recovery unfinished in `directory`, if any.
pub(crate) fn unfinished(directory: &Path) -> Result<Option<Marker>, Failure> {
    let marker = directory.join(MARKER);
    if !rocky_platform::fs::exists(&marker) {
        return Ok(None);
    }
    let saved: Marker = serde_json::from_slice(&rocky_platform::fs::read(&marker)?)?;
    Ok(saved.recovery.is_some().then_some(saved))
}
/// An open of a directory whose recovery is unfinished is refused: the
/// store holds no keys yet, and only the root revokes the lost nodes.
pub(crate) fn refuse_unfinished(directory: &Path) -> Result<(), Failure> {
    if unfinished(directory)?.is_some() {
        return Err(
            "a recovery is unfinished in this directory: recover again with \
                    its backup and the root"
                .into(),
        );
    }
    Ok(())
}

pub(crate) fn mark(directory: &Path, marker: &Marker) -> Result<(), Failure> {
    crate::secrets::write_private(&directory.join(MARKER), &serde_json::to_vec(marker)?)?;
    Ok(())
}

/// Verifies the archive, opens the escrow records its store holds with the
/// root, saves a fresh trusted identity holding those keys and puts the
/// archive's files in place, behind the marker that rebinds them at open.
/// Nothing is written until every key opened. A recovery of the same
/// archive left unfinished continues under the identity it saved. Returns
/// it and the archived identity record, whose revocations say which
/// trusted identities the recovery revokes.
pub(crate) async fn recover(
    path: &Path,
    directory: &Path,
    secrets: &Arc<dyn SecretStore>,
    archive: &Path,
    root: &crate::RootKey,
    provider: Option<Arc<dyn crate::IdentityProvider>>,
) -> Result<(Settings, Settings), Failure> {
    let staging = directory.join("restore");
    let opened = crate::backup::open(archive, &staging)?;
    if root.public_key() != opened.inventory.publisher {
        return Err("the root key is not the backup's domain root".into());
    }
    let saved = match unfinished(directory)? {
        // A marker saved without its identity is a recovery not yet begun.
        Some(saved) => rocky_platform::fs::exists(path).then_some(saved),
        None if rocky_platform::fs::exists(directory.join(MARKER)) => {
            return Err("a restore is unfinished in this directory".into());
        }
        None => None,
    };
    let (settings, stage) = match saved {
        Some(saved) => {
            if saved.archive != opened.id || saved.predecessor != opened.inventory.node {
                return Err("another recovery is unfinished in this directory".into());
            }
            let settings = Settings::load(path)?;
            (settings, saved.recovery)
        }
        None => {
            let store = opened
                .files
                .iter()
                .find(|(name, _)| name == "rocky.sqlite")
                .map(|(_, staged)| staged)
                .ok_or("the backup holds no replica")?;
            let escrowed =
                EncryptedReplica::<rocky_core::TypedRows>::archived_escrow(store).await?;
            let recovered = crate::escrow::Recovered::open(&opened.identity, escrowed, root)?;
            let settings = Settings::recover(
                secrets,
                (&opened.identity, opened.inventory.node),
                &recovered,
                provider,
                root,
            )?;
            // The marker first: an identity without it would open as if
            // installed.
            mark(
                directory,
                &Marker {
                    predecessor: opened.inventory.node,
                    archive: opened.id.clone(),
                    recovery: Some(Recovery::Placing),
                },
            )?;
            fault::check(directory, fault::Stage::Marked)?;
            settings.save(path)?;
            (settings, Some(Recovery::Placing))
        }
    };
    fault::check(directory, fault::Stage::Saved)?;
    // Until placed, the files were never opened: they are put in place again.
    if stage == Some(Recovery::Placing) {
        crate::backup::place(&opened, directory)?;
        fault::check(directory, fault::Stage::Moved)?;
        mark(
            directory,
            &Marker {
                predecessor: opened.inventory.node,
                archive: opened.id,
                recovery: Some(Recovery::Placed),
            },
        )?;
    }
    rocky_platform::fs::remove_dir_all(&staging)?;
    fault::check(directory, fault::Stage::Placed)?;
    Ok((settings, opened.identity))
}

/// Deterministic interruptions of a recovery or a restore, for their crash
/// sweeps. A restore stops at `Enrolled` and at the stages from `Rebound`
/// to `Attached`, which both open; any installation at `Started`.
pub(crate) mod fault {
    use crate::Failure;
    use std::path::Path;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stage {
        /// A restore enrolled and saved its identity; its files are not
        /// placed.
        Enrolled,
        /// The marker is saved, the identity not.
        Marked,
        /// The identity is saved, the files not placed.
        Saved,
        /// The files are placed, the marker not advanced.
        Moved,
        /// The marker says placed; the store was never opened.
        Placed,
        /// The store is rebound to the recovered node, without its tables.
        Rebound,
        /// The store holds its tables, not its keys.
        Attached,
        /// The store took one of its keys.
        Keyed,
        /// The marker says keyed; the root revoked no lost identity yet.
        Revoking,
        /// The root revoked the lost identities the store names, not yet
        /// those granted before the recovery that it does not.
        Named,
        /// The node started; the database is not handed out yet.
        Started,
    }
    /// The directories and stages at which a recovery stops once.
    /// Process-wide, since each database runs on a thread of its own;
    /// keyed by directory, so concurrent tests do not meet each other's.
    #[cfg(test)]
    pub static STOPS: std::sync::Mutex<Vec<(std::path::PathBuf, Stage)>> =
        std::sync::Mutex::new(Vec::new());
    #[cfg(test)]
    pub fn check(directory: &Path, stage: Stage) -> Result<(), Failure> {
        let mut stops = STOPS.lock().map_err(|_| "poisoned fault")?;
        match stops
            .iter()
            .position(|(at, stop)| at == directory && *stop == stage)
        {
            Some(index) => {
                stops.remove(index);
                Err(format!("injected stop at {stage:?}").into())
            }
            None => Ok(()),
        }
    }
    #[cfg(not(test))]
    pub fn check(_: &Path, _: Stage) -> Result<(), Failure> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
