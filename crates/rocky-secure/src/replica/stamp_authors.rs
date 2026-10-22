use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_stamp_authors(domain BLOB NOT NULL,author INTEGER NOT NULL,signer BLOB NOT NULL,PRIMARY KEY(domain,author)) WITHOUT ROWID",
];

pub(super) type Authors = BTreeMap<u32, PublicKey>;
pub(super) type Cache = std::sync::Mutex<HashMap<(rocky_core::Scope, u32), PublicKey>>;

impl<A: RowApplication> Inner<A> {
    pub(super) async fn stamp_author(
        &self,
        domain: rocky_core::Scope,
        author: u32,
    ) -> Result<PublicKey, Error> {
        if let Some(key) = self
            .stamp_authors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(domain, author))
            .copied()
        {
            return Ok(key);
        }
        let mut rows = self
            .conn
            .query(
                "SELECT signer FROM secure_stamp_authors WHERE domain=? AND author=?",
                bindings(vec![
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Integer(i64::from(author)),
                ]),
            )
            .await?;
        let bytes: Vec<u8> = rows
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("stored stamp author is unknown".into()))?
            .get(0)?;
        let key = PublicKey::from_bytes(
            &bytes
                .try_into()
                .map_err(|_| Error::Corrupt("stamp author key".into()))?,
        )
        .map_err(|_| Error::Corrupt("stamp author key".into()))?;
        self.stamp_authors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((domain, author), key);
        Ok(key)
    }

    pub(super) async fn keep_stamp_author(
        &self,
        domain: rocky_core::Scope,
        author: u32,
        signer: PublicKey,
    ) -> Result<(), Error> {
        if !(1..=0x7fff_ffff).contains(&author) {
            return Err(denied("invalid stamp author number"));
        }
        let cached = self
            .stamp_authors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(domain, author))
            .copied();
        if let Some(key) = cached {
            return if key == signer {
                Ok(())
            } else {
                Err(denied("a domain author number names another signer"))
            };
        }
        let blob = encode(&domain)?;
        self.conn.execute("INSERT INTO secure_stamp_authors VALUES(?,?,?) ON CONFLICT(domain,author) DO NOTHING", bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(i64::from(author)), SqlValue::Blob(signer.as_bytes().to_vec())])).await?;
        if self.stamp_author(domain, author).await? != signer {
            return Err(denied("a domain author number names another signer"));
        }
        Ok(())
    }

    pub(super) async fn carried_authors(
        &self,
        domain: rocky_core::Scope,
        stamps: &BTreeMap<Commitment, Stamp>,
    ) -> Result<Authors, Error> {
        let mut authors = Authors::new();
        for author in stamps
            .values()
            .map(|stamp| stamp.author)
            .collect::<BTreeSet<_>>()
        {
            let key = match self
                .node_author
                .as_ref()
                .filter(|node| node.number == author)
            {
                Some(node) => node.key.public_key(),
                None => self.stamp_author(domain, author).await?,
            };
            authors.insert(author, key);
        }
        Ok(authors)
    }
}
