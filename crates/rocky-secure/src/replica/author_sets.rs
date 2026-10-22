//! Signer sets are interned locally and remain part of canonical row commitments.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_row_authors(id INTEGER PRIMARY KEY AUTOINCREMENT,authors BLOB NOT NULL UNIQUE)",
];

#[derive(Default)]
pub(super) struct Cache {
    ids: HashMap<Vec<u8>, i64>,
    sets: HashMap<i64, stamp_authors::Authors>,
}

impl Materialized {
    pub(super) fn trim_authors(&mut self) -> Result<(), Error> {
        let used: BTreeSet<_> = self
            .stamps
            .values()
            .chain(&self.created)
            .map(|stamp| stamp.author)
            .collect();
        if used.iter().any(|author| !self.authors.contains_key(author)) {
            return Err(denied("row stamp signer missing"));
        }
        self.authors.retain(|author, _| used.contains(author));
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    fn author_sets(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.author_sets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(super) async fn intern_author_set(
        &self,
        authors: &stamp_authors::Authors,
        allocate: bool,
    ) -> Result<i64, Error> {
        let bytes = encode(authors)?;
        if let Some(id) = self.author_sets().ids.get(&bytes).copied() {
            return Ok(id);
        }
        let mut rows = self
            .conn
            .query(
                "SELECT id FROM secure_row_authors WHERE authors=?",
                [bytes.clone()],
            )
            .await?;
        let id: Option<i64> = match rows.next().await? {
            Some(row) => Some(row.get(0)?),
            None => None,
        };
        drop(rows);
        let id = match id {
            Some(id) => id,
            None if allocate => {
                self.conn
                    .execute(
                        "INSERT INTO secure_row_authors(authors) VALUES(?)",
                        [bytes.clone()],
                    )
                    .await?;
                let mut rows = self
                    .conn
                    .query(
                        "SELECT id FROM secure_row_authors WHERE authors=?",
                        [bytes.clone()],
                    )
                    .await?;
                rows.next()
                    .await?
                    .ok_or_else(|| Error::Corrupt("interned row signers".into()))?
                    .get(0)?
            }
            None => return Err(Error::Corrupt("stored row signer set missing".into())),
        };
        let mut cache = self.author_sets();
        cache.ids.insert(bytes, id);
        cache.sets.insert(id, authors.clone());
        Ok(id)
    }
    pub(super) async fn stored_author_set(&self, id: i64) -> Result<stamp_authors::Authors, Error> {
        if let Some(authors) = self.author_sets().sets.get(&id).cloned() {
            return Ok(authors);
        }
        let mut rows = self
            .conn
            .query("SELECT authors FROM secure_row_authors WHERE id=?", [id])
            .await?;
        let bytes: Vec<u8> = rows
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("unknown row signer set".into()))?
            .get(0)?;
        let authors: stamp_authors::Authors = decode(&bytes)?;
        let mut cache = self.author_sets();
        cache.ids.insert(bytes, id);
        cache.sets.insert(id, authors.clone());
        Ok(authors)
    }
}
