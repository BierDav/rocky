//! Domain-scoped seal, tip-opening and bookmark statements.
use crate::revocation::{Cut, Head};
use crate::{
    CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet, SealFinality,
    SignedCheckpointRecord, TerminalAck, decode_bounded, denied, encode,
};
use rocky_core::{Commitment, Scope, TipId};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, MutexGuard, PoisonError},
};

pub const SEAL_FORMAT: u32 = 2;
pub const TIP_OPENING_FORMAT: u32 = 3;
pub const BOOKMARK_MOVE_FORMAT: u32 = 2;
pub const GENESIS_LINE_FORMAT: u32 = 2;
pub const MAX_BOOKMARK_NAME: usize = 128;

/// A node's place in its chain of signed tip statements, or of terminal
/// acknowledgments: every one names the hash (ID) of the one before. A
/// root revocation keeps a revoked node's chain up to the heads it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChainLink {
    /// The signing node.
    pub lane: PublicKey,
    pub sequence: u64,
    /// The ID of the lane's previous statement; `None` only at sequence 0.
    pub previous: Option<Commitment>,
    /// A staged schema switch's own branch of the lane, which may never be
    /// published; `None` on the live chain.
    pub stage: Option<Commitment>,
}
impl ChainLink {
    /// The first link of `lane`.
    pub fn first(lane: PublicKey) -> Self {
        Self {
            lane,
            sequence: 0,
            previous: None,
            stage: None,
        }
    }
    /// The link after the statement `id`, which carried this one, on the
    /// same branch.
    pub fn next(&self, id: Commitment) -> Self {
        Self {
            lane: self.lane,
            sequence: self.sequence.saturating_add(1),
            previous: Some(id),
            stage: self.stage,
        }
    }
    /// This link on the branch the staged schema seal `seal` opens, whose
    /// first statement it is.
    pub fn branch(self, seal: &SealStatement) -> Result<Self, Error> {
        Ok(Self {
            stage: Some(Self::stage_of(seal)?),
            ..self
        })
    }
    /// The stage tag of the branch `seal` opens: bound to the seal, so a
    /// node cannot branch its chain without signing a schema seal there.
    pub fn stage_of(seal: &SealStatement) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.stage-branch.v1",
            &encode(seal)?,
        ))
    }
    pub fn validate(&self) -> Result<(), Error> {
        if (self.sequence == 0) != self.previous.is_none() || self.sequence == u64::MAX {
            return Err(denied("invalid chain link"));
        }
        Ok(())
    }
}
/// A statement kind that may open a stage branch of its signer's chain:
/// only a seal does.
pub trait OpensStage {
    /// The stage tag of the branch this statement opens as its first link.
    fn opened_stage(&self) -> Result<Option<Commitment>, Error> {
        Ok(None)
    }
}
impl OpensStage for SealStatement {
    fn opened_stage(&self) -> Result<Option<Commitment>, Error> {
        ChainLink::stage_of(self).map(Some)
    }
}
impl OpensStage for TipOpeningStatement {}
impl OpensStage for BookmarkMoveStatement {}
impl OpensStage for GenesisLineStatement {}

/// Two statements of one node's chain at the same place: a signer whose
/// key signed both, which no honest node does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Equivocation {
    /// The trusted identity the chain belongs to.
    pub identity: PublicKey,
    pub lane: PublicKey,
    pub sequence: u64,
    pub acknowledgment: bool,
}
/// A statement a root revocation's cut left out: past the kept head of
/// its chain, or off that branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PastCut {
    pub id: Commitment,
    pub identity: PublicKey,
    pub link: ChainLink,
    pub acknowledgment: bool,
}

