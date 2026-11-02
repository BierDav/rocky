//! Shared todo lists on Rocky's replayed tables.
//!
//! Every write is one transaction whose statements replicate as intent.
//! A todo's place in its list is a [fractional index](position), so moving
//! it rewrites only that todo, and concurrent moves both keep their place.
//! A list holds each title once: of two devices adding or renaming todos
//! to the same title concurrently, replay keeps the earlier transaction in
//! canonical order and drops the other on every replica.
//!
//! Each list is its own partition. Devices take one of three [roles]:
//! trusted devices enroll others and seal, full devices receive every
//! list, partial devices only the lists shared with them.
pub mod position;
pub mod roles;

use rocky_sdk::{
    Affinity, Column, Configuration, Database, DeleteAction, DropReason, Failure,
    ForeignKeyDefinition, IndexColumn, IndexDefinition, Row, Schema, SqlValue, Table,
    TableDefinition, Transaction, TransactionOutcome, TransactionToken, params,
};
pub use roles::{Member, Role};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

pub const APPLICATION: &str = "todo";
pub const LISTS: &str = "lists";
pub const TODOS: &str = "todos";
pub const DEVICES: &str = "devices";
/// The partition group holding one list and its todos.
pub const LIST_GROUP: &str = "list";
pub const MAX_TITLE_BYTES: usize = 1024;

/// The replicated schema: lists, todos ordered within their list, and the
/// devices with their roles.
pub fn schema() -> Result<Schema, Failure> {
    let title_check = format!("length(trim(title)) > 0 AND length(title) <= {MAX_TITLE_BYTES}");
    let lists = grouped(
        Table::replayed(LISTS)
            .column(Column::new("title", Affinity::Text).required())
            .check(title_check.clone())
            .partition_by(["id"]),
    );
    let mut todos = grouped(
        Table::replayed(TODOS)
            .column(Column::new("list", Affinity::Integer).required())
            .column(Column::new("title", Affinity::Text).required())
            .column(
                Column::new("completed", Affinity::Integer)
                    .required()
                    .default(false),
            )
            .column(Column::new("position", Affinity::Text).required())
            .column(
                Column::new("priority", Affinity::Integer)
                    .required()
                    .default(0),
            )
            .check(title_check)
            .check("completed IN (0, 1)")
            .index(unique("todos_list_title", ["list", "title"]))
            .index(unique("todos_list_position", ["list", "position"]))
            .primary_key(["list", "id"])
            .partition_by(["list"]),
    );
    todos.foreign_keys.push(ForeignKeyDefinition {
        columns: vec!["list".into()],
        table: LISTS.into(),
        on_delete: DeleteAction::Cascade,
        deferred: false,
    });
    let devices = Table::replayed(DEVICES)
        .column(Column::new("node", Affinity::Text).required())
        .column(Column::new("role", Affinity::Text).required())
        .column(Column::new("name", Affinity::Text).required())
        .check("role IN ('trusted', 'full', 'partial')")
        .index(unique("devices_node", ["node"]));
    Schema::builder(APPLICATION, 1)
        .table(lists)
        .table(todos)
        .table(devices)
        .build()
}
fn grouped(table: Table) -> TableDefinition {
    let mut table = table.definition();
    table.partition_group = Some(LIST_GROUP.into());
    table
}
fn unique<const N: usize>(name: &str, columns: [&str; N]) -> IndexDefinition {
    IndexDefinition {
        name: name.into(),
        columns: columns
            .into_iter()
            .map(|name| IndexColumn {
                name: name.into(),
                descending: false,
            })
            .collect(),
        unique: true,
        expressions: Vec::new(),
        predicate: None,
    }
}

/// An installation of the todo schema in `directory`.
pub fn configuration(directory: impl Into<PathBuf>) -> Result<Configuration, Failure> {
    Configuration::new(directory, &schema()?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    pub id: i64,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    pub id: i64,
    pub list: i64,
    pub title: String,
    pub completed: bool,
    pub priority: i64,
    pub position: String,
}
impl Todo {
    fn read(row: &Row) -> Result<Self, Failure> {
        Ok(Self {
            id: row.get_at(0)?,
            list: row.get_at(1)?,
            title: row.get_at(2)?,
            completed: row.get_at(3)?,
            priority: row.get_at(4)?,
            position: row.get_at(5)?,
        })
    }
}
const TODO_COLUMNS: &str = "id,list,title,completed,priority,position";

/// A committed insert: the new row's id and the transaction replay may
/// still drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Created {
    pub id: i64,
    pub token: Option<TransactionToken>,
}

