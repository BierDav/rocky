#![cfg(target_family = "wasm")]

use rocky_sdk::{Affinity, Column, Configuration, Database, ErrorKind, Schema, Table, params};
use wasm_bindgen::{JsError, JsValue, prelude::wasm_bindgen};

const DIRECTORY: &str = "browser-notes";

fn schema() -> Result<Schema, rocky_sdk::Error> {
    Schema::builder("notes", 1)
        .table(Table::replayed("notes").column(Column::new("body", Affinity::Text).required()))
        .build()
}

#[wasm_bindgen]
pub struct Notes(Database);

#[wasm_bindgen]
impl Notes {
    pub async fn open(invitation: Option<String>) -> Result<Notes, JsError> {
        let configuration = Configuration::new(DIRECTORY, &schema()?)?
            .network(rocky_tcp::Options::browser().into());
        match configuration.open().await {
            Ok(database) => Ok(Notes(database)),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NoDatabase | ErrorKind::JoinUnfinished
                ) =>
            {
                let code = invitation.ok_or(error)?.parse()?;
                Ok(Notes(configuration.join(&code).await?.database))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn add(&self, body: String) -> Result<(), JsError> {
        let insert = "INSERT INTO notes(body) VALUES(?)";
        self.0
            .write(async |tx| tx.execute(insert, params![body.as_str()]).await)
            .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<String>, JsError> {
        let rows = self.0.query("SELECT body FROM notes", params![]).await?;
        let rows = rows.collect().await?;
        Ok(rows
            .iter()
            .map(|row| row.get("body"))
            .collect::<Result<_, _>>()?)
    }

    pub async fn watch(&self, changed: js_sys::Function) -> Result<(), JsError> {
        let mut changes = self.0.observe(&["notes"]).await?;
        while changes.next().await.is_ok() {
            changed
                .call0(&JsValue::NULL)
                .map_err(|_| JsError::new("the callback threw"))?;
        }
        Ok(())
    }
}
