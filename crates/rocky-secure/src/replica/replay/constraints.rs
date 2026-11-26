use super::references::SettledScopes;
use super::*;

pub(in crate::replica) type Written = (String, Vec<SqlValue>);

pub(in crate::replica) fn note(written: &mut Vec<Written>, row: Written) {
    if !written.contains(&row) {
        written.push(row);
    }
}

pub(in crate::replica) fn written(
    logical: &rocky_core::LogicalTable,
    image: &[SqlValue],
) -> Result<Written, Error> {
    let partition = logical
        .partition_by
        .iter()
        .map(|name| {
            logical
                .columns
                .iter()
                .position(|column| column.sql_name.eq_ignore_ascii_case(name))
                .and_then(|position| image.get(position).cloned())
                .ok_or_else(|| Error::Corrupt("replayed partition column".into()))
        })
        .collect::<Result<_, _>>()?;
    Ok((logical.sql_name.to_ascii_lowercase(), partition))
}

pub(in crate::replica) fn leaf_first<'a>(
    tables: impl IntoIterator<Item = &'a rocky_core::LogicalTable>,
) -> Vec<&'a rocky_core::LogicalTable> {
    let mut left: Vec<_> = tables.into_iter().collect();
    let mut ordered = Vec::with_capacity(left.len());
    while !left.is_empty() {
        let leaf = left
            .iter()
            .position(|table| {
                !left.iter().any(|child| {
                    !child.sql_name.eq_ignore_ascii_case(&table.sql_name)
                        && child
                            .foreign_keys
                            .iter()
                            .any(|key| key.table.eq_ignore_ascii_case(&table.sql_name))
                })
            })
            .unwrap_or(0);
        ordered.push(left.remove(leaf));
    }
    ordered
}

pub(in crate::replica) fn leaf_first_ranks(
    schema: &rocky_core::LogicalSchema,
) -> BTreeMap<String, usize> {
    leaf_first(&schema.tables)
        .into_iter()
        .enumerate()
        .map(|(rank, table)| (table.sql_name.to_ascii_lowercase(), rank))
        .collect()
}

pub(in crate::replica) fn cascades(
    schema: &rocky_core::LogicalSchema,
    table: &str,
) -> BTreeSet<String> {
    let mut reached = BTreeSet::from([table.to_ascii_lowercase()]);
    let mut work = vec![table.to_ascii_lowercase()];
    while let Some(parent) = work.pop() {
        for child in &schema.tables {
            let acts = child
                .foreign_keys
                .iter()
                .any(|key| key.table.eq_ignore_ascii_case(&parent) && key.acts_on_delete());
            let name = child.sql_name.to_ascii_lowercase();
            if child.class == TableClass::Replayed && acts && reached.insert(name.clone()) {
                work.push(name);
            }
        }
    }
    reached
}

pub(in crate::replica) fn with_referencing(
    schema: &rocky_core::LogicalSchema,
    deleted: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut read = deleted.clone();
    for child in schema
        .tables
        .iter()
        .filter(|child| child.class == TableClass::Replayed)
    {
        if child
            .foreign_keys
            .iter()
            .any(|key| deleted.contains(&key.table.to_ascii_lowercase()))
        {
            read.insert(child.sql_name.to_ascii_lowercase());
        }
    }
    read
}

pub(in crate::replica) fn referenced(
    schema: &rocky_core::LogicalSchema,
    table: &str,
) -> BTreeSet<String> {
    let own = table.to_ascii_lowercase();
    let mut reached = BTreeSet::new();
    let mut work = vec![own.clone()];
    while let Some(child) = work.pop() {
        let Some(child) = schema.table(&child) else {
            continue;
        };
        for key in &child.foreign_keys {
            let parent = key.table.to_ascii_lowercase();
            let replayed = schema
                .table(&parent)
                .is_some_and(|table| table.class == TableClass::Replayed);
            if replayed && reached.insert(parent.clone()) {
                work.push(parent);
            }
        }
    }
    reached.remove(&own);
    reached
}

pub(in crate::replica) fn linked(
    schema: &rocky_core::LogicalSchema,
    table: &str,
) -> BTreeSet<String> {
    let replayed = |name: &str| {
        schema
            .table(name)
            .is_some_and(|table| table.class == TableClass::Replayed)
    };
    let mut reached = BTreeSet::from([table.to_ascii_lowercase()]);
    let mut work = vec![table.to_ascii_lowercase()];
    while let Some(name) = work.pop() {
        let mut next = Vec::new();
        for child in schema
            .tables
            .iter()
            .filter(|child| child.class == TableClass::Replayed)
        {
            let child_name = child.sql_name.to_ascii_lowercase();
            for key in &child.foreign_keys {
                if child_name == name {
                    next.push(key.table.to_ascii_lowercase());
                } else if key.table.eq_ignore_ascii_case(&name) {
                    next.push(child_name.clone());
                }
            }
        }
        for assertion in &schema.assertions {
            let tables = assertion.tables();
            if tables.iter().any(|read| read.eq_ignore_ascii_case(&name)) {
                next.extend(tables.iter().map(|read| read.to_ascii_lowercase()));
            }
        }
        for name in next {
            if replayed(&name) && reached.insert(name.clone()) {
                work.push(name);
            }
        }
    }
    reached
}