#[derive(Debug)]
pub enum TodoError {
    InvalidTitle,
    /// A reply of the call contract lacked a field.
    Reply(&'static str),
}
impl std::fmt::Display for TodoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTitle => {
                write!(f, "a title needs text and at most {MAX_TITLE_BYTES} bytes")
            }
            Self::Reply(field) => write!(f, "Rocky's reply lacks {field}"),
        }
    }
}
impl std::error::Error for TodoError {}

fn title(title: &str) -> Result<&str, TodoError> {
    let title = title.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES {
        return Err(TodoError::InvalidTitle);
    }
    Ok(title)
}

/// The todo operations over one database. Each write commits one
/// transaction and returns once it is locally durable.
#[derive(Clone)]
pub struct Todos {
    database: Arc<Database>,
}
impl Todos {
    pub fn new(database: Arc<Database>) -> Self {
        Self { database }
    }
    pub fn database(&self) -> &Arc<Database> {
        &self.database
    }
    /// One request of Rocky's call contract; partitions have no typed API.
    pub(crate) async fn call(&self, request: Value) -> Result<Value, Failure> {
        let scope: Value = serde_json::from_str(&self.database.scope())?;
        let reply: Value = serde_json::from_str(
            &self
                .database
                .session()
                .call(json!({"version": 1, "scope": scope, "request": request}).to_string())
                .await?,
        )?;
        if reply["status"] == "ok" {
            Ok(reply["value"].clone())
        } else {
            Err(reply["error"]["message"]
                .as_str()
                .unwrap_or("Rocky refused the request")
                .to_owned()
                .into())
        }
    }

    /// Creates a list in a new partition and shares it with the full and
    /// trusted devices.
    pub async fn create_list(&self, name: &str) -> Result<Created, Failure> {
        let name = title(name)?;
        self.call(json!({"type": "partition_next", "table": LISTS}))
            .await?;
        let mut transaction = self.database.transaction().await?;
        let id = transaction.next_id(LISTS, params![]).await?;
        transaction
            .execute("INSERT INTO lists(id,title) VALUES(?,?)", params![id, name])
            .await?;
        let token = transaction.commit().await?;
        self.share_with_members(id).await?;
        Ok(Created { id, token })
    }
    pub async fn rename_list(
        &self,
        list: i64,
        name: &str,
    ) -> Result<Option<TransactionToken>, Failure> {
        let name = title(name)?;
        self.write("UPDATE lists SET title=? WHERE id=?", params![name, list])
            .await
    }
    /// Deletes the list and, by cascade, its todos.
    pub async fn delete_list(&self, list: i64) -> Result<Option<TransactionToken>, Failure> {
        self.write("DELETE FROM lists WHERE id=?", params![list])
            .await
    }
    /// The lists this device holds.
    pub async fn lists(&self) -> Result<Vec<List>, Failure> {
        let rows = self
            .database
            .query("SELECT id,title FROM lists ORDER BY title,id", params![])
            .await?
            .collect()
            .await?;
        rows.iter()
            .map(|row| {
                Ok(List {
                    id: row.get_at(0)?,
                    title: row.get_at(1)?,
                })
            })
            .collect()
    }

