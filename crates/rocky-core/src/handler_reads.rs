//! Immutable keyed access to one confirmed command predecessor.
use crate::SqlValue;
use std::{cmp::Ordering, collections::BTreeMap, sync::Arc};

/// Maximum rows returned by a handler scan.
pub const MAX_HANDLER_SCAN_ROWS: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Table {
    key: Vec<usize>,
    rows: Vec<Vec<SqlValue>>,
}
/// A command's complete resources, exposed only through keyed, bounded reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HandlerReads {
    tables: BTreeMap<String, Arc<Table>>,
}
fn compare(a: &SqlValue, b: &SqlValue) -> Ordering {
    let rank = |v: &SqlValue| match v {
        SqlValue::Null => 0,
        SqlValue::Integer(_) => 1,
        SqlValue::Real(_) => 2,
        SqlValue::Text(_) => 3,
        SqlValue::Blob(_) => 4,
    };
    match (a, b) {
        (SqlValue::Integer(a), SqlValue::Integer(b)) => a.cmp(b),
        (SqlValue::Real(a), SqlValue::Real(b)) => a.get().total_cmp(&b.get()),
        (SqlValue::Text(a), SqlValue::Text(b)) => a.as_bytes().cmp(b.as_bytes()),
        (SqlValue::Blob(a), SqlValue::Blob(b)) => a.cmp(b),
        _ => rank(a).cmp(&rank(b)),
    }
}
fn compare_keys(a: &[SqlValue], b: &[SqlValue]) -> Ordering {
    a.iter()
        .zip(b)
        .map(|(a, b)| compare(a, b))
        .find(|c| *c != Ordering::Equal)
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}
impl Table {
    fn key(&self, row: &[SqlValue]) -> Vec<SqlValue> {
        self.key.iter().map(|i| row[*i].clone()).collect()
    }
    fn check(&self, key: &[SqlValue]) -> Result<(), String> {
        if key.len() != self.key.len() || key.contains(&SqlValue::Null) {
            return Err("a handler key must contain every non-null primary-key component".into());
        }
        Ok(())
    }
}
impl HandlerReads {
    /// Builds a snapshot from rows and their primary-key column positions.
    pub fn new(tables: BTreeMap<String, (Vec<usize>, Vec<Vec<SqlValue>>)>) -> Result<Self, String> {
        let mut result = Self::default();
        for (name, (key, rows)) in tables {
            if key.is_empty() || key.iter().enumerate().any(|(i, k)| key[..i].contains(k)) {
                return Err(format!("{name} has an invalid primary key"));
            }
            let mut table = Table { key, rows };
            for row in &table.rows {
                if table
                    .key
                    .iter()
                    .any(|i| row.get(*i).is_none_or(|v| *v == SqlValue::Null))
                {
                    return Err(format!("{name} has a missing or null primary key"));
                }
            }
            let key = table.key.clone();
            table.rows.sort_by(|a, b| {
                key.iter()
                    .map(|i| compare(&a[*i], &b[*i]))
                    .find(|c| *c != Ordering::Equal)
                    .unwrap_or(Ordering::Equal)
            });
            if table
                .rows
                .windows(2)
                .any(|pair| table.key(&pair[0]) == table.key(&pair[1]))
            {
                return Err(format!("{name} has duplicate primary keys"));
            }
            result.tables.insert(name, Arc::new(table));
        }
        Ok(result)
    }
    fn table(&self, name: &str) -> Result<&Table, String> {
        self.tables
            .get(name)
            .map(AsRef::as_ref)
            .ok_or_else(|| format!("{name} is not a command resource"))
    }
    /// Exact primary key, in declared key order; an absent row is `None`.
    pub fn get(&self, table: &str, key: &[SqlValue]) -> Result<Option<Vec<SqlValue>>, String> {
        let table = self.table(table)?;
        table.check(key)?;
        Ok(table
            .rows
            .binary_search_by(|row| compare_keys(&table.key(row), key))
            .ok()
            .map(|i| table.rows[i].clone()))
    }
    /// Ascending primary-key order, inclusive lower and exclusive upper bound.
    /// Bounds are complete keys; `None` is unbounded. Limit is 1..=1024.
    pub fn scan(
        &self,
        table: &str,
        lower: Option<&[SqlValue]>,
        upper: Option<&[SqlValue]>,
        limit: usize,
    ) -> Result<Vec<Vec<SqlValue>>, String> {
        let table = self.table(table)?;
        if !(1..=MAX_HANDLER_SCAN_ROWS).contains(&limit) {
            return Err(format!(
                "handler scan limit must be 1..={MAX_HANDLER_SCAN_ROWS}"
            ));
        }
        for bound in [lower, upper].into_iter().flatten() {
            table.check(bound)?;
        }
        if lower
            .zip(upper)
            .is_some_and(|(a, b)| compare_keys(a, b) == Ordering::Greater)
        {
            return Err("handler scan bounds are reversed".into());
        }
        let start = lower.map_or(0, |key| {
            table
                .rows
                .partition_point(|row| compare_keys(&table.key(row), key) == Ordering::Less)
        });
        Ok(table.rows[start..]
            .iter()
            .take_while(|row| {
                upper.is_none_or(|key| compare_keys(&table.key(row), key) == Ordering::Less)
            })
            .take(limit)
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(s: &str) -> SqlValue {
        SqlValue::Text(s.into())
    }
    fn reads(mut rows: Vec<Vec<SqlValue>>) -> HandlerReads {
        rows.reverse();
        HandlerReads::new([("items".into(), (vec![1, 0], rows))].into()).unwrap()
    }
    #[test]
    fn compound_reads_are_ordered_bounded_and_independent_of_input_order() {
        let rows = vec![
            vec![SqlValue::Integer(10), text("a")],
            vec![SqlValue::Integer(-2), text("a")],
            vec![SqlValue::Integer(3), text("b")],
        ];
        let a = reads(rows.clone());
        let mut reversed = rows;
        reversed.reverse();
        let b = reads(reversed);
        assert_eq!(a, b);
        assert_eq!(
            a.get("items", &[text("a"), SqlValue::Integer(10)]).unwrap(),
            Some(vec![SqlValue::Integer(10), text("a")])
        );
        assert_eq!(
            a.get("items", &[text("x"), SqlValue::Integer(1)]).unwrap(),
            None
        );
        let lower = [text("a"), SqlValue::Integer(-2)];
        let upper = [text("b"), SqlValue::Integer(3)];
        assert_eq!(
            a.scan("items", Some(&lower), Some(&upper), 1).unwrap(),
            vec![vec![SqlValue::Integer(-2), text("a")]]
        );
        assert_eq!(
            a.scan("items", Some(&lower), Some(&upper), 10)
                .unwrap()
                .len(),
            2
        );
        assert!(a.scan("items", Some(&upper), Some(&lower), 1).is_err());
        assert!(a.scan("items", None, None, 0).is_err());
        assert!(a.scan("items", None, None, 1025).is_err());
        assert!(a.get("other", &lower).is_err());
        assert!(a.get("items", &[text("a")]).is_err());
        assert!(
            a.get("items", &[SqlValue::Null, SqlValue::Integer(1)])
                .is_err()
        );
    }
    #[test]
    fn malformed_state_is_refused_and_empty_resources_are_available() {
        assert!(
            HandlerReads::new(
                [(
                    "t".into(),
                    (vec![0], vec![vec![text("x")], vec![text("x")]])
                )]
                .into()
            )
            .is_err()
        );
        assert!(
            HandlerReads::new([("t".into(), (vec![1], vec![vec![text("x")]]))].into()).is_err()
        );
        let empty = HandlerReads::new([("t".into(), (vec![0], vec![]))].into()).unwrap();
        assert_eq!(empty.get("t", &[text("x")]).unwrap(), None);
        assert!(empty.scan("t", None, None, 1).unwrap().is_empty());
    }
}
