//! Held operation and void headers of one domain, read in validated pages.
//!
//! Each page is its own background acquisition. A scan is valid only while
//! the source it started from is unchanged: the header tables, the
//! authority, the followed tip and the domain's graph. The context epoch is
//! not part of it: every SQL session bumps it, even after a plain commit. A caller acts on the
//! result only in an acquisition that checked the source.
use super::*;
use rocky_core::Scope;
use rocky_credentials::TipGraph;

/// Header rows read per acquisition; provisional until measured.
pub(crate) const HEADER_ROWS: usize = if cfg!(test) { 2 } else { 256 };
/// Refused paged passes before a scan runs in one acquisition.
pub(super) const SCAN_PASSES: usize = 3;
const TABLES: [&str; 3] = ["secure_operations", "secure_voids", "secure_held_frames"];

/// Where a scanned header came from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HeaderSource {
    /// A held operation or void.
    Retained,
    /// A frame held back from application.
    HeldFrame,
}

pub(super) struct HeaderScan {
    domain: Scope,
    writes: Arc<crate::sqlite::Writes>,
    tables: Vec<String>,
    versions: Vec<u64>,
    authority: u64,
    tip: TipId,
    graph: [usize; 6],
    /// The table being read, then the last hash read in it.
    table: usize,
    after: Vec<u8>,
}

fn graph_shape(graph: Option<&TipGraph>) -> [usize; 6] {
    graph.map_or([usize::MAX; 6], |graph| {
        [
            graph.seals.len(),
            graph.openings.len(),
            graph.moves.len(),
            graph.finalities.len(),
            graph.checkpoints.len(),
            graph.revoked.len(),
        ]
    })
}

