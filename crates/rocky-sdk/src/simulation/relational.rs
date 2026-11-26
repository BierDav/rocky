use super::schedule::Change;
use crate::{Database, Failure, SqlValue, TransactionToken};
use rocky_core::{SecureStamp, replay::TransactionOutcome};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const SCHEMA: &str = r#"{"application":"relational","version":1,"format":10,
"tables":[
 {"name":"boards","class":"replayed","columns":[
   {"name":"list","type":"text","not_null":true},
   {"name":"code","type":"text","not_null":true},
   {"name":"cap","type":"integer","not_null":true}],
  "checks":["cap BETWEEN 1 AND 12"],
  "primary_key":["list","code"],"partition_by":["list"],"partition_group":"list"},
 {"name":"cards","class":"replayed","columns":[
   {"name":"list","type":"text","not_null":true},
   {"name":"board","type":"text","not_null":true},
   {"name":"pinned","type":"text"},
   {"name":"name","type":"text","not_null":true},
   {"name":"points","type":"integer","not_null":true}],
  "checks":["points BETWEEN 0 AND 9"],
  "indexes":[{"name":"cards_name","unique":true,"columns":[{"name":"list","descending":false},{"name":"name","descending":false}]}],
  "foreign_keys":[
   {"columns":["list","board"],"table":"boards","on_delete":"cascade"},
   {"columns":["list","pinned"],"table":"boards","on_delete":"set_null"}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list"}],
"roles":[{"name":"list_member","tables":[{"table":"boards","access":"write"},{"table":"cards","access":"write"}],"per_partition":["list"]}],
"assertions":[{"name":"board_cap","select_sql":"SELECT b.code FROM boards b JOIN cards c ON c.list = b.list AND c.board = b.code GROUP BY b.list, b.code HAVING SUM(c.points) > MIN(b.cap)"}]}"#;

const REFERENCE_DDL: &str = "PRAGMA foreign_keys=ON;
CREATE TABLE boards(list TEXT NOT NULL, code TEXT NOT NULL, cap INTEGER NOT NULL,
  PRIMARY KEY(list, code), CHECK(cap BETWEEN 1 AND 12));
CREATE TABLE cards(list TEXT NOT NULL, board TEXT NOT NULL, pinned TEXT, name TEXT NOT NULL,
  points INTEGER NOT NULL, id BLOB NOT NULL, PRIMARY KEY(list, id), UNIQUE(list, name),
  CHECK(points BETWEEN 0 AND 9),
  FOREIGN KEY(list, board) REFERENCES boards(list, code) ON DELETE CASCADE,
  FOREIGN KEY(list, pinned) REFERENCES boards(list, code));
CREATE TRIGGER cards_unpin AFTER DELETE ON boards BEGIN
  UPDATE cards SET pinned = NULL WHERE list = OLD.list AND pinned = OLD.code; END;";
const ASSERTION: &str = "SELECT b.code FROM boards b JOIN cards c ON c.list = b.list AND c.board = b.code GROUP BY b.list, b.code HAVING SUM(c.points) > MIN(b.cap)";

pub(super) const LIST_MEMBER: crate::RoleName = crate::RoleName::from_static("list_member");

pub(super) const BOARDS: &str = "SELECT list, code, cap FROM boards WHERE list = ? ORDER BY code";
pub(super) const CARDS: &str =
    "SELECT list, id, board, pinned, name, points FROM cards WHERE list = ? ORDER BY id";
pub(super) const VIOLATIONS: [(&str, &str); 5] = [
    (
        "orphan card",
        "SELECT c.id FROM cards c WHERE c.list = ? AND NOT EXISTS (SELECT 1 FROM boards b WHERE b.list = c.list AND b.code = c.board)",
    ),
    (
        "dangling pin",
        "SELECT c.id FROM cards c WHERE c.list = ? AND c.pinned IS NOT NULL AND NOT EXISTS (SELECT 1 FROM boards b WHERE b.list = c.list AND b.code = c.pinned)",
    ),
    (
        "points check",
        "SELECT id FROM cards WHERE list = ? AND points NOT BETWEEN 0 AND 9",
    ),
    (
        "cap check",
        "SELECT code FROM boards WHERE list = ? AND cap NOT BETWEEN 1 AND 12",
    ),
    (
        "board_cap",
        "SELECT b.code FROM boards b JOIN cards c ON c.list = b.list AND c.board = b.code WHERE b.list = ? GROUP BY b.list, b.code HAVING SUM(c.points) > MIN(b.cap)",
    ),
];

pub(super) type ListRows = (Vec<Vec<SqlValue>>, Vec<Vec<SqlValue>>);

#[derive(Clone, Debug)]
pub(super) struct Authored {
    pub(super) token: TransactionToken,
    pub(super) node: usize,
    pub(super) step: usize,
    pub(super) statements: Vec<(String, Vec<SqlValue>)>,
    pub(super) lists: BTreeSet<String>,
    pub(super) scopes: BTreeSet<(&'static str, String)>,
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}

async fn column<T: crate::FromSqlValue>(
    transaction: &mut crate::Transaction,
    table: &str,
    column: &str,
    list: &str,
) -> Result<Vec<T>, Failure> {
    let rows = transaction
        .query(
            &format!("SELECT {column} FROM {table} WHERE list = ? ORDER BY {column}"),
            vec![text(list)],
        )
        .await?
        .collect()
        .await?;
    rows.iter()
        .map(|row| row.get_at::<T>(0).map_err(|error| error.0.into()))
        .collect()
}
async fn codes(transaction: &mut crate::Transaction, list: &str) -> Result<Vec<String>, Failure> {
    column(transaction, "boards", "code", list).await
}
async fn cards(
    transaction: &mut crate::Transaction,
    list: &str,
) -> Result<Vec<rocky_core::RowIdentity>, Failure> {
    column(transaction, "cards", "id", list).await
}
fn pick<T: Clone>(values: &[T], index: u8) -> Result<T, Failure> {
    match values {
        [] => Err("skipped: nothing to pick".into()),
        values => Ok(values[usize::from(index) % values.len()].clone()),
    }
}
const CARD: &str = "INSERT INTO cards(list, board, pinned, name, points) VALUES(?, ?, ?, ?, ?)";
async fn insert_card(
    transaction: &mut crate::Transaction,
    values: Vec<SqlValue>,
) -> Result<(String, Vec<SqlValue>), Failure> {
    let rows = transaction
        .query(&format!("{CARD} RETURNING id"), values.clone())
        .await?
        .collect()
        .await?;
    let id: rocky_core::RowIdentity = rows
        .first()
        .ok_or("an inserted card returned no id")?
        .get_at(0)
        .map_err(|error| error.0)?;
    let mut replayed = values;
    replayed.push(id.sql());
    Ok((
        "INSERT INTO cards(list, board, pinned, name, points, id) VALUES(?, ?, ?, ?, ?, ?)".into(),
        replayed,
    ))
}

async fn statements(
    transaction: &mut crate::Transaction,
    change: &Change,
) -> Result<Vec<(String, Vec<SqlValue>)>, Failure> {
    use SqlValue::Integer;
    let mut run: Vec<(String, Vec<SqlValue>)> = Vec::new();
    let mut executed = Vec::new();
    match change {
        Change::Board { list, code, cap } => {
            run.push((
                "INSERT INTO boards(list, code, cap) VALUES(?, ?, ?)".into(),
                vec![
                    text(list),
                    text(&format!("c{code}")),
                    Integer(i64::from(*cap)),
                ],
            ));
        }
        Change::Card {
            list,
            board,
            pinned,
            name,
            points,
        } => {
            let boards = codes(transaction, list).await?;
            let board = pick(&boards, *board)?;
            let pinned = match pinned {
                Some(index) => text(&pick(&boards, *index)?),
                None => SqlValue::Null,
            };
            executed.push(
                insert_card(
                    transaction,
                    vec![
                        text(list),
                        text(&board),
                        pinned,
                        text(&format!("n{name}")),
                        Integer(i64::from(*points)),
                    ],
                )
                .await?,
            );
        }
        Change::Pair {
            list,
            code,
            name,
            points,
        } => {
            let board = format!("c{code}");
            let created = (
                "INSERT INTO boards(list, code, cap) VALUES(?, ?, 6)".to_string(),
                vec![text(list), text(&board)],
            );
            transaction.execute(&created.0, created.1.clone()).await?;
            executed.push(created);
            executed.push(
                insert_card(
                    transaction,
                    vec![
                        text(list),
                        text(&board),
                        text(&board),
                        text(&format!("n{name}")),
                        Integer(i64::from(*points)),
                    ],
                )
                .await?,
            );
        }
        Change::Raise { list, card, by } => {
            let card = pick(&cards(transaction, list).await?, *card)?;
            run.push((
                "UPDATE cards SET points = points + ? WHERE list = ? AND id = ?".into(),
                vec![Integer(i64::from(*by)), text(list), card.sql()],
            ));
        }
        Change::Upsert { list, code, cap } => {
            run.push((
                "INSERT INTO boards(list, code, cap) VALUES(?, ?, ?) \
                 ON CONFLICT(list, code) DO UPDATE SET cap = excluded.cap"
                    .into(),
                vec![
                    text(list),
                    text(&format!("c{code}")),
                    Integer(i64::from(*cap)),
                ],
            ));
        }
        Change::DropBoard { list, board } => {
            let board = pick(&codes(transaction, list).await?, *board)?;
            run.push((
                "DELETE FROM boards WHERE list = ? AND code = ?".into(),
                vec![text(list), text(&board)],
            ));
        }
        Change::DropCard { list, card } => {
            let card = pick(&cards(transaction, list).await?, *card)?;
            run.push((
                "DELETE FROM cards WHERE list = ? AND id = ?".into(),
                vec![text(list), card.sql()],
            ));
        }
        Change::MoveCard {
            from,
            to,
            card,
            board,
        } => {
            let card = pick(&cards(transaction, from).await?, *card)?;
            let board = pick(&codes(transaction, to).await?, *board)?;
            run.push((
                "UPDATE cards SET list = ?, board = ?, pinned = NULL WHERE list = ? AND id = ?"
                    .into(),
                vec![text(to), text(&board), text(from), card.sql()],
            ));
        }
        Change::MoveBoard { from, to, board } => {
            let board = pick(&codes(transaction, from).await?, *board)?;
            run.push((
                "UPDATE boards SET list = ? WHERE list = ? AND code = ?".into(),
                vec![text(to), text(from), text(&board)],
            ));
        }
    }
    for (sql, values) in &run {
        transaction.execute(sql, values.clone()).await?;
    }
    executed.extend(run);
    Ok(executed)
}

fn touched(change: &Change) -> (BTreeSet<String>, BTreeSet<(&'static str, String)>) {
    let (lists, scopes): (Vec<&String>, Vec<(&'static str, &String)>) = match change {
        Change::Board { list, .. }
        | Change::Upsert { list, .. }
        | Change::DropBoard { list, .. } => (vec![list], vec![("boards", list)]),
        Change::Card { list, .. } | Change::Raise { list, .. } | Change::DropCard { list, .. } => {
            (vec![list], vec![("cards", list)])
        }
        Change::Pair { list, .. } => (vec![list], vec![("boards", list), ("cards", list)]),
        Change::MoveCard { from, to, .. } => (vec![from, to], vec![("cards", from), ("cards", to)]),
        Change::MoveBoard { from, to, .. } => {
            (vec![from, to], vec![("boards", from), ("boards", to)])
        }
    };
    (
        lists.into_iter().cloned().collect(),
        scopes
            .into_iter()
            .map(|(table, list)| (table, list.clone()))
            .collect(),
    )
}

pub(super) async fn author(
    database: &Database,
    node: usize,
    step: usize,
    change: &Change,
) -> Result<(Authored, Result<(), Failure>), Failure> {
    let mut ran = None;
    let committed = database
        .write(async |transaction| {
            let token = transaction.token();
            ran = Some((token, statements(transaction, change).await?));
            Ok::<_, Failure>(())
        })
        .await;
    let Some((token, run)) = ran else {
        return Err(committed
            .err()
            .unwrap_or_else(|| "a write ran no statements".into()));
    };
    let (lists, scopes) = touched(change);
    let authored = Authored {
        token,
        node,
        step,
        statements: run,
        lists,
        scopes,
    };
    Ok((authored, committed))
}

pub(super) async fn list_rows(database: &Database, list: &str) -> Result<ListRows, Failure> {
    let read = |sql: &'static str| async move {
        let rows = database
            .query(sql, vec![text(list)])
            .await?
            .collect()
            .await?;
        Ok::<_, Failure>(
            rows.iter()
                .map(|row| row.values().to_vec())
                .collect::<Vec<_>>(),
        )
    };
    Ok((read(BOARDS).await?, read(CARDS).await?))
}
pub(super) async fn violations(database: &Database, list: &str) -> Result<Vec<String>, Failure> {
    let mut found = Vec::new();
    for (name, sql) in VIOLATIONS {
        let rows = database
            .query(sql, vec![text(list)])
            .await?
            .collect()
            .await?;
        if !rows.is_empty() {
            found.push(format!("{name} in {list}: {:?}", rows[0].values()));
        }
    }
    Ok(found)
}

pub(super) type Position = (i64, SecureStamp);

#[derive(Debug, Default)]
pub(super) struct Reference {
    pub(super) lists: BTreeMap<String, ListRows>,
    pub(super) outcomes: BTreeMap<TransactionToken, TransactionOutcome>,
}

fn bound(value: &SqlValue) -> rusqlite::types::Value {
    use rusqlite::types::Value;
    match value {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(value) => Value::Integer(*value),
        SqlValue::Text(value) => Value::Text(value.clone()),
        SqlValue::Blob(value) => Value::Blob(value.clone()),
        SqlValue::Real(value) => Value::Real(value.get()),
    }
}
fn read(value: rusqlite::types::Value) -> SqlValue {
    use rusqlite::types::Value;
    match value {
        Value::Null => SqlValue::Null,
        Value::Integer(value) => SqlValue::Integer(value),
        Value::Text(value) => SqlValue::Text(value),
        Value::Blob(value) => SqlValue::Blob(value),
        Value::Real(value) => SqlValue::real(value).unwrap_or(SqlValue::Null),
    }
}

pub(super) fn replay(
    transactions: &[&Authored],
    positions: &BTreeMap<TransactionToken, Position>,
    lists: &BTreeSet<String>,
) -> Result<Reference, String> {
    let fail = |error: rusqlite::Error| error.to_string();
    let mut order: Vec<(&Position, &Authored)> = transactions
        .iter()
        .map(|authored| {
            positions
                .get(&authored.token)
                .map(|position| (position, *authored))
                .ok_or_else(|| format!("no canonical position for step {}", authored.step))
        })
        .collect::<Result<_, _>>()?;
    order.sort_by(|a, b| a.0.cmp(b.0));
    let db = rusqlite::Connection::open_in_memory().map_err(fail)?;
    db.execute_batch(REFERENCE_DDL).map_err(fail)?;
    let mut reference = Reference::default();
    for (_, authored) in order {
        let several = authored.scopes.len() > 1;
        db.execute_batch("SAVEPOINT t").map_err(fail)?;
        if several {
            db.execute_batch("PRAGMA defer_foreign_keys=ON")
                .map_err(fail)?;
        }
        let failed = authored.statements.iter().any(|(sql, values)| {
            db.execute(sql, rusqlite::params_from_iter(values.iter().map(bound)))
                .is_err()
        }) || exists(&db, "PRAGMA foreign_key_check")?
            || exists(&db, ASSERTION)?;
        if failed {
            db.execute_batch("ROLLBACK TO t").map_err(fail)?;
        }
        db.execute_batch("PRAGMA defer_foreign_keys=OFF; RELEASE t")
            .map_err(fail)?;
        reference.outcomes.insert(
            authored.token,
            match failed {
                true => TransactionOutcome::Dropped {
                    reason: rocky_core::replay::DropReason::Constraint,
                    sealed: true,
                },
                false => TransactionOutcome::Applied { sealed: true },
            },
        );
    }
    for list in lists {
        let rows = |sql: &str| -> Result<Vec<Vec<SqlValue>>, String> {
            let mut statement = db.prepare(sql).map_err(fail)?;
            let width = statement.column_count();
            let rows = statement
                .query_map([list], |row| {
                    (0..width)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index).map(read))
                        .collect()
                })
                .map_err(fail)?
                .collect::<Result<_, _>>()
                .map_err(fail)?;
            Ok(rows)
        };
        reference
            .lists
            .insert(list.clone(), (rows(BOARDS)?, rows(CARDS)?));
    }
    Ok(reference)
}
fn exists(db: &rusqlite::Connection, sql: &str) -> Result<bool, String> {
    let mut statement = db.prepare(sql).map_err(|error| error.to_string())?;
    let mut rows = statement.query([]).map_err(|error| error.to_string())?;
    Ok(rows.next().map_err(|error| error.to_string())?.is_some())
}