    /// Appends a todo to the end of `list`.
    pub async fn add(&self, list: i64, name: &str) -> Result<Created, Failure> {
        let name = title(name)?;
        let mut transaction = self.database.transaction().await?;
        let id = transaction.next_id(TODOS, params![list]).await?;
        let last = positions(&mut transaction, list, None).await?.pop();
        let position = position::between(last.as_deref(), None, id)?;
        transaction
            .execute(
                "INSERT INTO todos(id,list,title,completed,position,priority) VALUES(?,?,?,0,?,0)",
                params![id, list, name, position],
            )
            .await?;
        let token = transaction.commit().await?;
        Ok(Created { id, token })
    }
    pub async fn rename(
        &self,
        list: i64,
        todo: i64,
        name: &str,
    ) -> Result<Option<TransactionToken>, Failure> {
        let name = title(name)?;
        self.write(
            "UPDATE todos SET title=? WHERE list=? AND id=?",
            params![name, list, todo],
        )
        .await
    }
    pub async fn set_completed(
        &self,
        list: i64,
        todo: i64,
        completed: bool,
    ) -> Result<Option<TransactionToken>, Failure> {
        self.write(
            "UPDATE todos SET completed=? WHERE list=? AND id=?",
            params![completed, list, todo],
        )
        .await
    }
    /// Raises the priority by one; concurrent raises all count.
    pub async fn raise_priority(
        &self,
        list: i64,
        todo: i64,
    ) -> Result<Option<TransactionToken>, Failure> {
        self.write(
            "UPDATE todos SET priority=priority+? WHERE list=? AND id=?",
            params![1_i64, list, todo],
        )
        .await
    }
    /// Moves `todo` to `index` among the other todos of its list; an index
    /// past the end appends.
    pub async fn move_to(
        &self,
        list: i64,
        todo: i64,
        index: usize,
    ) -> Result<Option<TransactionToken>, Failure> {
        let mut transaction = self.database.transaction().await?;
        let others = positions(&mut transaction, list, Some(todo)).await?;
        let index = index.min(others.len());
        let before = index.checked_sub(1).map(|index| others[index].as_str());
        let position = position::between(before, others.get(index).map(String::as_str), todo)?;
        transaction
            .execute(
                "UPDATE todos SET position=? WHERE list=? AND id=?",
                params![position, list, todo],
            )
            .await?;
        Ok(transaction.commit().await?)
    }
    pub async fn delete(&self, list: i64, todo: i64) -> Result<Option<TransactionToken>, Failure> {
        self.write(
            "DELETE FROM todos WHERE list=? AND id=?",
            params![list, todo],
        )
        .await
    }
    /// `list`'s todos in order.
    pub async fn todos(&self, list: i64) -> Result<Vec<Todo>, Failure> {
        self.select(
            &format!("SELECT {TODO_COLUMNS} FROM todos WHERE list=? ORDER BY position"),
            params![list],
        )
        .await
    }
    /// Every todo held here, ordered by list then position: what replicas
    /// holding the same lists at one seal agree on.
    pub async fn all_todos(&self) -> Result<Vec<Todo>, Failure> {
        self.select(
            &format!("SELECT {TODO_COLUMNS} FROM todos ORDER BY list,position"),
            params![],
        )
        .await
    }
    /// Why replay dropped `token` here, and whether a seal made that final;
    /// `None` while it stands.
    pub async fn dropped(
        &self,
        token: TransactionToken,
    ) -> Result<Option<(DropReason, bool)>, Failure> {
        Ok(match self.database.transaction_outcome(token).await? {
            Some(TransactionOutcome::Dropped { reason, sealed }) => Some((reason, sealed)),
            _ => None,
        })
    }

    async fn write(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Option<TransactionToken>, Failure> {
        let mut transaction = self.database.transaction().await?;
        transaction.execute(sql, params).await?;
        Ok(transaction.commit().await?)
    }
    async fn select(&self, sql: &str, params: Vec<SqlValue>) -> Result<Vec<Todo>, Failure> {
        let rows = self.database.query(sql, params).await?.collect().await?;
        rows.iter().map(Todo::read).collect()
    }
}

/// `list`'s positions in order, without `except`.
async fn positions(
    transaction: &mut Transaction,
    list: i64,
    except: Option<i64>,
) -> Result<Vec<String>, Failure> {
    let rows = transaction
        .query(
            "SELECT position FROM todos WHERE list=? AND id<>? ORDER BY position",
            // Ids are positive, so 0 excludes nothing.
            params![list, except.unwrap_or(0)],
        )
        .await?
        .collect()
        .await?;
    rows.iter().map(|row| Ok(row.get_at(0)?)).collect()
}
