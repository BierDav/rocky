//! The canonical logical schema descriptor: what an ORM declared, how each
//! part is replicated and which identities it has. See
//! `docs/runtime/logical-schema.md`.
use crate::{
    AppId, ApplicationManifest, AuthorLogScope, Commitment, DefinitionItem, DomainId, DomainRule,
    SqlValue, commitment, definition_item_id,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Current descriptor format, including row identities and partition groups.
pub const LOGICAL_SCHEMA_FORMAT: u32 = 5;
/// Largest canonical descriptor a parser accepts.
pub const MAX_LOGICAL_SCHEMA_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affinity {
    Integer,
    Real,
    Text,
    Blob,
    /// Installed for local tables only; it has no single storage class.
    Numeric,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Collation {
    Binary = 0,
    Nocase = 1,
    Rtrim = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableClass {
    /// Replicated rows with register columns.
    Mergeable,
    /// Installed, never written by a replicated operation.
    LocalOnly,
    /// Written only from confirmed strong results.
    ConfirmedReadOnly,
    /// Written only by strong commands.
    CommandControlled,
}
impl TableClass {
    /// Rows with a key and register fields that replicas agree on: mergeable
    /// rows, or confirmed rows of a command-controlled table.
    pub fn typed(self) -> bool {
        matches!(self, Self::Mergeable | Self::CommandControlled)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnClass {
    Mergeable,
    LocalOnly,
    /// Recomputed from the row's other columns by this installed expression.
    Derived {
        rule: String,
    },
}
/// Which assigned values an update publishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePolicy {
    Changed,
    Assigned,
}
/// The ORM's own schema identity, carried and never interpreted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaSource {
    pub kind: String,
    pub version: u32,
    pub identity: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalColumn {
    /// The field's identity, forever; see decision 0067.
    pub sql_name: String,
    pub affinity: Affinity,
    pub not_null: bool,
    pub default: Option<String>,
    pub collation: Collation,
    pub class: ColumnClass,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexColumn {
    pub name: String,
    pub descending: bool,
}
/// What a remote write that collides on a unique index of a mergeable table
/// does to the row with the earlier stamp. See `docs/runtime/mergeable-constraints.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    /// Its indexed columns read NULL while the clash lasts.
    SetNull,
    /// The whole row is hidden.
    LatestWins,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalIndex {
    pub name: String,
    pub unique: bool,
    pub columns: Vec<IndexColumn>,
    pub create_sql: String,
    /// Required on a unique index of a mergeable table; omitted otherwise.
    pub on_conflict: Option<ConflictPolicy>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalForeignKey {
    pub table: String,
    pub columns: Vec<String>,
    pub references: Vec<String>,
    pub on_delete: String,
    pub on_update: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalView {
    pub name: String,
    pub create_sql: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalTable {
    /// The table's identity, forever; see decision 0067.
    pub sql_name: String,
    pub class: TableClass,
    /// The ORM's own statement, naming `sql_name`.
    pub create_sql: String,
    /// The engine or ORM assigns the key.
    pub auto_generate: bool,
    /// Declaration order.
    pub columns: Vec<LogicalColumn>,
    /// Column SQL names in key order.
    pub primary_key: Vec<String>,
    pub partition_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_group: Option<String>,
    pub indexes: Vec<LogicalIndex>,
    pub foreign_keys: Vec<LogicalForeignKey>,
    /// Publish row liveness to readers of other scopes.
    pub existence_log: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogicalSchema {
    pub format: u32,
    pub publisher: [u8; 32],
    pub application: String,
    pub source: SchemaSource,
    pub update_policy: UpdatePolicy,
    /// Sorted by name, each table's identity.
    pub tables: Vec<LogicalTable>,
    pub views: Vec<LogicalView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incompatibility {
    AutoGeneratedKey,
    UniqueConstraint,
    ForeignKey,
    CheckConstraint,
    GeneratedColumn,
    NullableKey(String),
    Affinity(String),
    LocalColumn(String),
    DerivedRule(String),
    View,
    /// The secure profile stores no derived column.
    DerivedColumn(String),
    /// The secure profile installs mergeable and local-only tables only.
    TableClass(TableClass),
    /// A conflict policy this index cannot have, named by the index.
    ConflictPolicy(String),
    /// A foreign key Rocky cannot resolve, named by its first column.
    Reference(String),
    /// Parent and child of a foreign key lie in different reader scopes.
    CrossScopeReference(String),
    /// A table with an existence log, or one its support rows need, cannot
    /// hold deterministic support rows; names the conflicting key/constraint.
    SupportRow(String),
}
/// The storage profile a descriptor is installed on. Their differences are
/// listed in `docs/runtime/schema-definitions.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// `rocky-runtime`: typed tables over replicated incarnations.
    Trusted,
    /// `rocky-secure`: typed tables over encrypted rows of reader scopes.
    Secure,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaError {
    /// Not a well-formed canonical descriptor.
    Invalid(String),
    /// A well-formed schema this runtime cannot install without breaking
    /// convergence or a stated guarantee.
    Incompatible {
        table: String,
        reason: Incompatibility,
    },
}
impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "invalid logical schema: {reason}"),
            Self::Incompatible { table, reason } => {
                write!(f, "{table} cannot be installed: {reason:?}")
            }
        }
    }
}
impl std::error::Error for SchemaError {}

/// Why a column cannot store a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueError {
    /// NULL in a key or a NOT NULL column.
    Null,
    /// Not the storage class of the column's affinity.
    Class(Affinity),
    /// An INTEGER a REAL column cannot hold exactly.
    Precision,
    Arity,
    KeyTooLong,
}
impl std::fmt::Display for ValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ValueError {}

impl Affinity {
    /// SQLite's rule for a declared column type.
    pub fn of_declared(declared: &str) -> Self {
        let declared = declared.to_ascii_uppercase();
        let has = |part: &str| declared.contains(part);
        if has("INT") {
            Self::Integer
        } else if has("CHAR") || has("CLOB") || has("TEXT") {
            Self::Text
        } else if has("BLOB") || declared.trim().is_empty() {
            Self::Blob
        } else if has("REAL") || has("FLOA") || has("DOUB") {
            Self::Real
        } else {
            Self::Numeric
        }
    }
    /// The value as a column of this affinity stores it. Columns are strict:
    /// NULL or one storage class. An integral REAL is an INTEGER and an
    /// exactly representable INTEGER is a REAL; nothing else converts.
    pub fn store(self, value: SqlValue) -> Result<SqlValue, ValueError> {
        const EXACT: i64 = 1 << 53;
        match (self, value) {
            (_, SqlValue::Null) => Ok(SqlValue::Null),
            (Self::Integer, value @ SqlValue::Integer(_))
            | (Self::Real, value @ SqlValue::Real(_))
            | (Self::Text, value @ SqlValue::Text(_))
            | (Self::Blob, value @ SqlValue::Blob(_)) => Ok(value),
            (Self::Integer, SqlValue::Real(real)) => {
                let real = real.get();
                // 2^63 is an exact double; an integral REAL below it is an INTEGER.
                let limit = -(i64::MIN as f64);
                if real.fract() == 0.0 && (-limit..limit).contains(&real) {
                    Ok(SqlValue::Integer(real as i64))
                } else {
                    Err(ValueError::Class(self))
                }
            }
            (Self::Real, SqlValue::Integer(integer)) if (-EXACT..=EXACT).contains(&integer) => {
                SqlValue::real(integer as f64).ok_or(ValueError::Precision)
            }
            (Self::Real, SqlValue::Integer(_)) => Err(ValueError::Precision),
            _ => Err(ValueError::Class(self)),
        }
    }
}
impl Collation {
    pub fn named(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "BINARY" => Some(Self::Binary),
            "NOCASE" => Some(Self::Nocase),
            "RTRIM" => Some(Self::Rtrim),
            _ => None,
        }
    }
    /// The bytes two texts share exactly when the collation calls them equal.
    pub fn fold(self, text: &str) -> Vec<u8> {
        match self {
            Self::Binary => text.as_bytes().to_vec(),
            Self::Nocase => text.bytes().map(|byte| byte.to_ascii_lowercase()).collect(),
            Self::Rtrim => text.trim_end_matches(' ').as_bytes().to_vec(),
        }
    }
}

/// A plain SQL identifier the runtime can quote and rewrite.
pub fn identifier(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && !bytes[0].is_ascii_digit()
}
fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
/// The statement of a unique index of a mergeable table: real rows only.
pub fn unique_index_sql(table: &str, index: &str, columns: &[IndexColumn]) -> String {
    let terms: Vec<String> = columns
        .iter()
        .map(|column| {
            let order = if column.descending { " DESC" } else { "" };
            format!("{}{order}", quote(&column.name))
        })
        .collect();
    format!(
        "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {} ({}) WHERE \"row_kind\" = 0",
        quote(index),
        quote(table),
        terms.join(",")
    )
}
/// The clause of a foreign key of a mergeable table. SQLite checks it at
/// commit and performs no action; Rocky derives the delete action.
pub fn foreign_key_clause(key: &LogicalForeignKey) -> String {
    let list = |names: &[String]| {
        names
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(",")
    };
    format!(
        "FOREIGN KEY({}) REFERENCES {}({}) DEFERRABLE INITIALLY DEFERRED",
        list(&key.columns),
        quote(&key.table),
        list(&key.references)
    )
}
/// The delete actions a foreign key of a mergeable table may declare.
pub const DELETE_CASCADE: &str = "CASCADE";
pub const DELETE_SET_NULL: &str = "SET NULL";
fn reserved(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("rocky_") || name.starts_with("sqlite_") || name == "excluded"
}

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Quoted(String),
    Literal,
    Symbol(char),
}
/// Words, quoted identifiers, literals and symbols with their byte spans;
/// None if a quote is open.
fn spanned(sql: &str) -> Option<Vec<(Token, std::ops::Range<usize>)>> {
    let mut chars = sql.char_indices().peekable();
    let mut tokens = Vec::new();
    while let Some((start, first)) = chars.next() {
        if first.is_whitespace() {
            continue;
        }
        let token = if ['\'', '"', '`', '['].contains(&first) {
            let end = if first == '[' { ']' } else { first };
            let mut content = String::new();
            loop {
                let (_, next) = chars.next()?;
                if next == end {
                    if end != ']' && chars.peek().is_some_and(|(_, next)| *next == end) {
                        chars.next();
                    } else {
                        break;
                    }
                }
                content.push(next);
            }
            if first == '\'' {
                Token::Literal
            } else {
                Token::Quoted(content)
            }
        } else if first.is_ascii_alphanumeric() || first == '_' {
            let mut word = String::from(first);
            while let Some((_, next)) = chars
                .peek()
                .filter(|(_, next)| next.is_ascii_alphanumeric() || *next == '_' || *next == '.')
            {
                // A dot continues a number, never a name.
                if *next == '.' && !word.bytes().all(|byte| byte.is_ascii_digit()) {
                    break;
                }
                word.push(*next);
                chars.next();
            }
            if first.is_ascii_digit() {
                Token::Literal
            } else {
                Token::Word(word)
            }
        } else {
            Token::Symbol(first)
        };
        let end = chars.peek().map_or(sql.len(), |(end, _)| *end);
        tokens.push((token, start..end));
    }
    Some(tokens)
}
fn tokenize(sql: &str) -> Option<Vec<Token>> {
    Some(spanned(sql)?.into_iter().map(|(token, _)| token).collect())
}
fn keyword(token: &Token, expected: &str) -> bool {
    matches!(token, Token::Word(word) if word.eq_ignore_ascii_case(expected))
}
/// The `COLLATE` name each column of a `CREATE TABLE` statement declares.
pub fn declared_collations(create_sql: &str) -> Vec<(String, String)> {
    let tokens = tokenize(create_sql).unwrap_or_default();
    let (mut depth, mut column, mut collations) = (0usize, None::<String>, Vec::new());
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::Symbol('(') => depth += 1,
            Token::Symbol(')') => depth = depth.saturating_sub(1),
            _ => {}
        }
        if depth != 1 {
            continue;
        }
        let starts = matches!(
            tokens.get(index.wrapping_sub(1)),
            Some(Token::Symbol('(' | ','))
        );
        match token {
            Token::Quoted(name) if starts => column = Some(name.clone()),
            Token::Word(name) if starts => {
                let constraint = ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"]
                    .iter()
                    .any(|word| name.eq_ignore_ascii_case(word));
                column = (!constraint).then(|| name.clone());
            }
            token if keyword(token, "COLLATE") => {
                if let (Some(column), Some(Token::Word(name) | Token::Quoted(name))) =
                    (&column, tokens.get(index + 1))
                {
                    collations.push((column.clone(), name.clone()));
                }
            }
            _ => {}
        }
    }
    collations
}
/// Deterministic scalar functions a derived rule may call.
const RULE_FUNCTIONS: &[&str] = &[
    "abs",
    "coalesce",
    "hex",
    "ifnull",
    "instr",
    "length",
    "lower",
    "ltrim",
    "nullif",
    "replace",
    "round",
    "rtrim",
    "substr",
    "substring",
    "trim",
    "upper",
];
const RULE_WORDS: &[&str] = &[
    "AND", "CASE", "ELSE", "END", "IS", "NOT", "NULL", "OR", "THEN", "WHEN",
];
/// How a rule token is read: a literal part, or a reference to a column.
fn rule_reference(
    tokens: &[(Token, std::ops::Range<usize>)],
    index: usize,
) -> Option<Option<&str>> {
    match &tokens[index].0 {
        Token::Literal => Some(None),
        Token::Quoted(name) => Some(Some(name)),
        Token::Symbol(symbol) => "(),+-*/%|<>=!".contains(*symbol).then_some(None),
        Token::Word(word) => {
            let call = matches!(tokens.get(index + 1), Some((Token::Symbol('('), _)));
            if call {
                RULE_FUNCTIONS
                    .contains(&word.to_ascii_lowercase().as_str())
                    .then_some(None)
            } else if RULE_WORDS.contains(&word.to_ascii_uppercase().as_str()) {
                Some(None)
            } else {
                Some(Some(word))
            }
        }
    }
}
/// `rule` with each column reference replaced by what `bind` returns for it.
/// None unless `rule` is one expression: literals, operators, `CASE`, the
/// listed deterministic functions and columns `bind` knows. No subquery,
/// parameter, qualified name or statement separator can appear.
pub fn bind_rule(rule: &str, bind: impl Fn(&str) -> Option<String>) -> Option<String> {
    let tokens = spanned(rule).filter(|tokens| !tokens.is_empty() && rule.len() <= 512)?;
    let (mut bound, mut copied) = (String::new(), 0);
    for (index, (_, span)) in tokens.iter().enumerate() {
        if let Some(column) = rule_reference(&tokens, index)? {
            bound.push_str(&rule[copied..span.start]);
            bound.push_str(&bind(column)?);
            copied = span.end;
        }
    }
    bound.push_str(&rule[copied..]);
    Some(bound)
}
/// Whether `rule` is a valid derived expression over `columns`.
pub fn derived_rule(rule: &str, columns: &[&str]) -> bool {
    bind_rule(rule, |column| columns.contains(&column).then(String::new)).is_some()
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, SchemaError> {
    Err(SchemaError::Invalid(reason.into()))
}
impl LogicalTable {
    /// The identity index columns, which foreign keys reference: the
    /// partition columns in front of `id`.
    pub fn identity_columns(&self) -> Vec<String> {
        let mut columns = self.partition_by.clone();
        if !columns.iter().any(|name| name.eq_ignore_ascii_case("id")) {
            columns.push("id".into());
        }
        columns
    }
    pub fn is_identity_index(&self, index: &LogicalIndex) -> bool {
        index.unique
            && index.columns.iter().all(|column| !column.descending)
            && index
                .columns
                .iter()
                .map(|column| &column.name)
                .zip(self.identity_columns())
                .all(|(column, identity)| column.eq_ignore_ascii_case(&identity))
            && index.columns.len() == self.identity_columns().len()
    }

    pub fn column(&self, sql_name: &str) -> Option<&LogicalColumn> {
        self.columns
            .iter()
            .find(|column| column.sql_name.eq_ignore_ascii_case(sql_name))
    }
    /// Key columns in key order.
    pub fn key(&self) -> impl Iterator<Item = &LogicalColumn> {
        self.primary_key.iter().filter_map(|name| self.column(name))
    }
    /// Replicated register columns, the key excluded, in declaration order.
    pub fn registers(&self) -> impl Iterator<Item = &LogicalColumn> {
        self.columns.iter().filter(|column| {
            column.class == ColumnClass::Mergeable
                && !self
                    .primary_key
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&column.sql_name))
        })
    }
    fn validate(&self) -> Result<(), SchemaError> {
        let name = &self.sql_name;
        if !identifier(name) || reserved(name) {
            return invalid(format!("table name {name}"));
        }
        let statement = tokenize(&self.create_sql).unwrap_or_default();
        let word = |index: usize, expected: &str| matches!(statement.get(index), Some(Token::Word(word)) if word.eq_ignore_ascii_case(expected));
        if !word(0, "CREATE") || !word(1, "TABLE") || statement.contains(&Token::Symbol(';')) {
            return invalid(format!("{name} needs one CREATE TABLE statement"));
        }
        if self.columns.is_empty() || self.columns.len() > 2000 {
            return invalid(format!("{name} column count"));
        }
        let mut names = BTreeSet::new();
        for column in &self.columns {
            if !identifier(&column.sql_name) || !names.insert(column.sql_name.to_ascii_lowercase())
            {
                return invalid(format!("{name} column {}", column.sql_name));
            }
        }
        let mut key = BTreeSet::new();
        if self.primary_key.len() > usize::from(u8::MAX)
            || self.primary_key.iter().any(|column| {
                !key.insert(column.to_ascii_lowercase())
                    || self
                        .column(column)
                        .is_none_or(|column| column.class != ColumnClass::Mergeable)
            })
        {
            return invalid(format!("{name} primary key"));
        }
        for index in &self.indexes {
            let statement = tokenize(&index.create_sql).unwrap_or_default();
            let create = matches!(statement.first(), Some(Token::Word(word)) if word.eq_ignore_ascii_case("CREATE"));
            if !identifier(&index.name)
                || reserved(&index.name)
                || !create
                || statement.contains(&Token::Symbol(';'))
                || index.columns.is_empty()
                || index
                    .columns
                    .iter()
                    .any(|column| self.column(&column.name).is_none())
                || (index.on_conflict.is_some() && !index.unique)
            {
                return invalid(format!("{name} index {}", index.name));
            }
        }
        for key in &self.foreign_keys {
            if key.columns.is_empty() || key.columns.len() != key.references.len() {
                return invalid(format!("{name} foreign key"));
            }
            for column in &key.columns {
                if self.column(column).is_none() {
                    return invalid(format!("{name} column {column}"));
                }
            }
        }
        Ok(())
    }
    /// Unique indexes that resolve remote collisions by a policy.
    pub fn policy_indexes(&self) -> impl Iterator<Item = (&LogicalIndex, ConflictPolicy)> {
        self.indexes
            .iter()
            .filter_map(|index| index.on_conflict.map(|policy| (index, policy)))
    }
    fn installable(&self) -> Result<(), Incompatibility> {
        // Partitioned command control and confirmed rows remain unsupported.
        if !self.partition_by.is_empty() {
            if matches!(
                self.class,
                TableClass::CommandControlled | TableClass::ConfirmedReadOnly
            ) {
                return Err(Incompatibility::TableClass(self.class));
            }
            for (index, policy) in self.policy_indexes() {
                if policy == ConflictPolicy::SetNull
                    || self
                        .partition_by
                        .iter()
                        .any(|name| !index.columns.iter().any(|column| &column.name == name))
                {
                    return Err(Incompatibility::ConflictPolicy(index.name.clone()));
                }
            }
        }
        if self.class != TableClass::Mergeable {
            if !self.foreign_keys.is_empty()
                || tokenize(&self.create_sql)
                    .unwrap_or_default()
                    .iter()
                    .any(|token| keyword(token, "REFERENCES"))
            {
                return Err(Incompatibility::ForeignKey);
            }
            if let Some((index, _)) = self.policy_indexes().next() {
                return Err(Incompatibility::ConflictPolicy(index.name.clone()));
            }
            if !self.class.typed() {
                return Ok(());
            }
        }
        let statement = tokenize(&self.create_sql).unwrap_or_default();
        let has = |keyword: &str| {
            statement.iter().any(
                |token| matches!(token, Token::Word(word) if word.eq_ignore_ascii_case(keyword)),
            )
        };
        let generated = statement.windows(2).any(|pair| {
            matches!(&pair[0], Token::Word(word) if word.eq_ignore_ascii_case("AS"))
                && pair[1] == Token::Symbol('(')
        });
        if self.auto_generate || has("AUTOINCREMENT") {
            return Err(Incompatibility::AutoGeneratedKey);
        }
        if has("UNIQUE")
            || self.indexes.iter().any(|index| {
                index.unique && !self.is_identity_index(index) && index.on_conflict.is_none()
            })
        {
            return Err(Incompatibility::UniqueConstraint);
        }
        for (index, policy) in self.policy_indexes() {
            let reserved = |name: &str| {
                ["id", "row_kind"]
                    .iter()
                    .any(|reserved| name.eq_ignore_ascii_case(reserved))
            };
            if index.create_sql != unique_index_sql(&self.sql_name, &index.name, &index.columns)
                || index.columns.iter().any(|column| {
                    reserved(&column.name)
                        || (policy == ConflictPolicy::SetNull
                            && (self
                                .primary_key
                                .iter()
                                .any(|name| name.eq_ignore_ascii_case(&column.name))
                                || self
                                    .column(&column.name)
                                    .is_none_or(|column| column.not_null)))
                })
            {
                return Err(Incompatibility::ConflictPolicy(index.name.clone()));
            }
        }
        let references = statement
            .iter()
            .filter(|token| keyword(token, "REFERENCES"))
            .count();
        if references != self.foreign_keys.len() {
            return Err(Incompatibility::ForeignKey);
        }
        for key in &self.foreign_keys {
            let clause = foreign_key_clause(key);
            let set_null = key.on_delete == DELETE_SET_NULL;
            let refused = !(key.on_delete == DELETE_CASCADE || set_null)
                || key.on_update != "NO ACTION"
                || !(self.create_sql.contains(&format!("{clause},"))
                    || self.create_sql.contains(&format!("{clause})")))
                || key.columns.iter().any(|name| {
                    ["id", "row_kind"]
                        .iter()
                        .any(|reserved| name.eq_ignore_ascii_case(reserved))
                        || self.column(name).is_none_or(|column| {
                            (column.affinity != Affinity::Integer
                                && !self
                                    .partition_by
                                    .iter()
                                    .any(|partition| partition.eq_ignore_ascii_case(name)))
                                || (set_null
                                    && !self
                                        .partition_by
                                        .iter()
                                        .any(|partition| partition.eq_ignore_ascii_case(name))
                                    && (column.not_null
                                        || self
                                            .primary_key
                                            .iter()
                                            .any(|column| column.eq_ignore_ascii_case(name))))
                        })
                });
            if set_null
                && key.columns.iter().all(|name| {
                    self.partition_by
                        .iter()
                        .any(|partition| partition.eq_ignore_ascii_case(name))
                })
            {
                return Err(Incompatibility::Reference(
                    "SET NULL requires a non-partition reference column".into(),
                ));
            }
            if refused {
                return Err(Incompatibility::Reference(
                    key.columns.first().cloned().unwrap_or_default(),
                ));
            }
        }
        if has("CHECK") {
            return Err(Incompatibility::CheckConstraint);
        }
        if has("GENERATED") || generated {
            return Err(Incompatibility::GeneratedColumn);
        }
        if self.primary_key.is_empty() {
            return Err(Incompatibility::NullableKey("no primary key".into()));
        }
        if let Some(column) = self.key().find(|column| !column.not_null) {
            return Err(Incompatibility::NullableKey(column.sql_name.clone()));
        }
        let sources: Vec<&str> = self
            .columns
            .iter()
            .filter(|column| column.class == ColumnClass::Mergeable)
            .map(|column| column.sql_name.as_str())
            .collect();
        for column in &self.columns {
            let name = || column.sql_name.clone();
            if column.affinity == Affinity::Numeric {
                return Err(Incompatibility::Affinity(name()));
            }
            match &column.class {
                ColumnClass::Mergeable => {}
                ColumnClass::LocalOnly => return Err(Incompatibility::LocalColumn(name())),
                ColumnClass::Derived { rule } if derived_rule(rule, &sources) => {}
                ColumnClass::Derived { .. } => return Err(Incompatibility::DerivedRule(name())),
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ReplicatedColumn<'a> {
    name: &'a str,
    affinity: Affinity,
    not_null: bool,
    collation: Collation,
}
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct ReplicatedUnique<'a> {
    columns: Vec<&'a str>,
    on_conflict: ConflictPolicy,
}
#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct ReplicatedReference<'a> {
    columns: Vec<&'a str>,
    table: &'a str,
    on_delete: &'a str,
}
#[derive(Serialize)]
struct ReplicatedTable<'a> {
    class: TableClass,
    name: &'a str,
    key: Vec<ReplicatedColumn<'a>>,
    fields: Vec<ReplicatedColumn<'a>>,
    unique: Vec<ReplicatedUnique<'a>>,
    references: Vec<ReplicatedReference<'a>>,
    partition_by: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition_group: Option<&'a str>,
    existence_log: bool,
}
#[derive(Serialize)]
struct Replicated<'a> {
    format: u32,
    update_policy: UpdatePolicy,
    tables: Vec<ReplicatedTable<'a>>,
}
fn replicated_column(column: &LogicalColumn) -> ReplicatedColumn<'_> {
    ReplicatedColumn {
        name: &column.sql_name,
        affinity: column.affinity,
        not_null: column.not_null,
        collation: column.collation,
    }
}
fn replicated_table<'a>(schema: &'a LogicalSchema, table: &'a LogicalTable) -> ReplicatedTable<'a> {
    let mut fields: Vec<_> = table.registers().map(replicated_column).collect();
    fields.sort_by_key(|column| column.name);
    let name = |name: &str| {
        table
            .column(name)
            .map_or_else(|| "", |column| column.sql_name.as_str())
    };
    let mut unique: Vec<_> = table
        .policy_indexes()
        .map(|(index, on_conflict)| ReplicatedUnique {
            columns: index
                .columns
                .iter()
                .map(|column| name(&column.name))
                .collect(),
            on_conflict,
        })
        .collect();
    unique.sort();
    let mut references: Vec<_> = table
        .foreign_keys
        .iter()
        .filter(|_| table.class == TableClass::Mergeable)
        .map(|key| ReplicatedReference {
            columns: key.columns.iter().map(|column| name(column)).collect(),
            table: schema
                .table(&key.table)
                .map_or(key.table.as_str(), |parent| parent.sql_name.as_str()),
            on_delete: &key.on_delete,
        })
        .collect();
    references.sort();
    ReplicatedTable {
        class: table.class,
        name: &table.sql_name,
        key: table.key().map(replicated_column).collect(),
        fields,
        unique,
        references,
        partition_by: table
            .partition_by
            .iter()
            .filter_map(|name| table.column(name).map(|column| column.sql_name.as_str()))
            .collect(),
        partition_group: table.partition_group.as_deref(),
        existence_log: table.existence_log,
    }
}
/// The replicated shape of a whole table, its class included.
pub(crate) fn table_shape(schema: &LogicalSchema, table: &LogicalTable) -> String {
    json(&replicated_table(schema, table))
}
/// The replicated shape of a table's key, in key order, for comparison.
pub(crate) fn key_shape(table: &LogicalTable) -> String {
    json(&table.key().map(replicated_column).collect::<Vec<_>>())
}
/// The replicated shape of every register, by name.
pub(crate) fn register_shapes(table: &LogicalTable) -> std::collections::BTreeMap<String, String> {
    table
        .registers()
        .map(|column| (column.sql_name.clone(), json(&replicated_column(column))))
        .collect()
}
fn json(value: &impl Serialize) -> String {
    // These values hold only strings, numbers, booleans and lists.
    serde_json::to_string(value).unwrap_or_default()
}

