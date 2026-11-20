use crate::{
    Capability, DatabaseContext, Error, PersonalBinding, PrivateKey, PublicKey, Signature, denied,
    signing_bytes,
};
use serde::{Deserialize, Serialize};

const LABEL: &str = "rocky.node.possession.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodePossession(Signature);

impl NodePossession {
    pub fn sign(
        node: &PrivateKey,
        identity: PublicKey,
        database: DatabaseContext,
        personal: Option<&PersonalBinding>,
    ) -> Result<Self, Error> {
        let claim = claim(identity, node.public_key(), database, personal)?;
        Ok(Self(node.sign(&claim)))
    }
}

fn claim(
    identity: PublicKey,
    node: PublicKey,
    database: DatabaseContext,
    personal: Option<&PersonalBinding>,
) -> Result<Vec<u8>, Error> {
    signing_bytes(LABEL, &(database, identity, node, personal))
}

pub(crate) fn check(capability: &Capability, identity: PublicKey) -> Result<(), Error> {
    let extended = capability.extended()?;
    let node = capability.subject;
    let proven = match (&extended.possession, extended.database) {
        (Some(NodePossession(signature)), Some(database)) => node.verify(
            &claim(identity, node, database, extended.personal.as_ref())?,
            signature,
        ),
        _ => false,
    };
    match (proven, extended.personal) {
        (true, _) => Ok(()),
        (false, Some(_)) => Err(crate::PersonalError::Possession.into()),
        (false, None) => Err(denied("the node did not prove possession of its key")),
    }
}
