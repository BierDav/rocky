use super::replica::Inner;
use crate::{
    Error,
    storage::{TRANSACTION_ATTEMPTS, backoff, retryable},
};
use rocky_core::RowApplication;

macro_rules! transaction {
    (@without_wal_checkpoint $inner:expr, |$name:ident| $body:expr) => {{
        transaction!(@attempt $inner, false, |$name| $body)
    }};
    (@keeping_frontier_cache $inner:expr, |$name:ident| $body:expr) => {{
        let owner = &mut *$inner;
        let result = transaction!(@attempt owner, true, |$name| $body);
        if result.is_ok() { owner.checkpoint_if_due().await; }
        result
    }};
    (@attempt $inner:expr, $keep_frontiers:expr, |$name:ident| $body:expr) => {{
        let owner = &mut *$inner;
        let mut attempt = 0;
        loop {
            if !$keep_frontiers { owner.invalidate_frontiers(); }
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
        let result = transaction!(@without_wal_checkpoint owner, |$name| $body);
        if result.is_ok() {
            owner.checkpoint_if_due().await;
        }
        result
    }};
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn retry<T>(
        &mut self,
        attempt: usize,
        result: Result<T, Error>,
    ) -> Option<Result<T, Error>> {
        if result.is_err() {
            self.invalidate_context();
            self.invalidate_held_refresh();
        }
        match result {
            Err(error) if retryable(&error) => {
                #[cfg(test)]
                if let Some(hook) = self.busy_hook.as_mut() {
                    hook(attempt);
                }
                if attempt + 1 < TRANSACTION_ATTEMPTS {
                    rocky_platform::time::sleep(backoff(attempt)).await;
                    None
                } else {
                    Some(Err(Error::RetryExhausted))
                }
            }
            Ok(value) => {
                self.note_commit().await;
                let typed_tables_changed = self.touched();
                if typed_tables_changed && let Err(error) = self.publish_revision().await {
                    self.invalidate_context();
                    return Some(Err(error));
                }
                Some(Ok(value))
            }
            result => Some(result),
        }
    }
}
