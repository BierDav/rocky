use super::*;
use rocky_storage::capture::Table;

type Plans = BTreeMap<String, Table>;
type RowKey = (String, Vec<u8>);

pub(super) fn plan_table<'a>(
    typed: &typed::Typed,
    plans: &'a mut Plans,
    name: &str,
) -> Result<Option<&'a Table>, Error> {
    let lowered = name.to_ascii_lowercase();
    if !plans.contains_key(&lowered) {
        let Some(projected) = typed.by_name(name) else {
            return Ok(None);
        };
        let Some(table) = typed
            .plan(&BTreeSet::from([projected.scope]))?
            .tables
            .into_iter()
            .find(|table| table.name.eq_ignore_ascii_case(name))
        else {
            return Ok(None);
        };
        plans.insert(lowered.clone(), table);
    }
    Ok(plans.get(&lowered))
}

pub(super) struct Bystanders<'a> {
    schema: &'a rocky_core::LogicalSchema,
    plans: &'a Plans,
    undone: BTreeSet<RowKey>,
    kept: BTreeMap<RowKey, (&'a Table, Vec<SqlValue>, Vec<SqlValue>)>,
}

impl<'a> Bystanders<'a> {
    pub(super) fn new(
        schema: &'a rocky_core::LogicalSchema,
        plans: &'a Plans,
        undo: &[Undo],
    ) -> Self {
        Self {
            schema,
            plans,
            undone: undo
                .iter()
                .map(|undo| (undo.table.to_ascii_lowercase(), values_blob(&undo.key)))
                .collect(),
            kept: BTreeMap::new(),
        }
    }

    pub(super) async fn keep<A: RowApplication>(
        &mut self,
        inner: &Inner<A>,
        table: &str,
        key: &[SqlValue],
    ) -> Result<(), Error> {
        let mut work = vec![(table.to_ascii_lowercase(), key.to_vec())];
        while let Some((name, key)) = work.pop() {
            let (Some(parent), Some(logical)) = (self.plans.get(&name), self.schema.table(&name))
            else {
                continue;
            };
            let Some(image) = collect_rows(inner.conn.query(&parent.select, bindings(key)).await?)
                .await?
                .into_iter()
                .next()
            else {
                continue;
            };
            for child in self
                .schema
                .tables
                .iter()
                .filter(|child| child.class == TableClass::Replayed)
            {
                let child_name = child.sql_name.to_ascii_lowercase();
                let Some(plan) = self.plans.get(&child_name) else {
                    continue;
                };
                for reference in child
                    .foreign_keys
                    .iter()
                    .filter(|reference| reference.table.eq_ignore_ascii_case(&name))
                    .filter(|reference| reference.acts_on_delete())
                {
                    let referenced = match reference.references.is_empty() {
                        true => &logical.primary_key,
                        false => &reference.references,
                    };
                    let values = referenced
                        .iter()
                        .map(|column| {
                            logical
                                .columns
                                .iter()
                                .position(|known| known.sql_name.eq_ignore_ascii_case(column))
                                .and_then(|position| image.get(position).cloned())
                                .ok_or_else(|| Error::Corrupt("referenced column".into()))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    if values.iter().any(|value| matches!(value, SqlValue::Null)) {
                        continue;
                    }
                    let matching = reference
                        .columns
                        .iter()
                        .map(|column| format!("{}=?", typed::quoted(column)))
                        .collect::<Vec<_>>()
                        .join(" AND ");
                    let referencing = collect_rows(
                        inner
                            .conn
                            .query(&format!("{} WHERE {matching}", plan.scan), bindings(values))
                            .await?,
                    )
                    .await?;
                    let cascades = reference.on_delete.eq_ignore_ascii_case("CASCADE");
                    for row in referencing {
                        let key = plan
                            .key
                            .iter()
                            .map(|index| row.get(*index).cloned())
                            .collect::<Option<Vec<_>>>()
                            .ok_or_else(|| Error::Corrupt("referencing row key".into()))?;
                        if cascades {
                            work.push((child_name.clone(), key.clone()));
                        }
                        let id = (child_name.clone(), values_blob(&key));
                        if !self.undone.contains(&id) {
                            self.kept.entry(id).or_insert((plan, key, row));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn kept(&self) -> impl Iterator<Item = (&'a Table, &[SqlValue], &[SqlValue])> {
        self.kept
            .values()
            .map(|(table, key, image)| (*table, key.as_slice(), image.as_slice()))
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn read_after_undoing_inserts_for_test(
        &self,
        inserts: &[(&str, SqlValue)],
        read: &str,
    ) -> Result<Vec<Vec<SqlValue>>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        let undo: Vec<Undo> = inserts
            .iter()
            .map(|(table, key)| Undo {
                table: (*table).into(),
                key: vec![key.clone()],
                before: None,
            })
            .collect();
        inner.conn.execute("SAVEPOINT undoing", ()).await?;
        inner
            .conn
            .execute("PRAGMA defer_foreign_keys=ON", ())
            .await?;
        let read = async {
            inner.restore(&undo, &mut BTreeMap::new()).await?;
            rows(&inner.conn, read).await
        }
        .await;
        inner.conn.execute("ROLLBACK TO undoing", ()).await?;
        inner.conn.execute("RELEASE undoing", ()).await?;
        inner
            .conn
            .execute("PRAGMA defer_foreign_keys=OFF", ())
            .await?;
        read
    }
}
