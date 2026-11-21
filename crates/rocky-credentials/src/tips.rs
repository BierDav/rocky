use crate::revocation::{Cut, Head};
use crate::{
    CredentialChain, Endorsement, Error, IdentityProvider, PublicKey, RevocationSet, SealFinality,
    SignedCheckpointRecord, TerminalAck, decode_bounded, denied, encode,
};
use rocky_core::{Commitment, CoordinatorIdentity, Scope, TipId};
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, MutexGuard, PoisonError},
};

pub const SEAL_FORMAT: u32 = 3;
pub const TIP_OPENING_FORMAT: u32 = 3;
pub const BOOKMARK_MOVE_FORMAT: u32 = 2;
pub const GENESIS_LINE_FORMAT: u32 = 2;
pub const MAX_BOOKMARK_NAME: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChainLink {
    pub lane: PublicKey,
    pub sequence: u64,
    pub previous: Option<Commitment>,
    pub stage: Option<Commitment>,
}
impl ChainLink {
    pub fn first(lane: PublicKey) -> Self {
        Self {
            lane,
            sequence: 0,
            previous: None,
            stage: None,
        }
    }
    pub fn next(&self, id: Commitment) -> Self {
        Self {
            lane: self.lane,
            sequence: self.sequence.saturating_add(1),
            previous: Some(id),
            stage: self.stage,
        }
    }
    pub fn branch(self, seal: &SealStatement) -> Result<Self, Error> {
        Ok(Self {
            stage: Some(Self::stage_of(seal)?),
            ..self
        })
    }
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
pub trait OpensStage {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Equivocation {
    pub identity: PublicKey,
    pub lane: PublicKey,
    pub sequence: u64,
    pub acknowledgment: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PastCut {
    pub id: Commitment,
    pub identity: PublicKey,
    pub link: ChainLink,
    pub acknowledgment: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrippedFinality {
    pub record: Commitment,
    pub kept: Option<SealFinality>,
    pub acknowledgments: Vec<TerminalAck>,
}

pub fn genesis_tip(schema: Commitment) -> TipId {
    TipId(rocky_core::commitment("rocky.genesis-tip.v1", &schema))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorMark {
    pub author: rocky_core::WriterId,
    pub sequence: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SealAction {
    Continue { schema: Commitment },
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogPoint {
    pub position: u64,
    pub running: Commitment,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
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
            || self
                .frontier
                .iter()
                .any(|mark| mark.sequence > i64::MAX as u64)
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
            pub fn endorse<'s>(
                statement: $statement,
                signer: impl Into<$crate::Signer<'s>>,
                chain: &CredentialChain,
            ) -> Result<Self, Error> {
                Self::endorse_linked(statement, signer, chain, None)
            }
            pub fn endorse_linked<'s>(
                statement: $statement,
                signer: impl Into<$crate::Signer<'s>>,
                chain: &CredentialChain,
                link: Option<$crate::tips::ChainLink>,
            ) -> Result<Self, Error> {
                let signer = signer.into();
                statement.validate()?;
                if let Some(link) = &link {
                    link.validate()?;
                    if signer.signing_node() != Some(link.lane) {
                        return Err(denied("a chained statement's lane is its signing node"));
                    }
                }
                let signed = Self {
                    endorsement: Endorsement::sign(signer, chain, $label, &(&statement, &link))?,
                    statement,
                    link,
                };
                signed.to_bytes()?;
                Ok(signed)
            }
            pub fn verify_historic(
                &self,
                publisher: PublicKey,
                graph: &TipGraph,
            ) -> Result<(), Error> {
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
                let signer = self.endorsement.claimed_signer(publisher);
                let current_unless_cut = current.filter(|(revocations, _)| {
                    self.link
                        .is_none_or(|link| revocations.cut(&signer, &link.lane).is_none())
                });
                let (trusted, node) = self.endorsement.verify_tip_statement(
                    publisher,
                    self.statement.scope,
                    $label,
                    &(&self.statement, &self.link),
                    current_unless_cut,
                )?;
                if self.link.is_some_and(|link| Some(link.lane) != node) {
                    return Err(denied("a chained statement's lane is its signing node"));
                }
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
            .verify_historic(owner, "rocky.seal.v2", &(&self.statement, &self.link))?;
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

struct Walk {
    head: Head,
    held: Vec<(u64, Option<Commitment>)>,
}
impl Walk {
    fn new(links: &BTreeMap<Commitment, (PublicKey, ChainLink)>, head: &Head) -> Self {
        let mut held = Vec::new();
        let mut cursor = Some(head.id);
        let mut lowest = u64::MAX;
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

fn beyond(walks: &[Walk], link: &ChainLink, id: Commitment) -> bool {
    for walk in walks {
        if walk.head.id == id {
            return false;
        }
        let below_head = link.sequence < walk.head.sequence;
        if below_head && walk.reaches(link.sequence, id) {
            return false;
        }
    }
    true
}

#[derive(Default)]
struct ChainIndex(Mutex<Places>);
const INDEXED_LISTS: usize = 7;
#[derive(Clone, Default)]
struct Places {
    indexed_prefix: [(usize, Option<Commitment>); INDEXED_LISTS],
    links: BTreeMap<Commitment, (PublicKey, ChainLink)>,
    acks: BTreeMap<Commitment, (PublicKey, ChainLink)>,
    lanes: BTreeMap<(PublicKey, PublicKey), BTreeSet<Commitment>>,
    refs: BTreeMap<Commitment, Vec<Commitment>>,
    referrers: BTreeMap<Commitment, BTreeSet<Commitment>>,
    seals: BTreeMap<Commitment, usize>,
    finals: BTreeMap<Commitment, BTreeSet<Commitment>>,
    final_coordinators: BTreeMap<Commitment, CoordinatorIdentity>,
    decided: BTreeSet<Commitment>,
    founding: BTreeMap<Scope, ((u64, Commitment), CoordinatorIdentity)>,
}
impl Places {
    fn refer(&mut self, id: Commitment, refs: Vec<Commitment>) {
        for target in &refs {
            self.referrers.entry(*target).or_default().insert(id);
        }
        self.refs.entry(id).or_default().extend(refs);
    }
    fn chain(
        &mut self,
        id: Commitment,
        mut refs: Vec<Commitment>,
        link: Option<ChainLink>,
        endorsement: &Endorsement,
    ) {
        refs.extend(link.and_then(|link| link.previous));
        self.refer(id, refs);
        if let (Some(link), Some(signer)) =
            (link, Endorsement::claimed_signer_of(&endorsement.chain))
        {
            self.links.insert(id, (signer, link));
            self.lanes
                .entry((signer, link.lane))
                .or_default()
                .insert(id);
        }
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
trait Indexed {
    fn index(&self, id: Commitment, position: usize, places: &mut Places) -> Result<(), Error>;
}
macro_rules! indexed_statement {
    ($($signed:ty => |$s:ident| $refs:expr),* $(,)?) => {$(
        impl Indexed for $signed {
            fn index(&self, id: Commitment, _: usize, places: &mut Places) -> Result<(), Error> {
                let $s = self;
                places.chain(id, $refs, self.link, self.endorsement());
                Ok(())
            }
        }
    )*};
}
impl Indexed for SignedSeal {
    fn index(&self, id: Commitment, position: usize, places: &mut Places) -> Result<(), Error> {
        places.seals.entry(id).or_insert(position);
        places.chain(
            id,
            vec![self.statement.tip.0],
            self.link,
            self.endorsement(),
        );
        Ok(())
    }
}
indexed_statement!(
    SignedTipOpening => |s| vec![s.statement.seal],
    SignedBookmarkMove => |s| [s.statement.from, s.statement.to]
        .into_iter()
        .flatten()
        .map(|tip| tip.0)
        .collect(),
    SignedGenesisLine => |_s| Vec::new(),
    SignedCheckpointRecord => |s| vec![s.statement.seal],
);
impl Indexed for crate::SignedDatabaseSettings {
    fn index(&self, id: Commitment, _: usize, places: &mut Places) -> Result<(), Error> {
        if let Some(bundle) = self.statement.coordinator() {
            let key = (self.statement.revision, id);
            let founding = places
                .founding
                .entry(self.statement.scope)
                .or_insert((key, bundle.identity));
            if key < founding.0 {
                *founding = (key, bundle.identity);
            }
        }
        Ok(())
    }
}
impl Indexed for SealFinality {
    fn index(&self, id: Commitment, _: usize, places: &mut Places) -> Result<(), Error> {
        places.refer(id, vec![self.seal]);
        places.finals.entry(self.seal).or_default().insert(id);
        if let Some(coordinator) = self.coordinator() {
            places.final_coordinators.insert(id, coordinator);
        }
        if self.decided() {
            places.decided.insert(id);
        }
        for ack in &self.acknowledgments {
            if let Some(identity) = ack.claimed_identity() {
                places.acks.insert(ack.id()?, (identity, ack.link));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TipAuthority {
    Issuer,
    Trusted,
    Owner,
}
impl TipAuthority {
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
    pub settings: Vec<crate::SignedDatabaseSettings>,
    #[serde(skip)]
    pub revoked: BTreeSet<Commitment>,
    #[serde(skip)]
    pub past_cut: BTreeSet<PastCut>,
    #[serde(skip)]
    pub equivocations: BTreeSet<Equivocation>,
    #[serde(skip)]
    index: ChainIndex,
}
struct Chained<'a> {
    id: Commitment,
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
            settings: Vec::new(),
            revoked: BTreeSet::new(),
            past_cut: BTreeSet::new(),
            equivocations: BTreeSet::new(),
            index: ChainIndex::default(),
        }
    }
    pub(crate) fn check_predecessor(
        &self,
        signer: PublicKey,
        link: &ChainLink,
        opens: Option<Commitment>,
    ) -> Result<(), Error> {
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
    fn indexed(&self) -> Result<MutexGuard<'_, Places>, Error> {
        let mut index = self.index.places();
        macro_rules! lists {
            ($($n:tt $list:ident),*) => {
                let stale = false $(|| match index.indexed_prefix[$n] {
                    (0, _) => false,
                    (count, last) => self.$list.get(count - 1).map(|signed| signed.id()).transpose()? != last,
                })*;
                if stale {
                    *index = Places::default();
                }
                $(
                    let from = index.indexed_prefix[$n].0;
                    for (position, signed) in self.$list.iter().enumerate().skip(from) {
                        let id = signed.id()?;
                        signed.index(id, position, &mut index)?;
                        index.indexed_prefix[$n] = (from.max(index.indexed_prefix[$n].0) + 1, Some(id));
                    }
                )*
            };
        }
        lists!(0 seals, 1 openings, 2 moves, 3 genesis_lines, 4 checkpoints, 5 finalities, 6 settings);
        Ok(index)
    }
    #[expect(
        clippy::type_complexity,
        reason = "lazy statement IDs borrow heterogeneous signed records"
    )]
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
    fn chained(&self) -> Result<(Vec<Chained<'_>>, Vec<Chained<'_>>), Error> {
        let mut statements = Vec::new();
        for (id, credentials, link) in self.signed() {
            if let (Some(link), Some(identity)) =
                (link, Endorsement::claimed_signer_of(credentials))
            {
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
                if let Some(identity) = ack.claimed_identity()
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
        for (id, chain, link) in self.signed() {
            let unchained = link.is_none();
            if unchained && revocations.ends_chain(chain)? {
                revoked.insert(id()?);
            }
        }
        let unfinal = self.unfinal_records(&revoked, &ended_acks)?;
        revoked.extend(unfinal);
        let changed = revoked != self.revoked;
        self.revoked = revoked;
        self.past_cut = past_cut;
        self.equivocations = equivocations;
        Ok(changed)
    }
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
    pub fn ack_beyond_cut(
        &self,
        revocations: &RevocationSet,
        ack: &TerminalAck,
    ) -> Result<bool, Error> {
        let Some(heads) = ack
            .claimed_identity()
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
    pub fn meets_minimum(&self, record: &SealFinality) -> bool {
        !record.acknowledgments.is_empty()
            && self
                .finality_minimum(record)
                .ok()
                .is_none_or(|minimum| record.acknowledgments.len() >= minimum)
    }
    pub fn strip_finalities(
        &mut self,
        revocations: &RevocationSet,
    ) -> Result<Vec<StrippedFinality>, Error> {
        let cut = |ack: &TerminalAck| {
            ack.claimed_identity()
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
    pub fn prunable(
        &self,
        revocations: &RevocationSet,
        pinned: &BTreeSet<Commitment>,
    ) -> Result<BTreeSet<Commitment>, Error> {
        let index = self.indexed()?;
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
        let mut referenced_by_kept: Vec<Commitment> = prunable
            .iter()
            .filter(|id| {
                index
                    .referrers
                    .get(*id)
                    .is_some_and(|by| by.iter().any(|referrer| !prunable.contains(referrer)))
            })
            .copied()
            .collect();
        while let Some(id) = referenced_by_kept.pop() {
            if prunable.remove(&id) {
                referenced_by_kept.extend(
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
    #[expect(
        clippy::type_complexity,
        reason = "returns retained cuts and omitted lane heads together"
    )]
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
    pub fn next_ack_link(&self, lane: &PublicKey) -> Result<ChainLink, Error> {
        let (_, acknowledgments) = self.chained()?;
        Ok(acknowledgments
            .iter()
            .filter(|item| item.link.lane == *lane)
            .min_by_key(|item| (Reverse(item.link.sequence), item.id))
            .map_or(ChainLink::first(*lane), |head| head.link.next(head.id)))
    }
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
    pub fn chain_identities(&self) -> Result<BTreeSet<PublicKey>, Error> {
        let (statements, acknowledgments) = self.chained()?;
        Ok(statements
            .iter()
            .chain(&acknowledgments)
            .map(|item| item.identity)
            .collect())
    }
    pub fn lanes_of(&self, identity: &PublicKey) -> Result<BTreeSet<PublicKey>, Error> {
        let (statements, acknowledgments) = self.chained()?;
        Ok(statements
            .iter()
            .chain(&acknowledgments)
            .filter(|item| item.identity == *identity)
            .map(|item| item.link.lane)
            .collect())
    }
    fn precedence(
        &self,
        endorsement: &Endorsement,
        id: Commitment,
    ) -> (Reverse<TipAuthority>, Commitment) {
        (Reverse(TipAuthority::of(endorsement, self.scope)), id)
    }
    pub fn is_final(&self, seal: Commitment) -> Result<bool, Error> {
        self.final_by(seal, false)
    }
    pub fn counts_finality(&self, record: &SealFinality) -> Result<bool, Error> {
        let named = self
            .indexed()?
            .founding
            .get(&self.scope)
            .map(|(_, identity)| *identity);
        Ok(named.is_none_or(|named| record.coordinator() == Some(named)))
    }
    pub fn finality_coordinators(&self) -> Result<BTreeSet<CoordinatorIdentity>, Error> {
        let mut named = BTreeSet::new();
        for record in &self.finalities {
            if record.scope == self.scope
                && !self.revoked.contains(&record.id()?)
                && let Some(coordinator) = record.coordinator()
            {
                named.insert(coordinator);
            }
        }
        Ok(named)
    }
    pub fn compacts_final(&self, seal: Commitment) -> Result<bool, Error> {
        Ok(self.indexed()?.founding.contains_key(&self.scope) && self.is_final(seal)?)
    }
    pub fn decided_by_named(&self, seal: Commitment) -> Result<bool, Error> {
        Ok(self.indexed()?.founding.contains_key(&self.scope) && self.final_by(seal, true)?)
    }
    fn final_by(&self, seal: Commitment, decided: bool) -> Result<bool, Error> {
        let index = self.indexed()?;
        let named = index
            .founding
            .get(&self.scope)
            .map(|(_, identity)| *identity);
        Ok(index.finals.get(&seal).is_some_and(|records| {
            records.iter().any(|id| {
                !self.revoked.contains(id)
                    && (!decided || index.decided.contains(id))
                    && named.is_none_or(|named| index.final_coordinators.get(id) == Some(&named))
            })
        }))
    }
    pub fn checkpoints_at(&self, seal: Commitment) -> Result<Vec<&SignedCheckpointRecord>, Error> {
        let mut listed = Vec::new();
        for record in &self.checkpoints {
            if record.statement.seal == seal && !self.revoked.contains(&record.id()?) {
                listed.push(record);
            }
        }
        Ok(listed)
    }
    pub fn settings(&self) -> Result<Option<&crate::DatabaseSettingsStatement>, Error> {
        Ok(self.governing_settings()?.map(|signed| &signed.statement))
    }
    pub fn coordinator(&self) -> Result<Option<&crate::CoordinatorBundle>, Error> {
        Ok(self
            .governing_settings()?
            .and_then(|signed| signed.statement.coordinator()))
    }
    fn governing_settings(&self) -> Result<Option<&crate::SignedDatabaseSettings>, Error> {
        let founding = self
            .indexed()?
            .founding
            .get(&self.scope)
            .map(|(_, identity)| *identity);
        let mut governing: Option<((u64, Reverse<Commitment>), &crate::SignedDatabaseSettings)> =
            None;
        for signed in self.settings.iter().filter(|s| {
            s.statement.scope == self.scope
                && founding.is_none_or(|founding| {
                    s.statement
                        .coordinator()
                        .is_some_and(|bundle| bundle.identity == founding)
                })
        }) {
            let key = (signed.statement.revision, Reverse(signed.id()?));
            if governing.as_ref().is_none_or(|(current, _)| key > *current) {
                governing = Some((key, signed));
            }
        }
        Ok(governing.map(|(_, signed)| signed))
    }
    pub fn next_settings(
        &self,
        coordination: crate::Coordination,
    ) -> Result<Option<crate::DatabaseSettingsStatement>, Error> {
        if let crate::Coordination::Coordinated { bundle } = &coordination
            && self.coordinator()?.is_none()
            && self
                .finality_coordinators()?
                .iter()
                .any(|named| *named != bundle.identity)
        {
            return Err(denied(
                "the database's finality records name another coordinator: name that one",
            ));
        }
        if let Some(held) = self.coordinator()? {
            match &coordination {
                crate::Coordination::Uncoordinated => {
                    return Err(denied(
                        "a coordinated database stays coordinated: its settings never uncoordinate it",
                    ));
                }
                crate::Coordination::Coordinated { bundle } if bundle.identity != held.identity => {
                    return Err(denied(
                        "a coordinated database keeps its coordinator: changing it is not supported",
                    ));
                }
                crate::Coordination::Coordinated { .. } => {}
            }
        }
        if self
            .settings()?
            .is_some_and(|governing| governing.coordination == coordination)
        {
            return Ok(None);
        }
        let revision = self
            .settings
            .iter()
            .filter(|s| s.statement.scope == self.scope)
            .map(|s| s.statement.revision)
            .max()
            .unwrap_or(0)
            + 1;
        Ok(Some(crate::DatabaseSettingsStatement {
            format: crate::DATABASE_SETTINGS_FORMAT,
            scope: self.scope,
            revision,
            coordination,
        }))
    }
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
    fn unfinal_records(
        &self,
        revoked: &BTreeSet<Commitment>,
        ended_acks: &BTreeSet<Commitment>,
    ) -> Result<Vec<Commitment>, Error> {
        let mut unfinal = Vec::new();
        for record in &self.finalities {
            let Ok(minimum) = self.finality_minimum_in(record, revoked) else {
                continue;
            };
            let mut ended = 0;
            for ack in &record.acknowledgments {
                ended += usize::from(ended_acks.contains(&ack.id()?));
            }
            if record.acknowledgments.len() - ended < minimum {
                unfinal.push(record.id()?);
            }
        }
        Ok(unfinal)
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
    pub fn abandons(&self, tip: TipId, position: u64) -> Result<bool, Error> {
        Ok(self.seal(tip)?.is_some_and(|seal| {
            seal.statement
                .recovery
                .is_some_and(|recovery| recovery.abandons(position))
        }))
    }
    pub fn authority(&self, endorsement: &Endorsement) -> TipAuthority {
        TipAuthority::of(endorsement, self.scope)
    }
    pub fn seal_by_id(&self, id: Commitment) -> Result<&SignedSeal, Error> {
        let position = self.indexed()?.seals.get(&id).copied();
        position
            .and_then(|position| self.seals.get(position))
            .filter(|seal| seal.statement.scope == self.scope)
            .ok_or_else(|| denied("unknown seal"))
    }
    pub fn schema(&self, tip: TipId) -> Result<Commitment, Error> {
        self.schema_inner(tip, true)
    }
    pub fn schema_any(&self, tip: TipId) -> Result<Commitment, Error> {
        self.schema_inner(tip, false)
    }
    fn seals_by_continuation(&self) -> BTreeMap<TipId, &SignedSeal> {
        let mut continuations = BTreeMap::new();
        for seal in &self.seals {
            if seal.statement.scope == self.scope
                && let Ok(Some(successor)) = seal.successor()
            {
                continuations.entry(successor).or_insert(seal);
            }
        }
        continuations
    }
    fn schema_inner(&self, tip: TipId, active: bool) -> Result<Commitment, Error> {
        if tip == self.genesis() {
            return Ok(self.genesis_schema);
        }
        let continuations = self.seals_by_continuation();
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
    pub fn late_anchor(&self, tip: TipId) -> Result<TipId, Error> {
        if !self.indexed()?.founding.contains_key(&self.scope) || self.schema(tip).is_ok() {
            return Ok(tip);
        }
        let Ok(schema) = self.schema_any(tip) else {
            return Ok(tip);
        };
        let continuations = self.seals_by_continuation();
        let mut cursor = tip;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            let Some(seal) = continuations.get(&cursor) else {
                return Ok(tip);
            };
            if self.revoked.contains(&seal.id()?) {
                return Ok(tip);
            }
            cursor = seal.statement.tip;
            if let Ok(active) = self.schema(cursor) {
                let decided = match self.seal(cursor)? {
                    Some(winner) => self.decided_by_named(winner.id()?)?,
                    None => false,
                };
                return Ok(if active == schema && decided {
                    cursor
                } else {
                    tip
                });
            }
        }
        Err(denied("cyclic tip graph"))
    }
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
    pub fn succeeds(&self, from: TipId, to: TipId) -> Result<bool, Error> {
        self.schema(from)?;
        self.schema(to)?;
        self.succeeds_inner(from, to, true)
    }
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
    pub fn canonical_tip(&self, tip: TipId) -> Result<TipId, Error> {
        let actively_valid = self.schema(tip).is_ok();
        if actively_valid {
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
    pub fn author_line_root(&self, tip: TipId) -> Result<TipId, Error> {
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
            if parent.is_none() {
                for opening in &self.openings {
                    if opening.statement.scope == self.scope
                        && opening.tip()? == cursor
                        && self.migration_opening(opening, true)?
                    {
                        parent = Some(self.seal_by_id(opening.statement.seal)?.statement.tip);
                        break;
                    }
                }
            }
            match parent {
                Some(tip) => cursor = tip,
                None => return Ok(cursor),
            }
        }
        Err(denied("cyclic tip graph"))
    }
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
    pub fn bookmark(&self, name: &str) -> Result<Option<TipId>, Error> {
        Ok(self.bookmark_walk(name)?.0)
    }
    pub fn losing_moves(&self, name: &str) -> Result<Vec<&SignedBookmarkMove>, Error> {
        Ok(self.bookmark_walk(name)?.1)
    }
    pub fn bookmark_path(&self, name: &str) -> Result<Vec<(TipId, TipId)>, Error> {
        Ok(self.bookmark_walk(name)?.2)
    }
    fn recovery_continuation(&self, tip: TipId) -> Result<Option<TipId>, Error> {
        match self.seal(tip)? {
            Some(seal) if seal.statement.recovery.is_some() => seal.successor(),
            _ => Ok(None),
        }
    }
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
                    if let Some(tip) = cursor
                        && let Some(next) = self.recovery_continuation(tip)?
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