impl LogicalSchema {
    pub fn app_id(&self) -> AppId {
        ApplicationManifest::app_id(self.publisher, &self.application)
    }
    /// Structural validity of a canonical descriptor.
    pub fn validate(&self) -> Result<(), SchemaError> {
        self.require_row_identity()?;
        if self.application.is_empty() || self.application.len() > 128 {
            return invalid("application name");
        }
        if self.source.kind.is_empty() || self.tables.is_empty() {
            return invalid("source or tables missing");
        }
        if !self.tables.is_sorted_by(|a, b| a.sql_name < b.sql_name) {
            return invalid("tables are not sorted by unique name");
        }
        self.partition_groups()?;
        let mut names = BTreeSet::new();
        for table in &self.tables {
            table.validate()?;
            let mut partitions = BTreeSet::new();
            if table.partition_by.len() > 16
                || table.partition_by.iter().any(|name| {
                    !partitions.insert(name.to_ascii_lowercase())
                        || !table
                            .primary_key
                            .iter()
                            .any(|column| column.eq_ignore_ascii_case(name))
                        || table.column(name).is_none_or(|column| {
                            !column.not_null
                                || column.class != ColumnClass::Mergeable
                                || column.affinity == Affinity::Numeric
                        })
                })
            {
                return invalid(format!(
                    "{}: partition columns must be distinct non-null primary-key columns",
                    table.sql_name
                ));
            }
            {
                for name in ["id", "row_kind"] {
                    let column = table.column(name).ok_or_else(|| {
                        SchemaError::Invalid(format!(
                            "{}.{} is required by descriptor format 5",
                            table.sql_name, name
                        ))
                    })?;
                    if column.sql_name != name
                        || column.affinity != Affinity::Integer
                        || !column.not_null
                        || column.collation != Collation::Binary
                        || column.class != ColumnClass::Mergeable
                    {
                        return invalid(format!(
                            "{}.{} must be a non-null integer identity column",
                            table.sql_name, name
                        ));
                    }
                }
                if !(table.primary_key.len() == table.identity_columns().len()
                    && table
                        .primary_key
                        .iter()
                        .zip(table.identity_columns())
                        .all(|(name, identity)| name.eq_ignore_ascii_case(&identity)))
                    && !table
                        .indexes
                        .iter()
                        .any(|index| table.is_identity_index(index))
                {
                    return invalid(format!(
                        "{} needs its unique identity index",
                        table.sql_name
                    ));
                }
            }
            let objects = [&table.sql_name]
                .into_iter()
                .chain(table.indexes.iter().map(|index| &index.name));
            for name in objects {
                if !names.insert(name.to_ascii_lowercase()) {
                    return invalid(format!("duplicate name {name}"));
                }
            }
        }
        for view in &self.views {
            let tokens = tokenize(&view.create_sql).unwrap_or_default();
            if !matches!(tokens.as_slice(), [Token::Word(create), Token::Word(kind), Token::Word(name) | Token::Quoted(name), Token::Word(as_word), ..] if create.eq_ignore_ascii_case("CREATE") && kind.eq_ignore_ascii_case("VIEW") && name == &view.name && as_word.eq_ignore_ascii_case("AS"))
                || tokens.contains(&Token::Symbol(';'))
            {
                return invalid(format!(
                    "view {} must contain one CREATE VIEW AS statement",
                    view.name
                ));
            }
            if !identifier(&view.name)
                || reserved(&view.name)
                || !names.insert(view.name.to_ascii_lowercase())
            {
                return invalid(format!("view name {}", view.name));
            }
        }
        Ok(())
    }
    /// Checks stored descriptor format before an opener changes the file.
    pub fn check_stored_row_identity(bytes: &[u8]) -> Result<(), SchemaError> {
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        let format: Format = serde_json::from_slice(bytes)
            .map_err(|error| SchemaError::Invalid(error.to_string()))?;
        if format.format != LOGICAL_SCHEMA_FORMAT {
            return invalid("unsupported schema descriptor format");
        }
        Ok(())
    }
    /// Refuses historical and unknown row formats before installation or writes.
    pub fn require_row_identity(&self) -> Result<(), SchemaError> {
        if self.format != LOGICAL_SCHEMA_FORMAT {
            return invalid("unsupported schema descriptor format");
        }
        Ok(())
    }
    /// Whether this runtime can install the schema and keep it convergent.
    pub fn installable(&self) -> Result<(), SchemaError> {
        self.require_row_identity()?;
        self.validate()?;
        let incompatible = |table: &str, reason| {
            Err(SchemaError::Incompatible {
                table: table.to_string(),
                reason,
            })
        };
        for table in &self.tables {
            if let Err(reason) = table.installable() {
                return incompatible(&table.sql_name, reason);
            }
            for key in &table.foreign_keys {
                let target = self
                    .tables
                    .iter()
                    .find(|target| target.sql_name.eq_ignore_ascii_case(&key.table));
                let mergeable = target.is_some_and(|target| target.class == TableClass::Mergeable);
                if let Some(target) = target {
                    for name in &key.references {
                        if target.column(name).is_none() {
                            return invalid(format!("{} column {name}", target.sql_name));
                        }
                    }
                }
                if let Some(target) = target {
                    let reason = if !target.partition_by.is_empty()
                        && (table.partition_by.is_empty()
                            || table.partition_group.is_none()
                            || table.partition_group != target.partition_group)
                    {
                        Some("partitioned references require the same explicit partition group")
                    } else if !table.partition_by.is_empty()
                        && target.partition_by.is_empty()
                        && !target.existence_log
                    {
                        Some(
                            "a partitioned child requires an unpartitioned logged parent outside its group",
                        )
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        return incompatible(
                            &table.sql_name,
                            Incompatibility::CrossScopeReference(reason.into()),
                        );
                    }
                }
                // Only the parent's identity names one incarnation.
                if !target.is_some_and(|target| {
                    mergeable
                        && (target.partition_by.is_empty()
                            && (table.partition_by.is_empty() || target.existence_log)
                            || !table.partition_by.is_empty()
                                && table.partition_group.is_some()
                                && table.partition_group == target.partition_group
                                && target.partition_by.iter().zip(&table.partition_by).all(
                                    |(parent, child)| {
                                        key.references.iter().zip(&key.columns).any(|(r, c)| {
                                            r.eq_ignore_ascii_case(parent)
                                                && c.eq_ignore_ascii_case(child)
                                        })
                                    },
                                ))
                        && target
                            .identity_columns()
                            .iter()
                            .map(String::as_str)
                            .zip(&key.references)
                            .all(|(identity, reference)| identity.eq_ignore_ascii_case(reference))
                        && target.identity_columns().len() == key.references.len()
                }) {
                    return incompatible(
                        &table.sql_name,
                        Incompatibility::Reference(
                            key.columns.first().cloned().unwrap_or_default(),
                        ),
                    );
                }
            }
        }
        // Support rows are keyed by `id` alone: puppets copy it from a
        // reference and the virtual row is `id = 0`.
        for table in self.support_tables() {
            let reason = if table.class != TableClass::Mergeable {
                Some("existence log on a table that is not mergeable".to_string())
            } else if !table.partition_by.is_empty() {
                Some(format!("partition_by ({})", table.partition_by.join(",")))
            } else if !(table.primary_key.len() == 1
                && table.primary_key[0].eq_ignore_ascii_case("id"))
            {
                Some(format!("primary key ({})", table.primary_key.join(",")))
            } else {
                None
            };
            if let Some(reason) = reason {
                return incompatible(&table.sql_name, Incompatibility::SupportRow(reason));
            }
        }
        Ok(())
    }
    /// Every reason `profile` cannot install the schema, in table order.
    /// Empty when `supported_on` accepts it.
    pub fn diagnose(&self, profile: Profile) -> Vec<SchemaError> {
        if let Err(error) = self.installable() {
            return vec![error];
        }
        let mut found = Vec::new();
        let mut refuse = |table: &LogicalTable, reason| {
            found.push(SchemaError::Incompatible {
                table: table.sql_name.clone(),
                reason,
            })
        };
        if profile == Profile::Trusted {
            // The trusted profile does not resolve hidden rows.
            for table in self.mergeable() {
                if table.existence_log {
                    refuse(table, Incompatibility::SupportRow("existence log".into()));
                }
                if table.policy_indexes().next().is_some() {
                    refuse(table, Incompatibility::UniqueConstraint);
                }
                if !table.foreign_keys.is_empty() {
                    refuse(table, Incompatibility::ForeignKey);
                }
            }
            return found;
        }
        for table in &self.tables {
            if table.class == TableClass::ConfirmedReadOnly {
                refuse(table, Incompatibility::TableClass(table.class));
                continue;
            }
            if !table.class.typed() {
                continue;
            }
            for column in &table.columns {
                if let ColumnClass::Derived { .. } = column.class {
                    refuse(
                        table,
                        Incompatibility::DerivedColumn(column.sql_name.clone()),
                    );
                }
            }
        }
        found
    }
    /// Whether `profile` can install the schema with every stated guarantee.
    /// Refusals happen here, before anything is created or applied.
    pub fn supported_on(&self, profile: Profile) -> Result<(), SchemaError> {
        match self.diagnose(profile).into_iter().next() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    /// Canonical bytes: the JSON encoding of a valid descriptor.
    pub fn canonical(&self) -> Result<Vec<u8>, SchemaError> {
        self.validate()?;
        serde_json::to_vec(self).map_err(|error| SchemaError::Invalid(error.to_string()))
    }
    /// Accepts only bytes `canonical` would produce.
    pub fn parse(bytes: &[u8]) -> Result<Self, SchemaError> {
        if bytes.len() > MAX_LOGICAL_SCHEMA_BYTES {
            return invalid("descriptor quota");
        }
        Self::check_stored_row_identity(bytes)?;
        let schema: Self = serde_json::from_slice(bytes)
            .map_err(|error| SchemaError::Invalid(error.to_string()))?;
        if schema.canonical()? != bytes {
            return invalid("descriptor is not canonical");
        }
        Ok(schema)
    }
    /// Commits to everything installed, local tables and SQL spelling included.
    pub fn fingerprint(&self) -> Result<Commitment, SchemaError> {
        Ok(commitment("rocky.logical-schema.v1", &self.canonical()?))
    }
    /// Commits to what replicas must agree on: mergeable and command-
    /// controlled tables by name, their class, keys and registers,
    /// and the update policy.
    pub fn replicated_fingerprint(&self) -> Commitment {
        let replicated = Replicated {
            format: self.format,
            update_policy: self.update_policy,
            tables: self
                .typed()
                .map(|table| replicated_table(self, table))
                .collect(),
        };
        commitment(
            "rocky.logical-schema.replicated.v1",
            json(&replicated).as_bytes(),
        )
    }
    pub fn mergeable(&self) -> impl Iterator<Item = &LogicalTable> {
        self.tables
            .iter()
            .filter(|table| table.class == TableClass::Mergeable)
    }
    /// Whether a mergeable table takes part in constraint resolution: it has
    /// a policy index or a foreign key, or a foreign key references it.
    pub fn constrained(&self, table: &LogicalTable) -> bool {
        table.class == TableClass::Mergeable
            && (table.policy_indexes().next().is_some()
                || !table.foreign_keys.is_empty()
                || self.mergeable().any(|child| {
                    child
                        .foreign_keys
                        .iter()
                        .any(|key| key.table.eq_ignore_ascii_case(&table.sql_name))
                }))
    }
    /// Tables whose rows' liveness their existence log decides.
    pub fn existence_tables(&self) -> impl Iterator<Item = &LogicalTable> {
        self.tables.iter().filter(|table| table.existence_log)
    }
    /// Tables that may hold support rows: those with an existence log and,
    /// transitively, the parents of a support row's `NOT NULL` references,
    /// which receive a virtual row. In schema order.
    pub fn support_tables(&self) -> Vec<&LogicalTable> {
        let mut found: BTreeSet<&str> = self
            .existence_tables()
            .map(|table| table.sql_name.as_str())
            .collect();
        loop {
            let reached: Vec<&str> = self
                .tables
                .iter()
                .filter(|table| found.contains(table.sql_name.as_str()))
                .flat_map(|table| {
                    table.foreign_keys.iter().filter(|key| {
                        key.columns
                            .iter()
                            .any(|name| table.column(name).is_some_and(|column| column.not_null))
                    })
                })
                .filter_map(|key| self.table(&key.table))
                .map(|parent| parent.sql_name.as_str())
                .collect();
            let before = found.len();
            found.extend(reached);
            if found.len() == before {
                break;
            }
        }
        self.tables
            .iter()
            .filter(|table| found.contains(table.sql_name.as_str()))
            .collect()
    }
    /// Mergeable and command-controlled tables: those with signed entities
    /// and fields. See `docs/runtime/confirmed-tables.md`.
    pub fn typed(&self) -> impl Iterator<Item = &LogicalTable> {
        self.tables.iter().filter(|table| table.class.typed())
    }
    pub fn table(&self, sql_name: &str) -> Option<&LogicalTable> {
        self.tables
            .iter()
            .find(|table| table.sql_name.eq_ignore_ascii_case(sql_name))
    }
    pub fn table_id(&self, table: &LogicalTable) -> Commitment {
        definition_item_id(self.publisher, self.app_id(), "entity", &table.sql_name)
    }
    pub fn field_id(&self, table: &LogicalTable, column: &LogicalColumn) -> Commitment {
        let name = format!("{}.{}", table.sql_name, column.sql_name);
        definition_item_id(self.publisher, self.app_id(), "field", &name)
    }
    /// The replicated definition as an unsigned manifest for `domains`. Its
    /// hash does not change with SQL spelling, indexes or local tables.
    pub fn manifest(&self, domains: &[DomainId]) -> ApplicationManifest {
        self.manifest_at(domains, 1)
    }
    /// The manifest of schema generation `schema`: the first is 1, and a
    /// signed transition makes each next one.
    pub fn manifest_at(&self, domains: &[DomainId], schema: u32) -> ApplicationManifest {
        let (publisher, app) = (self.publisher, self.app_id());
        let item = |kind: &str, name: String, shape: String| DefinitionItem {
            id: definition_item_id(publisher, app, kind, &name),
            name,
            shape,
        };
        let (mut entities, mut fields) = (Vec::new(), Vec::new());
        for table in self.typed() {
            let replicated = replicated_table(self, table);
            for column in replicated.key.iter().chain(&replicated.fields) {
                let name = format!("{}.{}", table.sql_name, column.name);
                fields.push(item("field", name, json(column)));
            }
            entities.push(item("entity", table.sql_name.clone(), json(&replicated)));
        }
        let shape = format!("rocky.sdk.typed.v1:{:?}", self.update_policy);
        ApplicationManifest {
            publisher,
            name: self.application.clone(),
            app,
            schema,
            projection: 1,
            handler: 1,
            entities,
            fields,
            methods: Vec::new(),
            operations: ["insert", "update", "delete"]
                .into_iter()
                .map(|name| item("operation", name.to_string(), shape.clone()))
                .collect(),
            domains: domains
                .iter()
                .map(|domain| DomainRule {
                    domain: *domain,
                    author_log: AuthorLogScope::Domain,
                    rule: "rocky.sdk.typed.v1".into(),
                })
                .collect(),
        }
    }
}

impl LogicalIndex {
    /// The unpartitioned identity index. Partition-aware validation is phase 11.
    pub fn is_identity(&self) -> bool {
        self.unique
            && self.columns.len() == 1
            && self.columns[0].name.eq_ignore_ascii_case("id")
            && !self.columns[0].descending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, affinity: Affinity) -> LogicalColumn {
        LogicalColumn {
            sql_name: name.into(),
            affinity,
            not_null: true,
            default: None,
            collation: Collation::Binary,
            class: ColumnClass::Mergeable,
        }
    }
    fn schema() -> LogicalSchema {
        LogicalSchema {
            format: LOGICAL_SCHEMA_FORMAT,
            publisher: [7; 32],
            application: "shop".into(),
            source: SchemaSource {
                kind: "room".into(),
                version: 2,
                identity: "abc".into(),
            },
            update_policy: UpdatePolicy::Changed,
            tables: vec![LogicalTable {
                sql_name: "items".into(),
                class: TableClass::Mergeable,
                create_sql: "CREATE TABLE `items` (`key` TEXT NOT NULL, `title` TEXT NOT NULL, `id` INTEGER NOT NULL, `row_kind` INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(`key`))".into(),
                auto_generate: false,
                columns: vec![column("key", Affinity::Text), column("title", Affinity::Text), column("id", Affinity::Integer), column("row_kind", Affinity::Integer)],
                primary_key: vec!["key".into()],
                partition_by: Vec::new(),
            partition_group: None,
                indexes: vec![LogicalIndex { name: "items_identity".into(), unique: true, columns: vec![IndexColumn { name: "id".into(), descending: false }], create_sql: "CREATE UNIQUE INDEX items_identity ON items(id)".into(), on_conflict: None }],
                foreign_keys: Vec::new(),

                existence_log: false,
            }],
            views: Vec::new(),
        }
    }
    #[test]
    fn sql_column_and_reference_names_use_ascii_case_insensitive_lookup() {
        let mut schema = schema();
        let table = &mut schema.tables[0];
        table.primary_key = vec!["KEY".into()];
        assert_eq!(table.column("TITLE").unwrap().sql_name, "title");
        assert_eq!(table.key().next().unwrap().sql_name, "key");
        assert!(!table.registers().any(|column| column.sql_name == "key"));
        let key = LogicalForeignKey {
            columns: vec!["link".into()],
            table: "ITEMS".into(),
            references: vec!["ID".into()],
            on_delete: DELETE_CASCADE.into(),
            on_update: "NO ACTION".into(),
        };
        table.columns.push(column("link", Affinity::Integer));
        table.create_sql.pop();
        table
            .create_sql
            .push_str(&format!(",{})", foreign_key_clause(&key)));
        table.foreign_keys.push(key);
        schema.installable().unwrap();
        schema.tables[0].foreign_keys[0].references = vec!["missing".into()];
        let clause = foreign_key_clause(&schema.tables[0].foreign_keys[0]);
        schema.tables[0].create_sql = format!("CREATE TABLE items ({clause})");
        assert_eq!(
            schema.installable(),
            Err(SchemaError::Invalid("items column missing".into()))
        );
        let mut duplicate = schema.clone();
        duplicate.tables[0]
            .columns
            .push(column("TITLE", Affinity::Text));
        assert_eq!(
            duplicate.validate(),
            Err(SchemaError::Invalid("items column TITLE".into()))
        );
    }

