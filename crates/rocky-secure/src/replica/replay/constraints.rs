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
                .any(|key| key.table.eq_ignore_ascii_case(&parent) && key.on_delete != "NO ACTION");
            let name = child.sql_name.to_ascii_lowercase();
            if child.class == TableClass::Replayed && acts && reached.insert(name.clone()) {
                work.push(name);
            }
        }
    }
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

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn replayed_violation(
        &self,
        written: &[Written],
        deferred_all: bool,
    ) -> Result<Option<String>, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(None);
        };
        let schema = typed.rows.schema();
        let wrote = |name: &str| {
            written
                .iter()
                .any(|(table, _)| table.eq_ignore_ascii_case(name))
        };
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
            let reached = written.iter().filter(|(name, _)| {
                name.eq_ignore_ascii_case(&table.sql_name)
                    || checked_parents
                        .iter()
                        .any(|parent| name.eq_ignore_ascii_case(parent))
            });
            if self.breaks_a_reference(table, reached).await? {
                return Ok(Some(format!(
                    "FOREIGN KEY constraint failed: {}",
                    table.sql_name
                )));
            }
        }
        for assertion in &schema.assertions {
            let tables = assertion.tables();
            let mut partitions: Vec<&Vec<SqlValue>> = Vec::new();
            for (_, partition) in written.iter().filter(|(table, _)| tables.contains(table)) {
                if !partitions.contains(&partition) {
                    partitions.push(partition);
                }
            }
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
                let mut rows = match self
                    .conn
                    .query(&checked.sql, storage::bindings(checked.params))
                    .await
                {
                    Err(error) if error.is_overflow() => {
                        return Ok(Some(format!(
                            "assertion {} failed: {}",
                            assertion.name,
                            rocky_storage::exact::OVERFLOW
                        )));
                    }
                    rows => rows?,
                };
                if rows.next().await?.is_some() {
                    return Ok(Some(format!("assertion {} failed", assertion.name)));
                }
            }
        }
        Ok(None)
    }
    async fn breaks_a_reference(
        &self,
        child: &rocky_core::LogicalTable,
        reached: impl Iterator<Item = &Written>,
    ) -> Result<bool, Error> {
        if child.partition_by.is_empty() {
            let mut rows = self
                .conn
                .query(
                    "SELECT 1 FROM pragma_foreign_key_check(?) LIMIT 1",
                    [child.sql_name.clone()],
                )
                .await?;
            return Ok(rows.next().await?.is_some());
        }
        let in_partition = child
            .partition_by
            .iter()
            .enumerate()
            .map(|(index, column)| format!("c.{}=?{}", typed::quoted(column), index + 2))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = format!(
            "SELECT 1 FROM pragma_foreign_key_check(?1) k JOIN {} c ON c.rowid=k.rowid WHERE {in_partition} LIMIT 1",
            typed::quoted(&child.sql_name)
        );
        let mut partitions: Vec<&Vec<SqlValue>> = Vec::new();
        for (_, partition) in reached {
            if !partitions.contains(&partition) {
                partitions.push(partition);
            }
        }
        for partition in partitions {
            if partition.len() != child.partition_by.len() {
                return Err(Error::Corrupt(
                    "a reference crosses partition groups".into(),
                ));
            }
            let mut values = vec![SqlValue::Text(child.sql_name.clone())];
            values.extend(partition.iter().cloned());
            let mut rows = self.conn.query(&sql, bindings(values)).await?;
            if rows.next().await?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