pub(in crate::replica) fn checked(
    schema: &rocky_core::LogicalSchema,
    changed: &BTreeSet<String>,
) -> BTreeSet<String> {
    let mut read = changed.clone();
    for name in changed {
        if let Some(table) = schema.table(name) {
            read.extend(
                table
                    .foreign_keys
                    .iter()
                    .map(|key| key.table.to_ascii_lowercase()),
            );
        }
    }
    read
}

pub(in crate::replica) fn check_member_order<'a>(
    schema: &rocky_core::LogicalSchema,
    members: impl IntoIterator<Item = (&'a str, &'a [rocky_core::replay::IntentStatement])> + Clone,
) -> Result<(), Error> {
    let deletes = |sql: &str| {
        let words: Vec<String> = sql
            .split_whitespace()
            .take(3)
            .map(str::to_ascii_uppercase)
            .collect();
        matches!(
            words.first().map(String::as_str),
            Some("DELETE" | "REPLACE")
        ) || words.windows(2).any(|pair| pair == ["OR", "REPLACE"])
    };
    for (table, statements) in members.clone() {
        if !statements.iter().any(|statement| deletes(&statement.sql)) {
            continue;
        }
        let reached = cascades(schema, table);
        if let Some((other, _)) = members.clone().into_iter().find(|(other, _)| {
            !other.eq_ignore_ascii_case(table) && reached.contains(&other.to_ascii_lowercase())
        }) {
            return Err(denied(&format!(
                "deleting from {table} changes {other}, which the transaction also writes; commit them apart"
            )));
        }
    }
    Ok(())
}

enum Check<'a> {
    Reference {
        child: &'a rocky_core::LogicalTable,
        partitions: Vec<&'a Vec<SqlValue>>,
    },
    Assertion {
        name: &'a str,
        sql: String,
        params: Vec<SqlValue>,
    },
}

impl Check<'_> {
    fn failure(&self) -> String {
        match self {
            Check::Reference { child, .. } => {
                format!("FOREIGN KEY constraint failed: {}", child.sql_name)
            }
            Check::Assertion { name, .. } => format!("assertion {name} failed"),
        }
    }
}

enum Evaluated {
    Rows(Vec<Vec<SqlValue>>),
    Overflow,
}

fn distinct_partitions<'a>(written: impl Iterator<Item = &'a Written>) -> Vec<&'a Vec<SqlValue>> {
    let mut partitions: Vec<&Vec<SqlValue>> = Vec::new();
    for (_, partition) in written {
        if !partitions.contains(&partition) {
            partitions.push(partition);
        }
    }
    partitions
}