/// A finality record a cut changed: its old ID, the record that replaced
/// it, if any still meets its line's minimum, and the acknowledgments
/// stripped from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrippedFinality {
    pub record: Commitment,
    pub kept: Option<SealFinality>,
    pub acknowledgments: Vec<TerminalAck>,
}

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
/// A position of a tip's coordinated log and the running hash after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogPoint {
    pub position: u64,
    pub running: Commitment,
}
/// What the owner's recovery seal of a stuck tip abandons: the committed
/// entries of the tip's log after `retained` through `replaced`, the exact
/// commit it ends. See `docs/runtime/stuck-recovery.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// The end of the prefix that stays: `retained ≤ replaced`.
    pub retained: LogPoint,
    pub replaced: LogPoint,
}
impl Recovery {
    fn validate(&self) -> Result<(), Error> {
        if self.retained.position > self.replaced.position
            || self.replaced.position >= rocky_core::MAX_COUNTER
            || (self.retained.position == self.replaced.position
                && self.retained.running != self.replaced.running)
        {
            return Err(denied("invalid recovery"));
        }
        Ok(())
    }
    /// Whether the entry at `position` of the sealed tip's log is abandoned.
    pub fn abandons(&self, position: u64) -> bool {
        self.retained.position < position && position <= self.replaced.position
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealStatement {
    pub format: u32,
    pub scope: Scope,
    pub tip: TipId,
    pub frontier: Vec<AuthorMark>,
    pub action: SealAction,
    /// Only the owner's `continue` seal of a stuck tip carries one.
    pub recovery: Option<Recovery>,
}
impl SealStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != SEAL_FORMAT {
            return Err(denied("unsupported seal format"));
        }
        if let Some(recovery) = &self.recovery {
            recovery.validate()?;
            if self.action == SealAction::Close {
                return Err(denied("a recovery seal continues its tip"));
            }
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
    pub schema: Commitment,
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

/// A trusted identity's minimum of distinct trusted nodes whose acknowledgments make
/// a seal on the genesis line final. Several resolve to the largest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenesisLineStatement {
    pub format: u32,
    pub scope: Scope,
    pub minimum_in_sync: u32,
}
impl GenesisLineStatement {
    pub fn validate(&self) -> Result<(), Error> {
        if self.format != GENESIS_LINE_FORMAT {
            return Err(denied("unsupported genesis line format"));
        }
        if self.minimum_in_sync == 0 || self.minimum_in_sync as usize > crate::MAX_FINALITY_ACKS {
            return Err(denied("genesis line minimum out of range"));
        }
        Ok(())
    }
}

macro_rules! signed_statement {
    ($signed:ident, $statement:ident, $format:ident, $label:literal, $id:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub struct $signed {
            pub statement: $statement,
            /// Present exactly when a trusted identity signs.
            pub link: Option<$crate::tips::ChainLink>,
            endorsement: Endorsement,
        }
        impl $signed {
            pub fn sign(
                statement: $statement,
                owner: &dyn IdentityProvider,
            ) -> Result<Self, Error> {
                Self::endorse(statement, owner, &CredentialChain::default())
            }
            /// The owner or an issuer signs; neither has a chain link.
            pub fn endorse(
                statement: $statement,
                signer: &dyn IdentityProvider,
                chain: &CredentialChain,
            ) -> Result<Self, Error> {
                Self::endorse_linked(statement, signer, chain, None)
            }
            /// A trusted identity signs at `link` of its node's chain.
            pub fn endorse_linked(
                statement: $statement,
                signer: &dyn IdentityProvider,
                chain: &CredentialChain,
                link: Option<$crate::tips::ChainLink>,
            ) -> Result<Self, Error> {
                statement.validate()?;
                if let Some(link) = &link {
                    link.validate()?;
                }
                let signed = Self {
                    endorsement: Endorsement::sign(signer, chain, $label, &(&statement, &link))?,
                    statement,
                    link,
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
                if let Some(link) = &self.link {
                    link.validate()?;
                }
                // A cut chain's statements are judged by the cut, not refused;
                // only the identity the cut names signs on that chain.
                let signer = self.endorsement.signer(publisher);
                let current = current.filter(|(revocations, _)| {
                    self.link
                        .is_none_or(|link| revocations.cut(&signer, &link.lane).is_none())
                });
                let trusted = self.endorsement.verify_tip_statement(
                    publisher,
                    self.statement.scope,
                    $label,
                    &(&self.statement, &self.link),
                    current,
                )?;
                let unlinked = self.endorsement.chain.0.is_empty() || !trusted;
                match (unlinked, &self.link) {
                    (true, Some(_)) => {
                        return Err(denied("only a trusted identity's statement is chained"));
                    }
                    (false, None) => {
                        return Err(denied(
                            "a trusted identity's statement names its chain link",
                        ));
                    }
                    (_, Some(link)) => graph.check_predecessor(
                        signer,
                        link,
                        $crate::tips::OpensStage::opened_stage(&self.statement)?,
                    )?,
                    (true, None) => {}
                }
                self.check_graph(graph, trusted)
            }
            pub fn endorsement(&self) -> &Endorsement {
                &self.endorsement
            }
            pub fn id(&self) -> Result<Commitment, Error> {
                #[cfg(test)]
                $crate::tips::tests::count_id();
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
pub(crate) use signed_statement;
signed_statement!(
    SignedSeal,
    SealStatement,
    SEAL_FORMAT,
    "rocky.seal.v2",
    "rocky.seal.id.v2"
);
signed_statement!(
    SignedTipOpening,
    TipOpeningStatement,
    TIP_OPENING_FORMAT,
    "rocky.tip-opening.v3",
    "rocky.tip-opening.id.v3"
);
signed_statement!(
    SignedGenesisLine,
    GenesisLineStatement,
    GENESIS_LINE_FORMAT,
    "rocky.genesis-line.v2",
    "rocky.genesis-line.id.v2"
);
signed_statement!(
    SignedBookmarkMove,
    BookmarkMoveStatement,
    BOOKMARK_MOVE_FORMAT,
    "rocky.bookmark-move.v2",
    "rocky.bookmark-move.id.v2"
);

impl SignedSeal {
    fn check_graph(&self, graph: &TipGraph, _trusted: bool) -> Result<(), Error> {
        if self.statement.recovery.is_some() && !self.endorsement.chain.0.is_empty() {
            return Err(denied("only the owner signs a recovery seal"));
        }
        let schema = graph.schema_any(self.statement.tip)?;
        if self.statement.recovery.is_some()
            && matches!(self.statement.action, SealAction::Continue { schema: next } if next != schema)
        {
            return Err(denied("a recovery seal must keep its parent's schema"));
        }
        Ok(())
    }
    /// The recovery of a seal `owner` signed directly: what a coordinator
    /// checks before ending a stuck log. Devices verify it in the graph.
    pub fn verify_recovery(&self, owner: PublicKey) -> Result<&Recovery, Error> {
        self.statement.validate()?;
        let recovery = self
            .statement
            .recovery
            .as_ref()
            .ok_or_else(|| denied("not a recovery seal"))?;
        if !self.endorsement.chain.0.is_empty() {
            return Err(denied("only the owner signs a recovery seal"));
        }
        if self.link.is_some() {
            return Err(denied("only the owner signs a recovery seal"));
        }
        self.endorsement
            .verify(owner, "rocky.seal.v2", &(&self.statement, &self.link))?;
        Ok(recovery)
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
        graph.seal_by_id(self.statement.seal)?;
        if !trusted && !graph.migration_opening(self, false)? {
            return Err(denied("an issuer only opens migration successors"));
        }
        Ok(())
    }
    pub fn tip(&self) -> Result<TipId, Error> {
        Ok(TipId(self.id()?))
    }
}
impl SignedGenesisLine {
    fn check_graph(&self, _graph: &TipGraph, trusted: bool) -> Result<(), Error> {
        if !trusted {
            return Err(denied(
                "only a trusted identity signs the genesis line's minimum",
            ));
        }
        Ok(())
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
                (Some(from), Some(to)) if graph.succeeds_any(from, to)? => {}
                _ => {
                    return Err(denied(
                        "an issuer only moves bookmarks forward along schema successors",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The IDs of `identity`'s chain through `lane` reachable from `heads`
/// through predecessors: what a cut keeps of that chain.
fn kept(
    chained: &[Chained<'_>],
    identity: &PublicKey,
    lane: &PublicKey,
    heads: &BTreeSet<Head>,
) -> BTreeSet<Commitment> {
    let by_id: BTreeMap<Commitment, &Chained<'_>> = chained
        .iter()
        .filter(|item| item.identity == *identity && item.link.lane == *lane)
        .map(|item| (item.id, item))
        .collect();
    let mut kept = BTreeSet::new();
    for head in heads {
        let mut cursor = Some(head.id);
        while let Some(id) = cursor {
            let Some(item) = by_id.get(&id) else { break };
            if !kept.insert(id) {
                break;
            }
            cursor = item.link.previous;
        }
    }
    kept
}

/// A cut head's walk back through the held items, to judge many items
/// against it without walking again.
struct Walk {
    head: Head,
    /// Each held item from the head back: the lowest sequence so far, and
    /// its predecessor. Ends at the first item without one, or before the
    /// first one not held.
    held: Vec<(u64, Option<Commitment>)>,
}
impl Walk {
    fn new(links: &BTreeMap<Commitment, (PublicKey, ChainLink)>, head: &Head) -> Self {
        let mut held = Vec::new();
        let mut cursor = Some(head.id);
        let mut lowest = u64::MAX;
        // IDs are hashes, so a walk cannot cycle; the bound only stops a hang.
        while let Some(id) = cursor.filter(|_| held.len() <= links.len()) {
            let Some((_, link)) = links.get(&id) else {
                break;
            };
            lowest = lowest.min(link.sequence);
            held.push((lowest, link.previous));
            cursor = link.previous;
        }
        Self { head: *head, held }
    }
    /// Whether the walk, from the head, leads to `id` at `sequence` below
    /// it, or stops at an item not held: undecided until it arrives.
    fn reaches(&self, sequence: u64, id: Commitment) -> bool {
        let next = sequence.saturating_add(1);
        let stop = self
            .held
            .partition_point(|(lowest, previous)| *lowest > next && previous.is_some());
        match self.held.get(stop) {
            Some((_, previous)) => *previous == Some(id),
            None => true,
        }
    }
}
fn walks(
    links: &BTreeMap<Commitment, (PublicKey, ChainLink)>,
    heads: &BTreeSet<Head>,
) -> Vec<Walk> {
    heads.iter().map(|head| Walk::new(links, head)).collect()
}

/// Whether a cut's head `walks` leave out the chain item `id` at `link`
/// for good: at or after a head's place without being it, or before one
/// and off the path the held items lead back along.
fn beyond(walks: &[Walk], link: &ChainLink, id: Commitment) -> bool {
    for walk in walks {
        if walk.head.id == id {
            return false;
        }
        // A head reaches only lower places.
        if link.sequence < walk.head.sequence && walk.reaches(link.sequence, id) {
            return false;
        }
    }
    true
}

/// Chained statements' identities and links by ID: built over the graph's lists as they grow, rebuilt when one
/// changed otherwise. Never compared or serialized.
#[derive(Default)]
struct ChainIndex(Mutex<Places>);
#[derive(Clone, Default)]
struct Places {
    /// Per list, finality records last: how many it indexed, and the last
    /// one's ID.
    built: [(usize, Option<Commitment>); 6],
    /// Each chained statement's identity and link, by ID.
    links: BTreeMap<Commitment, (PublicKey, ChainLink)>,
    /// Each acknowledgment's identity and link, by ID.
    acks: BTreeMap<Commitment, (PublicKey, ChainLink)>,
    /// Each chained statement's ID, by identity and lane.
    lanes: BTreeMap<(PublicKey, PublicKey), BTreeSet<Commitment>>,
    /// What each statement and finality record refers to: its
    /// predecessor, sealed tip, opened seal, a bookmark's tips or a
    /// record's seal.
    refs: BTreeMap<Commitment, Vec<Commitment>>,
    /// The reverse of `refs`.
    referrers: BTreeMap<Commitment, BTreeSet<Commitment>>,
}
impl Places {
    fn refer(&mut self, id: Commitment, refs: Vec<Commitment>) {
        for target in &refs {
            self.referrers.entry(*target).or_default().insert(id);
        }
        self.refs.entry(id).or_default().extend(refs);
    }
}
impl Clone for ChainIndex {
    fn clone(&self) -> Self {
        Self(Mutex::new(self.places().clone()))
    }
}
impl PartialEq for ChainIndex {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for ChainIndex {}
impl std::fmt::Debug for ChainIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChainIndex")
    }
}
impl ChainIndex {
    fn places(&self) -> MutexGuard<'_, Places> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
/// What a graph list's item, with ID `id`, adds to the chain index.
trait Indexed {
    fn index(&self, id: Commitment, places: &mut Places) -> Result<(), Error>;
}
macro_rules! indexed_statement {
    ($($signed:ty => |$s:ident| $refs:expr),* $(,)?) => {$(
        impl Indexed for $signed {
            fn index(&self, id: Commitment, places: &mut Places) -> Result<(), Error> {
                let $s = self;
                let mut refs: Vec<Commitment> = $refs;
                refs.extend(self.link.and_then(|link| link.previous));
                places.refer(id, refs);
                if let (Some(link), Some(signer)) =
                    (self.link, Endorsement::signer_of(&self.endorsement().chain))
                {
                    places.links.insert(id, (signer, link));
                    places.lanes.entry((signer, link.lane)).or_default().insert(id);
                }
                Ok(())
            }
        }
    )*};
}
indexed_statement!(
    SignedSeal => |s| vec![s.statement.tip.0],
    SignedTipOpening => |s| vec![s.statement.seal],
    SignedBookmarkMove => |s| [s.statement.from, s.statement.to]
        .into_iter()
        .flatten()
        .map(|tip| tip.0)
        .collect(),
    SignedGenesisLine => |_s| Vec::new(),
    SignedCheckpointRecord => |s| vec![s.statement.seal],
);
impl Indexed for SealFinality {
    fn index(&self, id: Commitment, places: &mut Places) -> Result<(), Error> {
        places.refer(id, vec![self.seal]);
        for ack in &self.acknowledgments {
            if let Some(identity) = ack.identity() {
                places.acks.insert(ack.id()?, (identity, ack.link));
            }
        }
        Ok(())
    }
}

/// Who signed a tip statement, in the order races resolve by: the owner,
/// then a trusted identity, then an issuer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TipAuthority {
    Issuer,
    Trusted,
    Owner,
}
impl TipAuthority {
    /// Reads the shape of an endorsement admission already verified: no
    /// links is the owner, one owner-direct trusted grant for `scope` a
    /// trusted identity, anything else an issuer.
    pub fn of(endorsement: &Endorsement, scope: Scope) -> Self {
        match endorsement.chain.0.as_slice() {
            [] => Self::Owner,
            [link]
                if link
                    .capability
                    .extended()
                    .is_ok_and(|extended| extended.trusted == Some(scope)) =>
            {
                Self::Trusted
            }
            _ => Self::Issuer,
        }
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
    pub genesis_lines: Vec<SignedGenesisLine>,
    pub finalities: Vec<SealFinality>,
    pub checkpoints: Vec<SignedCheckpointRecord>,
    /// Seals, moves and checkpoint records a revocation ended the signer of, and finality
    /// records it left below their line's minimum: kept, inspectable, and
    /// left out of resolution. See [`Self::exclude_revoked`].
    #[serde(skip)]
    pub revoked: BTreeSet<Commitment>,
    /// Of `revoked`, the statements and acknowledgments a root
    /// revocation's cut left out. See [`Self::exclude_revoked`].
    #[serde(skip)]
    pub past_cut: BTreeSet<PastCut>,
    /// Places of a node's chain that hold two statements.
    #[serde(skip)]
    pub equivocations: BTreeSet<Equivocation>,
    #[serde(skip)]
    index: ChainIndex,
}
/// A chained statement or acknowledgment of a graph: its ID, signer and
/// link, and the chain it was signed through.
struct Chained<'a> {
    id: Commitment,
    /// The trusted identity whose chain it is.
    identity: PublicKey,
    link: ChainLink,
    credentials: &'a CredentialChain,
}
type BookmarkWalk<'a> = (
    Option<TipId>,
    Vec<&'a SignedBookmarkMove>,
    Vec<(TipId, TipId)>,
);
impl TipGraph {
    pub fn new(scope: Scope, genesis_schema: Commitment) -> Self {
        Self {
            scope,
            genesis_schema,
            seals: Vec::new(),
            openings: Vec::new(),
            moves: Vec::new(),
            genesis_lines: Vec::new(),
            finalities: Vec::new(),
            checkpoints: Vec::new(),
            revoked: BTreeSet::new(),
            past_cut: BTreeSet::new(),
            equivocations: BTreeSet::new(),
            index: ChainIndex::default(),
        }
    }
    /// Refuses a chained statement whose predecessor in its lane, signed
    /// by the same identity one place before, the graph lacks, or that
    /// enters a stage branch other than by continuing it or by being the
    /// seal `opens` names its tag.
    pub(crate) fn check_predecessor(
        &self,
        signer: PublicKey,
        link: &ChainLink,
        opens: Option<Commitment>,
    ) -> Result<(), Error> {
        // A stage branch starts at its own schema seal; a statement enters
        // it only by continuing it.
        let enters = |held: Option<Commitment>| {
            link.stage
                .is_none_or(|stage| held == Some(stage) || opens == Some(stage))
        };
        let Some(previous) = link.previous else {
            return match enters(None) {
                true => Ok(()),
                false => Err(denied("a stage branch opens with its own schema seal")),
            };
        };
        // On any branch of the same identity's lane: a stage branches off
        // the live chain, which continues after a published stage.
        let index = self.indexed()?;
        let Some((identity, held)) = index.links.get(&previous) else {
            return Err(denied(
                "a tip statement's predecessor is missing or does not match",
            ));
        };
        if *identity != signer
            || held.lane != link.lane
            || held.sequence.saturating_add(1) != link.sequence
        {
            return Err(denied(
                "a tip statement's predecessor is missing or does not match",
            ));
        }
        if !enters(held.stage) {
            return Err(denied("a stage branch opens with its own schema seal"));
        }
        Ok(())
    }
    /// The chain index, brought up to the graph's lists.
    fn indexed(&self) -> Result<MutexGuard<'_, Places>, Error> {
        let mut index = self.index.places();
        macro_rules! lists {
            ($($n:tt $list:ident),*) => {
                let stale = false $(|| match index.built[$n] {
                    (0, _) => false,
                    (count, last) => self.$list.get(count - 1).map(|signed| signed.id()).transpose()? != last,
                })*;
                if stale {
                    *index = Places::default();
                }
                $(
                    let from = index.built[$n].0;
                    for signed in self.$list.iter().skip(from) {
                        let id = signed.id()?;
                        signed.index(id, &mut index)?;
                        index.built[$n] = (from.max(index.built[$n].0) + 1, Some(id));
                    }
                )*
            };
        }
        lists!(0 seals, 1 openings, 2 moves, 3 genesis_lines, 4 checkpoints, 5 finalities);
        Ok(index)
    }
    /// Every signed statement: a lazy ID, its chain and its link.
    #[allow(clippy::type_complexity)]
    fn signed(
        &self,
    ) -> impl Iterator<
        Item = (
            Box<dyn Fn() -> Result<Commitment, Error> + '_>,
            &CredentialChain,
            Option<ChainLink>,
        ),
    > + '_ {
        macro_rules! of {
            ($list:expr) => {
                $list.iter().map(|signed| {
                    (
                        Box::new(move || signed.id()) as Box<dyn Fn() -> Result<Commitment, Error>>,
                        &signed.endorsement().chain,
                        signed.link,
                    )
                })
            };
        }
        of!(self.seals)
            .chain(of!(self.openings))
            .chain(of!(self.moves))
            .chain(of!(self.genesis_lines))
            .chain(of!(self.checkpoints))
    }
    /// The chained statements, then the acknowledgments of every finality
    /// record, each once.
    fn chained(&self) -> Result<(Vec<Chained<'_>>, Vec<Chained<'_>>), Error> {
        let mut statements = Vec::new();
        for (id, credentials, link) in self.signed() {
            if let (Some(link), Some(identity)) = (link, Endorsement::signer_of(credentials)) {
                statements.push(Chained {
                    id: id()?,
                    identity,
                    link,
                    credentials,
                });
            }
        }
        let mut acknowledgments = Vec::new();
        let mut seen = BTreeSet::new();
        for record in &self.finalities {
            for ack in &record.acknowledgments {
                let id = ack.id()?;
                if let Some(identity) = ack.identity()
                    && seen.insert(id)
                {
                    acknowledgments.push(Chained {
                        id,
                        identity,
                        link: ack.link,
                        credentials: &ack.credentials,
                    });
                }
            }
        }
        Ok((statements, acknowledgments))
    }
    /// Leaves out of resolution every seal, opening, move and checkpoint
    /// record whose signer's chain `revocations` ends; the owner's never
    /// are. A chain a root statement cuts keeps the statements up to the
    /// heads it names and loses the rest; those are `past_cut`. Two
    /// chained statements at one place are an `equivocation`. Returns
    /// whether the left-out set changed, so callers resolve again.
    pub fn exclude_revoked(&mut self, revocations: &RevocationSet) -> Result<bool, Error> {
        let mut revoked = BTreeSet::new();
        let mut past_cut = BTreeSet::new();
        let mut equivocations = BTreeSet::new();
        let (statements, acknowledgments) = self.chained()?;
        let mut kept_statements = BTreeSet::new();
        let mut kept_acks = BTreeSet::new();
        for ((identity, lane), heads) in revocations.cut_lanes() {
            kept_statements.extend(kept(&statements, identity, lane, &heads.statements));
            kept_acks.extend(kept(
                &acknowledgments,
                identity,
                lane,
                &heads.acknowledgments,
            ));
        }
        let mut judge = |chained: &[Chained<'_>], kept: &BTreeSet<Commitment>, ack: bool| {
            let mut places = BTreeMap::new();
            let mut ended = BTreeSet::new();
            for item in chained {
                let place = (
                    item.identity,
                    item.link.lane,
                    item.link.stage,
                    item.link.sequence,
                );
                if let Some(other) = places.insert(place, item.id)
                    && other != item.id
                {
                    equivocations.insert(Equivocation {
                        identity: item.identity,
                        lane: item.link.lane,
                        sequence: item.link.sequence,
                        acknowledgment: ack,
                    });
                }
                let cut = revocations.cut(&item.identity, &item.link.lane).is_some();
                let out = match cut {
                    true => !kept.contains(&item.id),
                    false => revocations.ends_chain(item.credentials)?,
                };
                if out {
                    ended.insert(item.id);
                    if cut {
                        past_cut.insert(PastCut {
                            id: item.id,
                            identity: item.identity,
                            link: item.link,
                            acknowledgment: ack,
                        });
                    }
                }
            }
            Ok::<_, Error>(ended)
        };
        revoked.extend(judge(&statements, &kept_statements, false)?);
        let ended_acks = judge(&acknowledgments, &kept_acks, true)?;
        // Unchained statements: the owner's and issuers'.
        for (id, chain, link) in self.signed() {
            if link.is_none() && revocations.ends_chain(chain)? {
                revoked.insert(id()?);
            }
        }
        // Revoked acknowledgments no longer count toward a seal's finality,
        // nor revoked genesis lines toward its minimum.
        let statements_revoked = revoked.clone();
        for record in &self.finalities {
            let minimum = match self.finality_minimum_in(record, &statements_revoked) {
                Ok(minimum) => minimum,
                Err(_) => continue,
            };
            let mut ended = 0;
            for ack in &record.acknowledgments {
                ended += usize::from(ended_acks.contains(&ack.id()?));
            }
            if record.acknowledgments.len() - ended < minimum {
                revoked.insert(record.id()?);
            }
        }
        let changed = revoked != self.revoked;
        self.revoked = revoked;
        self.past_cut = past_cut;
        self.equivocations = equivocations;
        Ok(changed)
    }
    /// Whether a root statement's cut leaves out, for good, the statement
    /// `id` that `identity` signed at `link`: it is at or after a kept
    /// head's place without being that head, or before one this graph
    /// holds without leading to it. Admission refuses it and holders prune it.
    pub fn beyond_cut(
        &self,
        revocations: &RevocationSet,
        identity: &PublicKey,
        link: &ChainLink,
        id: Commitment,
    ) -> Result<bool, Error> {
        let Some(heads) = revocations.cut(identity, &link.lane) else {
            return Ok(false);
        };
        let index = self.indexed()?;
        Ok(beyond(&walks(&index.links, &heads.statements), link, id))
    }
    /// Whether a root statement's cut leaves out `ack` for good, as
    /// [`Self::beyond_cut`] judges statements against its acknowledgment head.
    pub fn ack_beyond_cut(
        &self,
        revocations: &RevocationSet,
        ack: &TerminalAck,
    ) -> Result<bool, Error> {
        let Some(heads) = ack
            .identity()
            .and_then(|identity| revocations.cut(&identity, &ack.link.lane))
        else {
            return Ok(false);
        };
        let index = self.indexed()?;
        Ok(beyond(
            &walks(&index.acks, &heads.acknowledgments),
            &ack.link,
            ack.id()?,
        ))
    }
    /// `record` without the acknowledgments a cut leaves out for good, and
    /// those, in its order.
    pub fn strip_past_cut(
        &self,
        revocations: &RevocationSet,
        record: &SealFinality,
    ) -> Result<(SealFinality, Vec<TerminalAck>), Error> {
        let mut kept = record.clone();
        let mut stripped = Vec::new();
        kept.acknowledgments.clear();
        for ack in &record.acknowledgments {
            match self.ack_beyond_cut(revocations, ack)? {
                true => stripped.push(ack.clone()),
                false => kept.acknowledgments.push(ack.clone()),
            }
        }
        Ok((kept, stripped))
    }
    /// Whether `record`, judged without verifying its acknowledgments, has
    /// as many as its seal's line needs. A record whose seal or minimum
    /// the graph lacks passes; verification judges it.
    pub fn meets_minimum(&self, record: &SealFinality) -> bool {
        !record.acknowledgments.is_empty()
            && self
                .finality_minimum(record)
                .ok()
                .is_none_or(|minimum| record.acknowledgments.len() >= minimum)
    }
    /// Strips every finality record of the acknowledgments a cut leaves
    /// out for good, dropping a record left below its line's minimum or
    /// equal to another. Returns, per changed record, its old ID, what
    /// replaced it and the stripped acknowledgments.
    pub fn strip_finalities(
        &mut self,
        revocations: &RevocationSet,
    ) -> Result<Vec<StrippedFinality>, Error> {
        let cut = |ack: &TerminalAck| {
            ack.identity()
                .is_some_and(|identity| revocations.cut(&identity, &ack.link.lane).is_some())
        };
        if !self
            .finalities
            .iter()
            .any(|record| record.acknowledgments.iter().any(cut))
        {
            return Ok(Vec::new());
        }
        let mut changed = Vec::new();
        let mut finalities = Vec::with_capacity(self.finalities.len());
        let mut held = BTreeSet::new();
        for record in &self.finalities {
            let (kept, acknowledgments) = self.strip_past_cut(revocations, record)?;
            if acknowledgments.is_empty() {
                if held.insert(record.id()?) {
                    finalities.push(record.clone());
                }
                continue;
            }
            let kept = Some(kept).filter(|kept| self.meets_minimum(kept));
            let replaced = match &kept {
                Some(kept) if held.insert(kept.id()?) => {
                    finalities.push(kept.clone());
                    kept.clone().into()
                }
                _ => None,
            };
            changed.push(StrippedFinality {
                record: record.id()?,
                kept: replaced,
                acknowledgments,
            });
        }
        if !changed.is_empty() {
            self.finalities = finalities;
            *self.index.places() = Places::default();
        }
        Ok(changed)
    }
    /// The statements a cut leaves out for good (see [`Self::beyond_cut`])
    /// that no other statement this graph keeps refers to, whether as a
    /// predecessor, a sealed tip, an opened seal, a bookmark's tip or a
    /// finality record's seal, nor `pinned`, which the holder keeps state
    /// for. A holder deletes them.
    pub fn prunable(
        &self,
        revocations: &RevocationSet,
        pinned: &BTreeSet<Commitment>,
    ) -> Result<BTreeSet<Commitment>, Error> {
        let index = self.indexed()?;
        // Only the chains a cut names hold statements past it.
        let mut prunable = BTreeSet::new();
        for ((identity, lane), heads) in revocations.cut_lanes() {
            let Some(ids) = index.lanes.get(&(*identity, *lane)) else {
                continue;
            };
            let walks = walks(&index.links, &heads.statements);
            for id in ids.iter().filter(|id| !pinned.contains(*id)) {
                if let Some((_, link)) = index.links.get(id)
                    && beyond(&walks, link, *id)
                {
                    prunable.insert(*id);
                }
            }
        }
        // What a kept statement refers to stays, and so on.
        let mut needed: Vec<Commitment> = prunable
            .iter()
            .filter(|id| {
                index
                    .referrers
                    .get(*id)
                    .is_some_and(|by| by.iter().any(|referrer| !prunable.contains(referrer)))
            })
            .copied()
            .collect();
        while let Some(id) = needed.pop() {
            if prunable.remove(&id) {
                needed.extend(
                    index
                        .refs
                        .get(&id)
                        .into_iter()
                        .flatten()
                        .filter(|target| prunable.contains(*target)),
                );
            }
        }
        Ok(prunable)
    }
    /// Deletes `ids` from the graph's statement lists.
    pub fn remove_statements(&mut self, ids: &BTreeSet<Commitment>) -> Result<(), Error> {
        macro_rules! remove {
            ($($list:ident),*) => {$(
                let mut kept = Vec::with_capacity(self.$list.len());
                for signed in std::mem::take(&mut self.$list) {
                    if !ids.contains(&signed.id()?) {
                        kept.push(signed);
                    }
                }
                self.$list = kept;
            )*};
        }
        remove!(seals, openings, moves, genesis_lines, checkpoints);
        *self.index.places() = Places::default();
        Ok(())
    }
    /// The heads this graph holds of `identity`'s chains through `lane`,
    /// for a root statement's cut: every statement no other one follows,
    /// stage branches included, and the latest acknowledgment.
    pub fn heads(&self, identity: &PublicKey, lane: &PublicKey) -> Result<Cut, Error> {
        let (statements, acknowledgments) = self.chained()?;
        let ours: Vec<&Chained<'_>> = statements
            .iter()
            .filter(|item| item.identity == *identity && item.link.lane == *lane)
            .collect();
        let followed: BTreeSet<Commitment> =
            ours.iter().filter_map(|item| item.link.previous).collect();
        let leaves: Vec<Head> = ours
            .iter()
            .filter(|item| !followed.contains(&item.id))
            .map(|item| Head {
                sequence: item.link.sequence,
                id: item.id,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let head = |chained: &[Chained<'_>]| {
            chained
                .iter()
                .filter(|item| item.identity == *identity && item.link.lane == *lane)
                .min_by_key(|item| (Reverse(item.link.sequence), item.id))
                .map(|item| Head {
                    sequence: item.link.sequence,
                    id: item.id,
                })
        };
        Ok(Cut {
            identity: *identity,
            lane: *lane,
            statements: leaves,
            acknowledgments: head(&acknowledgments),
        })
    }
    /// The cut a root proposal keeps of `identity`'s chains through
    /// `lanes`: every leaf of each, as [`Self::heads`]. Over
    /// [`crate::MAX_CUT_HEADS`] it leaves out stage-branch leaves, oldest
    /// first, and returns them; a stage the cut leaves out loses only what
    /// it signed past the live chain. Refuses more chains than a cut names,
    /// or more live leaves (forks) than fit.
    #[allow(clippy::type_complexity)]
    pub fn proposed_cut(
        &self,
        identity: &PublicKey,
        lanes: &BTreeSet<PublicKey>,
    ) -> Result<(Vec<Cut>, Vec<(PublicKey, Head)>), Error> {
        let mut cut = Vec::new();
        for lane in lanes {
            let heads = self.heads(identity, lane)?;
            if !heads.statements.is_empty() || heads.acknowledgments.is_some() {
                cut.push(heads);
            }
        }
        if cut.len() > crate::MAX_CUT_LANES {
            return Err(denied(
                "the revoked identity signed through more nodes than a cut names",
            ));
        }
        let mut staged = Vec::new();
        let index = self.indexed()?;
        for (chain, heads) in cut.iter().enumerate() {
            for head in &heads.statements {
                if index
                    .links
                    .get(&head.id)
                    .is_some_and(|(_, link)| link.stage.is_some())
                {
                    staged.push((*head, chain));
                }
            }
        }
        drop(index);
        let excess = cut
            .iter()
            .map(|heads| heads.statements.len())
            .sum::<usize>()
            .saturating_sub(crate::MAX_CUT_HEADS);
        if excess > staged.len() {
            return Err(denied(
                "a revoked node forked its live chain more often than a cut names",
            ));
        }
        staged.sort();
        let mut dropped = Vec::new();
        for (head, chain) in staged.into_iter().take(excess) {
            if let Some(heads) = cut.get_mut(chain) {
                heads.statements.retain(|kept| *kept != head);
                dropped.push((heads.lane, head));
            }
        }
        Ok((cut, dropped))
    }
    /// The live link `signer` signs its next statement at through its node
    /// `lane`: after the highest place this graph holds on any branch, so no
    /// place repeats, or the first.
    pub fn next_link(&self, signer: &PublicKey, lane: &PublicKey) -> Result<ChainLink, Error> {
        let (statements, _) = self.chained()?;
        Ok(statements
            .iter()
            .filter(|item| item.identity == *signer && item.link.lane == *lane)
            .min_by_key(|item| (Reverse(item.link.sequence), item.id))
            .map_or(ChainLink::first(*lane), |head| ChainLink {
                stage: None,
                ..head.link.next(head.id)
            }))
    }
    /// The link the node `lane` acknowledges at next, after the latest of
    /// its acknowledgments this graph's finality records hold.
    pub fn next_ack_link(&self, lane: &PublicKey) -> Result<ChainLink, Error> {
        let (_, acknowledgments) = self.chained()?;
        Ok(acknowledgments
            .iter()
            .filter(|item| item.link.lane == *lane)
            .min_by_key(|item| (Reverse(item.link.sequence), item.id))
            .map_or(ChainLink::first(*lane), |head| head.link.next(head.id)))
    }
    /// The trusted identities whose chained statements or acknowledgments
    /// the graph holds, each with the latest `not_before` of the root's
    /// trusted grant its credentials carry.
    pub fn trusted_grants(&self) -> Result<BTreeMap<PublicKey, u64>, Error> {
        let (statements, acknowledgments) = self.chained()?;
        let mut grants = BTreeMap::new();
        for item in statements.iter().chain(&acknowledgments) {
            let Some(grant) = item.credentials.0.first().map(|link| &link.capability) else {
                continue;
            };
            if grant.subject == item.identity && grant.extended()?.trusted == Some(self.scope) {
                let since = grants.entry(item.identity).or_insert(grant.not_before);
                *since = (*since).max(grant.not_before);
            }
        }
        Ok(grants)
    }
    /// Every identity a chained statement or acknowledgment names.
    pub fn chain_identities(&self) -> Result<BTreeSet<PublicKey>, Error> {
        let (statements, acknowledgments) = self.chained()?;
        Ok(statements
            .iter()
            .chain(&acknowledgments)
            .map(|item| item.identity)
            .collect())
    }
    /// The lanes `identity` signed chained statements or acknowledgments
    /// through.
    pub fn lanes_of(&self, identity: &PublicKey) -> Result<BTreeSet<PublicKey>, Error> {
        let (statements, acknowledgments) = self.chained()?;
        Ok(statements
            .iter()
            .chain(&acknowledgments)
            .filter(|item| item.identity == *identity)
            .map(|item| item.link.lane)
            .collect())
    }
    /// The order races resolve by: higher authority first, then the lower
    /// digest. Smaller wins.
    fn precedence(
        &self,
        endorsement: &Endorsement,
        id: Commitment,
    ) -> (Reverse<TipAuthority>, Commitment) {
        (Reverse(TipAuthority::of(endorsement, self.scope)), id)
    }
    /// Whether a finality record that revocation left whole names `seal`.
    /// Admission verified the record against its line's minimum.
    pub fn is_final(&self, seal: Commitment) -> Result<bool, Error> {
        for record in &self.finalities {
            if record.seal == seal && !self.revoked.contains(&record.id()?) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// The checkpoints listed at `seal` that no revocation ended.
    pub fn checkpoints_at(&self, seal: Commitment) -> Result<Vec<&SignedCheckpointRecord>, Error> {
        let mut listed = Vec::new();
        for record in &self.checkpoints {
            if record.statement.seal == seal && !self.revoked.contains(&record.id()?) {
                listed.push(record);
            }
        }
        Ok(listed)
    }
    /// The minimum of distinct trusted nodes a seal of `tip` needs to be
    /// final: its line's opening's, or on the genesis line
    /// [`Self::genesis_minimum`]'s.
    pub fn line_minimum(&self, tip: TipId) -> Result<Option<usize>, Error> {
        self.line_minimum_in(tip, &self.revoked)
    }
    fn line_minimum_in(
        &self,
        tip: TipId,
        revoked: &BTreeSet<Commitment>,
    ) -> Result<Option<usize>, Error> {
        let root = self.line_root_any(tip)?;
        if root == self.genesis() {
            return self.genesis_minimum(revoked);
        }
        for opening in &self.openings {
            if opening.statement.scope == self.scope && opening.tip()? == root {
                return Ok(Some(opening.statement.minimum_in_sync as usize));
            }
        }
        Err(denied("unknown tip"))
    }
    /// The genesis line's minimum, without `revoked` lines: the largest
    /// the owner signed, so the root can lower a raise without revoking
    /// anyone, or while it signed none the largest a trusted identity
    /// signed, so no single trusted node lowers it; none without either.
    fn genesis_minimum(&self, revoked: &BTreeSet<Commitment>) -> Result<Option<usize>, Error> {
        let (mut owner, mut trusted) = (None, None);
        for line in &self.genesis_lines {
            if line.statement.scope != self.scope
                || (!revoked.is_empty() && revoked.contains(&line.id()?))
            {
                continue;
            }
            let largest = match TipAuthority::of(line.endorsement(), self.scope) {
                TipAuthority::Owner => &mut owner,
                _ => &mut trusted,
            };
            *largest = (*largest).max(Some(line.statement.minimum_in_sync as usize));
        }
        Ok(owner.or(trusted))
    }
    fn finality_minimum(&self, record: &SealFinality) -> Result<usize, Error> {
        self.finality_minimum_in(record, &self.revoked)
    }
    fn finality_minimum_in(
        &self,
        record: &SealFinality,
        revoked: &BTreeSet<Commitment>,
    ) -> Result<usize, Error> {
        let seal = self.seal_by_id(record.seal)?;
        self.line_minimum_in(seal.statement.tip, revoked)?
            .ok_or_else(|| denied("the genesis line has no signed minimum"))
    }
    /// Verifies a finality record against the graph: it names a held seal,
    /// its acknowledgments name that seal's tip and continuation, and
    /// enough distinct trusted nodes signed them.
    pub fn verify_finality(
        &self,
        record: &SealFinality,
        publisher: PublicKey,
        current: Option<(&RevocationSet, u64)>,
    ) -> Result<(), Error> {
        record.validate()?;
        self.check_scope(record.scope)?;
        let seal = self.seal_by_id(record.seal)?;
        if record.terminal() != Some((seal.statement.tip, seal.successor()?)) {
            return Err(denied("seal finality names another terminal"));
        }
        if let Some(recovery) = seal.statement.recovery {
            let position = recovery.replaced.position + 1;
            let running =
                rocky_core::coordinated_chain(recovery.replaced.running, position, record.seal);
            if record
                .acknowledgments
                .iter()
                .any(|ack| ack.statement.position != position || ack.statement.running != running)
            {
                return Err(denied("recovery finality names another replaced head"));
            }
        }
        if record.verified_nodes(publisher, current)? < self.finality_minimum(record)? {
            return Err(denied("seal finality below its line's minimum"));
        }
        Ok(())
    }
    pub fn genesis(&self) -> TipId {
        genesis_tip(self.genesis_schema)
    }
    pub(crate) fn check_scope(&self, scope: Scope) -> Result<(), Error> {
        if scope != self.scope {
            return Err(denied("tip statement names another domain"));
        }
        Ok(())
    }
    /// Concurrent seals have one winner, independent of receipt order: a
    /// final seal, then the highest authority, then the lower digest.
    /// Revoked seals never win.
    pub fn seal(&self, tip: TipId) -> Result<Option<&SignedSeal>, Error> {
        type Key = (Reverse<bool>, (Reverse<TipAuthority>, Commitment));
        let mut winner: Option<(Key, &SignedSeal)> = None;
        for seal in self
            .seals
            .iter()
            .filter(|seal| seal.statement.tip == tip && seal.statement.scope == self.scope)
        {
            let id = seal.id()?;
            if self.revoked.contains(&id) {
                continue;
            }
            let key = (
                Reverse(self.is_final(id)?),
                self.precedence(seal.endorsement(), id),
            );
            if winner.as_ref().is_none_or(|(current, _)| key < *current) {
                winner = Some((key, seal));
            }
        }
        Ok(winner.map(|(_, seal)| seal))
    }
    /// Whether the winning seal of `tip` is the owner's recovery and
    /// abandons the entry at `position` of the tip's coordinated log.
    pub fn abandons(&self, tip: TipId, position: u64) -> Result<bool, Error> {
        Ok(self.seal(tip)?.is_some_and(|seal| {
            seal.statement
                .recovery
                .is_some_and(|recovery| recovery.abandons(position))
        }))
    }
    /// The authority a statement was signed with.
    pub fn authority(&self, endorsement: &Endorsement) -> TipAuthority {
        TipAuthority::of(endorsement, self.scope)
    }
    pub fn seal_by_id(&self, id: Commitment) -> Result<&SignedSeal, Error> {
        for seal in &self.seals {
            if seal.statement.scope == self.scope && seal.id()? == id {
                return Ok(seal);
            }
        }
        Err(denied("unknown seal"))
    }
    /// Each tip commits its schema. Losing seals
    /// remain inspectable, but their continuations never become active.
    pub fn schema(&self, tip: TipId) -> Result<Commitment, Error> {
        self.schema_inner(tip, true)
    }
    /// Structural lookup for retained statements whose seal later lost a race.
    pub fn schema_any(&self, tip: TipId) -> Result<Commitment, Error> {
        self.schema_inner(tip, false)
    }
    fn schema_inner(&self, tip: TipId, active: bool) -> Result<Commitment, Error> {
        if tip == self.genesis() {
            return Ok(self.genesis_schema);
        }
        // Each ancestor lookup otherwise rehashes every earlier signed seal.
        let mut continuations = std::collections::BTreeMap::new();
        for seal in &self.seals {
            if seal.statement.scope == self.scope
                && let Ok(Some(successor)) = seal.successor()
            {
                continuations.entry(successor).or_insert(seal);
            }
        }
        let mut cursor = tip;
        let mut requested = None;
        let mut visited = std::collections::BTreeSet::new();
        loop {
            if !visited.insert(cursor) {
                return Err(denied("cyclic tip graph"));
            }
            if cursor == self.genesis() {
                return Ok(requested.unwrap_or(self.genesis_schema));
            }
            if let Some(&seal) = continuations.get(&cursor) {
                let canonical = self.seal(seal.statement.tip)? == Some(seal);
                if active && !canonical {
                    return Err(denied("tip belongs to a losing seal"));
                }
                if let SealAction::Continue { schema } = seal.statement.action {
                    requested.get_or_insert(schema);
                }
                cursor = seal.statement.tip;
                continue;
            }
            let mut parent = None;
            for opening in &self.openings {
                if opening.statement.scope == self.scope && opening.tip()? == cursor {
                    let seal = self.seal_by_id(opening.statement.seal)?;
                    if active {
                        if self.revoked.contains(&opening.id()?) {
                            return Err(denied("tip opening is revoked"));
                        }
                        if self.seal(seal.statement.tip)? != Some(seal) {
                            return Err(denied("tip opens on a losing seal"));
                        }
                        if self.authority(opening.endorsement()) == TipAuthority::Issuer
                            && !self.migration_opening(opening, true)?
                        {
                            return Err(denied(
                                "migration opening does not inherit the line minimum",
                            ));
                        }
                    }
                    requested.get_or_insert(opening.statement.schema);
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
    /// Whether an opening is a restricted schema migration, independent of
    /// its signer's authority. Historical validation retains old minima.
    pub(crate) fn migration_opening(
        &self,
        opening: &SignedTipOpening,
        active: bool,
    ) -> Result<bool, Error> {
        self.check_scope(opening.statement.scope)?;
        let seal = self.seal_by_id(opening.statement.seal)?;
        if seal.statement.action != SealAction::Close
            || opening.statement.schema == self.schema_any(seal.statement.tip)?
        {
            return Ok(false);
        }
        let minimum = opening.statement.minimum_in_sync as usize;
        if active || self.line_root_any(seal.statement.tip)? != self.genesis() {
            return Ok(self.line_minimum(seal.statement.tip)? == Some(minimum));
        }
        Ok(self.genesis_lines.iter().any(|line| {
            line.statement.scope == self.scope && line.statement.minimum_in_sync as usize == minimum
        }))
    }
    /// Forward schema successors along canonical ancestry. Migration edges
    /// start new author lines; frame continuation still uses `continues`.
    pub fn succeeds(&self, from: TipId, to: TipId) -> Result<bool, Error> {
        self.schema(from)?;
        self.schema(to)?;
        self.succeeds_inner(from, to, true)
    }
    /// Structural forward authorization, including retained losing migrations.
    pub fn succeeds_any(&self, from: TipId, to: TipId) -> Result<bool, Error> {
        self.schema_any(from)?;
        self.schema_any(to)?;
        self.succeeds_inner(from, to, false)
    }
    fn succeeds_inner(&self, from: TipId, to: TipId, active: bool) -> Result<bool, Error> {
        let mut cursor = to;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            let mut parent = None;
            for seal in &self.seals {
                if seal.statement.scope == self.scope && seal.successor()? == Some(cursor) {
                    parent = Some(seal.statement.tip);
                    break;
                }
            }
            if parent.is_none() {
                for opening in &self.openings {
                    if opening.statement.scope == self.scope && opening.tip()? == cursor {
                        if !self.migration_opening(opening, active)? {
                            return Ok(false);
                        }
                        parent = Some(self.seal_by_id(opening.statement.seal)?.statement.tip);
                        break;
                    }
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
        // Active ancestry also proves structural ancestry; valid tips need one walk.
        if self.schema(tip).is_ok() {
            return Ok(tip);
        }
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
            if let Some(seal) = self.seal(cursor)?
                && let Some(next) = seal.successor()?
            {
                cursor = next;
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
    /// The tip a line starts at, through seals whatever their resolution.
    fn line_root_any(&self, tip: TipId) -> Result<TipId, Error> {
        self.schema_any(tip)?;
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
    /// Resolve conflicts per `(name, from)` by authority, then the lower
    /// move digest, and walk from creation; a tip without a move that the
    /// owner's recovery sealed continues on. A revisit ends the walk at the
    /// revisited tip. Revoked moves never win.
    pub fn bookmark(&self, name: &str) -> Result<Option<TipId>, Error> {
        Ok(self.bookmark_walk(name)?.0)
    }
    /// The moves of `name` that lost a race along its path, in path and
    /// then resolution order: inspection's `main/1`, `main/2`. They stay
    /// branches; revoked moves are not listed.
    pub fn losing_moves(&self, name: &str) -> Result<Vec<&SignedBookmarkMove>, Error> {
        Ok(self.bookmark_walk(name)?.1)
    }
    /// The resolved edges a bookmark traverses, using the same authority,
    /// revocation and ancestry rules as its final target.
    pub fn bookmark_path(&self, name: &str) -> Result<Vec<(TipId, TipId)>, Error> {
        Ok(self.bookmark_walk(name)?.2)
    }
    /// The target, losing moves and resolved edges of a bookmark.
    fn bookmark_walk(&self, name: &str) -> Result<BookmarkWalk<'_>, Error> {
        let mut cursor = None;
        let mut losers = Vec::new();
        let mut path = Vec::new();
        let mut visited = std::collections::BTreeSet::new();
        while visited.insert(cursor) {
            let mut candidates = Vec::new();
            for movement in self.moves.iter().filter(|movement| {
                movement.statement.scope == self.scope
                    && movement.statement.name == name
                    && movement.statement.from == cursor
            }) {
                let id = movement.id()?;
                if self.revoked.contains(&id) {
                    continue;
                }
                candidates.push((self.precedence(movement.endorsement(), id), movement));
            }
            candidates.sort_by_key(|candidate| candidate.0);
            let mut winner = None;
            for (_, movement) in candidates {
                let target = movement
                    .statement
                    .to
                    .map(|tip| self.canonical_tip(tip))
                    .transpose();
                if let Ok(target) = target
                    && (target.is_none() || target != cursor)
                    && winner.is_none()
                {
                    winner = Some(target);
                } else {
                    losers.push(movement);
                }
            }
            match winner {
                Some(target) => {
                    if let (Some(from), Some(to)) = (cursor, target) {
                        path.push((from, to));
                    }
                    cursor = target;
                }
                None => {
                    // The owner's recovery carries the tip's bookmarks to
                    // its continuation; an explicit move from it comes first.
                    if let Some(tip) = cursor
                        && let Some(seal) = self.seal(tip)?
                        && seal.statement.recovery.is_some()
                        && let Some(next) = seal.successor()?
                    {
                        path.push((tip, next));
                        cursor = Some(next);
                        continue;
                    }
                    return Ok((cursor, losers, path));
                }
            }
        }
        Ok((cursor, losers, path))
    }
}

#[cfg(test)]
pub(crate) mod tests;
