use rocky_sdk::{Failure, Network, transport::memory::Hub};
use std::{path::Path, sync::Arc};
use todo_app::{Role, Todos, configuration, roles};

pub struct World {
    pub root: tempfile::TempDir,
    pub hub: Arc<Hub>,
    pub devices: Vec<Todos>,
}
impl World {
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
    pub async fn join(&mut self, name: &str, role: Role) -> Result<Todos, Failure> {
        let code = self.trusted().invite(role).await?;
        let joined = roles::join(
            &Self::config(self.root.path(), &self.hub, name)?,
            &code,
            name,
        )
        .await?;
        for other in &self.devices {
            joined
                .database()
                .add_peer(&other.database().contact().await?.uri)
                .await?;
            other
                .database()
                .add_peer(&joined.database().contact().await?.uri)
                .await?;
        }
        self.devices.push(joined.clone());
        Ok(joined)
    }
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

pub async fn sync(devices: &[&Todos]) -> Result<(), Failure> {
    for _ in 0..2 {
        for device in devices {
            device.database().sync_now().await?;
        }
    }
    Ok(())
}

pub async fn titles(device: &Todos, list: rocky_sdk::RowIdentity) -> Result<Vec<String>, Failure> {
    Ok(device
        .todos(list)
        .await?
        .into_iter()
        .map(|todo| todo.title)
        .collect())
}
