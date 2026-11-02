//! Devices of one todo domain over an in-process link.
#![allow(dead_code)]
use rocky_sdk::{Failure, Network, transport::memory::Hub};
use std::{path::Path, sync::Arc};
use todo_app::{Role, Todos, configuration, roles};

pub struct World {
    pub root: tempfile::TempDir,
    pub hub: Arc<Hub>,
    pub devices: Vec<Todos>,
}
impl World {
    /// A domain whose first device, the trusted "laptop", created it.
    pub async fn new() -> Result<Self, Failure> {
        let root = tempfile::tempdir()?;
        let hub = Arc::new(Hub::default());
        let laptop = roles::create(&Self::config(root.path(), &hub, "laptop")?, "Laptop").await?;
        Ok(Self {
            root,
            hub,
            devices: vec![laptop],
        })
    }
    pub fn config(
        root: &Path,
        hub: &Arc<Hub>,
        name: &str,
    ) -> Result<rocky_sdk::Configuration, Failure> {
        Ok(configuration(root.join(name))?.network(Network::Owned(hub.connector(name))))
    }
    pub fn trusted(&self) -> &Todos {
        &self.devices[0]
    }
    /// Joins a device invited by the first and meshes it with every other.
    pub async fn join(&mut self, name: &str, role: Role) -> Result<Todos, Failure> {
        let code = self.devices[0].invite(role).await?;
        let joined = roles::join(
            &Self::config(self.root.path(), &self.hub, name)?,
            &code,
            name,
        )
        .await?;
        for other in &self.devices {
            joined
                .database()
                .session()
                .add_peer(&other.database().session().contact().await?)
                .await?;
            other
                .database()
                .session()
                .add_peer(&joined.database().session().contact().await?)
                .await?;
        }
        self.devices.push(joined.clone());
        Ok(joined)
    }
    /// Two sync rounds over every device.
    pub async fn sync(&self) -> Result<(), Failure> {
        sync(&self.devices.iter().collect::<Vec<_>>()).await
    }
    pub async fn close(self) -> Result<(), Failure> {
        for device in &self.devices {
            device.database().close().await?;
        }
        Ok(())
    }
}

/// Two sync rounds over `devices` only.
pub async fn sync(devices: &[&Todos]) -> Result<(), Failure> {
    for _ in 0..2 {
        for device in devices {
            device.database().session().sync_once().await?;
        }
    }
    Ok(())
}

pub async fn titles(device: &Todos, list: i64) -> Result<Vec<String>, Failure> {
    Ok(device
        .todos(list)
        .await?
        .into_iter()
        .map(|todo| todo.title)
        .collect())
}
