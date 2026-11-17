use crate::{
    BoundQuery, Completeness, Error, ReadCursor, ReadOptions, ReadSnapshot,
    scoped_connection::{Names, tokens},
    sqlite, storage,
};
use rocky_core::{Commitment, LocalRevision, ReadView, Scope, SqlValue};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError, Weak},
};
use tokio::sync::watch;

const POOL_CONNECTIONS: usize = 4;
pub(crate) const MAX_READ_BYTES: usize = crate::MAX_FRAME_BYTES;
pub(crate) const MAX_PAGE_ROWS: usize = 1024;
const MAX_READ_SQL_BYTES: usize = 8192;
pub(crate) const MAX_KEYSET_COLUMNS: usize = 16;

struct View {
    subquery: String,
    tables: Vec<String>,
    key: Vec<String>,
    not_null: Vec<String>,
}
#[derive(Clone)]
struct Rewritten {
    sql: String,
    view: Option<usize>,
    hidden: Option<usize>,
}
struct Keyset {
    terms: Vec<String>,
    ordered: usize,
    descending: bool,
    filter: Option<usize>,
    order: usize,
}
fn wrong_cursor() -> Error {
    Error::Rejected("read cursor belongs to another query".into())
}
pub(crate) struct ReadPlan {
    views: Vec<View>,
    by_name: HashMap<String, usize>,
    by_source: HashMap<String, Vec<usize>>,
    rewritten: Mutex<HashMap<(String, Option<usize>), Rewritten>>,
    revision: String,
    pending: String,
}
fn denied() -> Error {
    Error::Rejected("read queries require a single bounded SELECT over authorized tables".into())
}
fn invalid_view() -> Error {
    Error::Incompatible("invalid authorized read view".into())
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !name.as_bytes()[0].is_ascii_digit()
}
fn reserved(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("rocky_") || name.starts_with("sqlite_")
}
fn unquoted(token: &str) -> &str {
    token.trim_matches(['"', '`', '[', ']'])
}
fn is_numbered_parameter(token: &str, next: Option<&str>) -> bool {
    token == "?" && next.is_some_and(|next| next.as_bytes()[0].is_ascii_digit())
}
fn real_literal(value: f64) -> String {
    match value {
        value if value.is_finite() => format!("{value:?}"),
        value if value > 0.0 => "9e999".into(),
        _ => "-9e999".into(),
    }
}
fn literal(value: &SqlValue) -> Result<String, Error> {
    Ok(match value {
        SqlValue::Null => "NULL".into(),
        SqlValue::Integer(value) => value.to_string(),
        SqlValue::Real(value) => real_literal(value.get()),
        SqlValue::Text(text) if text.contains('\0') => return Err(invalid_view()),
        SqlValue::Text(text) => format!("'{}'", text.replace('\'', "''")),
        SqlValue::Blob(bytes) => {
            let mut hex = String::with_capacity(bytes.len() * 2 + 3);
            hex.push_str("X'");
            for byte in bytes {
                use std::fmt::Write;
                let _ = write!(hex, "{byte:02X}");
            }
            hex.push('\'');
            hex
        }
    })
}
impl ReadPlan {
    pub(crate) fn compile(names: &Names, views: Vec<ReadView>) -> Result<Self, Error> {
        let mut compiled: Vec<View> = Vec::new();
        let mut by_name = HashMap::new();
        let mut by_source: HashMap<String, Vec<usize>> = HashMap::new();
        for view in views {
            let name = view.name.to_ascii_lowercase();
            if !identifier(&name) || reserved(&name) || by_name.contains_key(&name) {
                return Err(invalid_view());
            }
            let key: Vec<String> = view.key.iter().map(|c| c.to_ascii_lowercase()).collect();
            let mut not_null = key.clone();
            not_null.extend(view.not_null.iter().map(|c| c.to_ascii_lowercase()));
            if key.len() > MAX_KEYSET_COLUMNS || !not_null.iter().all(|column| identifier(column)) {
                return Err(invalid_view());
            }
            let mut tokens = tokens(&view.sql);
            let significant: Vec<usize> = (0..tokens.len())
                .filter(|index| !tokens[*index].trim().is_empty())
                .collect();
            if !significant
                .first()
                .is_some_and(|index| tokens[*index].eq_ignore_ascii_case("SELECT"))
            {
                return Err(invalid_view());
            }
            let (mut tables, mut sources) = (Vec::new(), Vec::new());
            let mut values = view.values.iter();
            for (position, index) in significant.iter().enumerate() {
                let next = significant
                    .get(position + 1)
                    .map(|next| tokens[*next].as_str());
                let token = tokens[*index].as_str();
                if token == ";" || is_numbered_parameter(token, next) {
                    return Err(invalid_view());
                }
                if token == "?" {
                    tokens[*index] = literal(values.next().ok_or_else(invalid_view)?)?;
                    continue;
                }
                let source =
                    token.eq_ignore_ascii_case("FROM") || token.eq_ignore_ascii_case("JOIN");
                if let (true, Some(table)) = (source, next.filter(|next| *next != "(")) {
                    let logical = unquoted(table).to_ascii_lowercase();
                    let physical = names.physical(&logical).filter(|_| !reserved(&logical));
                    let physical = physical.ok_or_else(invalid_view)?.to_string();
                    if !tables.contains(&physical) {
                        tables.push(physical);
                        sources.push(logical);
                    }
                }
            }
            if values.next().is_some() {
                return Err(invalid_view());
            }
            by_name.insert(name.clone(), compiled.len());
            for source in &sources {
                by_source
                    .entry(source.clone())
                    .or_default()
                    .push(compiled.len());
            }
            compiled.push(View {
                subquery: names.sql(&tokens.concat()),
                tables,
                key,
                not_null,
            });
        }
        Ok(Self {
            views: compiled,
            by_name,
            by_source,
            rewritten: Mutex::new(HashMap::new()),
            revision: names.sql("SELECT revision FROM rocky_replica WHERE id=1"),
            pending: names.sql(
                "SELECT COUNT(*) FROM rocky_operations WHERE status<>'applied' AND status='pending'",
            ),
        })
    }
    pub(crate) fn views(&self) -> usize {
        self.views.len()
    }
    pub(crate) fn reading<'a>(&'a self, table: &'a str) -> impl Iterator<Item = usize> + 'a {
        self.by_source.get(table).into_iter().flatten().copied()
    }
    fn rewrite(&self, sql: &str, seek: Option<usize>) -> Result<Rewritten, Error> {
        let key = (sql.to_string(), seek);
        let mut cache = self
            .rewritten
            .lock()
            .map_err(|_| Error::Corrupt("read statement cache".into()))?;
        if let Some(rewritten) = cache.get(&key) {
            return Ok(rewritten.clone());
        }
        let rewritten = self.rewrite_uncached(sql, seek)?;
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(key, rewritten.clone());
        Ok(rewritten)
    }
    fn rewrite_uncached(&self, sql: &str, seek: Option<usize>) -> Result<Rewritten, Error> {
        validate(sql)?;
        let mut tokens = tokens(sql);
        let significant: Vec<usize> = (0..tokens.len())
            .filter(|index| !tokens[*index].trim().is_empty())
            .collect();
        let Some(from) = significant
            .iter()
            .position(|index| tokens[*index].eq_ignore_ascii_case("FROM"))
        else {
            return match seek {
                Some(_) => Err(wrong_cursor()),
                None => Ok(Rewritten {
                    sql: sql.to_string(),
                    view: None,
                    hidden: None,
                }),
            };
        };
        let table = *significant.get(from + 1).ok_or_else(denied)?;
        let name = unquoted(&tokens[table]).to_ascii_lowercase();
        let view = *self.by_name.get(&name).ok_or_else(denied)?;
        let query_aliases_table = significant.get(from + 2).is_some_and(|next| {
            !["WHERE", "GROUP", "ORDER", "LIMIT", "HAVING"]
                .iter()
                .any(|word| tokens[*next].eq_ignore_ascii_case(word))
        });
        let keyset = keyset(&tokens, &significant, from, &self.views[view]);
        let subquery = &self.views[view].subquery;
        tokens[table] = if query_aliases_table {
            format!("({subquery})")
        } else {
            format!("({subquery}) AS \"{name}\"")
        };
        let Some(keyset) = keyset else {
            return match seek {
                Some(_) => Err(wrong_cursor()),
                None => Ok(Rewritten {
                    sql: tokens.concat(),
                    view: Some(view),
                    hidden: None,
                }),
            };
        };
        let columns = keyset.terms.join(",");
        tokens[significant[from]] = format!(",{columns} FROM");
        if let Some(values) = seek {
            if values != keyset.terms.len() {
                return Err(wrong_cursor());
            }
            let after = seek_predicate(&columns, keyset.descending, values);
            match keyset.filter {
                Some(filter) => {
                    tokens[filter] = format!("WHERE {after} AND (");
                    tokens[keyset.order] = ") ORDER".into();
                }
                None => tokens[keyset.order] = format!(" WHERE {after} ORDER"),
            }
        }
        let direction = if keyset.descending { " DESC" } else { "" };
        for tiebreak in &keyset.terms[keyset.ordered..] {
            tokens.push(format!(",{tiebreak}{direction}"));
        }
        Ok(Rewritten {
            sql: tokens.concat(),
            view: Some(view),
            hidden: Some(keyset.terms.len()),
        })
    }
    pub(crate) fn view(&self, sql: &str) -> Option<usize> {
        self.rewrite(sql, None).ok().and_then(|read| read.view)
    }
}
fn seek_predicate(columns: &str, descending: bool, values: usize) -> String {
    format!(
        "({columns}){}({})",
        if descending { "<" } else { ">" },
        vec!["?"; values].join(",")
    )
}
fn keyset(tokens: &[String], significant: &[usize], from: usize, view: &View) -> Option<Keyset> {
    if view.key.is_empty() {
        return None;
    }
    let token = |position: usize| {
        significant
            .get(position)
            .map(|index| tokens[*index].as_str())
    };
    let column = |token: &str| {
        (!token.starts_with('\'') && identifier(unquoted(token)))
            .then(|| unquoted(token).to_ascii_lowercase())
    };
    let is =
        |position: usize, word: &str| token(position).is_some_and(|t| t.eq_ignore_ascii_case(word));
    if (0..significant.len())
        .any(|at| token(at).is_some_and(|t| is_numbered_parameter(t, token(at + 1))))
    {
        return None;
    }
    let selected = 1..from;
    let star = selected.len() == 1 && is(1, "*");
    let plain_columns = selected.len() % 2 == 1
        && selected.clone().all(|at| match (at - 1) % 2 {
            0 => token(at).and_then(column).is_some(),
            _ => is(at, ","),
        });
    if !star && !plain_columns {
        return None;
    }
    let mut at = from + 2;
    if is(at, "AS") {
        at += 2;
    } else if !is(at, "WHERE") && !is(at, "ORDER") {
        at += 1;
    }
    let filter = is(at, "WHERE").then(|| significant[at]);
    if (at..significant.len()).any(|at| {
        ["GROUP", "HAVING", "LIMIT", "OFFSET"]
            .iter()
            .any(|word| is(at, word))
    }) {
        return None;
    }
    let order = (at..significant.len()).find(|at| is(*at, "ORDER"))?;
    if filter.is_none() && order != at {
        return None;
    }
    if !is(order + 1, "BY") {
        return None;
    }
    let (mut terms, mut names, mut directions) = (Vec::new(), Vec::new(), Vec::new());
    let mut at = order + 2;
    loop {
        let written = token(at)?;
        let name = column(written)?;
        if names.contains(&name) {
            return None;
        }
        terms.push(written.to_string());
        names.push(name);
        at += 1;
        let descending = is(at, "DESC");
        if descending || is(at, "ASC") {
            at += 1;
        }
        directions.push(descending);
        match token(at) {
            None => break,
            Some(",") => at += 1,
            Some(_) => return None,
        }
    }
    let descending = directions[0];
    if directions.iter().any(|direction| *direction != descending)
        || (descending && !names.iter().all(|name| view.not_null.contains(name)))
    {
        return None;
    }
    let ordered = terms.len();
    terms.extend(view.key.iter().filter(|key| !names.contains(key)).cloned());
    (terms.len() <= MAX_KEYSET_COLUMNS).then_some(Keyset {
        terms,
        ordered,
        descending,
        filter,
        order: significant[order],
    })
}
fn validate(sql: &str) -> Result<(), Error> {
    let tokens: Vec<_> = tokens(sql)
        .into_iter()
        .filter(|token| !token.trim().is_empty())
        .collect();
    if !tokens
        .first()
        .is_some_and(|token| token.eq_ignore_ascii_case("SELECT"))
    {
        return Err(denied());
    }
    let mut selects = 0;
    for (index, token) in tokens.iter().enumerate() {
        let word = token
            .trim_matches(['\'', '"', '`', '[', ']'])
            .to_ascii_uppercase();
        if word == "SELECT" {
            selects += 1;
        }
        if token == ";"
            || token == "."
            || word.starts_with("ROCKY_")
            || word.starts_with("SQLITE_")
            || [
                "INSERT",
                "UPDATE",
                "DELETE",
                "CREATE",
                "DROP",
                "ALTER",
                "ATTACH",
                "DETACH",
                "PRAGMA",
                "WITH",
                "JOIN",
                "UNION",
                "INTERSECT",
                "EXCEPT",
                "LOAD_EXTENSION",
                "READFILE",
                "WRITEFILE",
            ]
            .contains(&word.as_str())
        {
            return Err(denied());
        }
        if (token == "-" && tokens.get(index + 1).is_some_and(|next| next == "-"))
            || (token == "/" && tokens.get(index + 1).is_some_and(|next| next == "*"))
        {
            return Err(denied());
        }
        if identifier(&word)
            && tokens.get(index + 1).is_some_and(|next| next == "(")
            && ![
                "COUNT", "SUM", "MIN", "MAX", "LENGTH", "COALESCE", "IN", "NOT", "AND", "OR",
            ]
            .contains(&word.as_str())
        {
            return Err(denied());
        }
    }
    if selects != 1 || !reads_one_source(&tokens) {
        return Err(denied());
    }
    Ok(())
}
fn reads_one_source(tokens: &[String]) -> bool {
    let Some(start) = tokens
        .iter()
        .position(|token| token.eq_ignore_ascii_case("FROM"))
    else {
        return true;
    };
    if start + 1 >= tokens.len() {
        return false;
    }
    tokens
        .get(start + 2..)
        .unwrap_or(&[])
        .iter()
        .take_while(|token| {
            !["WHERE", "ORDER", "GROUP", "LIMIT"]
                .iter()
                .any(|word| token.eq_ignore_ascii_case(word))
        })
        .all(|token| token != "," && token != "(")
}

