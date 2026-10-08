use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

/// Logical to physical names of one domain's tables and indexes.
#[derive(Clone)]
pub struct Names {
    names: Arc<HashMap<String, String>>,
    scoped: bool,
    namespace: Option<String>,
}
/// Protocol tables and indexes every domain owns in its namespace.
const PROTOCOL: &[&str] = &[
    "rocky_replica",
    "rocky_additive_indexes",
    "rocky_operations",
    "rocky_operations_unapplied",
    "rocky_mutation_handles",
    "rocky_outbox",
    "rocky_peer_ack",
    "rocky_strong_meta",
    "rocky_command_handles",
    "rocky_requests",
    "rocky_preparations",
    "rocky_strong_outbox",
    "rocky_rejected_receipts",
    "rocky_strong_ack",
    "rocky_checkpoint",
    "rocky_rejected_local",
    "rocky_mutation_handles_sequence",
    "rocky_request_holds",
    "rocky_logical_schema",
    "rocky_sql_transactions",
    "rocky_sql_transactions_first",
];
/// Tables a schema creates, and whether each is `WITHOUT ROWID`.
pub fn tables(schema: &[impl AsRef<str>]) -> Vec<(String, bool)> {
    let mut tables = Vec::new();
    for sql in schema {
        let tokens: Vec<_> = tokens(sql.as_ref())
            .into_iter()
            .filter(|token| !token.trim().is_empty())
            .collect();
        let Some(mut index) = tokens
            .iter()
            .position(|token| token.eq_ignore_ascii_case("TABLE"))
        else {
            continue;
        };
        if tokens[..index]
            .iter()
            .any(|token| token.eq_ignore_ascii_case("INDEX"))
        {
            continue;
        }
        index += 1;
        if tokens
            .get(index)
            .is_some_and(|token| token.eq_ignore_ascii_case("IF"))
        {
            index += 3;
        }
        if let Some(name) = tokens.get(index) {
            let without_rowid = tokens.windows(2).any(|pair| {
                pair[0].eq_ignore_ascii_case("WITHOUT") && pair[1].eq_ignore_ascii_case("ROWID")
            });
            tables.push((
                name.trim_matches(['"', '`', '[', ']']).to_string(),
                without_rowid,
            ));
        }
    }
    tables
}
impl Names {
    /// Without a namespace every name maps to itself.
    pub fn new(namespace: Option<&str>, schema: &[impl AsRef<str>]) -> Self {
        let mut names = HashMap::new();
        let physical = |name: &str| match namespace {
            Some(namespace) => format!("{namespace}_{name}"),
            None => name.to_string(),
        };
        if namespace.is_some() {
            for table in PROTOCOL {
                names.insert((*table).to_string(), physical(table));
            }
        }
        for sql in schema {
            let tokens: Vec<_> = tokens(sql.as_ref())
                .into_iter()
                .filter(|token| !token.trim().is_empty())
                .collect();
            if let Some(index) = tokens.iter().position(|token| {
                token.eq_ignore_ascii_case("TABLE")
                    || token.eq_ignore_ascii_case("INDEX")
                    || token.eq_ignore_ascii_case("VIEW")
            }) {
                let mut index = index + 1;
                if tokens
                    .get(index)
                    .is_some_and(|token| token.eq_ignore_ascii_case("IF"))
                {
                    index += 3;
                }
                if let Some(name) = tokens.get(index) {
                    let name = name.trim_matches(['"', '`', '[', ']']);
                    names.insert(name.to_ascii_lowercase(), physical(name));
                }
            }
        }
        Self {
            names: Arc::new(names),
            scoped: namespace.is_some(),
            namespace: namespace.map(str::to_string),
        }
    }
    /// Exactly these names, lowercase, to physical tables of several domains.
    pub fn bound(names: HashMap<String, String>) -> Self {
        Self {
            names: Arc::new(names),
            scoped: true,
            namespace: None,
        }
    }
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
    /// The name a statement uses for a physical table, if it may name it.
    pub fn visible(&self, physical: &str) -> Option<String> {
        if !self.scoped {
            return Some(physical.to_ascii_lowercase());
        }
        self.names
            .iter()
            .find(|(_, candidate)| candidate.eq_ignore_ascii_case(physical))
            .map(|(logical, _)| logical.clone())
    }
    /// The physical name of an application table or index.
    pub fn physical(&self, logical: &str) -> Option<&str> {
        self.names
            .get(&logical.to_ascii_lowercase())
            .map(String::as_str)
    }
    pub fn sql(&self, sql: &str) -> String {
        if !self.scoped {
            return sql.to_string();
        }
        let tokens = tokens(sql);
        let significant: Vec<_> = tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| !token.trim().is_empty())
            .collect();
        let mut rewritten = tokens.clone();
        let mut from = false;
        for (position, (index, token)) in significant.iter().enumerate() {
            let word = token.to_ascii_uppercase();
            if word == "FROM" || word == "JOIN" {
                from = true;
            }
            if ["WHERE", "GROUP", "ORDER", "LIMIT", "UNION", "SET", "VALUES"]
                .contains(&word.as_str())
            {
                from = false;
            }
            let previous = position
                .checked_sub(1)
                .map(|index| significant[index].1.to_ascii_uppercase())
                .unwrap_or_default();
            let qualified = significant
                .get(position + 1)
                .is_some_and(|(_, token)| token.as_str() == ".");
            let after_exists = position >= 4
                && significant[position - 3].1.eq_ignore_ascii_case("IF")
                && significant[position - 2].1.eq_ignore_ascii_case("NOT")
                && previous == "EXISTS"
                && ["TABLE", "INDEX", "VIEW"]
                    .iter()
                    .any(|word| significant[position - 4].1.eq_ignore_ascii_case(word));
            let index_table = previous == "ON"
                && significant
                    .first()
                    .is_some_and(|(_, token)| token.eq_ignore_ascii_case("CREATE"))
                && significant[..position]
                    .iter()
                    .any(|(_, token)| token.eq_ignore_ascii_case("INDEX"));
            let table = [
                "TABLE",
                "INDEX",
                "VIEW",
                "INTO",
                "UPDATE",
                "FROM",
                "JOIN",
                "REFERENCES",
                "AS",
            ]
            .contains(&previous.as_str())
                || after_exists
                || index_table
                || (from && previous == ",")
                || qualified;
            if table {
                let name = token
                    .trim_matches(['"', '`', '[', ']'])
                    .to_ascii_lowercase();
                if let Some(physical) = self.names.get(&name) {
                    rewritten[*index] = format!("\"{}\"", physical.replace('"', "\"\""));
                }
            }
        }
        rewritten.concat()
    }
    pub fn logical_tables(
        &self,
        tables: Vec<Vec<rocky_core::SqlValue>>,
    ) -> Vec<Vec<rocky_core::SqlValue>> {
        if !self.scoped {
            return tables;
        }
        let present: std::collections::HashSet<_> = tables
            .iter()
            .filter_map(|row| match row.as_slice() {
                [rocky_core::SqlValue::Text(name)] => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let mut logical: Vec<_> = self
            .names
            .iter()
            .filter(|(_, physical)| present.contains(physical.as_str()))
            .map(|(logical, _)| logical.clone())
            .collect();
        logical.sort();
        logical
            .into_iter()
            .map(|name| vec![rocky_core::SqlValue::Text(name)])
            .collect()
    }
}
/// Physical table namespaces isolate domain conflicts and revision hot rows.
/// Several scoped views may share one connection, such as a file's writer.
pub struct ScopedConnection {
    raw: Arc<crate::sqlite::Connection>,
    names: Names,
    rewritten: Mutex<HashMap<String, String>>,
}
impl ScopedConnection {
    pub fn new(
        raw: crate::sqlite::Connection,
        namespace: Option<&str>,
        schema: &[impl AsRef<str>],
    ) -> Self {
        Self::shared(Arc::new(raw), namespace, schema)
    }
    /// The same namespace over another connection.
    pub fn with_connection(&self, raw: Arc<crate::sqlite::Connection>) -> Self {
        Self {
            raw,
            names: self.names.clone(),
            rewritten: Mutex::new(HashMap::new()),
        }
    }
    pub fn connection(&self) -> &crate::sqlite::Connection {
        &self.raw
    }
    /// Another namespace over the same connection.
    pub fn rescoped(&self, namespace: Option<&str>, schema: &[impl AsRef<str>]) -> Self {
        Self::shared(self.raw.clone(), namespace, schema)
    }
    /// The connection, once no other scoped view shares it.
    pub fn into_connection(self) -> Result<crate::sqlite::Connection, crate::Error> {
        Arc::try_unwrap(self.raw)
            .map_err(|_| crate::Error::Corrupt("connection is still shared".into()))
    }
    pub fn shared(
        raw: Arc<crate::sqlite::Connection>,
        namespace: Option<&str>,
        schema: &[impl AsRef<str>],
    ) -> Self {
        Self {
            raw,
            names: Names::new(namespace, schema),
            rewritten: Mutex::new(HashMap::new()),
        }
    }
    pub fn names(&self) -> &Names {
        &self.names
    }
    pub fn sql(&self, sql: &str) -> String {
        if let Ok(mut cache) = self.rewritten.lock() {
            if let Some(rewritten) = cache.get(sql) {
                return rewritten.clone();
            }
            let rewritten = self.names.sql(sql);
            // Bound caller-generated SQL independently of application width.
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(sql.to_string(), rewritten.clone());
            rewritten
        } else {
            self.names.sql(sql)
        }
    }
    pub fn logical_tables(
        &self,
        tables: Vec<Vec<rocky_core::SqlValue>>,
    ) -> Vec<Vec<rocky_core::SqlValue>> {
        self.names.logical_tables(tables)
    }
    pub async fn query(
        &self,
        sql: impl AsRef<str>,
        params: impl crate::sqlite::IntoParams,
    ) -> crate::sqlite::Result<crate::sqlite::Rows> {
        self.raw.query(self.sql(sql.as_ref()), params).await
    }
    pub async fn execute(
        &self,
        sql: impl AsRef<str>,
        params: impl crate::sqlite::IntoParams,
    ) -> crate::sqlite::Result<u64> {
        self.raw.execute(self.sql(sql.as_ref()), params).await
    }
    pub fn is_autocommit(&self) -> crate::sqlite::Result<bool> {
        self.raw.is_autocommit()
    }
}
/// Preserve literal strings and quoted identifiers during private SQL rewriting.
pub fn tokens(sql: &str) -> Vec<String> {
    let mut chars = sql.chars().peekable();
    let mut result = Vec::new();
    while let Some(first) = chars.next() {
        let mut token = String::from(first);
        if ['\'', '"', '`', '['].contains(&first) {
            let end = if first == '[' { ']' } else { first };
            while let Some(ch) = chars.next() {
                token.push(ch);
                if ch == end {
                    if chars.peek() == Some(&end) {
                        token.push(chars.next().unwrap_or(end));
                    } else {
                        break;
                    }
                }
            }
        } else if first.is_ascii_alphanumeric() || first == '_' {
            while chars
                .peek()
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            {
                if let Some(ch) = chars.next() {
                    token.push(ch);
                }
            }
        }
        result.push(token);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn namespaces_preserve_same_named_columns_literals_and_case() {
        let conn = ScopedConnection::new(
            crate::sqlite::Connection::open_in_memory().unwrap(),
            Some("scope"),
            &["CREATE TABLE stock(stock INTEGER)"],
        );
        conn.execute("CREATE TABLE stock(stock INTEGER)", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO STOCK(stock) VALUES(7)", ())
            .await
            .unwrap();
        let rows = crate::storage::rows(&conn, "SELECT stock FROM stock WHERE stock=7")
            .await
            .unwrap();
        assert_eq!(rows, vec![vec![rocky_core::SqlValue::Integer(7)]]);
        let rows = crate::storage::rows(&conn, "SELECT stock.stock,'stock' FROM stock AS stock")
            .await
            .unwrap();
        assert_eq!(
            rows,
            vec![vec![
                rocky_core::SqlValue::Integer(7),
                rocky_core::SqlValue::Text("stock".into())
            ]]
        );
    }
}
