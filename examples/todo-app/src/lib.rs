pub mod position;
pub mod roles;

use rocky_sdk::{
    Affinity, Column, Commit, Configuration, Database, DeleteAction, DropReason,
    ForeignKeyDefinition, IndexColumn, IndexDefinition, Row, RowIdentity, Schema, SqlValue, Table,
    Transaction, TransactionOutcome, TransactionToken, params,
};
pub use roles::{Member, Role};
use std::path::PathBuf;

pub type Failure = Box<dyn std::error::Error + Send + Sync>;

pub const APPLICATION: &str = "todo";
pub const LISTS: &str = "lists";
pub const TODOS: &str = "todos";
pub const DEVICES: &str = "devices";
pub const LIST_GROUP: &str = "list";
pub const MAX_TITLE_BYTES: usize = 1024;

pub fn schema() -> Result<Schema, Failure> {
    let title_check = format!("length(trim(title)) > 0 AND length(title) <= {MAX_TITLE_BYTES}");
    let lists = Table::replayed(LISTS)
        .column(Column::new("title", Affinity::Text).required())
        .check(title_check.clone())
        .partition_by(["id"])
        .partition_group(LIST_GROUP);
    let todos = Table::replayed(TODOS)
        .column(Column::new("list", Affinity::Blob).required())
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
        .partition_by(["list"])
        .partition_group(LIST_GROUP)
        .foreign_key(ForeignKeyDefinition {
            columns: vec!["list".into()],
            table: LISTS.into(),
            on_delete: DeleteAction::Cascade,
            deferred: false,
        });
    let devices = Table::replayed(DEVICES)
        .column(Column::new("node", Affinity::Text).required())
        .column(Column::new("role", Affinity::Text).required())
        .column(Column::new("name", Affinity::Text).required())
        .check("role IN ('admin', 'full', 'partial')")
        .index(unique("devices_node", ["node"]));
    Ok(Schema::builder(APPLICATION, 1)
        .table(lists)
        .table(todos)
        .table(devices)
        .role(
            rocky_sdk::Role::new(roles::PARTIAL)
                .write(LISTS)
                .write(TODOS)
                .write(DEVICES)
                .per_partition(LIST_GROUP),
        )
        .build()?)
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

pub fn configuration(directory: impl Into<PathBuf>) -> Result<Configuration, Failure> {
    Ok(Configuration::new(directory, &schema()?)?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List {
    pub id: RowIdentity,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    pub id: RowIdentity,
    pub list: RowIdentity,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Created {
    pub id: RowIdentity,
    pub token: Commit,
}

#[derive(Debug)]
pub enum TodoError {
    InvalidTitle,
    UnknownRole,
    NotInvitable,
}
impl std::fmt::Display for TodoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTitle => {
                write!(f, "a title needs text and at most {MAX_TITLE_BYTES} bytes")
            }
            Self::UnknownRole => write!(f, "the device has an unknown role"),
            Self::NotInvitable => write!(
                f,
                "only the device that created the lists administers them; invite full or partial devices"
            ),
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

#[derive(Clone)]
pub struct Todos {
    database: Database,
}
impl Todos {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
    pub fn database(&self) -> &Database {
        &self.database
    }
    pub async fn create_list(&self, name: &str) -> Result<Created, Failure> {
        let name = title(name)?;
        self.database.next_partition(LISTS).await?;
        let mut transaction = self.database.transaction().await?;
        let id = transaction.next_id(LISTS, params![]).await?;
        transaction
            .execute("INSERT INTO lists(id,title) VALUES(?,?)", params![id, name])
            .await?;
        let token = transaction.commit().await?;
        Ok(Created { id, token })
    }
    pub async fn rename_list(&self, list: RowIdentity, name: &str) -> Result<Commit, Failure> {
        let name = title(name)?;
        self.write("UPDATE lists SET title=? WHERE id=?", params![name, list])
            .await
    }
    pub async fn delete_list(&self, list: RowIdentity) -> Result<Commit, Failure> {
        self.write("DELETE FROM lists WHERE id=?", params![list])
            .await
    }
    pub async fn lists(&self) -> Result<Vec<List>, Failure> {
        let query = self
            .database
            .query("SELECT id,title FROM lists ORDER BY title,id", params![])
            .await;
        let partial = self.database.me().await?.access == rocky_sdk::Access::Role(roles::PARTIAL);
        let rows = match query {
            Ok(rows) => rows.collect().await?,
            Err(_) if partial => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        rows.iter()
            .map(|row| {
                Ok(List {
                    id: row.get_at(0)?,
                    title: row.get_at(1)?,
                })
            })
            .collect()
    }

    pub async fn add(&self, list: RowIdentity, name: &str) -> Result<Created, Failure> {
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
        list: RowIdentity,
        todo: RowIdentity,
        name: &str,
    ) -> Result<Commit, Failure> {
        let name = title(name)?;
        self.write(
            "UPDATE todos SET title=? WHERE list=? AND id=?",
            params![name, list, todo],
        )
        .await
    }
    pub async fn set_completed(
        &self,
        list: RowIdentity,
        todo: RowIdentity,
        completed: bool,
    ) -> Result<Commit, Failure> {
        self.write(
            "UPDATE todos SET completed=? WHERE list=? AND id=?",
            params![completed, list, todo],
        )
        .await
    }
    pub async fn raise_priority(
        &self,
        list: RowIdentity,
        todo: RowIdentity,
    ) -> Result<Commit, Failure> {
        self.write(
            "UPDATE todos SET priority=priority+? WHERE list=? AND id=?",
            params![1_i64, list, todo],
        )
        .await
    }
    pub async fn move_to(
        &self,
        list: RowIdentity,
        todo: RowIdentity,
        index: usize,
    ) -> Result<Commit, Failure> {
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
    pub async fn delete(&self, list: RowIdentity, todo: RowIdentity) -> Result<Commit, Failure> {
        self.write(
            "DELETE FROM todos WHERE list=? AND id=?",
            params![list, todo],
        )
        .await
    }
    pub async fn todos(&self, list: RowIdentity) -> Result<Vec<Todo>, Failure> {
        self.select(
            &format!("SELECT {TODO_COLUMNS} FROM todos WHERE list=? ORDER BY position"),
            params![list],
        )
        .await
    }
    pub async fn all_todos(&self) -> Result<Vec<Todo>, Failure> {
        self.select(
            &format!("SELECT {TODO_COLUMNS} FROM todos ORDER BY list,position"),
            params![],
        )
        .await
    }
    pub async fn dropped(
        &self,
        token: TransactionToken,
    ) -> Result<Option<(DropReason, bool)>, Failure> {
        Ok(match self.database.transaction_outcome(token).await? {
            Some(TransactionOutcome::Dropped { reason, sealed }) => Some((reason, sealed)),
            _ => None,
        })
    }

    async fn write(&self, sql: &str, params: Vec<SqlValue>) -> Result<Commit, Failure> {
        let mut transaction = self.database.transaction().await?;
        transaction.execute(sql, params).await?;
        Ok(transaction.commit().await?)
    }
    async fn select(&self, sql: &str, params: Vec<SqlValue>) -> Result<Vec<Todo>, Failure> {
        let rows = self.database.query(sql, params).await?.collect().await?;
        rows.iter().map(Todo::read).collect()
    }
}

fn excluded_identity(except: Option<RowIdentity>) -> SqlValue {
    except.map_or(SqlValue::Blob(Vec::new()), RowIdentity::sql)
}

async fn positions(
    transaction: &mut Transaction,
    list: RowIdentity,
    except: Option<RowIdentity>,
) -> Result<Vec<String>, Failure> {
    let rows = transaction
        .query(
            "SELECT position FROM todos WHERE list=? AND id<>? ORDER BY position",
            params![list, excluded_identity(except)],
        )
        .await?
        .collect()
        .await?;
    rows.iter().map(|row| Ok(row.get_at(0)?)).collect()
}