struct Pooled {
    path: Option<PathBuf>,
    idle: Vec<sqlite::Connection>,
    out: usize,
    closed: bool,
}
pub(crate) struct ReadPool {
    permits: Arc<tokio::sync::Semaphore>,
    state: Mutex<Pooled>,
    out: rocky_platform::task::Held,
}
impl ReadPool {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            permits: Arc::new(tokio::sync::Semaphore::new(POOL_CONNECTIONS)),
            state: Mutex::new(Pooled {
                path: None,
                idle: Vec::new(),
                out: 0,
                closed: false,
            }),
            out: rocky_platform::task::Held::new(false),
        })
    }
    pub(crate) fn attach(&self, path: PathBuf) {
        if let Ok(mut state) = self.state.lock() {
            state.path = Some(path);
        }
    }
    pub(crate) fn close(&self) {
        self.permits.close();
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.out.wait_released(storage::READ_TRANSACTION_BOUND);
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .idle
            .clear();
    }
    fn checkout(&self) -> Result<sqlite::Connection, Error> {
        let mut state = self.state.lock().map_err(|_| Error::Closed)?;
        if state.closed {
            return Err(Error::Closed);
        }
        let connection = match state.idle.pop() {
            Some(connection) => connection,
            None => {
                let path = state.path.as_ref().ok_or(Error::Closed)?;
                let connection = sqlite::Connection::open_read_only(path)?;
                connection.set_query_only()?;
                connection
            }
        };
        state.out += 1;
        self.out.set(true);
        Ok(connection)
    }
    fn checkin(&self, connection: sqlite::Connection, reuse: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.out -= 1;
        if reuse && !state.closed {
            state.idle.push(connection);
        }
        self.out.set(state.out > 0);
    }
    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        read: impl FnOnce(&sqlite::Connection) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Closed)?;
        let pool = self.clone();
        rocky_platform::task::spawn_blocking(move || {
            let out = Out {
                connection: Some(pool.checkout()?),
                pool,
                _permit: permit,
            };
            read(out.connection.as_ref().ok_or(Error::Closed)?)
        })
        .await
        .map_err(|_| Error::Closed)?
    }
}
struct Out {
    pool: Arc<ReadPool>,
    connection: Option<sqlite::Connection>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
impl Drop for Out {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            self.pool.checkin(connection, !std::thread::panicking());
        }
    }
}