    #[test]
    fn local_sql_cannot_hide_an_undeclared_reference() {
        let mut schema = schema();
        schema.tables[0].class = TableClass::LocalOnly;
        schema.tables[0].create_sql = "CREATE TABLE items (id INTEGER REFERENCES items(id))".into();
        assert_eq!(
            schema.installable(),
            Err(SchemaError::Incompatible {
                table: "items".into(),
                reason: Incompatibility::ForeignKey,
            })
        );
        schema.tables[0].create_sql =
            "CREATE TABLE items (id INTEGER, FOREIGN KEY(id) REFERENCES items(id))".into();
        assert!(matches!(
            schema.installable(),
            Err(SchemaError::Incompatible {
                reason: Incompatibility::ForeignKey,
                ..
            })
        ));
        schema.tables[0].create_sql =
            "CREATE TABLE items (id INTEGER, title TEXT DEFAULT 'REFERENCES')".into();
        schema.installable().unwrap();
    }

    #[test]
    fn command_controlled_tables_install_with_their_class_signed() {
        let schema = schema();
        let mut commanded = schema.clone();
        commanded.tables[0].class = TableClass::CommandControlled;
        commanded.supported_on(crate::Profile::Secure).unwrap();
        // Mergeable shapes keep their bytes; the class is in the others.
        let domain = [DomainId([1; 16])];
        let (before, after) = (schema.manifest(&domain), commanded.manifest(&domain));
        assert!(before.entities[0].shape.contains("mergeable"));
        assert!(after.entities[0].shape.contains("command_controlled"));
        assert_eq!(before.fields, after.fields);
        assert_ne!(before.hash(), after.hash());
        assert_ne!(
            schema.replicated_fingerprint(),
            commanded.replicated_fingerprint()
        );
        let mut confirmed = schema.clone();
        confirmed.tables[0].class = TableClass::ConfirmedReadOnly;
        assert!(confirmed.supported_on(crate::Profile::Secure).is_err());
        // The same rules as a mergeable table apply.
        commanded.tables[0].auto_generate = true;
        assert!(commanded.installable().is_err());
    }
    #[test]
    fn canonical_descriptors_round_trip_and_reject_other_bytes() {
        let schema = schema();
        let bytes = schema.canonical().unwrap();
        assert_eq!(LogicalSchema::parse(&bytes).unwrap(), schema);
        let spaced = String::from_utf8(bytes.clone()).unwrap().replace(':', ": ");
        assert!(LogicalSchema::parse(spaced.as_bytes()).is_err());
        let unknown = String::from_utf8(bytes)
            .unwrap()
            .replacen('{', "{\"x\":1,", 1);
        assert!(LogicalSchema::parse(unknown.as_bytes()).is_err());
        let mut reserved = schema.clone();
        reserved.tables[0].sql_name = "rocky_items".into();
        assert!(reserved.validate().is_err());
        let mut missing = schema;
        missing.tables[0].primary_key = vec!["absent".into()];
        assert!(missing.validate().is_err());
    }
    #[test]
    fn local_details_keep_the_replicated_identity_and_renames_do_not() {
        let schema = schema();
        let mut local = schema.clone();
        local.tables[0].indexes.push(LogicalIndex {
            name: "by_title".into(),
            unique: false,
            columns: vec![IndexColumn {
                name: "title".into(),
                descending: false,
            }],
            create_sql: "CREATE INDEX by_title ON items(title)".into(),
            on_conflict: None,
        });
        local.source.identity = "def".into();
        local.installable().unwrap();
        assert_ne!(schema.fingerprint().unwrap(), local.fingerprint().unwrap());
        assert_eq!(
            schema.replicated_fingerprint(),
            local.replicated_fingerprint()
        );
        let domain = [DomainId([1; 16])];
        assert_eq!(
            schema.manifest(&domain).hash(),
            local.manifest(&domain).hash()
        );
        schema.manifest(&domain).validate().unwrap();
        // Names are identities: a renamed table or column is another item.
        let mut renamed = schema.clone();
        let table = &mut renamed.tables[0];
        table.sql_name = "shopping".into();
        table.columns[1].sql_name = "name".into();
        table.create_sql = table
            .create_sql
            .replace("items", "shopping")
            .replace("title", "name");
        renamed.installable().unwrap();
        assert_ne!(
            schema.manifest(&domain).hash(),
            renamed.manifest(&domain).hash()
        );
        assert_ne!(
            schema.field_id(&schema.tables[0], &schema.tables[0].columns[1]),
            renamed.field_id(&renamed.tables[0], &renamed.tables[0].columns[1])
        );
        // A new type or another publisher is another definition.
        let mut retyped = schema.clone();
        retyped.tables[0].columns[1].affinity = Affinity::Blob;
        assert_ne!(
            schema.replicated_fingerprint(),
            retyped.replicated_fingerprint()
        );
        let mut published = schema.clone();
        published.publisher = [8; 32];
        assert_ne!(schema.app_id(), published.app_id());
        assert_ne!(
            schema.table_id(&schema.tables[0]),
            published.table_id(&published.tables[0])
        );
    }
    #[test]
    fn schemas_that_cannot_converge_are_refused() {
        let reason = |change: &dyn Fn(&mut LogicalTable)| {
            let mut schema = schema();
            change(&mut schema.tables[0]);
            match schema.installable() {
                Err(SchemaError::Incompatible { reason, .. }) => Some(reason),
                Ok(()) => None,
                Err(error) => panic!("{error}"),
            }
        };
        assert_eq!(reason(&|_| {}), None);
        assert_eq!(
            reason(&|table| table.auto_generate = true),
            Some(Incompatibility::AutoGeneratedKey)
        );
        assert_eq!(
            reason(&|table| table.create_sql.push_str(" CHECK(1)")),
            Some(Incompatibility::CheckConstraint)
        );
        assert_eq!(
            reason(&|table| table.columns[0].not_null = false),
            Some(Incompatibility::NullableKey("key".into()))
        );
        assert_eq!(
            reason(&|table| table.columns[1].affinity = Affinity::Numeric),
            Some(Incompatibility::Affinity("title".into()))
        );
        assert_eq!(
            reason(&|table| table.columns[1].class = ColumnClass::LocalOnly),
            Some(Incompatibility::LocalColumn("title".into()))
        );
        assert_eq!(
            reason(&|table| table.foreign_keys.push(LogicalForeignKey {
                table: "items".into(),
                columns: vec!["title".into()],
                references: vec!["id".into()],
                on_delete: "NO ACTION".into(),
                on_update: "NO ACTION".into(),
            })),
            Some(Incompatibility::ForeignKey)
        );
        // The same constraints are local SQL checks on a local-only table.
        assert_eq!(
            reason(&|table| {
                table.class = TableClass::LocalOnly;
                table.auto_generate = true;
            }),
            None
        );
    }
    #[test]
    fn derived_rules_are_single_deterministic_expressions() {
        let columns = ["title", "count"];
        for rule in [
            "lower(title)",
            "trim(title) || '-' || count",
            "CASE WHEN count > 0 THEN upper(\"title\") ELSE '' END",
        ] {
            assert!(derived_rule(rule, &columns), "{rule}");
        }
        for rule in [
            "",
            "random()",
            "other",
            "(SELECT 1)",
            "lower(title); DROP TABLE items",
            "items.title",
            "lower(?)",
            "lower('open",
            "datetime('now')",
        ] {
            assert!(!derived_rule(rule, &columns), "{rule}");
        }
    }
    #[test]
    fn rules_bind_columns_and_statements_declare_collations() {
        let bind = |column: &str| (column == "title").then(|| "?2".to_string());
        assert_eq!(
            bind_rule("lower( title ) || 'title'", bind).as_deref(),
            Some("lower( ?2 ) || 'title'")
        );
        assert_eq!(bind_rule("lower(other)", bind), None);
        assert_eq!(
            declared_collations(
                "CREATE TABLE t(`a` TEXT COLLATE NOCASE NOT NULL, b TEXT, c TEXT COLLATE rtrim, PRIMARY KEY(a, b))"
            ),
            vec![
                ("a".to_string(), "NOCASE".to_string()),
                ("c".to_string(), "rtrim".to_string())
            ]
        );
    }
    #[test]
    fn affinity_follows_the_declared_type() {
        for (declared, affinity) in [
            ("INTEGER", Affinity::Integer),
            ("VARCHAR(10)", Affinity::Text),
            ("", Affinity::Blob),
            ("DOUBLE", Affinity::Real),
            ("DECIMAL", Affinity::Numeric),
        ] {
            assert_eq!(Affinity::of_declared(declared), affinity);
        }
    }
}
