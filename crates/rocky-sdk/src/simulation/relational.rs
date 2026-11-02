//! The relational fixture: `boards` and their `cards`, both partitioned by
//! `list` in one partition group, so each list's cards live in partitions
//! (scopes) other than its boards. A card references its board by an
//! `ON DELETE CASCADE` key and optionally pins a second board by an
//! `ON DELETE SET NULL` key. Codes and names are unique per list and drawn
//! from small pools so concurrent writers collide; `CHECK`s bound caps and
//! points, and the `board_cap` assertion bounds a board's points by its cap.
//!
//! The reference is independent of Rocky's replay: a plain in-memory SQLite
//! database with hand-written DDL (the `SET NULL` key as a trigger that
//! leaves the partition column alone, as Rocky's does) replays, from
//! scratch, the statements the driver itself bound and committed, in
//! canonical order `(era, stamp)`. Each transaction runs in a savepoint,
//! with foreign keys deferred when it spans several scopes; a failing
//! statement, a remaining foreign key violation or a violated assertion
//! drops it. Only the order comes from the replicas: the stamp the author
//! assigned and the era of the seal that covers it.
use super::schedule::Change;
use crate::{Database, Failure, SqlValue, TransactionToken};
use rocky_core::{SecureStamp, replay::TransactionOutcome};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const SCHEMA: &str = r#"{"application":"relational","version":1,"format":6,
"tables":[
 {"name":"boards","class":"replayed","columns":[
   {"name":"list","type":"text","not_null":true},
   {"name":"code","type":"text","not_null":true},
   {"name":"cap","type":"integer","not_null":true}],
  "checks":["cap BETWEEN 1 AND 12"],
  "indexes":[{"name":"boards_code","unique":true,"columns":[{"name":"list","descending":false},{"name":"code","descending":false}]}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list"},
 {"name":"cards","class":"replayed","columns":[
   {"name":"list","type":"text","not_null":true},
   {"name":"board","type":"integer","not_null":true},
   {"name":"pinned","type":"integer"},
   {"name":"name","type":"text","not_null":true},
   {"name":"points","type":"integer","not_null":true}],
  "checks":["points BETWEEN 0 AND 9"],
  "indexes":[{"name":"cards_name","unique":true,"columns":[{"name":"list","descending":false},{"name":"name","descending":false}]}],
  "foreign_keys":[
   {"columns":["list","board"],"table":"boards","on_delete":"cascade"},
   {"columns":["list","pinned"],"table":"boards","on_delete":"set_null"}],
  "primary_key":["list","id"],"partition_by":["list"],"partition_group":"list"}],
"assertions":[{"name":"board_cap","select_sql":"SELECT b.id FROM boards b JOIN cards c ON c.list = b.list AND c.board = b.id GROUP BY b.list, b.id HAVING SUM(c.points) > MIN(b.cap)"}]}"#;

const REFERENCE_DDL: &str = "PRAGMA foreign_keys=ON;
CREATE TABLE boards(list TEXT NOT NULL, code TEXT NOT NULL, cap INTEGER NOT NULL, id INTEGER NOT NULL,
  PRIMARY KEY(list, id), UNIQUE(list, code), CHECK(cap BETWEEN 1 AND 12));
CREATE TABLE cards(list TEXT NOT NULL, board INTEGER NOT NULL, pinned INTEGER, name TEXT NOT NULL,
  points INTEGER NOT NULL, id INTEGER NOT NULL, PRIMARY KEY(list, id), UNIQUE(list, name),
  CHECK(points BETWEEN 0 AND 9),
  FOREIGN KEY(list, board) REFERENCES boards(list, id) ON DELETE CASCADE,
  FOREIGN KEY(list, pinned) REFERENCES boards(list, id));
CREATE TRIGGER cards_unpin AFTER DELETE ON boards BEGIN
  UPDATE cards SET pinned = NULL WHERE list = OLD.list AND pinned = OLD.id; END;";
const ASSERTION: &str = "SELECT b.id FROM boards b JOIN cards c ON c.list = b.list AND c.board = b.id GROUP BY b.list, b.id HAVING SUM(c.points) > MIN(b.cap)";

/// What each list's rows are compared by.
pub(super) const BOARDS: &str = "SELECT list, id, code, cap FROM boards WHERE list = ? ORDER BY id";
pub(super) const CARDS: &str =
    "SELECT list, id, board, pinned, name, points FROM cards WHERE list = ? ORDER BY id";
/// Rows of a list that break a constraint or the assertion: none may exist.
pub(super) const VIOLATIONS: [(&str, &str); 5] = [
    (
        "orphan card",
        "SELECT c.id FROM cards c WHERE c.list = ? AND NOT EXISTS (SELECT 1 FROM boards b WHERE b.list = c.list AND b.id = c.board)",
    ),
    (
        "dangling pin",
        "SELECT c.id FROM cards c WHERE c.list = ? AND c.pinned IS NOT NULL AND NOT EXISTS (SELECT 1 FROM boards b WHERE b.list = c.list AND b.id = c.pinned)",
    ),
    (
        "points check",
        "SELECT id FROM cards WHERE list = ? AND points NOT BETWEEN 0 AND 9",
    ),
    (
        "cap check",
        "SELECT id FROM boards WHERE list = ? AND cap NOT BETWEEN 1 AND 12",
    ),
    (
        "board_cap",
        "SELECT b.id FROM boards b JOIN cards c ON c.list = b.list AND c.board = b.id WHERE b.list = ? GROUP BY b.list, b.id HAVING SUM(c.points) > MIN(b.cap)",
    ),
];

/// One list's rows: its boards, then its cards.
pub(super) type ListRows = (Vec<Vec<SqlValue>>, Vec<Vec<SqlValue>>);

/// A transaction the driver committed, with the values it bound.
#[derive(Clone, Debug)]
pub(super) struct Authored {
    pub(super) token: TransactionToken,
    pub(super) node: usize,
    pub(super) step: usize,
    pub(super) statements: Vec<(String, Vec<SqlValue>)>,
    /// Lists its statements touch, the target of a move included.
    pub(super) lists: BTreeSet<String>,
    /// Its bundle's members, as `(table, list)`: several defer keys.
    pub(super) scopes: BTreeSet<(&'static str, String)>,
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}

/// Ids of `table`'s rows in `list`, as the author reads them.
async fn ids(
    transaction: &mut crate::Transaction,
    table: &str,
    list: &str,
) -> Result<Vec<i64>, Failure> {
    let rows = transaction
        .query(
            &format!("SELECT id FROM {table} WHERE list = ? ORDER BY id"),
            vec![text(list)],
        )
        .await?
        .collect()
        .await?;
    rows.iter()
        .map(|row| row.get_at::<i64>(0).map_err(|error| error.0.into()))
        .collect()
}
fn pick(ids: &[i64], index: u8) -> Result<i64, Failure> {
    match ids {
        [] => Err("skipped: nothing to pick".into()),
        ids => Ok(ids[usize::from(index) % ids.len()]),
    }
}

/// The statements of a change with this author's values bound, run inside
/// `transaction`; each statement is also returned for the reference.
async fn statements(
    transaction: &mut crate::Transaction,
    change: &Change,
) -> Result<Vec<(String, Vec<SqlValue>)>, Failure> {
    use SqlValue::Integer;
    let mut run: Vec<(String, Vec<SqlValue>)> = Vec::new();
    match change {
        Change::Board { list, code, cap } => {
            let id = transaction.next_id("boards", vec![text(list)]).await?;
            run.push((
                "INSERT INTO boards(list, code, cap, id) VALUES(?, ?, ?, ?)".into(),
                vec![
                    text(list),
                    text(&format!("c{code}")),
                    Integer(i64::from(*cap)),
                    Integer(id),
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
            let boards = ids(transaction, "boards", list).await?;
            let board = pick(&boards, *board)?;
            let pinned = match pinned {
                Some(index) => Integer(pick(&boards, *index)?),
                None => SqlValue::Null,
            };
            let id = transaction.next_id("cards", vec![text(list)]).await?;
            run.push((
                "INSERT INTO cards(list, board, pinned, name, points, id) VALUES(?, ?, ?, ?, ?, ?)"
                    .into(),
                vec![
                    text(list),
                    Integer(board),
                    pinned,
                    text(&format!("n{name}")),
                    Integer(i64::from(*points)),
                    Integer(id),
                ],
            ));
        }
        Change::Pair {
            list,
            code,
            name,
            points,
        } => {
            let board = transaction.next_id("boards", vec![text(list)]).await?;
            let card = transaction.next_id("cards", vec![text(list)]).await?;
            run.push((
                "INSERT INTO boards(list, code, cap, id) VALUES(?, ?, 6, ?)".into(),
                vec![text(list), text(&format!("c{code}")), Integer(board)],
            ));
            run.push((
                "INSERT INTO cards(list, board, pinned, name, points, id) VALUES(?, ?, ?, ?, ?, ?)"
                    .into(),
                vec![
                    text(list),
                    Integer(board),
                    Integer(board),
                    text(&format!("n{name}")),
                    Integer(i64::from(*points)),
                    Integer(card),
                ],
            ));
        }
        Change::Raise { list, card, by } => {
            let card = pick(&ids(transaction, "cards", list).await?, *card)?;
            run.push((
                "UPDATE cards SET points = points + ? WHERE list = ? AND id = ?".into(),
                vec![Integer(i64::from(*by)), text(list), Integer(card)],
            ));
        }
        Change::Recode { list, board, code } => {
            let board = pick(&ids(transaction, "boards", list).await?, *board)?;
            run.push((
                "UPDATE boards SET code = ? WHERE list = ? AND id = ?".into(),
                vec![text(&format!("c{code}")), text(list), Integer(board)],
            ));
        }
        Change::DropBoard { list, board } => {
            let board = pick(&ids(transaction, "boards", list).await?, *board)?;
            run.push((
                "DELETE FROM boards WHERE list = ? AND id = ?".into(),
                vec![text(list), Integer(board)],
            ));
        }
        Change::DropCard { list, card } => {
            let card = pick(&ids(transaction, "cards", list).await?, *card)?;
            run.push((
                "DELETE FROM cards WHERE list = ? AND id = ?".into(),
                vec![text(list), Integer(card)],
            ));
        }
        Change::MoveCard {
            from,
            to,
            card,
            board,
        } => {
            let card = pick(&ids(transaction, "cards", from).await?, *card)?;
            let board = pick(&ids(transaction, "boards", to).await?, *board)?;
            run.push((
                "UPDATE cards SET list = ?, board = ?, pinned = NULL WHERE list = ? AND id = ?"
                    .into(),
                vec![text(to), Integer(board), text(from), Integer(card)],
            ));
        }
        Change::MoveBoard { from, to, board } => {
            let board = pick(&ids(transaction, "boards", from).await?, *board)?;
            run.push((
                "UPDATE boards SET list = ? WHERE list = ? AND id = ?".into(),
                vec![text(to), text(from), Integer(board)],
            ));
        }
    }
    for (sql, values) in &run {
        transaction.execute(sql, values.clone()).await?;
    }
    Ok(run)
}

/// Lists a change touches and the scopes its statements write; a delete
/// action's effects stay in the member that caused them.
fn touched(change: &Change) -> (BTreeSet<String>, BTreeSet<(&'static str, String)>) {
    let (lists, scopes): (Vec<&String>, Vec<(&'static str, &String)>) = match change {
        Change::Board { list, .. }
        | Change::Recode { list, .. }
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

/// Authors `change` on `database`. `Ok(None)` when a statement refused it
/// before commit; a failed commit is still returned, as uncertain.
pub(super) async fn author(
    database: &Database,
    node: usize,
    step: usize,
    change: &Change,
) -> Result<(Authored, Result<(), Failure>), Failure> {
    let mut transaction = database.transaction().await?;
    let token = transaction.token();
    let run = match statements(&mut transaction, change).await {
        Ok(run) => run,
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    let committed = transaction.commit().await;
    let (lists, scopes) = touched(change);
    let authored = Authored {
        token,
        node,
        step,
        statements: run,
        lists,
        scopes,
    };
    Ok((authored, committed.map(drop).map_err(Failure::from)))
}

/// One list's rows and violations on `database`.
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
/// Constraint or assertion violations `database` shows in `list`.
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

/// A transaction's canonical position: the era of the seal that covers
/// it, then its earliest member's stamp.
pub(super) type Position = (i64, SecureStamp);

/// The reference's result: every list's rows and every transaction's
/// outcome.
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

/// Replays `transactions` from scratch in the order of `positions`; every
/// drop is final, as the reference runs after the last seal.
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
                false => TransactionOutcome::Applied,
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
