use crate::*;
use rocky_core::{Coordinator, SqlValue, StrongHead};

#[derive(Clone, Debug)]
pub struct BoundQuery {
    pub sql: String,
    pub values: Vec<SqlValue>,
}
/// Bounds of one page. A result longer than `max_rows` rows or `max_bytes`
/// bytes ends early and reports a cursor in `ReadSnapshot::next`. What the
/// following page costs depends on the cursor's `CursorKind`.
#[derive(Clone, Debug)]
pub struct ReadOptions {
    pub scope: Scope,
    /// 1 to 1,024.
    pub max_rows: usize,
    /// 1 to 1 MiB; one row larger than this is a quota error.
    pub max_bytes: usize,
    /// `ReadSnapshot::next` of the previous page of the same query.
    pub cursor: Option<ReadCursor>,
}
impl ReadOptions {
    /// The first page, bounded by `max_rows` and 1 MiB.
    pub fn new(scope: Scope, max_rows: usize) -> Self {
        Self {
            scope,
            max_rows,
            max_bytes: MAX_FRAME_BYTES,
            cursor: None,
        }
    }
    /// The page after the one that reported `cursor`.
    pub fn after(self, cursor: ReadCursor) -> Self {
        Self {
            cursor: Some(cursor),
            ..self
        }
    }
}
/// How a cursor continues its query, and so what the next page costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    /// Seeks to the last row read. The next page costs about the same at any
    /// depth, and rows written between pages are neither repeated nor skipped.
    Keyset,
    /// Skips the rows already read. The cost grows with the depth, and rows
    /// inserted or deleted before the position shift later pages.
    Offset,
}
/// Continues one bound query in one scope; any other query rejects it. The
/// runtime issues a keyset cursor when the view declares a key and the query
/// is a plain column list ordered by plain columns in one direction, and an
/// offset cursor otherwise; `kind` tells which. Pages can come from different
/// revisions: each reports its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadCursor {
    pub(crate) binding: rocky_core::Commitment,
    pub(crate) offset: u64,
    /// The ordering values of the last row read.
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
    /// Text form for transports.
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
                // A key never holds NULL; see `read::Reads::snapshot`.
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
    /// Applied local subset, including this retained dependency backlog.
    Local { pending_operations: usize },
    /// Matches the head discovered for this read; later remote commits may exist.
    CoordinatorHead {
        sequence: u64,
        head: rocky_core::Commitment,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSnapshot {
    pub rows: Vec<Vec<SqlValue>>,
    pub scope: Scope,
    pub revision: LocalRevision,
    pub completeness: Completeness,
    pub strong_head: LocalStrongHead,
    /// Set when the result has more rows than this page holds.
    pub next: Option<ReadCursor>,
}
#[derive(Clone, Debug)]
pub struct TypedSnapshot<R> {
    pub rows: Vec<R>,
    pub scope: Scope,
    pub revision: LocalRevision,
    pub completeness: Completeness,
    pub strong_head: LocalStrongHead,
    pub next: Option<ReadCursor>,
}
/// Application-owned decoding; UI code uses this facade without engine values.
pub trait TypedQuery {
    type Row;
    fn query(&self) -> BoundQuery;
    fn decode(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Error>;
}
#[derive(Clone, Debug)]
pub enum StrongRead {
    Ready(ReadSnapshot),
    Pending {
        discovered: StrongHead,
        reason: PendingReason,
    },
    Unavailable,
}
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    /// One page of a bound query over the domain's authorized views, read
    /// from committed stored rows beside the writer. Work that is queued but
    /// not yet committed is not visible.
    pub async fn read(
        &self,
        query: BoundQuery,
        options: ReadOptions,
    ) -> Result<ReadSnapshot, Error> {
        self.inner.reads()?.read(query, options).await
    }
    /// The engine's query plan for `read(query, options)`, one step per line,
    /// to verify that a read uses a declared index.
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
            strong_head: snapshot.strong_head,
            next: snapshot.next,
        })
    }
    /// Registers before its first read. `next` returns again once a table the
    /// query reads, the strong head or the dependency backlog changed.
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
    /// Reconcile retained payloads, then require the exact discovered head.
    /// Transport supplies missing packages through `receive_result`; discovery
    /// alone never upgrades a local snapshot to a strong read.
    pub async fn read_strong(
        &self,
        query: BoundQuery,
        options: ReadOptions,
        coordinator: &dyn Coordinator,
    ) -> Result<StrongRead, Error> {
        let head = match coordinator.head(options.scope).await {
            Ok(head) => head,
            Err(rocky_core::CoordinationError::Unavailable) => return Ok(StrongRead::Unavailable),
            Err(error) => return Err(error.into()),
        };
        self.strong_request(crate::strong::Request::Bind(head.clone()))
            .await?;
        self.reconcile_strong(coordinator).await?;
        let mut snapshot = self.read(query, options).await?;
        if snapshot.strong_head.configuration != Some(head.configuration)
            || snapshot.strong_head.sequence != head.sequence
            || snapshot.strong_head.head != Some(head.head)
        {
            let retained = self.strong_results().await?;
            let held = self.sync_state().await?;
            let missing_input = retained.iter().any(|result| {
                result.package.base.sequence == snapshot.strong_head.sequence
                    && Some(result.package.base.head) == snapshot.strong_head.head
                    && result.package.required.iter().any(|id| !held.holds(*id))
            });
            return Ok(StrongRead::Pending {
                discovered: head,
                reason: if missing_input {
                    PendingReason::MissingInput
                } else {
                    PendingReason::MissingConfirmedPackage
                },
            });
        }
        if matches!(snapshot.completeness, Completeness::Local { pending_operations } if pending_operations > 0)
        {
            return Ok(StrongRead::Pending {
                discovered: head,
                reason: PendingReason::MissingInput,
            });
        }
        snapshot.completeness = Completeness::CoordinatorHead {
            sequence: head.sequence,
            head: head.head,
        };
        Ok(StrongRead::Ready(snapshot))
    }
}
pub struct ReadObserver<R> {
    _budget: tokio::sync::OwnedSemaphorePermit,
    reads: Arc<crate::read::Reads>,
    shared: Arc<crate::read::SharedRead>,
    /// Change counter of the view the query reads; none if it names no view.
    view: Option<watch::Receiver<u64>>,
    /// Advances when completeness or the strong head may have changed.
    meta: watch::Receiver<u64>,
    query: BoundQuery,
    options: ReadOptions,
    delivered: Option<(u64, u64)>,
    row: std::marker::PhantomData<fn() -> R>,
}
impl<R> ReadObserver<R> {
    /// Changes between two calls coalesce into one snapshot. Observers of the
    /// same query share one execution per change.
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