struct Prepared {
    sql: String,
    tables: Vec<String>,
    binding: Commitment,
    offset: u64,
    hidden: Option<usize>,
    seek: Vec<SqlValue>,
}
type Versions = (u64, u64);
#[derive(Default)]
pub(crate) struct SharedRead {
    latest: tokio::sync::Mutex<Option<(Versions, ReadSnapshot)>>,
}
type SharedKey = (Commitment, usize, usize, Option<String>);
pub(crate) struct Reads {
    scope: Scope,
    plan: Arc<ReadPlan>,
    pool: Arc<ReadPool>,
    views: Vec<watch::Receiver<u64>>,
    meta: watch::Receiver<u64>,
    shared: Mutex<HashMap<SharedKey, Weak<SharedRead>>>,
    audit: crate::checkpoint::RowAudit,
    logical: std::sync::OnceLock<Arc<rocky_core::LogicalSchema>>,
    events: std::sync::OnceLock<Weak<crate::observe::TableEvents>>,
    projection: std::sync::OnceLock<Arc<ProjectionReads>>,
    #[cfg(test)]
    pub(crate) executed: std::sync::atomic::AtomicUsize,
}
fn binding(scope: Scope, query: &BoundQuery) -> Commitment {
    let mut bytes = Vec::new();
    for id in [scope.mesh.0, scope.app.0, scope.domain.0] {
        bytes.extend(id);
    }
    bytes.extend((query.sql.len() as u64).to_le_bytes());
    bytes.extend(query.sql.as_bytes());
    for value in &query.values {
        let (tag, content): (u8, &[u8]) = match value {
            SqlValue::Null => (0, &[]),
            SqlValue::Integer(value) => {
                bytes.push(1);
                bytes.extend(value.to_le_bytes());
                continue;
            }
            SqlValue::Text(text) => (2, text.as_bytes()),
            SqlValue::Blob(blob) => (3, blob),
            SqlValue::Real(value) => {
                bytes.push(4);
                bytes.extend(value.bits().to_le_bytes());
                continue;
            }
        };
        bytes.push(tag);
        bytes.extend((content.len() as u64).to_le_bytes());
        bytes.extend(content);
    }
    rocky_core::commitment("rocky.read.cursor.v1", &bytes)
}
impl Reads {
    pub(crate) fn new(
        scope: Scope,
        plan: Arc<ReadPlan>,
        pool: Arc<ReadPool>,
        views: Vec<watch::Receiver<u64>>,
        meta: watch::Receiver<u64>,
        audit: crate::checkpoint::RowAudit,
    ) -> Self {
        Self {
            scope,
            plan,
            pool,
            views,
            meta,
            shared: Mutex::new(HashMap::new()),
            audit,
            logical: Default::default(),
            events: Default::default(),
            projection: Default::default(),
            #[cfg(test)]
            executed: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    pub(crate) fn logical(&self) -> Option<Arc<rocky_core::LogicalSchema>> {
        self.logical.get().cloned()
    }
    pub(crate) fn bind_events(&self, events: &Arc<crate::observe::TableEvents>) {
        let _ = self.events.set(Arc::downgrade(events));
    }
    pub(crate) fn events(&self) -> Result<Arc<crate::observe::TableEvents>, Error> {
        self.events
            .get()
            .and_then(Weak::upgrade)
            .ok_or(Error::Closed)
    }
    pub(crate) fn bind_projection(&self, statements: ProjectionReads) {
        let _ = self.projection.set(Arc::new(statements));
    }
    pub(crate) async fn projection(self: &Arc<Self>) -> Result<Projected, Error> {
        let statements = self.projection.get().cloned().ok_or(Error::Closed)?;
        let revision = self.plan.revision.clone();
        self.pool
            .run(move |conn| retried(|| project(conn, &statements, &revision)))
            .await
    }
    pub(crate) fn bind_logical(&self, schema: rocky_core::LogicalSchema) {
        let _ = self.logical.set(Arc::new(schema));
    }
    pub(crate) async fn audit_rows(self: &Arc<Self>) -> Result<u64, Error> {
        let audit = self.audit.clone();
        self.pool
            .run(move |conn| {
                let quoted = |name: &str| format!("\"{}\"", name.replace('"', "\"\""));
                conn.set_deadline(None)?;
                conn.run_now("BEGIN")?;
                let result = (|| {
                    let stored = conn.rows(
                        &format!(
                            "SELECT live,shadow FROM {} WHERE id=1",
                            quoted(&audit.marker)
                        ),
                        (),
                    )?;
                    let digest = |column: usize| match stored
                        .first()
                        .and_then(|row| row.get(column))
                    {
                        Some(sqlite::Value::Blob(bytes)) => crate::digest::SetDigest::decode(bytes)
                            .ok_or_else(|| Error::Corrupt("checkpoint state".into())),
                        _ => Err(Error::Corrupt("checkpoint state".into())),
                    };
                    let mut scanned = [crate::digest::SetDigest::default(); 2];
                    for table in &audit.tables {
                        let logical = &table.logical;
                        for (digest, table) in scanned.iter_mut().zip([&table.live, &table.shadow])
                        {
                            conn.scan(
                                &format!("SELECT * FROM {}", quoted(table)),
                                Vec::new(),
                                |row| {
                                    digest.add(&crate::digest::row_hash(logical, row.iter()));
                                    Ok(())
                                },
                            )?;
                        }
                    }
                    if scanned[0] != digest(0)? {
                        return Err(Error::Corrupt(
                            "projection rows differ from their digest".into(),
                        ));
                    }
                    if scanned[1] != digest(1)? {
                        return Err(Error::Corrupt(
                            "checkpoint rows differ from their digest".into(),
                        ));
                    }
                    Ok(scanned[0].count)
                })();
                let ended = conn.run_now("COMMIT");
                let rows = result?;
                ended?;
                Ok(rows)
            })
            .await
    }
    #[cfg(test)]
    pub(crate) fn counters(&self, sql: &str) -> Versions {
        self.versions(self.plan.view(sql))
    }
    fn prepare(&self, query: &BoundQuery, options: &ReadOptions) -> Result<Prepared, Error> {
        if options.scope != self.scope {
            return Err(rocky_core::ProtocolError::ForeignScope.into());
        }
        if options.max_rows == 0
            || options.max_rows > MAX_PAGE_ROWS
            || query.sql.len() > MAX_READ_SQL_BYTES
        {
            return Err(Error::Quota {
                resource: "read query",
                limit: MAX_PAGE_ROWS,
            });
        }
        if options.max_bytes == 0 || options.max_bytes > MAX_READ_BYTES {
            return Err(Error::Quota {
                resource: "read result bytes",
                limit: MAX_READ_BYTES,
            });
        }
        let binding = binding(self.scope, query);
        let (offset, seek) = match &options.cursor {
            Some(cursor) if cursor.binding != binding => return Err(wrong_cursor()),
            Some(cursor) => (cursor.offset, cursor.key.clone()),
            None => (0, None),
        };
        let read = self.plan.rewrite(&query.sql, seek.as_ref().map(Vec::len))?;
        Ok(Prepared {
            sql: match seek {
                None if offset > 0 => skipped_in_engine(&read.sql, offset),
                _ => read.sql,
            },
            tables: read
                .view
                .map_or_else(Vec::new, |view| self.plan.views[view].tables.clone()),
            binding,
            offset,
            hidden: read.hidden,
            seek: seek.unwrap_or_default(),
        })
    }
    pub(crate) async fn read(
        self: &Arc<Self>,
        query: BoundQuery,
        options: ReadOptions,
    ) -> Result<ReadSnapshot, Error> {
        let prepared = self.prepare(&query, &options)?;
        let reads = self.clone();
        self.pool
            .run(move |conn| {
                let values = [prepared.seek.clone(), query.values].concat();
                retried(|| reads.snapshot(conn, &prepared, &values, &options))
            })
            .await
    }
    pub(crate) async fn explain(
        self: &Arc<Self>,
        query: BoundQuery,
        options: ReadOptions,
    ) -> Result<Vec<String>, Error> {
        let prepared = self.prepare(&query, &options)?;
        self.pool
            .run(move |conn| {
                conn.restrict_reads(Some(prepared.tables))?;
                let plan = conn.rows(
                    &format!("EXPLAIN QUERY PLAN {}", prepared.sql),
                    storage::bindings([prepared.seek, query.values].concat()),
                );
                conn.restrict_reads(None)?;
                plan.map_err(read_error)?
                    .into_iter()
                    .map(|row| match row.into_iter().nth(3) {
                        Some(sqlite::Value::Text(step)) => Ok(step),
                        _ => Err(Error::Corrupt("query plan".into())),
                    })
                    .collect()
            })
            .await
    }
    fn snapshot(
        &self,
        conn: &sqlite::Connection,
        prepared: &Prepared,
        values: &[SqlValue],
        options: &ReadOptions,
    ) -> Result<ReadSnapshot, Error> {
        conn.set_deadline(Some(
            web_time::Instant::now() + storage::READ_TRANSACTION_BOUND,
        ))?;
        let result = (|| {
            conn.run_now("BEGIN")?;
            let result = (|| {
                let single = |sql: &str| -> Result<Vec<SqlValue>, Error> {
                    let row = conn.rows(sql, ())?.into_iter().next();
                    sql_values(row.ok_or_else(|| Error::Corrupt("read metadata".into()))?)
                };
                let revision = match single(&self.plan.revision)?.as_slice() {
                    [SqlValue::Integer(revision)] => LocalRevision::new(*revision as u64)?,
                    _ => return Err(Error::Corrupt("missing revision".into())),
                };
                let pending_operations = match single(&self.plan.pending)?.as_slice() {
                    [SqlValue::Integer(pending)] => *pending as usize,
                    _ => return Err(Error::Corrupt("pending operations".into())),
                };
                conn.restrict_reads(Some(prepared.tables.clone()))?;
                let page = conn.page(
                    &prepared.sql,
                    storage::bindings(values.to_vec()),
                    options.max_rows,
                    options.max_bytes,
                    prepared.hidden.unwrap_or(0),
                );
                conn.restrict_reads(None)?;
                let page = page.map_err(read_error)?;
                if page.oversized {
                    return Err(Error::Quota {
                        resource: "read result bytes",
                        limit: options.max_bytes,
                    });
                }
                let mut rows = page.rows;
                let mut last_ordering = None;
                if let Some(hidden) = prepared.hidden {
                    for row in &mut rows {
                        let visible = row
                            .len()
                            .checked_sub(hidden)
                            .ok_or_else(|| Error::Corrupt("read ordering columns".into()))?;
                        last_ordering = Some(row.split_off(visible));
                    }
                }
                let key = last_ordering.and_then(|ordering| {
                    sql_values(ordering)
                        .ok()
                        .filter(|ordering| seekable_after(ordering))
                });
                let next = if page.more {
                    Some(ReadCursor {
                        binding: prepared.binding,
                        offset: prepared.offset.checked_add(rows.len() as u64).ok_or_else(
                            || Error::Rejected("read cursor offset overflows".into()),
                        )?,
                        key,
                    })
                } else {
                    None
                };
                Ok(ReadSnapshot {
                    rows: rows.into_iter().map(sql_values).collect::<Result<_, _>>()?,
                    scope: self.scope,
                    revision,
                    completeness: Completeness::Local { pending_operations },
                    next,
                })
            })();
            end_read_transaction(conn)?;
            result
        })();
        conn.set_deadline(None)?;
        result
    }
    fn versions(&self, view: Option<usize>) -> Versions {
        (
            view.and_then(|view| self.views.get(view))
                .map_or(0, |view| *view.borrow()),
            *self.meta.borrow(),
        )
    }
    pub(crate) fn subscribe(
        &self,
        query: &BoundQuery,
    ) -> (Option<watch::Receiver<u64>>, watch::Receiver<u64>) {
        let view = self.plan.view(&query.sql);
        (
            view.and_then(|view| self.views.get(view)).cloned(),
            self.meta.clone(),
        )
    }
    pub(crate) fn share(&self, query: &BoundQuery, options: &ReadOptions) -> Arc<SharedRead> {
        let key = (
            binding(self.scope, query),
            options.max_rows,
            options.max_bytes,
            options.cursor.as_ref().map(ReadCursor::encode),
        );
        let Ok(mut shared) = self.shared.lock() else {
            return Arc::default();
        };
        if let Some(existing) = shared.get(&key).and_then(Weak::upgrade) {
            return existing;
        }
        shared.retain(|_, slot| slot.strong_count() > 0);
        let slot = Arc::<SharedRead>::default();
        shared.insert(key, Arc::downgrade(&slot));
        slot
    }
    pub(crate) async fn observed(
        self: &Arc<Self>,
        shared: &SharedRead,
        query: &BoundQuery,
        options: ReadOptions,
    ) -> Result<(Versions, ReadSnapshot), Error> {
        let mut latest = shared.latest.lock().await;
        let sampled_before_read = self.versions(self.plan.view(&query.sql));
        if let Some((as_of, snapshot)) = latest.as_ref()
            && *as_of == sampled_before_read
        {
            return Ok((sampled_before_read, snapshot.clone()));
        }
        let snapshot = self.read(query.clone(), options).await?;
        #[cfg(test)]
        self.executed
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        *latest = Some((sampled_before_read, snapshot.clone()));
        Ok((sampled_before_read, snapshot))
    }
}
pub(crate) struct ProjectionReads {
    pub(crate) query: String,
    pub(crate) applied: String,
    pub(crate) retention: String,
    pub(crate) authors: Vec<rocky_core::NodeId>,
}
pub(crate) struct Projected {
    pub(crate) rows: Vec<Vec<SqlValue>>,
    pub(crate) revision: LocalRevision,
    pub(crate) frontier: rocky_core::AppliedFrontier,
}
fn end_read_transaction(conn: &sqlite::Connection) -> Result<(), Error> {
    if !conn.is_autocommit()? {
        conn.run_now("ROLLBACK")?;
    }
    Ok(())
}
fn skipped_in_engine(sql: &str, offset: u64) -> String {
    format!("SELECT * FROM ({sql}) LIMIT -1 OFFSET {offset}")
}
fn seekable_after(ordering: &[SqlValue]) -> bool {
    !ordering.contains(&SqlValue::Null)
}
fn project(
    conn: &sqlite::Connection,
    statements: &ProjectionReads,
    revision: &str,
) -> Result<Projected, Error> {
    conn.set_deadline(Some(
        web_time::Instant::now() + storage::READ_TRANSACTION_BOUND,
    ))?;
    let result = (|| {
        conn.run_now("BEGIN")?;
        let result = (|| {
            let corrupt = || Error::Corrupt("projection snapshot".into());
            let revision = match conn.rows(revision, ())?.first().map(Vec::as_slice) {
                Some([sqlite::Value::Integer(revision)]) => LocalRevision::new(*revision as u64)?,
                _ => return Err(corrupt()),
            };
            let pruned = match conn
                .rows(&statements.retention, ())?
                .first()
                .map(Vec::as_slice)
            {
                Some([sqlite::Value::Blob(state)]) => {
                    crate::worker::retention::pruned_prefix(state)?
                }
                _ => return Err(corrupt()),
            };
            let mut frontier = std::collections::BTreeMap::new();
            for node in &statements.authors {
                let newest =
                    conn.rows(&statements.applied, [sqlite::Value::Blob(node.0.to_vec())])?;
                let applied = match newest.first().map(Vec::as_slice) {
                    Some([sqlite::Value::Integer(sequence)]) => *sequence as u64,
                    None => 0,
                    _ => return Err(corrupt()),
                };
                let pruned = pruned
                    .iter()
                    .find(|(author, _)| author == node)
                    .map_or(0, |(_, sequence)| *sequence);
                if applied.max(pruned) > 0 {
                    frontier.insert(*node, rocky_core::AuthorSequence::new(applied.max(pruned))?);
                }
            }
            let rows = conn
                .rows(&statements.query, ())?
                .into_iter()
                .map(sql_values)
                .collect::<Result<_, _>>()?;
            Ok(Projected {
                rows,
                revision,
                frontier: rocky_core::AppliedFrontier(frontier.into_iter().collect()),
            })
        })();
        end_read_transaction(conn)?;
        result
    })();
    conn.set_deadline(None)?;
    result
}
fn retried<T>(mut read: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    for attempt in 0..storage::TRANSACTION_ATTEMPTS {
        match read() {
            Err(error) if storage::retryable(&error) => {
                if attempt + 1 < storage::TRANSACTION_ATTEMPTS {
                    rocky_platform::task::backoff_blocking(storage::backoff(attempt));
                }
            }
            result => return result,
        }
    }
    Err(Error::RetryExhausted)
}
fn read_error(error: sqlite::Error) -> Error {
    match error {
        sqlite::Error::Denied => denied(),
        error => error.into(),
    }
}
fn sql_values(row: Vec<sqlite::Value>) -> Result<Vec<SqlValue>, Error> {
    row.into_iter().map(storage::sql_value).collect()
}
pub(crate) fn written_table(sql: &str) -> Option<String> {
    let tokens: Vec<_> = tokens(sql)
        .into_iter()
        .filter(|token| !token.trim().is_empty())
        .collect();
    let after = |word: &str| {
        let position = tokens
            .iter()
            .position(|token| token.eq_ignore_ascii_case(word))?;
        tokens.get(position + 1)
    };
    let first = tokens.first()?.to_ascii_uppercase();
    let table = match first.as_str() {
        "INSERT" | "REPLACE" => after("INTO")?,
        "DELETE" => after("FROM")?,
        "UPDATE" => tokens.iter().skip(1).find(|token| {
            !["OR", "ROLLBACK", "ABORT", "REPLACE", "FAIL", "IGNORE"]
                .iter()
                .any(|word| token.eq_ignore_ascii_case(word))
        })?,
        _ => return None,
    };
    Some(unquoted(table).to_ascii_lowercase())
}
#[derive(Default)]
pub(crate) struct Changed {
    views: std::collections::BTreeSet<usize>,
    tables: std::collections::BTreeSet<String>,
    all: bool,
}
impl Changed {
    pub(crate) fn record(&mut self, views: Option<&[usize]>) {
        match views {
            Some(views) => self.views.extend(views),
            None => self.all = true,
        }
    }
    pub(crate) fn table(&mut self, table: &str) {
        let table = table.to_ascii_lowercase();
        if !table.starts_with("rocky_") {
            self.tables.insert(table);
        }
    }
    pub(crate) fn merge(&mut self, other: Self) {
        self.views.extend(other.views);
        self.tables.extend(other.tables);
        self.all |= other.all;
    }
    pub(crate) fn any(&self) -> bool {
        self.all || !self.tables.is_empty() || !self.views.is_empty()
    }
    pub(crate) fn tables(&self) -> Option<&std::collections::BTreeSet<String>> {
        (!self.all).then_some(&self.tables)
    }
    pub(crate) fn publish(&self, views: &[watch::Sender<u64>]) {
        let advance = |view: &watch::Sender<u64>| {
            view.send_modify(|version| *version = version.wrapping_add(1));
        };
        if self.all {
            views.iter().for_each(advance);
        } else {
            self.views
                .iter()
                .filter_map(|index| views.get(*index))
                .for_each(advance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names::new(
            Some("ns"),
            &[
                "CREATE TABLE notes(id INTEGER PRIMARY KEY,owner TEXT,body TEXT)",
                "CREATE TABLE tags(id INTEGER PRIMARY KEY,note INTEGER)",
            ],
        )
    }
    fn view(name: &'static str, sql: &'static str, values: Vec<SqlValue>) -> ReadView {
        ReadView {
            name: name.into(),
            sql: sql.into(),
            values,
            ..ReadView::default()
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn views_inline_their_values_and_map_only_their_own_tables() {
        let plan = ReadPlan::compile(
            &names(),
            vec![view(
                "notes",
                "SELECT id,body FROM notes WHERE owner=? AND body<>?",
                vec![
                    SqlValue::Text("o'brien".into()),
                    SqlValue::Blob(vec![1, 171]),
                ],
            )],
        )
        .unwrap();
        let Rewritten { sql, view, .. } = plan
            .rewrite(
                "SELECT body, 'notes' FROM notes WHERE id=? ORDER BY id",
                None,
            )
            .unwrap();
        assert_eq!(view, Some(0));
        assert_eq!(
            sql,
            "SELECT body, 'notes' FROM (SELECT id,body FROM \"ns_notes\" WHERE owner='o''brien' AND body<>X'01AB') AS \"notes\" WHERE id=? ORDER BY id"
        );
        assert_eq!(plan.views[0].tables, ["ns_notes"]);
        assert_eq!(plan.reading("notes").collect::<Vec<_>>(), [0]);
        assert_eq!(plan.reading("tags").count(), 0);
        let sql = plan.rewrite("SELECT n FROM notes AS n", None).unwrap().sql;
        assert!(
            sql.ends_with(") AS n"),
            "a query's own alias replaces the view's name: {sql}"
        );
        let bare = plan.rewrite("SELECT 1", None).unwrap();
        assert_eq!(
            (bare.sql.as_str(), bare.view),
            ("SELECT 1", None),
            "without a table there is nothing to authorize"
        );
    }
    fn keyed() -> ReadPlan {
        ReadPlan::compile(
            &names(),
            vec![ReadView {
                name: "notes".into(),
                sql: "SELECT id,owner,body FROM notes WHERE owner<>?".into(),
                values: vec![SqlValue::Text("x".into())],
                key: rocky_core::columns(&["id"]),
                not_null: rocky_core::columns(&["owner"]),
            }],
        )
        .unwrap()
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn ordered_plain_reads_continue_by_key_and_other_shapes_by_offset() {
        let plan = keyed();
        let source = "(SELECT id,owner,body FROM \"ns_notes\" WHERE owner<>'x') AS \"notes\"";
        let first = plan
            .rewrite("SELECT body FROM notes WHERE body<>? ORDER BY owner", None)
            .unwrap();
        assert_eq!(
            (first.sql, first.hidden),
            (
                format!("SELECT body ,owner,id FROM {source} WHERE body<>? ORDER BY owner,id"),
                Some(2)
            )
        );
        let next = plan
            .rewrite(
                "SELECT body FROM notes WHERE body<>? ORDER BY owner",
                Some(2),
            )
            .unwrap();
        assert_eq!(
            next.sql,
            format!(
                "SELECT body ,owner,id FROM {source} WHERE (owner,id)>(?,?) AND ( body<>? ) ORDER BY owner,id"
            )
        );
        let descending = plan
            .rewrite("SELECT * FROM notes ORDER BY owner DESC, id DESC", Some(2))
            .unwrap();
        assert_eq!(
            descending.sql,
            format!(
                "SELECT * ,owner,id FROM {source}  WHERE (owner,id)<(?,?) ORDER BY owner DESC, id DESC"
            )
        );
        for (sql, keyset) in [
            ("SELECT * FROM notes ORDER BY id", true),
            (
                "SELECT id, body FROM notes n WHERE id>? ORDER BY \"body\" ASC, id",
                true,
            ),
            ("SELECT body FROM notes AS n ORDER BY id DESC", true),
            ("SELECT body FROM notes ORDER BY owner DESC", true),
            ("SELECT body FROM notes", false),
            ("SELECT body FROM notes WHERE id>?", false),
            ("SELECT body FROM notes ORDER BY id LIMIT 5", false),
            ("SELECT body FROM notes ORDER BY owner, id DESC", false),
            ("SELECT body FROM notes ORDER BY LENGTH(body)", false),
            ("SELECT body FROM notes ORDER BY id, id", false),
            ("SELECT body FROM notes ORDER BY 1", false),
            ("SELECT body FROM notes ORDER BY body COLLATE NOCASE", false),
            ("SELECT id FROM notes ORDER BY body DESC", false),
            ("SELECT body AS id FROM notes ORDER BY id", false),
            ("SELECT body id FROM notes ORDER BY id", false),
            ("SELECT COUNT(*) FROM notes ORDER BY id", false),
            ("SELECT DISTINCT body FROM notes ORDER BY id", false),
            (
                "SELECT owner FROM notes GROUP BY owner ORDER BY owner",
                false,
            ),
            ("SELECT body FROM notes WHERE id>?1 ORDER BY id", false),
        ] {
            let read = plan.rewrite(sql, None).unwrap();
            assert_eq!(read.hidden.is_some(), keyset, "{sql}");
            let values = read.hidden.unwrap_or(1);
            assert_eq!(
                plan.rewrite(sql, Some(values)).is_ok(),
                keyset,
                "a key cursor is accepted exactly where a key can continue: {sql}"
            );
        }
        assert!(plan.rewrite("SELECT 1", Some(1)).is_err());
        assert!(
            plan.rewrite("SELECT * FROM notes ORDER BY owner", Some(1))
                .is_err(),
            "a cursor with another number of values is not this query's"
        );
        let unkeyed = ReadPlan::compile(
            &names(),
            vec![view("notes", "SELECT id FROM notes", vec![])],
        )
        .unwrap();
        let read = unkeyed
            .rewrite("SELECT * FROM notes ORDER BY id", None)
            .unwrap();
        assert_eq!(read.hidden, None);
        for (key, not_null) in [(&["no-name"][..], &[][..]), (&["id"], &["a b"])] {
            let invalid = ReadView {
                key: rocky_core::columns(key),
                not_null: rocky_core::columns(not_null),
                ..view("notes", "SELECT id FROM notes", vec![])
            };
            assert!(ReadPlan::compile(&names(), vec![invalid]).is_err());
        }
    }
    fn notes(dir: &rocky_platform::fs::Scratch) -> (Arc<ReadPool>, Arc<Reads>, Scope) {
        let path = dir.path().join("reads.db");
        let conn = sqlite::Connection::open(&path).unwrap();
        for sql in [
            "PRAGMA journal_mode=wal",
            "CREATE TABLE rocky_replica(id INTEGER PRIMARY KEY,revision INTEGER)",
            "CREATE TABLE rocky_operations(status TEXT)",
            "CREATE TABLE notes(id INTEGER PRIMARY KEY,owner TEXT,body TEXT)",
            "INSERT INTO rocky_replica VALUES(1,7)",
        ] {
            conn.rows(sql, ()).unwrap();
        }
        for id in 1..=9i64 {
            let body = match id % 3 {
                0 => "NULL",
                1 => "'a'",
                _ => "'b'",
            };
            conn.rows(&format!("INSERT INTO notes VALUES({id},'o',{body})"), ())
                .unwrap();
        }
        let schema = ["CREATE TABLE notes(id INTEGER PRIMARY KEY,owner TEXT,body TEXT)"];
        let plan = ReadPlan::compile(
            &Names::new(None, &schema),
            vec![ReadView {
                name: "notes".into(),
                sql: "SELECT id,body FROM notes".into(),
                values: Vec::new(),
                key: rocky_core::columns(&["id"]),
                not_null: rocky_core::columns(&[]),
            }],
        )
        .unwrap();
        let pool = ReadPool::new();
        pool.attach(path);
        let scope = Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        let (_, meta) = watch::channel(0);
        let reads = Arc::new(Reads::new(
            scope,
            Arc::new(plan),
            pool.clone(),
            Vec::new(),
            meta,
            crate::checkpoint::RowAudit {
                marker: "rocky_checkpoint".into(),
                tables: Vec::new(),
            },
        ));
        (pool, reads, scope)
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn pages_through_null_ordering_values_cover_the_result_once() {
        let dir = rocky_platform::fs::Scratch::new().await;
        let (pool, reads, scope) = notes(&dir);
        let query = BoundQuery {
            sql: "SELECT id FROM notes WHERE id<? ORDER BY body".into(),
            values: vec![SqlValue::Integer(100)],
        };
        let (mut ids, mut kinds) = (Vec::new(), Vec::new());
        let mut options = ReadOptions::new(scope, 2);
        loop {
            let page = reads.read(query.clone(), options.clone()).await.unwrap();
            assert_eq!(page.revision.get(), 7);
            assert!(
                page.rows.iter().all(|row| row.len() == 1),
                "hidden ordering columns are not returned"
            );
            ids.extend(page.rows.into_iter().flatten());
            let Some(cursor) = page.next else { break };
            kinds.push(cursor.kind());
            assert_eq!(ReadCursor::decode(&cursor.encode()).unwrap(), cursor);
            options = options.after(cursor);
        }
        assert_eq!(
            ids,
            [3, 6, 9, 1, 4, 7, 2, 5, 8].map(SqlValue::Integer).to_vec()
        );
        use crate::CursorKind::{Keyset, Offset};
        assert_eq!(kinds, [Offset, Keyset, Keyset, Keyset]);
        pool.close();
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn a_cursor_offset_past_the_largest_is_refused() {
        let dir = rocky_platform::fs::Scratch::new().await;
        let (pool, reads, scope) = notes(&dir);
        let query = BoundQuery {
            sql: "SELECT id FROM notes ORDER BY id".into(),
            values: Vec::new(),
        };
        let options = ReadOptions::new(scope, 2);
        let page = reads.read(query.clone(), options.clone()).await.unwrap();
        let mut cursor = page.next.unwrap();
        assert_eq!(cursor.kind(), crate::CursorKind::Keyset);
        cursor.offset = u64::MAX;
        let refused = reads.read(query, options.after(cursor)).await;
        assert!(matches!(refused, Err(Error::Rejected(_))), "{refused:?}");
        pool.close();
    }
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_read_keeps_its_slot_until_its_connection_is_back() {
        let dir = rocky_platform::fs::Scratch::new().await;
        let (pool, _, _) = notes(&dir);
        let (entered, mut running) = tokio::sync::mpsc::unbounded_channel();
        let mut releases = Vec::new();
        let mut callers = Vec::new();
        for _ in 0..POOL_CONNECTIONS {
            let (release, held) = std::sync::mpsc::channel::<()>();
            releases.push(release);
            let (pool, entered) = (pool.clone(), entered.clone());
            callers.push(tokio::spawn(async move {
                pool.run(move |_| {
                    let _ = entered.send(());
                    let _ = held.recv();
                    Ok(())
                })
                .await
            }));
        }
        for _ in 0..POOL_CONNECTIONS {
            running.recv().await.unwrap();
        }
        for caller in callers {
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        }
        assert_eq!(pool.permits.available_permits(), 0);
        assert_eq!(pool.state.lock().unwrap().out, POOL_CONNECTIONS);
        drop(releases);
        pool.close();
        assert_eq!(
            pool.state.lock().unwrap().out,
            0,
            "close waits for the reads, which return every connection and permit"
        );
    }
    #[cfg(not(target_family = "wasm"))]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_panicking_read_returns_its_slot_and_close_finishes() {
        let dir = rocky_platform::fs::Scratch::new().await;
        let (pool, _, _) = notes(&dir);
        let panicked = pool
            .run(|_| -> Result<(), Error> { panic!("an injected read panic") })
            .await;
        assert!(matches!(panicked, Err(Error::Closed)), "{panicked:?}");
        let state = pool.state.lock().unwrap();
        assert_eq!((state.out, state.idle.len()), (0, 0));
        drop(state);
        assert_eq!(pool.permits.available_permits(), POOL_CONNECTIONS);
        pool.close();
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_read_can_name_only_a_view() {
        let plan = ReadPlan::compile(
            &names(),
            vec![view("notes", "SELECT id FROM notes", vec![])],
        )
        .unwrap();
        for sql in [
            "SELECT * FROM tags",
            "SELECT * FROM ns_notes",
            "SELECT * FROM \"ns_notes\"",
            "SELECT * FROM ns_rocky_operations",
            "SELECT * FROM rocky_operations",
            "SELECT * FROM notes, notes",
            "SELECT * FROM (SELECT * FROM notes)",
            "SELECT * FROM notes WHERE id IN (SELECT id FROM tags)",
            "SELECT * FROM notes UNION SELECT * FROM notes",
            "SELECT * FROM main.notes",
        ] {
            assert!(
                matches!(plan.rewrite(sql, None), Err(Error::Rejected(_))),
                "{sql}"
            );
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn views_over_protocol_or_unknown_tables_and_miscounted_values_are_refused() {
        for (sql, values) in [
            ("SELECT * FROM rocky_operations", vec![]),
            ("SELECT * FROM sqlite_schema", vec![]),
            ("SELECT * FROM elsewhere", vec![]),
            ("SELECT id FROM notes JOIN rocky_replica", vec![]),
            ("SELECT id FROM notes WHERE owner=?", vec![]),
            ("SELECT id FROM notes", vec![SqlValue::Integer(1)]),
            (
                "SELECT id FROM notes WHERE owner=?1",
                vec![SqlValue::Integer(1)],
            ),
            ("SELECT id FROM notes; DELETE FROM notes", vec![]),
            ("DELETE FROM notes", vec![]),
        ] {
            assert!(
                matches!(
                    ReadPlan::compile(&names(), vec![view("notes", sql, values)]),
                    Err(Error::Incompatible(_))
                ),
                "{sql}"
            );
        }
        for name in ["rocky_x", "sqlite_x", "no-name", ""] {
            assert!(
                ReadPlan::compile(&names(), vec![view(name, "SELECT id FROM notes", vec![])])
                    .is_err()
            );
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn projection_statements_name_the_table_they_write() {
        for (sql, table) in [
            ("INSERT INTO Notes VALUES(?,?)", Some("notes")),
            (
                "INSERT OR REPLACE INTO \"notes\"(id) VALUES(?)",
                Some("notes"),
            ),
            ("UPDATE OR IGNORE notes SET body=?", Some("notes")),
            ("DELETE FROM tags WHERE id=?", Some("tags")),
            ("SELECT 1", None),
        ] {
            assert_eq!(written_table(sql).as_deref(), table, "{sql}");
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = tokio::test)]
    async fn the_engine_refuses_tables_outside_a_restricted_read() {
        let conn = sqlite::Connection::open_in_memory().unwrap();
        for sql in [
            "CREATE TABLE a(x)",
            "CREATE TABLE b(x)",
            "INSERT INTO a VALUES(1)",
        ] {
            conn.execute(sql, ()).await.unwrap();
        }
        conn.restrict_reads(Some(vec!["a".into()])).unwrap();
        assert_eq!(
            conn.rows("SELECT COUNT(*),MAX(x) FROM a", ())
                .unwrap()
                .len(),
            1
        );
        for sql in [
            "SELECT * FROM b",
            "SELECT (SELECT x FROM b) FROM a",
            "SELECT * FROM sqlite_schema",
            "INSERT INTO a VALUES(2)",
            "DELETE FROM a",
            "PRAGMA table_info(a)",
            "ATTACH ':memory:' AS other",
        ] {
            assert!(
                matches!(conn.rows(sql, ()), Err(sqlite::Error::Denied)),
                "{sql}"
            );
        }
        conn.restrict_reads(None).unwrap();
        assert!(conn.rows("SELECT * FROM b", ()).unwrap().is_empty());
    }
}
