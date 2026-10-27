//! First references let resumed checkpoint streams send each birth header once.
use super::served::Served;
use super::*;

impl Served {
    pub(super) fn header_view(&self) -> i64 {
        match self {
            Self::Base => 0,
            Self::Kept => 1,
        }
    }
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn index_page_headers(
        &self,
        id: i64,
        served: &Served,
    ) -> Result<BTreeSet<Hash>, Error> {
        let mut first = BTreeMap::new();
        let mut creations = BTreeSet::new();
        let mut after = Vec::new();
        loop {
            let records = collect_rows(self.conn.query(
                format!("SELECT row,fields FROM {} WHERE scope=? AND row>? ORDER BY row LIMIT ?", served.rows()),
                bindings(vec![SqlValue::Integer(id), SqlValue::Blob(after.clone()), SqlValue::Integer(STEP_ROWS as i64)])).await?).await?;
            #[cfg(feature = "qualification")]
            self.flusher
                .measurements
                .work(super::super::lock::WorkUnit::BaseRow, records.len() as u64);
            for record in &records {
                let [SqlValue::Blob(row), SqlValue::Blob(fields)] = record.as_slice() else {
                    return Err(Error::Corrupt("checkpoint header row".into()));
                };
                let record = self.decode_record(fields).await?;
                if let Some(proof) = record.proof {
                    first.entry(proof.header()).or_insert_with(|| row.clone());
                }
                creations.insert(record.creation);
                after = row.clone();
            }
            if records.len() < STEP_ROWS {
                break;
            }
        }
        self.conn
            .execute(
                "DELETE FROM secure_checkpoint_page_headers WHERE scope=? AND view=?",
                [id, served.header_view()],
            )
            .await?;
        for (header, row) in first {
            self.conn
                .execute(
                    "INSERT INTO secure_checkpoint_page_headers VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Integer(served.header_view()),
                        SqlValue::Blob(header.to_vec()),
                        SqlValue::Blob(row),
                    ]),
                )
                .await?;
        }
        Ok(creations)
    }
    pub(super) async fn page_header_needed(
        &self,
        id: i64,
        served: &Served,
        header: Commitment,
        after: Option<Commitment>,
    ) -> Result<bool, Error> {
        let mut rows = self.conn.query("SELECT first_row FROM secure_checkpoint_page_headers WHERE scope=? AND view=? AND header=?", bindings(vec![
            SqlValue::Integer(id), SqlValue::Integer(served.header_view()), SqlValue::Blob(header.to_vec())])).await?;
        let first: Vec<u8> = rows
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("checkpoint header reference missing".into()))?
            .get(0)?;
        Ok(after.is_none_or(|after| first.as_slice() > after.as_slice()))
    }
}