fn replayed_checks<'a>(
    schema: &'a rocky_core::LogicalSchema,
    written: &'a [Written],
    deferred_all: bool,
) -> Result<Vec<Check<'a>>, Error> {
    let wrote = |name: &str| {
        written
            .iter()
            .any(|(table, _)| table.eq_ignore_ascii_case(name))
    };
    let mut checks = Vec::new();
    for table in &schema.tables {
        let checked_parents: Vec<&str> = table
            .foreign_keys
            .iter()
            .filter(|key| key.deferred || deferred_all)
            .map(|key| key.table.as_str())
            .collect();
        let checked = table.class == TableClass::Replayed
            && !checked_parents.is_empty()
            && (wrote(&table.sql_name) || checked_parents.iter().any(|parent| wrote(parent)));
        if !checked {
            continue;
        }
        let partitions = distinct_partitions(written.iter().filter(|(name, _)| {
            name.eq_ignore_ascii_case(&table.sql_name)
                || checked_parents
                    .iter()
                    .any(|parent| name.eq_ignore_ascii_case(parent))
        }));
        if !table.partition_by.is_empty()
            && partitions
                .iter()
                .any(|partition| partition.len() != table.partition_by.len())
        {
            return Err(Error::Corrupt(
                "a reference crosses partition groups".into(),
            ));
        }
        checks.push(Check::Reference {
            child: table,
            partitions,
        });
    }
    for assertion in &schema.assertions {
        let tables = assertion.tables();
        let partitions =
            distinct_partitions(written.iter().filter(|(table, _)| tables.contains(table)));
        for partition in partitions {
            let mut rules = BTreeMap::new();
            for name in &tables {
                let logical = schema
                    .table(name)
                    .ok_or_else(|| Error::Corrupt("assertion table".into()))?;
                let partitioned = !logical.partition_by.is_empty();
                rules.insert(
                    name.clone(),
                    TablePartitions {
                        columns: super::super::partition::partition_columns(logical)?,
                        complete: !partitioned,
                        held: if partitioned {
                            vec![partition.clone()]
                        } else {
                            Vec::new()
                        },
                        waiting: Vec::new(),
                    },
                );
            }
            let checked = rocky_storage::partition_sql::check_assertion(
                &assertion.select_sql,
                &schema.views,
                &rules,
            )
            .map_err(|refusal| {
                Error::Corrupt(format!("assertion {}: {refusal}", assertion.name))
            })?;
            checks.push(Check::Assertion {
                name: &assertion.name,
                sql: checked.sql,
                params: checked.params,
            });
        }
    }
    Ok(checks)
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn replayed_violation(
        &self,
        written: &[Written],
        undo_in_applied_order: &[&[Undo]],
        deferred_all: bool,
    ) -> Result<Option<String>, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(None);
        };
        let checks = replayed_checks(typed.rows.schema(), written, deferred_all)?;
        let mut settled = SettledScopes::new();
        let mut violated = Vec::new();
        for check in &checks {
            match self.evaluate(check, &mut settled).await? {
                Evaluated::Overflow => {
                    return Ok(Some(format!(
                        "{}: {}",
                        check.failure(),
                        rocky_storage::exact::OVERFLOW
                    )));
                }
                Evaluated::Rows(rows) if rows.is_empty() => {}
                Evaluated::Rows(rows) => violated.push((check, rows)),
            }
        }
        if violated.is_empty() {
            return Ok(None);
        }
        let before = self
            .before_the_transaction(undo_in_applied_order, async || {
                let mut before = Vec::with_capacity(violated.len());
                for (check, _) in &violated {
                    before.push(match self.evaluate(check, &mut settled).await? {
                        Evaluated::Rows(rows) => rows,
                        Evaluated::Overflow => Vec::new(),
                    });
                }
                Ok(before)
            })
            .await?;
        Ok(violated
            .iter()
            .zip(before)
            .find(|((_, after), before)| after.iter().any(|row| !before.contains(row)))
            .map(|((check, _), _)| check.failure()))
    }

    async fn before_the_transaction<T>(
        &self,
        undo_in_applied_order: &[&[Undo]],
        read: impl AsyncFnOnce() -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.conn.execute("SAVEPOINT before_replayed", ()).await?;
        let read = async {
            let mut plans = BTreeMap::new();
            let combined: Vec<Undo> = undo_in_applied_order
                .iter()
                .flat_map(|slice| slice.iter().cloned())
                .collect();
            self.restore(&combined, &mut plans).await?;
            read().await
        }
        .await;
        let rolled_back = self.conn.execute("ROLLBACK TO before_replayed", ()).await;
        let released = self.conn.execute("RELEASE before_replayed", ()).await;
        let read = read?;
        rolled_back?;
        released?;
        Ok(read)
    }

    async fn evaluate(
        &self,
        check: &Check<'_>,
        settled: &mut SettledScopes,
    ) -> Result<Evaluated, Error> {
        match check {
            Check::Reference { child, partitions } => self
                .violating_rows(child, partitions, settled)
                .await
                .map(Evaluated::Rows),
            Check::Assertion { sql, params, .. } => {
                match self
                    .conn
                    .query(sql, storage::bindings(params.clone()))
                    .await
                {
                    Err(error) if error.is_overflow() => Ok(Evaluated::Overflow),
                    rows => Ok(Evaluated::Rows(collect_rows(rows?).await?)),
                }
            }
        }
    }

    async fn violating_rows(
        &self,
        child: &rocky_core::LogicalTable,
        partitions: &[&Vec<SqlValue>],
        settled: &mut SettledScopes,
    ) -> Result<Vec<Vec<SqlValue>>, Error> {
        let mut violating = Vec::new();
        if child.partition_by.is_empty() {
            violating.extend(
                self.orphans_of_settled_parents(child, None, settled)
                    .await?,
            );
        }
        for partition in partitions.iter().filter(|_| !child.partition_by.is_empty()) {
            violating.extend(
                self.orphans_of_settled_parents(child, Some(partition), settled)
                    .await?,
            );
        }
        Ok(violating
            .into_iter()
            .map(|orphan| orphan.identity)
            .collect())
    }
}
