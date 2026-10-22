use super::*;

impl<A: RowApplication> Inner<A> {
    pub(super) async fn creation_number(&self, hash: Hash, allocate: bool) -> Result<i64, Error> {
        if let Some(id) = self.dictionary().creation_ids.get(&hash).copied() {
            return Ok(id);
        }
        let mut rows = self
            .conn
            .query(
                "SELECT id FROM secure_row_creations WHERE hash=?",
                [hash.as_bytes().to_vec()],
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
                        "INSERT INTO secure_row_creations(hash) VALUES(?)",
                        [hash.as_bytes().to_vec()],
                    )
                    .await?;
                let mut rows = self
                    .conn
                    .query(
                        "SELECT id FROM secure_row_creations WHERE hash=?",
                        [hash.as_bytes().to_vec()],
                    )
                    .await?;
                rows.next()
                    .await?
                    .ok_or_else(|| Error::Corrupt("interned creation frame".into()))?
                    .get(0)?
            }
            None => {
                return Err(Error::Corrupt(
                    "stored creation frame reference missing".into(),
                ));
            }
        };
        let mut dictionary = self.dictionary();
        dictionary.creation_ids.insert(hash, id);
        dictionary.creations.insert(id, hash);
        Ok(id)
    }
    pub(super) async fn creation_hash(&self, id: i64) -> Result<Hash, Error> {
        if let Some(hash) = self.dictionary().creations.get(&id).copied() {
            return Ok(hash);
        }
        let mut rows = self
            .conn
            .query("SELECT hash FROM secure_row_creations WHERE id=?", [id])
            .await?;
        let bytes: Vec<u8> = rows
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("unknown creation frame reference".into()))?
            .get(0)?;
        let hash = checkpoint::hash_of(&bytes)?;
        let mut dictionary = self.dictionary();
        dictionary.creation_ids.insert(hash, id);
        dictionary.creations.insert(id, hash);
        Ok(hash)
    }
}
