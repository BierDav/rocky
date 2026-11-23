use sha2::{Digest, Sha256};

pub type Commitment = [u8; 32];
pub struct CommitmentBuilder(Sha256);
impl CommitmentBuilder {
    pub fn new(prefix: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update((prefix.len() as u32).to_le_bytes());
        hash.update(prefix.as_bytes());
        Self(hash)
    }
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    pub fn finish(self) -> Commitment {
        self.0.finalize().into()
    }
}
pub fn commitment(prefix: &str, payload: &[u8]) -> Commitment {
    let mut hash = CommitmentBuilder::new(prefix);
    hash.update(payload);
    hash.finish()
}

#[cfg(test)]
mod commitment_tests {
    use super::*;
    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn incremental_commitments_keep_the_domain_and_payload_encoding() {
        let payload = b"abc\0xyz";
        let expected = "a14fc9ddace9459292e1194e88acfbeab9d76584c7438bdd702f9f62380ba618";
        let hex = |bytes: Commitment| {
            bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        assert_eq!(hex(commitment("rocky.hash-test.v1", payload)), expected);
        for size in 1..=payload.len() {
            let mut hash = CommitmentBuilder::new("rocky.hash-test.v1");
            for chunk in payload.chunks(size) {
                hash.update(chunk);
            }
            assert_eq!(hex(hash.finish()), expected);
        }
    }
}
