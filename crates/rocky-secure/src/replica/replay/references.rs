use super::*;

pub(in crate::replica) type SettledScopes = BTreeMap<ResourceScope, bool>;

pub(in crate::replica) struct Orphan {
    pub(in crate::replica) parent: String,
    pub(in crate::replica) identity: Vec<SqlValue>,
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn orphans_of_settled_parents(
        &self,
        child: &rocky_core::LogicalTable,
        partition: Option<&[SqlValue]>,
        settled: &mut SettledScopes,
    ) -> Result<Vec<Orphan>, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(Vec::new());
        };
        let projected = typed
            .by_name(&child.sql_name)
            .ok_or_else(|| Error::Corrupt("a replayed table is not projected".into()))?;
        let identity = child.primary_key.len() + 1;
        let selected = child
            .primary_key
            .iter()
            .chain(child.columns.iter().map(|column| &column.sql_name))
            .map(|column| format!("c.{}", typed::quoted(column)))
            .collect::<Vec<_>>()
            .join(",");
        let mut sql = format!(
            "SELECT k.parent,k.fkid,{selected} FROM pragma_foreign_key_check(?1) k JOIN {} c ON c.rowid=k.rowid",
            typed::quoted(&child.sql_name)
        );
        let mut values = vec![SqlValue::Text(child.sql_name.clone())];
        if let Some(partition) = partition.filter(|_| !child.partition_by.is_empty()) {
            let in_partition = child
                .partition_by
                .iter()
                .enumerate()
                .map(|(index, column)| format!("c.{}=?{}", typed::quoted(column), index + 2))
                .collect::<Vec<_>>()
                .join(" AND ");
            sql = format!("{sql} WHERE {in_partition}");
            values.extend(partition.iter().cloned());
        }
        let mut orphans = Vec::new();
        for record in collect_rows(self.conn.query(&sql, bindings(values)).await?).await? {
            let [SqlValue::Text(parent), rest @ ..] = record.as_slice() else {
                return Err(Error::Corrupt("foreign key check".into()));
            };
            let (identity, columns) = rest
                .split_at_checked(identity)
                .ok_or_else(|| Error::Corrupt("foreign key check".into()))?;
            if self
                .settled_parent(projected, parent, columns, settled)
                .await?
            {
                orphans.push(Orphan {
                    parent: parent.clone(),
                    identity: identity.to_vec(),
                });
            }
        }
        Ok(orphans)
    }

    async fn settled_parent(
        &self,
        child: &typed::Projected,
        parent: &str,
        columns: &[SqlValue],
        settled: &mut SettledScopes,
    ) -> Result<bool, Error> {
        let Some(scope) = self.parent_scope(child, parent, columns)? else {
            return Ok(false);
        };
        if let Some(settled) = settled.get(&scope) {
            return Ok(*settled);
        }
        let held_and_settled = self.holds(scope).await?
            && !self.restoring(scope).await?
            && Box::pin(self.readable_blockers(scope)).await?.is_empty();
        settled.insert(scope, held_and_settled);
        Ok(held_and_settled)
    }

    fn parent_scope(
        &self,
        child: &typed::Projected,
        parent: &str,
        columns: &[SqlValue],
    ) -> Result<Option<ResourceScope>, Error> {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(None);
        };
        let Some(parent) = typed.by_name(parent) else {
            return Ok(None);
        };
        if !typed.partitioned(parent) {
            return Ok(Some(parent.scope));
        }
        Ok(typed
            .registered_row_scope(child, columns, &self.partitions)?
            .and_then(|row| self.group_member(row, parent.table.id)))
    }
}