impl HeaderScan {
    /// Scans held operations and voids, then held frames with `frames`.
    pub(super) fn start<A: RowApplication>(
        inner: &Inner<A>,
        domain: Scope,
        frames: bool,
    ) -> Result<Self, Error> {
        let tables: Vec<String> = TABLES[..if frames { 3 } else { 2 }]
            .iter()
            .map(|table| {
                inner
                    .conn
                    .names()
                    .physical(table)
                    .unwrap_or(table)
                    .to_owned()
            })
            .collect();
        let writes = inner
            .conn
            .connection()
            .watch_writes(tables.iter().map(|table| (table.clone(), true)))?;
        Ok(Self {
            domain,
            versions: tables
                .iter()
                .map(|table| writes.table_count(table))
                .collect(),
            writes,
            tables,
            authority: inner.authority,
            tip: inner.workspace.tip,
            graph: graph_shape(inner.graphs.get(&domain)),
            table: 0,
            after: Vec::new(),
        })
    }
    /// Refuses a scan whose source changed since it started.
    pub(super) fn check<A: RowApplication>(&self, inner: &Inner<A>) -> Result<(), Error> {
        if self.authority != inner.authority
            || self.tip != inner.workspace.tip
            || self.graph != graph_shape(inner.graphs.get(&self.domain))
            || self
                .tables
                .iter()
                .zip(&self.versions)
                .any(|(table, version)| self.writes.table_count(table) != *version)
        {
            return Err(Error::RetryExhausted);
        }
        Ok(())
    }
    /// The next page of the domain's headers after checking the source;
    /// `None` once both tables are read.
    pub(super) async fn page<A: RowApplication>(
        &mut self,
        inner: &Inner<A>,
    ) -> Result<Option<(HeaderSource, Vec<Extensions>)>, Error> {
        self.check(inner)?;
        if self.table == self.tables.len() {
            return Ok(None);
        }
        let (source, column) = match self.table {
            2 => (HeaderSource::HeldFrame, "frame"),
            _ => (HeaderSource::Retained, "header"),
        };
        let table = TABLES[self.table];
        let records = collect_rows(
            inner
                .conn
                .query(
                    &format!(
                        "SELECT hash,{column} FROM {table} WHERE hash>? ORDER BY hash LIMIT ?"
                    ),
                    bindings(vec![
                        SqlValue::Blob(self.after.clone()),
                        SqlValue::Integer(HEADER_ROWS as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        #[cfg(any(test, feature = "qualification"))]
        inner
            .flusher
            .measurements
            .work(super::lock::WorkUnit::DomainHeader, records.len() as u64);
        let short = records.len() < HEADER_ROWS;
        let mut headers = Vec::new();
        for record in records {
            let [SqlValue::Blob(hash), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("domain header".into()));
            };
            let header = match source {
                HeaderSource::Retained => {
                    decode::<p2panda_core::Header<Extensions>>(bytes)?.extensions
                }
                HeaderSource::HeldFrame => decode::<EncryptedFrame>(bytes)?
                    .operation_ref()?
                    .header
                    .extensions
                    .clone(),
            };
            self.after.clone_from(hash);
            if header.scope.domain == self.domain {
                headers.push(header);
            }
        }
        if short {
            self.table += 1;
            self.after.clear();
        }
        Ok(Some((source, headers)))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Folds every held header of `domain` into the state `begin` makes
    /// a page at a time, then finishes in an acquisition that checked the
    /// source. `begin` runs in the acquisition the scan starts in.
    pub(super) async fn fold_headers<S, T>(
        &self,
        domain: Scope,
        begin: impl Fn(&Inner<A>) -> Result<S, Error>,
        visit: impl AsyncFnMut(&mut Inner<A>, &mut S, Vec<Extensions>) -> Result<(), Error>,
        finish: impl AsyncFnOnce(&mut Inner<A>, S) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut visit = visit;
        self.fold_scan(
            domain,
            false,
            begin,
            async |inner, state, _, headers| visit(inner, state, headers).await,
            finish,
        )
        .await
    }
    /// `fold_headers` that also visits held frames when `frames` is set.
    ///
    /// A pass whose source changes restarts. After `SCAN_PASSES` refused
    /// passes, the scan runs once in a single background acquisition, so it
    /// completes under steady writes; like a transition, that step starts
    /// only when no interactive write waits.
    pub(super) async fn fold_scan<S, T>(
        &self,
        domain: Scope,
        frames: bool,
        begin: impl Fn(&Inner<A>) -> Result<S, Error>,
        mut visit: impl AsyncFnMut(
            &mut Inner<A>,
            &mut S,
            HeaderSource,
            Vec<Extensions>,
        ) -> Result<(), Error>,
        finish: impl AsyncFnOnce(&mut Inner<A>, S) -> Result<T, Error>,
    ) -> Result<T, Error> {
        for pass in 0..=SCAN_PASSES {
            match self
                .scan_pass(domain, frames, pass == SCAN_PASSES, &begin, &mut visit)
                .await
            {
                Ok((mut inner, state)) => return finish(&mut inner, state).await,
                Err(Error::RetryExhausted) if pass < SCAN_PASSES => continue,
                Err(error) => return Err(error),
            }
        }
        Err(Error::RetryExhausted)
    }
    async fn scan_pass<S>(
        &self,
        domain: Scope,
        frames: bool,
        single: bool,
        begin: &impl Fn(&Inner<A>) -> Result<S, Error>,
        visit: &mut impl AsyncFnMut(
            &mut Inner<A>,
            &mut S,
            HeaderSource,
            Vec<Extensions>,
        ) -> Result<(), Error>,
    ) -> Result<(lock::Guard<tokio::sync::MutexGuard<'_, Inner<A>>>, S), Error> {
        let mut inner = self.inner.lock(Class::Background).await;
        let mut scan = HeaderScan::start(&inner, domain, frames)?;
        let mut state = begin(&inner)?;
        #[cfg(any(test, feature = "qualification"))]
        if single {
            inner
                .flusher
                .measurements
                .work(lock::WorkUnit::ScanFallback, 1);
        }
        loop {
            if !single {
                drop(inner);
                tokio::task::yield_now().await;
                inner = self.inner.lock(Class::Background).await;
            }
            match scan.page(&inner).await? {
                Some((source, headers)) => visit(&mut inner, &mut state, source, headers).await?,
                None => return Ok((inner, state)),
            }
            #[cfg(test)]
            if !single && let Some((reached, resume)) = inner.header_scan_pause.take() {
                drop(inner);
                let _ = reached.send(());
                let _ = resume.await;
                inner = self.inner.lock(Class::Background).await;
            }
        }
    }

    #[cfg(test)]
    pub(crate) async fn pause_header_scan_for_test(
        &self,
        reached: tokio::sync::oneshot::Sender<()>,
        resume: tokio::sync::oneshot::Receiver<()>,
    ) {
        self.inner.lock(Class::Background).await.header_scan_pause = Some((reached, resume));
    }
}
