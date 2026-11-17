use crate::{
    Failure, SecretStore,
    backup::{MARKER, Marker, Recovery},
    enrollment::Settings,
};
use rocky_secure::EncryptedReplica;
use std::{path::Path, sync::Arc};

pub(crate) fn unfinished(directory: &Path) -> Result<Option<Marker>, Failure> {
    let marker = directory.join(MARKER);
    if !rocky_platform::fs::exists(&marker) {
        return Ok(None);
    }
    let saved: Marker = serde_json::from_slice(&rocky_platform::fs::read(&marker)?)?;
    Ok(saved.recovery.is_some().then_some(saved))
}
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

pub(crate) mod fault {
    use crate::Failure;
    use std::path::Path;
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Stage {
        Enrolled,
        Marked,
        Saved,
        Moved,
        Placed,
        Rebound,
        Attached,
        Keyed,
        Revoking,
        Named,
        SettingsSigned,
        Started,
    }
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
