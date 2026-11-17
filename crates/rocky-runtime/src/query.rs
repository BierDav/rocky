use crate::*;
use rocky_core::SqlValue;

#[derive(Clone, Debug)]
pub struct BoundQuery {
    pub sql: String,
    pub values: Vec<SqlValue>,
}
#[derive(Clone, Debug)]
pub struct ReadOptions {
    pub scope: Scope,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub cursor: Option<ReadCursor>,
}
impl ReadOptions {
    pub fn new(scope: Scope, max_rows: usize) -> Self {
        Self {
            scope,
            max_rows,
            max_bytes: MAX_FRAME_BYTES,
            cursor: None,
        }
    }
    pub fn after(self, cursor: ReadCursor) -> Self {
        Self {
            cursor: Some(cursor),
            ..self
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    Keyset,
    Offset,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadCursor {
    pub(crate) binding: rocky_core::Commitment,
    pub(crate) offset: u64,
    pub(crate) key: Option<Vec<SqlValue>>,
}
fn hex(bytes: &[u8], text: &mut String) {
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
    }
}
fn unhex(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        return None;
    }
    (0..hex.len() / 2)
        .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok())
        .collect()
}
impl ReadCursor {
    pub fn kind(&self) -> CursorKind {
        if self.key.is_some() {
            CursorKind::Keyset
        } else {
            CursorKind::Offset
        }
    }
    pub fn encode(&self) -> String {
        let mut text = format!("{}:", self.offset);
        hex(&self.binding, &mut text);
        for (index, value) in self.key.iter().flatten().enumerate() {
            text.push(if index == 0 { ':' } else { ',' });
            match value {
                SqlValue::Integer(value) => text.push_str(&format!("i{value}")),
                SqlValue::Real(value) => text.push_str(&format!("r{:016x}", value.bits())),
                SqlValue::Text(value) => {
                    text.push('t');
                    hex(value.as_bytes(), &mut text);
                }
                SqlValue::Blob(value) => {
                    text.push('b');
                    hex(value, &mut text);
                }
                SqlValue::Null => text.push('n'),
            }
        }
        text
    }
    pub fn decode(text: &str) -> Result<Self, Error> {
        let invalid = || Error::Rejected("invalid read cursor".into());
        let mut parts = text.splitn(3, ':');
        let offset = parts.next().ok_or_else(invalid)?;
        let binding = parts.next().and_then(unhex).ok_or_else(invalid)?;
        let key = parts
            .next()
            .map(|key| {
                key.split(',')
                    .map(|value| {
                        let content = value.get(1..)?;
                        match value.as_bytes().first()? {
                            b'i' => content.parse().ok().map(SqlValue::Integer),
                            b'r' => u64::from_str_radix(content, 16)
                                .ok()
                                .and_then(rocky_core::Real::from_bits)
                                .map(SqlValue::Real),
                            b't' => String::from_utf8(unhex(content)?).ok().map(SqlValue::Text),
                            b'b' => unhex(content).map(SqlValue::Blob),
                            _ => None,
                        }
                    })
                    .collect::<Option<Vec<_>>>()
                    .filter(|key| key.len() <= crate::read::MAX_KEYSET_COLUMNS)
                    .ok_or_else(invalid)
            })
            .transpose()?;
        Ok(Self {
            binding: binding.try_into().map_err(|_| invalid())?,
            offset: offset.parse().map_err(|_| invalid())?,
            key,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completeness {
    Local { pending_operations: usize },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSnapshot {
    pub rows: Vec<Vec<SqlValue>>,
    pub scope: Scope,
    pub revision: LocalRevision,
    pub completeness: Completeness,
    pub next: Option<ReadCursor>,
}
#[derive(Clone, Debug)]
pub struct TypedSnapshot<R> {
    pub rows: Vec<R>,
    pub scope: Scope,
    pub revision: LocalRevision,
    pub completeness: Completeness,
    pub next: Option<ReadCursor>,
}
pub trait TypedQuery {
    type Row;
    fn query(&self) -> BoundQuery;
    fn decode(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Error>;
}
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub async fn read(
        &self,
        query: BoundQuery,
        options: ReadOptions,
    ) -> Result<ReadSnapshot, Error> {
        self.inner.reads()?.read(query, options).await
    }
    pub async fn explain_read(
        &self,
        query: BoundQuery,
        options: ReadOptions,
    ) -> Result<Vec<String>, Error> {
        self.inner.reads()?.explain(query, options).await
    }
    pub async fn read_typed<Q: TypedQuery>(
        &self,
        query: &Q,
        options: ReadOptions,
    ) -> Result<TypedSnapshot<Q::Row>, Error> {
        let snapshot = self.read(query.query(), options).await?;
        let rows = query.decode(snapshot.rows)?;
        Ok(TypedSnapshot {
            rows,
            scope: snapshot.scope,
            revision: snapshot.revision,
            completeness: snapshot.completeness,
            next: snapshot.next,
        })
    }
    pub fn observe_read(
        &self,
        query: BoundQuery,
        options: ReadOptions,
    ) -> Result<ReadObserver<A::Row>, Error> {
        let budget = self.observer_budget()?;
        let reads = self.inner.reads()?.clone();
        let (view, meta) = reads.subscribe(&query);
        Ok(ReadObserver {
            _budget: budget,
            shared: reads.share(&query, &options),
            reads,
            view,
            meta,
            query,
            options,
            delivered: None,
            row: std::marker::PhantomData,
        })
    }
}
pub struct ReadObserver<R> {
    _budget: tokio::sync::OwnedSemaphorePermit,
    reads: Arc<crate::read::Reads>,
    shared: Arc<crate::read::SharedRead>,
    view: Option<watch::Receiver<u64>>,
    meta: watch::Receiver<u64>,
    query: BoundQuery,
    options: ReadOptions,
    delivered: Option<(u64, u64)>,
    row: std::marker::PhantomData<fn() -> R>,
}
impl<R> ReadObserver<R> {
    pub async fn next(&mut self) -> Result<ReadSnapshot, Error> {
        loop {
            let current = (
                self.view
                    .as_mut()
                    .map_or(0, |view| *view.borrow_and_update()),
                *self.meta.borrow_and_update(),
            );
            if self.delivered != Some(current) {
                break;
            }
            let changed = match self.view.as_mut() {
                Some(view) => tokio::select! {
                    changed = view.changed() => changed,
                    changed = self.meta.changed() => changed,
                },
                None => self.meta.changed().await,
            };
            changed.map_err(|_| Error::Closed)?;
        }
        let (versions, snapshot) = self
            .reads
            .observed(&self.shared, &self.query, self.options.clone())
            .await?;
        self.delivered = Some(versions);
        Ok(snapshot)
    }
}
