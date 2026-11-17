use super::*;

pub(super) type Authors = BTreeMap<WriterId, PublicKey>;

impl<A: RowApplication> Inner<A> {
    pub(super) fn stamp_author(&self, author: WriterId) -> Result<PublicKey, Error> {
        writer_key(author)
    }
    pub(super) fn keep_stamp_author(
        &self,
        author: WriterId,
        signer: PublicKey,
    ) -> Result<(), Error> {
        if author != writer(&signer) {
            return Err(denied("a stamp writer names another signer"));
        }
        Ok(())
    }

    pub(super) fn carried_authors(
        &self,
        stamps: &BTreeMap<Commitment, Stamp>,
    ) -> Result<Authors, Error> {
        stamps
            .values()
            .map(|stamp| Ok((stamp.author, writer_key(stamp.author)?)))
            .collect()
    }
}
