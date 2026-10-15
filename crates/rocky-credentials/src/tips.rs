//! Domain-scoped seal, tip-opening and bookmark statements.
use crate::{
    CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet,
    decode_bounded, denied, encode,
};
use rocky_core::{Commitment, Scope, TipId};
use serde::{Deserialize, Serialize};

pub const SEAL_FORMAT: u32 = 1;
pub const TIP_OPENING_FORMAT: u32 = 1;
pub const BOOKMARK_MOVE_FORMAT: u32 = 1;
pub const MAX_BOOKMARK_NAME: usize = 128;

/// Genesis uses the current schema digest; its domain is the graph's scope.
pub fn genesis_tip(schema: Commitment) -> TipId {
    TipId(rocky_core::commitment("rocky.genesis-tip.v1", &schema))
}

/// Last included sequence of one node's author log, ordered by author number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorMark {
    pub author: u32,
    pub sequence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SealAction {
    Continue { schema: Commitment },
    Close,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealStatement {
    pub format: u32,
    pub scope: Scope,
    pub tip: TipId,
    pub frontier: Vec<AuthorMark>,
    pub action: SealAction,
}
impl SealStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != SEAL_FORMAT {
            return Err(denied("unsupported seal format"));
        }
        if self.frontier.len() > 256
            || self
                .frontier
                .windows(2)
                .any(|pair| pair[0].author >= pair[1].author)
            || self.frontier.iter().any(|mark| {
                mark.author == 0
                    || mark.author > crate::MAX_AUTHOR
                    || mark.sequence > i64::MAX as u64
            })
        {
            return Err(denied("invalid seal frontier"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipOpeningStatement {
    pub format: u32,
    pub scope: Scope,
    pub seal: Commitment,
    pub minimum_in_sync: u32,
}
impl TipOpeningStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != TIP_OPENING_FORMAT {
            return Err(denied("unsupported tip opening format"));
        }
        if self.minimum_in_sync == 0 {
            return Err(denied("tip minimum in-sync size must be positive"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkMoveStatement {
    pub format: u32,
    pub scope: Scope,
    pub name: String,
    pub from: Option<TipId>,
    pub to: Option<TipId>,
}
impl BookmarkMoveStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != BOOKMARK_MOVE_FORMAT {
            return Err(denied("unsupported bookmark move format"));
        }
        if self.name.is_empty()
            || self.name.len() > MAX_BOOKMARK_NAME
            || self.name.chars().any(char::is_control)
            || self.from == self.to
        {
            return Err(denied("invalid bookmark move"));
        }
        Ok(())
    }
}

macro_rules! signed_statement {
    ($signed:ident, $statement:ident, $format:ident, $label:literal, $id:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $signed {
            pub statement: $statement,
            endorsement: Endorsement,
        }
        impl $signed {
            pub fn sign(
                statement: $statement,
                owner: &dyn IdentityProvider,
            ) -> Result<Self, Error> {
                Self::endorse(statement, owner, &CredentialChain::default())
            }
            pub fn endorse(
                statement: $statement,
                signer: &dyn IdentityProvider,
                chain: &CredentialChain,
            ) -> Result<Self, Error> {
                statement.validate()?;
                let signed = Self {
                    endorsement: Endorsement::sign(signer, chain, $label, &statement)?,
                    statement,
                };
                signed.to_bytes()?;
                Ok(signed)
            }
            pub fn verify(&self, publisher: PublicKey, graph: &TipGraph) -> Result<(), Error> {
                self.check(publisher, graph, None)
            }
            pub fn verify_current(
                &self,
                publisher: PublicKey,
                graph: &TipGraph,
                revocations: &RevocationSet,
                time: u64,
            ) -> Result<(), Error> {
                self.check(publisher, graph, Some((revocations, time)))
            }
            fn check(
                &self,
                publisher: PublicKey,
                graph: &TipGraph,
                current: Option<(&RevocationSet, u64)>,
            ) -> Result<(), Error> {
                self.statement.validate()?;
                graph.check_scope(self.statement.scope)?;
                let trusted = self.endorsement.verify_tip_statement(
                    publisher,
                    self.statement.scope,
                    $label,
                    &self.statement,
                    current,
                )?;
                self.check_graph(graph, trusted)
            }
            pub fn endorsement(&self) -> &Endorsement {
                &self.endorsement
            }
            pub fn id(&self) -> Result<Commitment, Error> {
                Ok(rocky_core::commitment($id, &self.to_bytes()?))
            }
            pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
                let bytes = encode(self)?;
                if bytes.len() > crate::MAX_CREDENTIAL_BYTES {
                    return Err(denied("tip statement quota"));
                }
                Ok(bytes)
            }
            pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
                #[derive(Deserialize)]
                struct Format {
                    format: u32,
                }
                #[derive(Deserialize)]
                struct Envelope {
                    statement: Format,
                }
                if bytes.len() > crate::MAX_CREDENTIAL_BYTES {
                    return Err(denied("tip statement quota"));
                }
                let envelope: Envelope = p2panda_core::cbor::decode_cbor(bytes)
                    .map_err(|_| denied("invalid tip statement"))?;
                if envelope.statement.format != $format {
                    return Err(denied(concat!("unsupported ", $label, " format")));
                }
                let signed: Self = decode_bounded(bytes, crate::MAX_CREDENTIAL_BYTES)?;
                signed.statement.validate()?;
                Ok(signed)
            }
        }
    };
}
signed_statement!(
    SignedSeal,
    SealStatement,
    SEAL_FORMAT,
    "rocky.seal.v1",
    "rocky.seal.id.v1"
);
signed_statement!(
    SignedTipOpening,
    TipOpeningStatement,
    TIP_OPENING_FORMAT,
    "rocky.tip-opening.v1",
    "rocky.tip-opening.id.v1"
);
signed_statement!(
    SignedBookmarkMove,
    BookmarkMoveStatement,
    BOOKMARK_MOVE_FORMAT,
    "rocky.bookmark-move.v1",
    "rocky.bookmark-move.id.v1"
);

impl SignedSeal {
    fn check_graph(&self, graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        let schema = graph.schema_any(self.statement.tip)?;
        if matches!(self.statement.action, SealAction::Continue { schema: next } if next != schema)
        {
            return Err(denied("a continue seal must keep its parent's schema"));
        }
        Ok(())
    }
    pub fn successor(&self) -> Result<Option<TipId>, Error> {
        match self.statement.action {
            SealAction::Continue { .. } => Ok(Some(TipId(self.id()?))),
            SealAction::Close => Ok(None),
        }
    }
}
impl SignedTipOpening {
    fn check_graph(&self, graph: &TipGraph, trusted: bool) -> Result<(), Error> {
        if !trusted {
            return Err(denied("only the owner or a trusted identity opens tips"));
        }
        graph.seal_by_id(self.statement.seal)?;
        Ok(())
    }
    pub fn tip(&self) -> Result<TipId, Error> {
        Ok(TipId(self.id()?))
    }
}
impl SignedBookmarkMove {
    fn check_graph(&self, graph: &TipGraph, trusted: bool) -> Result<(), Error> {
        if let Some(from) = self.statement.from {
            graph.schema_any(from)?;
        }
        if let Some(to) = self.statement.to {
            graph.schema_any(to)?;
        }
        if !trusted {
            match (self.statement.from, self.statement.to) {
                (Some(from), Some(to)) if graph.continues_any(from, to)? => {}
                _ => {
                    return Err(denied(
                        "an issuer only moves bookmarks forward along continue seals",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Pure graph resolution over already verified statements. Durable admission
/// verifies statements and their dependencies before adding them to this value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TipGraph {
    pub scope: Scope,
    pub genesis_schema: Commitment,
    pub seals: Vec<SignedSeal>,
    pub openings: Vec<SignedTipOpening>,
    pub moves: Vec<SignedBookmarkMove>,
}
impl TipGraph {
    pub fn new(scope: Scope, genesis_schema: Commitment) -> Self {
        Self {
            scope,
            genesis_schema,
            seals: Vec::new(),
            openings: Vec::new(),
            moves: Vec::new(),
        }
    }
    pub fn genesis(&self) -> TipId {
        genesis_tip(self.genesis_schema)
    }
    fn check_scope(&self, scope: Scope) -> Result<(), Error> {
        if scope != self.scope {
            return Err(denied("tip statement names another domain"));
        }
        Ok(())
    }
    /// Concurrent seals have one winner, independent of receipt order.
    pub fn seal(&self, tip: TipId) -> Result<Option<&SignedSeal>, Error> {
        let mut winner: Option<(Commitment, &SignedSeal)> = None;
        for seal in self
            .seals
            .iter()
            .filter(|seal| seal.statement.tip == tip && seal.statement.scope == self.scope)
        {
            let id = seal.id()?;
            if winner.is_none_or(|(current, _)| id < current) {
                winner = Some((id, seal));
            }
        }
        Ok(winner.map(|(_, seal)| seal))
    }
    pub fn seal_by_id(&self, id: Commitment) -> Result<&SignedSeal, Error> {
        for seal in &self.seals {
            if seal.statement.scope == self.scope && seal.id()? == id {
                return Ok(seal);
            }
        }
        Err(denied("unknown seal"))
    }
    /// The schema is inherited from a tip's opening seal. Losing seals
    /// remain inspectable, but their continuations never become active.
    pub fn schema(&self, tip: TipId) -> Result<Commitment, Error> {
        self.schema_inner(tip, true)
    }
    /// Structural lookup for retained statements whose seal later lost a race.
    pub fn schema_any(&self, tip: TipId) -> Result<Commitment, Error> {
        self.schema_inner(tip, false)
    }
    fn schema_inner(&self, tip: TipId, active: bool) -> Result<Commitment, Error> {
        let mut cursor = tip;
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(cursor) {
                return Err(denied("cyclic tip graph"));
            }
            if cursor == self.genesis() {
                return Ok(self.genesis_schema);
            }
            if let Some(seal) = self.seals.iter().find(|seal| {
                seal.statement.scope == self.scope
                    && seal.successor().is_ok_and(|id| id == Some(cursor))
            }) {
                let canonical = self
                    .seal(seal.statement.tip)?
                    .is_some_and(|winner| winner == seal);
                if active && !canonical {
                    return Err(denied("tip belongs to a losing seal"));
                }
                cursor = seal.statement.tip;
                continue;
            }
            let mut parent = None;
            for opening in &self.openings {
                if opening.statement.scope == self.scope && opening.tip()? == cursor {
                    let seal = self.seal_by_id(opening.statement.seal)?;
                    if active && self.seal(seal.statement.tip)? != Some(seal) {
                        return Err(denied("tip opens on a losing seal"));
                    }
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            cursor = parent.ok_or_else(|| denied("unknown tip"))?;
        }
    }
    /// Whether `to` follows one or more canonical continue seals from `from`.
    pub fn continues(&self, from: TipId, to: TipId) -> Result<bool, Error> {
        self.schema(from)?;
        self.schema(to)?;
        let mut cursor = from;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(cursor) {
            let Some(seal) = self.seal(cursor)? else {
                return Ok(false);
            };
            let Some(next) = seal.successor()? else {
                return Ok(false);
            };
            if next == to {
                return Ok(true);
            }
            cursor = next;
        }
        Err(denied("cyclic tip graph"))
    }
    /// Structural forward reach also retains work signed before a seal lost.
    pub fn continues_any(&self, from: TipId, to: TipId) -> Result<bool, Error> {
        self.schema_any(from)?;
        self.schema_any(to)?;
        let mut cursor = to;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(cursor) {
            let mut parent = None;
            for seal in &self.seals {
                if seal.statement.scope == self.scope && seal.successor()? == Some(cursor) {
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            let Some(tip) = parent else { return Ok(false) };
            if tip == from {
                return Ok(true);
            }
            cursor = tip;
        }
        Err(denied("cyclic tip graph"))
    }
    /// A losing forward continuation resolves to the winner's continuation.
    pub fn canonical_tip(&self, tip: TipId) -> Result<TipId, Error> {
        self.schema_any(tip)?;
        let mut path = Vec::new();
        let mut cursor = tip;
        let mut visited = std::collections::BTreeSet::new();
        while self.schema(cursor).is_err() {
            if !visited.insert(cursor) {
                return Err(denied("cyclic tip graph"));
            }
            let mut parent = None;
            for seal in &self.seals {
                if seal.statement.scope == self.scope && seal.successor()? == Some(cursor) {
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            cursor = parent.ok_or_else(|| denied("tip opens on a losing seal"))?;
            path.push(());
        }
        for () in path {
            if let Some(seal) = self.seal(cursor)? {
                if let Some(next) = seal.successor()? {
                    cursor = next;
                }
            }
        }
        Ok(cursor)
    }
    /// A line starts at genesis or at an explicit opening, not at a continue.
    pub fn line_root(&self, tip: TipId) -> Result<TipId, Error> {
        self.schema(tip)?;
        let mut cursor = tip;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(cursor) {
            let mut parent = None;
            for seal in &self.seals {
                if seal.statement.scope == self.scope && seal.successor()? == Some(cursor) {
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            match parent {
                Some(tip) => cursor = tip,
                None => return Ok(cursor),
            }
        }
        Err(denied("cyclic tip graph"))
    }
    /// Resolve conflicts per `(name, from)` to the lower move digest, then
    /// walk from creation. A revisit ends the walk at the revisited tip.
    pub fn bookmark(&self, name: &str) -> Result<Option<TipId>, Error> {
        let mut cursor = None;
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(cursor) {
            let mut winner: Option<(Commitment, &SignedBookmarkMove)> = None;
            for movement in self.moves.iter().filter(|movement| {
                movement.statement.scope == self.scope
                    && movement.statement.name == name
                    && movement.statement.from == cursor
            }) {
                if movement
                    .statement
                    .to
                    .is_some_and(|tip| self.canonical_tip(tip).is_err())
                {
                    continue;
                }
                let id = movement.id()?;
                if winner.is_none_or(|(current, _)| id < current) {
                    winner = Some((id, movement));
                }
            }
            match winner {
                Some((_, movement)) => {
                    cursor = movement
                        .statement
                        .to
                        .map(|tip| self.canonical_tip(tip))
                        .transpose()?
                }
                None => return Ok(cursor),
            }
        }
        Ok(cursor)
    }
}
