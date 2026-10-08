//! Bound databases: one SQL session over the tables a `DatabaseBinding`
//! names, in one or several domains of one file.
//! See `docs/runtime/database-bindings.md`.
use crate::{Error, Replica, SqlOptions, SqlSession, TransactionOutcome, sql};
use rocky_core::{Commitment, DatabaseBinding, StoredApplication, TransactionToken};
use std::sync::Arc;

/// A handle on one recorded binding. Two handles of different tenants never
/// share a table, a statement or a transaction.
pub struct Database<A: StoredApplication> {
    binding: Arc<DatabaseBinding>,
    /// Stamps the sessions' statements: the binding and its generation.
    stamp: Commitment,
    /// The bound domains, in scope order.
    members: Vec<Replica<A>>,
}
impl<A: StoredApplication> Clone for Database<A> {
    fn clone(&self) -> Self {
        Self {
            binding: self.binding.clone(),
            stamp: self.stamp,
            members: self.members.clone(),
        }
    }
}
impl<A> Database<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    pub(crate) fn new(
        binding: DatabaseBinding,
        stamp: Commitment,
        members: Vec<Replica<A>>,
    ) -> Self {
        Self {
            binding: Arc::new(binding),
            stamp,
            members,
        }
    }
    pub fn binding(&self) -> &DatabaseBinding {
        &self.binding
    }
    /// The bound domains, in scope order.
    pub fn domains(&self) -> &[Replica<A>] {
        &self.members
    }
    /// Opens a session on the bound tables. Its transaction commits every
    /// domain's rows and operations together, or none of them.
    pub async fn sql_session(&self, options: SqlOptions) -> Result<SqlSession, Error> {
        let first = self.members.first().ok_or(Error::Closed)?;
        SqlSession::open(
            &first.inner.commands,
            options,
            Some(self.binding.clone()),
            self.stamp,
        )
        .await
    }
    /// Observes committed changes of bound tables, by their bound names;
    /// None is every bound table. Register before reading the initial
    /// snapshot. Each change names the domains' revisions after it.
    pub fn observe_tables(
        &self,
        tables: Option<&[&str]>,
    ) -> Result<crate::observe::TableObserver, Error> {
        if let Some(tables) = tables
            && let Some(unknown) = tables.iter().find(|name| {
                !self
                    .binding
                    .tables
                    .iter()
                    .any(|table| table.name.eq_ignore_ascii_case(name))
            })
        {
            return Err(Error::Rejected(format!("{unknown} is not a bound table")));
        }
        let mut observer = crate::observe::TableObserver::new();
        for member in &self.members {
            let labels: Vec<(String, String)> = self
                .binding
                .tables
                .iter()
                .filter(|table| table.scope == member.scope())
                .filter(|table| {
                    tables.is_none_or(|tables| {
                        tables
                            .iter()
                            .any(|name| table.name.eq_ignore_ascii_case(name))
                    })
                })
                .map(|table| (table.table.clone(), table.name.clone()))
                .collect();
            if !labels.is_empty() {
                member.register_tables(&mut observer, Some(&labels))?;
            }
        }
        Ok(observer)
    }
    /// The outcome committed under `token` in every bound domain, or None
    /// if no commit carried it. Answered once a flush covers the commit.
    pub async fn sql_outcome(
        &self,
        token: TransactionToken,
    ) -> Result<Option<TransactionOutcome>, Error> {
        let mut parts = Vec::new();
        for member in &self.members {
            parts.extend(member.sql_outcome(token).await?);
        }
        if parts.is_empty() {
            return Ok(None);
        }
        if parts.len() != self.members.len() {
            return Err(Error::Corrupt(
                "a transaction token is recorded in part of its domains".into(),
            ));
        }
        sql::combine(parts).map(Some)
    }
}
