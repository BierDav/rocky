use rocky_sdk::{Affinity, Column, Database, Error, RowIdentity, Schema, Table, params};

pub fn schema() -> Result<Schema, Error> {
    Schema::builder("notes", 1)
        .table(
            Table::replayed("notes")
                .column(Column::new("body", Affinity::Text).required())
                .column(
                    Column::new("done", Affinity::Integer)
                        .required()
                        .default(false),
                ),
        )
        .build()
}

pub async fn add(database: &Database, body: &str) -> Result<RowIdentity, Error> {
    database
        .write(async |transaction| {
            let id = transaction.next_id("notes", vec![]).await?;
            transaction
                .execute(
                    "INSERT INTO notes(id,body,done) VALUES(?,?,?)",
                    params![id, body, false],
                )
                .await?;
            Ok(id)
        })
        .await
}

pub struct Note {
    pub id: RowIdentity,
    pub body: String,
    pub done: bool,
}

pub async fn notes(database: &Database) -> Result<Vec<Note>, Error> {
    let mut rows = database
        .query("SELECT id,body,done FROM notes ORDER BY id", params![])
        .await?;
    let mut notes = Vec::new();
    while let Some(row) = rows.next().await? {
        notes.push(Note {
            id: row.get("id")?,
            body: row.get("body")?,
            done: row.get("done")?,
        });
    }
    Ok(notes)
}
