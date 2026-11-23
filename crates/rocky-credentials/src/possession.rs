use crate::{
    Capability, DatabaseContext, Error, PrivateKey, PublicKey, Signature, denied, signing_bytes,
};
use serde::{Deserialize, Serialize};

const LABEL: &str = "rocky.node.possession.v2";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodePossession(Signature);

impl NodePossession {
    pub fn sign(
        node: &PrivateKey,
        identity: PublicKey,
        database: DatabaseContext,
    ) -> Result<Self, Error> {
        let claim = claim(identity, node.public_key(), database)?;
        Ok(Self(node.sign(&claim)))
    }
}

fn claim(
    identity: PublicKey,
    node: PublicKey,
    database: DatabaseContext,
) -> Result<Vec<u8>, Error> {
    signing_bytes(LABEL, &(database, identity, node))
}

pub(crate) fn check(capability: &Capability, identity: PublicKey) -> Result<(), Error> {
    let extended = capability.extended()?;
    let node = capability.subject;
    let proven = match (&extended.possession, extended.database) {
        (Some(NodePossession(signature)), Some(database)) => {
            node.verify(&claim(identity, node, database)?, signature)
        }
        _ => false,
    };
    if proven {
        Ok(())
    } else {
        Err(denied("the node did not prove possession of its key"))
    }
}
