//! The single write-transaction helper of the encrypted path.
use super::replica::Inner;
use crate::{
    Error,
    storage::{TRANSACTION_ATTEMPTS, backoff, retryable},
};
use rocky_core::RowApplication;

/// Runs `$body`, with `$name` bound to `&mut Inner`, as one `BEGIN IMMEDIATE`
/// transaction that ends through `storage::finish`, then checks the WAL size.
/// The commit is buffered (`synchronous=NORMAL`) and counted for the durable
/// watermark; exposing callers flush with `Inner::flush`.
/// The whole transaction retries with backoff while another connection holds
/// the file and then reports `Error::RetryExhausted`; inside this process the
/// replica lock is the only writer. `@commit` skips the WAL check. A macro
/// because async closures would make the callers' futures lose `Send`.
macro_rules! transaction {
    (@commit $inner:expr, |$name:ident| $body:expr) => {{
        transaction!(@attempt $inner, false, |$name| $body)
    }};
    // Only offer/acknowledgment bookkeeping may preserve committed frontiers.
    (@cache_neutral $inner:expr, |$name:ident| $body:expr) => {{
        let owner = &mut *$inner;
        let result = transaction!(@attempt owner, true, |$name| $body);
        if result.is_ok() { owner.checkpoint_if_due().await; }
        result
    }};
    (@attempt $inner:expr, $neutral:expr, |$name:ident| $body:expr) => {{
        let owner = &mut *$inner;
        let mut attempt = 0;
        loop {
            if !$neutral { owner.invalidate_frontiers(); }
            let result = match owner.conn.execute("BEGIN IMMEDIATE", ()).await {
                Ok(_) => {
                    owner.untouch();
                    let result: ::core::result::Result<_, $crate::Error> = async {
                        let $name = &mut *owner;
                        let body_result: ::core::result::Result<_, $crate::Error> = async { $body }.await;
                        let value = body_result?;
                        $name.finish_admission_revision().await?;
                        Ok(value)
                    }
                    .await;
                    $crate::storage::finish(&owner.conn, result).await
                }
                Err(error) => Err($crate::Error::from(error)),
            };
            match owner.retry(attempt, result).await {
                Some(result) => break result,
                None => attempt += 1,
            }
        }
    }};
    ($inner:expr, |$name:ident| $body:expr) => {{
        let owner = &mut *$inner;
        let result = transaction!(@commit owner, |$name| $body);
        if result.is_ok() {
            owner.checkpoint_if_due().await;
        }
        result
    }};
}

impl<A: RowApplication> Inner<A> {
    /// Ends one attempt with its final result, or with `None` after the
    /// backoff when the file was busy and attempts remain.
    pub(super) async fn retry<T>(
        &mut self,
        attempt: usize,
        result: Result<T, Error>,
    ) -> Option<Result<T, Error>> {
        if result.is_err() {
            self.invalidate_context();
        }
        match result {
            Err(error) if retryable(&error) => {
                #[cfg(test)]
                if let Some(hook) = self.busy_hook.as_mut() {
                    hook(attempt);
                }
                if attempt + 1 < TRANSACTION_ATTEMPTS {
                    tokio::time::sleep(backoff(attempt)).await;
                    None
                } else {
                    Some(Err(Error::RetryExhausted))
                }
            }
            Ok(value) => {
                self.note_commit().await;
                // Typed tables changed: announce them at the committed revision.
                if self.touched()
                    && let Err(error) = self.publish_revision().await
                {
                    self.invalidate_context();
                    return Some(Err(error));
                }
                Some(Ok(value))
            }
            result => Some(result),
        }
    }
}
